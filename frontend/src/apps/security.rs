//! Security: SSH login attempts read from auth.log / journal, and a fail2ban console.
//! Reading is unprivileged; ban / unban / restart / install open a terminal with the command typed in.

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::json;
use sshos_shared::{AuthKind, Fail2banAction, Fail2banConfig, Fail2banJailConfig, SecurityReport};

use crate::icons::Icon;
use crate::state::*;
use crate::tauri::invoke;

#[derive(Clone, Copy, PartialEq)]
enum View {
    Overview,
    Attempts,
    Ips,
    Fail2ban,
    Sessions,
}

impl View {
    fn title(self) -> &'static str {
        match self {
            View::Overview => tr("Vue d'ensemble", "Overview"),
            View::Attempts => tr("Tentatives SSH", "SSH attempts"),
            View::Ips => tr("Adresses IP", "IP addresses"),
            View::Fail2ban => "Fail2ban",
            View::Sessions => tr("Sessions ouvertes", "Open sessions"),
        }
    }
}

fn kind_label(k: AuthKind) -> (&'static str, &'static str) {
    match k {
        AuthKind::Failed => ("crit", tr("Échec", "Failed")),
        AuthKind::InvalidUser => ("warn", tr("Utilisateur inconnu", "Invalid user")),
        AuthKind::Accepted => ("ok", tr("Acceptée", "Accepted")),
        AuthKind::Probe => ("info", tr("Scan", "Probe")),
    }
}

#[derive(Clone, Copy)]
struct S {
    c: Ctx,
    host: StoredValue<String>,
    report: RwSignal<Option<SecurityReport>>,
    error: RwSignal<Option<String>>,
    loading: RwSignal<bool>,
    /// Visual fail2ban editor: open flag, [DEFAULT] and one signal per jail.
    editing: RwSignal<bool>,
    /// The component's owner: the per-jail signals are created from click handlers, whose
    /// own owner (the view being replaced) is disposed when the editor opens.
    owner: StoredValue<Owner>,
    cfg_defaults: RwSignal<Fail2banJailConfig>,
    cfg_jails: RwSignal<Vec<RwSignal<Fail2banJailConfig>>>,
}

/// Jails shipped with fail2ban that are worth offering in the editor.
const PRESET_JAILS: &[&str] = &[
    "sshd", "recidive", "nginx-http-auth", "nginx-botsearch", "nginx-limit-req", "nginx-bad-request",
    "apache-auth", "apache-badbots", "postfix", "dovecot", "vsftpd", "proftpd", "mysqld-auth",
];
const TIME_PRESETS: &[&str] = &["1m", "5m", "10m", "30m", "1h", "6h", "12h", "1d", "1w", "-1"];

impl S {
    fn h(&self) -> String {
        self.host.get_value()
    }

    fn reload(self) {
        if self.loading.get_untracked() {
            return;
        }
        self.loading.set(true);
        let h = self.h();
        spawn_local(async move {
            match invoke::<SecurityReport>("security_report", json!({ "hostId": h })).await {
                Ok(r) => {
                    let _ = self.report.try_set(Some(r));
                    let _ = self.error.try_set(None);
                }
                Err(e) => {
                    let _ = self.error.try_set(Some(e.message()));
                }
            }
            let _ = self.loading.try_set(false);
        });
    }

    /// Jail used to ban an IP by hand: sshd when it exists, else the first one.
    fn ban_jail(self) -> String {
        self.report.with_untracked(|r| {
            r.as_ref()
                .and_then(|r| {
                    let j = &r.fail2ban.jails;
                    j.iter().find(|j| j.name == "sshd").or(j.first()).map(|j| j.name.clone())
                })
                .unwrap_or_else(|| "sshd".into())
        })
    }

    fn jail_of(self, ip: &str) -> String {
        self.report
            .with_untracked(|r| {
                r.as_ref()
                    .and_then(|r| r.fail2ban.jails.iter().find(|j| j.banned.iter().any(|b| b == ip)).map(|j| j.name.clone()))
            })
            .unwrap_or_else(|| self.ban_jail())
    }

    /// Show the fail2ban command, then type it in a terminal (it needs sudo).
    fn f2b(self, action: Fail2banAction, jail: Option<String>, ip: Option<String>) {
        let c = self.c;
        let h = self.h();
        spawn_local(async move {
            let cmd = match invoke::<String>("fail2ban_command", json!({ "action": action, "jail": jail, "ip": ip })).await {
                Ok(cmd) => cmd,
                Err(e) => return c.error(e.message()),
            };
            let title = match action {
                Fail2banAction::Ban => format!("{} {}", tr("Bannir", "Ban"), ip.clone().unwrap_or_default()),
                Fail2banAction::Unban => format!("{} {}", tr("Débannir", "Unban"), ip.clone().unwrap_or_default()),
                Fail2banAction::Restart => tr("Redémarrer fail2ban", "Restart fail2ban").to_string(),
                Fail2banAction::Install => tr("Installer fail2ban", "Install fail2ban").to_string(),
            };
            let cmd2 = cmd.clone();
            let d = Dialog::confirm(
                title,
                tr(
                    "Cette commande demande sudo : elle sera tapée dans un terminal, vous la validez avec Entrée.",
                    "This command needs sudo: it is typed in a terminal, you confirm it with Enter.",
                ),
                tr("Ouvrir le terminal", "Open terminal"),
                move || c.run_in_terminal(&h, &cmd2),
            )
            .code(cmd);
            c.show(if action == Fail2banAction::Ban { d.danger() } else { d });
        });
    }
}

impl S {
    /// Fill the editor from what the last report read on the server.
    fn start_edit(self) {
        let f = self.report.with_untracked(|r| r.as_ref().map(|r| r.fail2ban.clone()));
        let Some(f) = f else { return };
        self.cfg_defaults.set(f.defaults.clone());
        let jails = self.owner.with_value(|o| o.clone()).with(|| {
            f.jails
                .iter()
                .map(|j| {
                    RwSignal::new(Fail2banJailConfig {
                        name: j.name.clone(),
                        enabled: j.enabled,
                        maxretry: j.maxretry.clone(),
                        findtime: j.findtime.clone(),
                        bantime: j.bantime.clone(),
                        port: j.port.clone(),
                        ignoreip: String::new(),
                    })
                })
                .collect::<Vec<_>>()
        });
        self.cfg_jails.set(jails);
        self.editing.set(true);
    }

    fn add_jail(self, name: String) {
        if name.is_empty() || self.cfg_jails.with_untracked(|v| v.iter().any(|j| j.with_untracked(|j| j.name == name))) {
            return;
        }
        let port = match name.as_str() {
            "sshd" => "ssh",
            n if n.starts_with("nginx") || n.starts_with("apache") => "http,https",
            "vsftpd" | "proftpd" => "ftp,ftp-data,ftps,ftps-data",
            "postfix" => "smtp,465,submission",
            "dovecot" => "pop3,pop3s,imap,imaps,submission,465,sieve",
            "mysqld-auth" => "3306",
            _ => "",
        };
        let sig = self.owner.with_value(|o| o.clone()).with(|| {
            RwSignal::new(Fail2banJailConfig { name, enabled: true, port: port.into(), ..Default::default() })
        });
        self.cfg_jails.update(|v| v.push(sig));
    }

    /// The visitor's IP, read from the newest `who` line "(1.2.3.4)".
    fn my_ip(self) -> Option<String> {
        self.report.with_untracked(|r| {
            r.as_ref()?.sessions.iter().rev().find_map(|l| {
                let a = l.rfind('(')?;
                let b = l.rfind(')')?;
                let ip = l.get(a + 1..b)?.trim();
                (ip.chars().any(|c| c.is_ascii_digit()) && !ip.contains(' ')).then(|| ip.to_string())
            })
        })
    }

    /// Show the exact command, then type it in a terminal (it writes under /etc and needs sudo).
    fn apply_config(self) {
        let c = self.c;
        let h = self.h();
        let config = Fail2banConfig {
            defaults: self.cfg_defaults.get_untracked(),
            jails: self.cfg_jails.get_untracked().iter().map(|j| j.get_untracked()).collect(),
        };
        spawn_local(async move {
            let cmd = match invoke::<String>("fail2ban_config_command", json!({ "config": config })).await {
                Ok(cmd) => cmd,
                Err(e) => return c.error(e.message()),
            };
            let cmd2 = cmd.clone();
            let d = Dialog::confirm(
                tr("Appliquer la configuration fail2ban", "Apply fail2ban configuration"),
                tr(
                    "Écrit /etc/fail2ban/jail.d/zz-sshos.local, vérifie la configuration (fail2ban-client -t) puis recharge fail2ban. Si la vérification échoue, l'ancien fichier est restauré. La commande demande sudo : elle est tapée dans un terminal, vous la validez avec Entrée.",
                    "Writes /etc/fail2ban/jail.d/zz-sshos.local, checks the configuration (fail2ban-client -t) then reloads fail2ban. If the check fails, the previous file is restored. It needs sudo: it is typed in a terminal, you confirm it with Enter.",
                ),
                tr("Ouvrir le terminal", "Open terminal"),
                move || {
                    c.run_in_terminal(&h, &cmd2);
                    self.editing.set(false);
                },
            )
            .code(cmd);
            c.show(d);
        });
    }
}

/// A text input bound to one field of a jail signal.
fn cfg_input(
    sig: RwSignal<Fail2banJailConfig>,
    get: fn(&Fail2banJailConfig) -> String,
    set: fn(&mut Fail2banJailConfig, String),
    placeholder: String,
    list: Option<&'static str>,
) -> impl IntoView {
    view! {
        <input
            class="mono"
            placeholder=placeholder
            list=list
            prop:value=move || sig.with(get)
            on:input=move |e| {
                let v = event_target_value(&e);
                sig.update(|c| set(c, v));
            }
        />
    }
}

fn f2b_editor(s: S) -> impl IntoView {
    let d = s.cfg_defaults;
    let new_jail = RwSignal::new(String::new());
    let inherit = |v: String| if v.is_empty() { tr("hérité", "inherited").to_string() } else { v };
    let jails = move || {
        s.cfg_jails
            .get()
            .into_iter()
            .map(|j| {
                let name = j.with_untracked(|j| j.name.clone());
                let (dm, df, db) = d.with_untracked(|d| (d.maxretry.clone(), d.findtime.clone(), d.bantime.clone()));
                view! {
                    <div class="cfg-jail" class:off=move || !j.with(|j| j.enabled)>
                        <div class="cfg-jail-head">
                            <Icon name="lock" size=14 />
                            <span class="strong grow">{name}</span>
                            <button
                                class="switch"
                                class:on=move || j.with(|j| j.enabled)
                                title=tr("Activer / désactiver", "Enable / disable")
                                on:click=move |_| j.update(|j| j.enabled = !j.enabled)
                            >
                                <span></span>
                            </button>
                        </div>
                        <div class="cfg-grid">
                            <label>"maxretry"{cfg_input(j, |c| c.maxretry.clone(), |c, v| c.maxretry = v, inherit(dm), None)}</label>
                            <label>"findtime"{cfg_input(j, |c| c.findtime.clone(), |c, v| c.findtime = v, inherit(df), Some("f2b-times"))}</label>
                            <label>"bantime"{cfg_input(j, |c| c.bantime.clone(), |c, v| c.bantime = v, inherit(db), Some("f2b-times"))}</label>
                            <label>"port"{cfg_input(j, |c| c.port.clone(), |c, v| c.port = v, "ssh, 2222…".into(), None)}</label>
                        </div>
                    </div>
                }
            })
            .collect_view()
    };
    let presets = move || {
        let have: Vec<String> = s.cfg_jails.get().iter().map(|j| j.with_untracked(|j| j.name.clone())).collect();
        PRESET_JAILS
            .iter()
            .filter(|p| !have.iter().any(|h| h == *p))
            .map(|p| view! { <option value=*p>{*p}</option> })
            .collect_view()
    };
    view! {
        <datalist id="f2b-times">
            {TIME_PRESETS.iter().map(|t| view! { <option value=*t></option> }).collect_view()}
        </datalist>
        <div class="panel-card f2b-head">
            <Icon name="sliders" size=22 />
            <div class="grow">
                <div class="strong">{tr("Configurer fail2ban", "Configure fail2ban")}</div>
                <div class="muted small">
                    {tr("Durées : 10m, 1h, 1d, 1w… · -1 = bannissement permanent · vide = valeur héritée", "Durations: 10m, 1h, 1d, 1w… · -1 = permanent ban · empty = inherited value")}
                </div>
            </div>
            <button on:click=move |_| s.editing.set(false)>{tr("Annuler", "Cancel")}</button>
            <button class="primary" on:click=move |_| s.apply_config()>
                <Icon name="check" size=14 />{tr("Appliquer…", "Apply…")}
            </button>
        </div>
        <div class="panel-card">
            <div class="panel-head">
                <h4><Icon name="shield" size=14 />{tr("Valeurs par défaut", "Defaults")}" · [DEFAULT]"</h4>
            </div>
            <div class="cfg-grid">
                <label>"maxretry"{cfg_input(d, |c| c.maxretry.clone(), |c, v| c.maxretry = v, "5".into(), None)}</label>
                <label>"findtime"{cfg_input(d, |c| c.findtime.clone(), |c, v| c.findtime = v, "10m".into(), Some("f2b-times"))}</label>
                <label>"bantime"{cfg_input(d, |c| c.bantime.clone(), |c, v| c.bantime = v, "10m".into(), Some("f2b-times"))}</label>
            </div>
            <label class="cfg-wide">
                <span>{tr("IP jamais bannies (ignoreip)", "Never banned IPs (ignoreip)")}</span>
                <div class="row gap">
                    {cfg_input(d, |c| c.ignoreip.clone(), |c, v| c.ignoreip = v, "127.0.0.1/8 ::1".into(), None)}
                    {s.my_ip().map(|ip| {
                        let label = format!("{} {ip}", tr("+ mon IP", "+ my IP"));
                        view! {
                            <button class="small" title=tr("Ajouter l'IP de votre session SSH", "Add the IP of your SSH session") on:click=move |_| {
                                d.update(|c| {
                                    if !c.ignoreip.split_whitespace().any(|x| x == ip) {
                                        c.ignoreip = format!("{} {ip}", c.ignoreip.trim()).trim().to_string();
                                    }
                                })
                            }>{label.clone()}</button>
                        }
                    })}
                </div>
            </label>
        </div>
        <div class="panel-card">
            <div class="panel-head">
                <h4><Icon name="layers" size=14 />"Jails"</h4>
                <div class="row gap">
                    <select prop:value=move || new_jail.get() on:change=move |e| new_jail.set(event_target_value(&e))>
                        <option value="">{tr("Ajouter une jail…", "Add a jail…")}</option>
                        {presets}
                    </select>
                    <button class="small" disabled=move || new_jail.with(|n| n.is_empty()) on:click=move |_| {
                        s.add_jail(new_jail.get_untracked());
                        new_jail.set(String::new());
                    }>
                        <Icon name="plus" size=13 />{tr("Ajouter", "Add")}
                    </button>
                </div>
            </div>
            <div class="cfg-jails">{jails}</div>
        </div>
    }
}

fn hourly_chart(buckets: &[(String, u32)]) -> impl IntoView {
    let max = buckets.iter().map(|(_, n)| *n).max().unwrap_or(0).max(1);
    let n = buckets.len().max(1) as f64;
    let w = 600.0 / n;
    let bars = buckets
        .iter()
        .enumerate()
        .map(|(i, (label, v))| {
            let h = (*v as f64 / max as f64) * 110.0;
            view! {
                <rect class="hbar" x=format!("{:.1}", i as f64 * w + 1.0) y=format!("{:.1}", 120.0 - h) width=format!("{:.1}", (w - 2.0).max(1.0)) height=format!("{h:.1}") rx="2">
                    <title>{format!("{label} · {v}")}</title>
                </rect>
            }
        })
        .collect_view();
    let first = buckets.first().map(|b| b.0.clone()).unwrap_or_default();
    let last = buckets.last().map(|b| b.0.clone()).unwrap_or_default();
    view! {
        <svg class="hour-chart" viewBox="0 0 600 120" preserveAspectRatio="none">{bars}</svg>
        <div class="chart-axis muted small">
            <span>{first}</span>
            <span>{format!("max {max}/h")}</span>
            <span>{last}</span>
        </div>
    }
}

#[component]
pub fn Security(host: String) -> impl IntoView {
    let c = ctx();
    let s = S {
        c,
        host: StoredValue::new(host),
        report: RwSignal::new(None),
        error: RwSignal::new(None),
        loading: RwSignal::new(false),
        editing: RwSignal::new(false),
        owner: StoredValue::new(Owner::current().expect("component owner")),
        cfg_defaults: RwSignal::new(Fail2banJailConfig::default()),
        cfg_jails: RwSignal::new(Vec::new()),
    };
    let view_sig = RwSignal::new(View::Overview);
    let search = RwSignal::new(String::new());
    let kind = RwSignal::new(String::from("attempts"));
    s.reload();
    if let Ok(h) = set_interval_with_handle(move || s.reload(), std::time::Duration::from_secs(30)) {
        on_cleanup(move || h.clear());
    }

    let r = move || s.report.get();
    let num = move |f: fn(&SecurityReport) -> usize| move || s.report.with(|r| r.as_ref().map(f).unwrap_or(0));
    let side_item = move |v: View, icon: &'static str, n: Option<Box<dyn Fn() -> usize + Send + Sync>>| {
        view! {
            <button class="side-item" class:active=move || view_sig.get() == v on:click=move |_| view_sig.set(v)>
                <Icon name=icon size=15 />
                <span class="grow">{v.title()}</span>
                {n.map(|n| view! { <span class="side-count">{move || n()}</span> })}
            </button>
        }
    };

    let sidebar = view! {
        <aside class="side drag-region">
            <div class="side-top"></div>
            <div class="side-title">"SSH"</div>
            {side_item(View::Overview, "home", None)}
            {side_item(View::Attempts, "list", Some(Box::new(num(|r| (r.failed + r.invalid_user) as usize))))}
            {side_item(View::Ips, "globe", Some(Box::new(num(|r| r.top_ips.len()))))}
            {side_item(View::Sessions, "users", Some(Box::new(num(|r| r.sessions.len()))))}
            <div class="side-title">{tr("Protection", "Protection")}</div>
            {side_item(View::Fail2ban, "shield-check", Some(Box::new(num(|r| r.fail2ban.jails.iter().map(|j| j.banned.len()).sum()))))}
            <div class="grow"></div>
            <div class="side-foot muted small">{move || s.report.with(|r| r.as_ref().map(|r| r.source.clone()).unwrap_or_default())}</div>
        </aside>
    };

    let stat_card = |icon: &'static str, label: &'static str, value: String, cls: &'static str| {
        view! {
            <div class=format!("card {cls}")>
                <div class="card-label"><Icon name=icon size=13 />{label}</div>
                <div class="card-value">{value}</div>
            </div>
        }
    };

    let overview = move || {
        r().map(|r| {
            let banned: usize = r.fail2ban.jails.iter().map(|j| j.banned.len()).sum();
            let max_ip = r.top_ips.first().map(|i| i.attempts).unwrap_or(1).max(1);
            let max_user = r.top_users.first().map(|u| u.1).unwrap_or(1).max(1);
            view! {
                <div class="cards five">
                    {stat_card("alert", tr("Échecs", "Failed"), r.failed.to_string(), "c-red")}
                    {stat_card("users", tr("Utilisateurs inconnus", "Invalid users"), r.invalid_user.to_string(), "c-orange")}
                    {stat_card("globe", tr("IP distinctes", "Unique IPs"), r.top_ips.len().to_string(), "c-blue")}
                    {stat_card("ban", tr("Bannies", "Banned"), banned.to_string(), "c-purple")}
                    {stat_card("check", tr("Connexions OK", "Accepted"), r.accepted.to_string(), "c-green")}
                </div>
                <div class="panel-card">
                    <div class="panel-head">
                        <h4>{tr("Tentatives par heure", "Attempts per hour")}</h4>
                        <span class="muted small">{format!("{} → {}", r.first, r.last)}</span>
                    </div>
                    {hourly_chart(&r.hourly)}
                </div>
                <div class="two-col">
                    <div class="panel-card">
                        <div class="panel-head"><h4>{tr("IP les plus actives", "Top IPs")}</h4></div>
                        {r.top_ips
                            .iter()
                            .take(10)
                            .map(|i| {
                                view! {
                                    <div class="rank-row">
                                        <span class="mono">{i.ip.clone()}</span>
                                        {i.banned.then(|| view! { <span class="pill crit">{tr("bannie", "banned")}</span> })}
                                        <span class="rank-bar"><span style=format!("width:{:.0}%", i.attempts as f64 * 100.0 / max_ip as f64)></span></span>
                                        <span class="num">{i.attempts}</span>
                                    </div>
                                }
                            })
                            .collect_view()}
                    </div>
                    <div class="panel-card">
                        <div class="panel-head"><h4>{tr("Utilisateurs visés", "Targeted users")}</h4></div>
                        {r.top_users
                            .iter()
                            .take(10)
                            .map(|(u, n)| {
                                view! {
                                    <div class="rank-row">
                                        <span class="mono">{u.clone()}</span>
                                        <span class="rank-bar"><span style=format!("width:{:.0}%", *n as f64 * 100.0 / max_user as f64)></span></span>
                                        <span class="num">{*n}</span>
                                    </div>
                                }
                            })
                            .collect_view()}
                    </div>
                </div>
                <p class="muted small">
                    {format!("{} {} · {} {}", r.probes, tr("scans sans authentification", "unauthenticated probes"), r.events.len(), tr("derniers évènements affichés", "latest events shown"))}
                </p>
            }
        })
    };

    let ips = move || {
        r().map(|r| {
            let q = search.get().to_lowercase();
            let has_f2b = r.fail2ban.installed;
            view! {
                <div class="table-card">
                    <table class="table data-table">
                        <thead>
                            <tr>
                                <th>"IP"</th>
                                <th class="num">{tr("Tentatives", "Attempts")}</th>
                                <th>{tr("Utilisateurs", "Users")}</th>
                                <th>{tr("Dernière", "Last")}</th>
                                <th>{tr("État", "Status")}</th>
                                <th class="right">"Actions"</th>
                            </tr>
                        </thead>
                        <tbody>
                            {r.top_ips
                                .into_iter()
                                .filter(|i| q.is_empty() || i.ip.contains(&q) || i.users.iter().any(|u| u.to_lowercase().contains(&q)))
                                .map(|i| {
                                    let ip = i.ip.clone();
                                    let users = if i.users.len() > 4 {
                                        format!("{} +{}", i.users[..4].join(", "), i.users.len() - 4)
                                    } else {
                                        i.users.join(", ")
                                    };
                                    view! {
                                        <tr>
                                            <td class="mono">{i.ip.clone()}</td>
                                            <td class="num">{i.attempts}</td>
                                            <td class="ellipsis small" title=i.users.join(", ")>{users}</td>
                                            <td class="muted small">{i.last.clone()}</td>
                                            <td>
                                                {if i.banned {
                                                    view! { <span class="pill crit">{tr("Bannie", "Banned")}</span> }
                                                } else {
                                                    view! { <span class="pill off">{tr("Libre", "Free")}</span> }
                                                }}
                                            </td>
                                            <td class="row-actions">
                                                {has_f2b
                                                    .then(|| {
                                                        if i.banned {
                                                            view! {
                                                                <button class="small" on:click=move |_| s.f2b(Fail2banAction::Unban, Some(s.jail_of(&ip)), Some(ip.clone()))>
                                                                    {tr("Débannir", "Unban")}
                                                                </button>
                                                            }
                                                                .into_any()
                                                        } else {
                                                            view! {
                                                                <button class="small danger" on:click=move |_| s.f2b(Fail2banAction::Ban, Some(s.ban_jail()), Some(ip.clone()))>
                                                                    <Icon name="ban" size=12 />{tr("Bannir", "Ban")}
                                                                </button>
                                                            }
                                                                .into_any()
                                                        }
                                                    })}
                                            </td>
                                        </tr>
                                    }
                                })
                                .collect_view()}
                        </tbody>
                    </table>
                </div>
            }
        })
    };

    let attempts = move || {
        r().map(|r| {
            let q = search.get().to_lowercase();
            let k = kind.get();
            let rows: Vec<_> = r
                .events
                .into_iter()
                .filter(|e| match k.as_str() {
                    "attempts" => matches!(e.kind, AuthKind::Failed | AuthKind::InvalidUser),
                    "failed" => e.kind == AuthKind::Failed,
                    "invalid" => e.kind == AuthKind::InvalidUser,
                    "accepted" => e.kind == AuthKind::Accepted,
                    "probe" => e.kind == AuthKind::Probe,
                    _ => true,
                })
                .filter(|e| q.is_empty() || e.ip.contains(&q) || e.user.to_lowercase().contains(&q))
                .collect();
            let empty = rows.is_empty();
            view! {
                <div class="filter-row">
                    <select class="seg" on:change=move |e| kind.set(event_target_value(&e)) prop:value=move || kind.get()>
                        <option value="attempts">{tr("Tentatives (échecs + inconnus)", "Attempts (failed + invalid)")}</option>
                        <option value="failed">{tr("Échecs", "Failed")}</option>
                        <option value="invalid">{tr("Utilisateurs inconnus", "Invalid users")}</option>
                        <option value="accepted">{tr("Connexions acceptées", "Accepted logins")}</option>
                        <option value="probe">{tr("Scans", "Probes")}</option>
                        <option value="all">{tr("Tout", "Everything")}</option>
                    </select>
                </div>
                <div class="table-card">
                    <table class="table data-table">
                        <thead>
                            <tr>
                                <th>{tr("Date", "Time")}</th>
                                <th>{tr("Type", "Kind")}</th>
                                <th>{tr("Utilisateur", "User")}</th>
                                <th>"IP"</th>
                                <th>{tr("Méthode", "Method")}</th>
                            </tr>
                        </thead>
                        <tbody>
                            {rows
                                .into_iter()
                                .map(|e| {
                                    let (cls, label) = kind_label(e.kind);
                                    view! {
                                        <tr>
                                            <td class="muted small mono">{e.time}</td>
                                            <td><span class=format!("pill {cls}")>{label}</span></td>
                                            <td class="mono">{e.user}</td>
                                            <td class="mono">{e.ip}</td>
                                            <td class="muted small">{e.method}</td>
                                        </tr>
                                    }
                                })
                                .collect_view()}
                        </tbody>
                    </table>
                    {empty.then(|| view! { <div class="empty">{tr("Aucun évènement", "No events")}</div> })}
                </div>
            }
        })
    };

    let fail2ban = move || {
        r().map(|r| {
            let f = r.fail2ban;
            let status = if !f.installed {
                ("off", tr("Non installé", "Not installed"))
            } else if f.running {
                ("ok", tr("Actif", "Running"))
            } else {
                ("crit", tr("Arrêté", "Stopped"))
            };
            let source = if f.source == "client" {
                tr("état exact (fail2ban-client)", "exact state (fail2ban-client)")
            } else {
                tr("estimé depuis fail2ban.log", "estimated from fail2ban.log")
            };
            let installed = f.installed;
            view! {
                <div class="panel-card f2b-head">
                    <Icon name="shield-check" size=22 />
                    <div class="grow">
                        <div class="strong">"fail2ban " <span class=format!("pill {}", status.0)>{status.1}</span></div>
                        <div class="muted small">{if installed { source } else { tr("Bannit les IP qui échouent trop souvent à se connecter.", "Bans IPs that fail to log in too often.") }}</div>
                    </div>
                    {if installed {
                        view! {
                            <button on:click=move |_| s.start_edit()>
                                <Icon name="sliders" size=14 />{tr("Configurer", "Configure")}
                            </button>
                            <button on:click=move |_| s.f2b(Fail2banAction::Restart, None, None)>
                                <Icon name="rotate" size=14 />{tr("Redémarrer", "Restart")}
                            </button>
                        }
                            .into_any()
                    } else {
                        view! {
                            <button class="primary" on:click=move |_| s.f2b(Fail2banAction::Install, None, None)>
                                <Icon name="download" size=14 />{tr("Installer", "Install")}
                            </button>
                        }
                            .into_any()
                    }}
                </div>
                {f.hint.clone().map(|h| view! { <div class="banner warn"><Icon name="alert" size=16 /><span>{h}</span></div> })}
                <div class="jail-grid">
                    {f.jails
                        .into_iter()
                        .map(|j| {
                            let jail = j.name.clone();
                            view! {
                                <div class="panel-card jail">
                                    <div class="panel-head">
                                        <h4><Icon name="lock" size=14 />{j.name.clone()}</h4>
                                        {if j.enabled {
                                            view! { <span class="pill ok">{tr("activée", "enabled")}</span> }
                                        } else {
                                            view! { <span class="pill off">{tr("désactivée", "disabled")}</span> }
                                        }}
                                    </div>
                                    <div class="jail-stats">
                                        <div><span class="big">{j.banned.len()}</span><span class="muted small">{tr("bannies", "banned")}</span></div>
                                        <div><span class="big">{j.total_bans}</span><span class="muted small">{tr("bans au total", "total bans")}</span></div>
                                        <div><span class="big">{j.failures}</span><span class="muted small">{tr("échecs", "failures")}</span></div>
                                    </div>
                                    <div class="kv small">
                                        <span>"maxretry"</span><b>{j.maxretry.clone()}</b>
                                        <span>"findtime"</span><b>{j.findtime.clone()}</b>
                                        <span>"bantime"</span><b>{j.bantime.clone()}</b>
                                        {(!j.port.is_empty()).then(|| view! { <span>"port"</span><b>{j.port.clone()}</b> })}
                                    </div>
                                    {j.banned
                                        .into_iter()
                                        .map(|ip| {
                                            let (jl, ip2) = (jail.clone(), ip.clone());
                                            view! {
                                                <div class="banned-row">
                                                    <Icon name="ban" size=12 />
                                                    <span class="mono grow">{ip}</span>
                                                    <button class="small" on:click=move |_| s.f2b(Fail2banAction::Unban, Some(jl.clone()), Some(ip2.clone()))>
                                                        {tr("Débannir", "Unban")}
                                                    </button>
                                                </div>
                                            }
                                        })
                                        .collect_view()}
                                </div>
                            }
                        })
                        .collect_view()}
                </div>
                <div class="two-col">
                    <div class="panel-card">
                        <div class="panel-head"><h4>{tr("Derniers bans", "Recent bans")}</h4></div>
                        {f.recent
                            .into_iter()
                            .take(30)
                            .map(|b| {
                                let cls = if b.action == "ban" { "crit" } else { "ok" };
                                view! {
                                    <div class="rank-row">
                                        <span class="muted small mono">{b.time}</span>
                                        <span class=format!("pill {cls}")>{b.action}</span>
                                        <span class="mono grow">{b.ip}</span>
                                        <span class="muted small">{b.jail}</span>
                                    </div>
                                }
                            })
                            .collect_view()}
                    </div>
                    <div class="panel-card">
                        <div class="panel-head"><h4>{tr("Configuration", "Configuration")}</h4></div>
                        {f.config_files
                            .into_iter()
                            .map(|p| {
                                let p2 = p.clone();
                                view! {
                                    <button class="file-row" on:click=move |_| c.open(AppKind::Editor { path: p2.clone(), nginx: false }, Some(s.h()))>
                                        <Icon name="file" size=14 />
                                        <span class="mono">{p}</span>
                                    </button>
                                }
                            })
                            .collect_view()}
                    </div>
                </div>
            }
        })
    };

    let sessions = move || {
        r().map(|r| {
            let empty = r.sessions.is_empty();
            view! {
                <div class="table-card">
                    {r.sessions
                        .into_iter()
                        .map(|l| view! { <div class="session-row"><Icon name="terminal" size=14 /><span class="mono">{l}</span></div> })
                        .collect_view()}
                    {empty.then(|| view! { <div class="empty">{tr("Aucune session", "No sessions")}</div> })}
                </div>
            }
        })
    };

    app_menus(vec![
        MenuSection::new(
            tr("Sécurité", "Security"),
            vec![MenuItem::new(tr("Actualiser", "Refresh"), move || s.reload())],
        ),
        MenuSection::new(
            tr("Présentation", "View"),
            vec![
                MenuItem::new(View::Overview.title(), move || view_sig.set(View::Overview)).checked(move || view_sig.get() == View::Overview),
                MenuItem::new(View::Attempts.title(), move || view_sig.set(View::Attempts)).checked(move || view_sig.get() == View::Attempts),
                MenuItem::new(View::Ips.title(), move || view_sig.set(View::Ips)).checked(move || view_sig.get() == View::Ips),
                MenuItem::new(View::Sessions.title(), move || view_sig.set(View::Sessions)).checked(move || view_sig.get() == View::Sessions),
                MenuItem::new(View::Fail2ban.title(), move || view_sig.set(View::Fail2ban)).checked(move || view_sig.get() == View::Fail2ban),
            ],
        ),
    ]);

    view! {
        <div class="split-app">
            {sidebar}
            <section class="main">
                <header class="main-head drag-region">
                    <h2>{move || view_sig.get().title()}</h2>
                    <div class="grow"></div>
                    <Show when=move || matches!(view_sig.get(), View::Attempts | View::Ips)>
                        <label class="search-field">
                            <Icon name="search" size=13 />
                            <input placeholder=tr("IP ou utilisateur", "IP or user") bind:value=search />
                        </label>
                    </Show>
                    <button class="icon-btn" class:spin=move || s.loading.get() title=tr("Actualiser", "Refresh") on:click=move |_| s.reload()>
                        <Icon name="refresh" size=15 />
                    </button>
                </header>
                <div class="main-body">
                    {move || s.error.get().map(|e| view! { <div class="banner crit"><Icon name="alert" size=16 /><span>{e}</span></div> })}
                    {move || {
                        s.report
                            .with(|r| r.as_ref().and_then(|r| r.hint.clone().map(|h| (h, r.fix_command.clone()))))
                            .map(|(hint, fix)| {
                                view! {
                                    <div class="banner warn">
                                        <Icon name="alert" size=16 />
                                        <span class="grow">{hint}</span>
                                        {fix.map(|cmd| {
                                            view! {
                                                <button class="small" on:click=move |_| c.run_in_terminal(&s.h(), &cmd)>
                                                    {tr("Corriger dans le terminal", "Fix in terminal")}
                                                </button>
                                            }
                                        })}
                                    </div>
                                }
                            })
                    }}
                    <Show when=move || s.report.with(|r| r.is_none()) && s.error.with(|e| e.is_none())>
                        <div class="empty">{tr("Analyse des journaux…", "Reading logs…")}</div>
                    </Show>
                    {move || match view_sig.get() {
                        View::Overview => overview().into_any(),
                        View::Attempts => attempts().into_any(),
                        View::Ips => ips().into_any(),
                        View::Fail2ban if s.editing.get() => f2b_editor(s).into_any(),
                        View::Fail2ban => fail2ban().into_any(),
                        View::Sessions => sessions().into_any(),
                    }}
                </div>
            </section>
        </div>
    }
}
