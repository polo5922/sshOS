//! Nginx: native sites (config files, `nginx -t` before every reload) and Nginx Proxy Manager
//! (REST API tunnelled through SSH) in one list.

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::{json, Value};
use sshos_shared::{
    ErrorPayload, NewSiteKind, NewSiteRequest, NginxDetect, NginxLayout, NginxPlan, NginxSite, NpmLocation,
    NpmOverview, NpmProxyHost,
};

use crate::icons::Icon;
use crate::state::*;
use crate::tauri::{invoke, invoke_unit};

/// Show the exact root command, then run it (directly as root, or through a pre-typed terminal).
pub fn apply_plan(c: Ctx, host: String, plan: NginxPlan, after: Callback<()>) {
    let cmd = plan.command.clone();
    c.show(
        Dialog::confirm(
            tr("Appliquer la configuration", "Apply configuration"),
            tr(
                "Commande exécutée en root. nginx -t est lancé avant le rechargement ; en cas d'échec la sauvegarde est restaurée.",
                "Command run as root. nginx -t runs before the reload; on failure the backup is restored.",
            ),
            tr("Appliquer", "Apply"),
            move || {
                let (h, plan) = (host.clone(), plan.clone());
                spawn_local(async move {
                    match invoke::<String>("nginx_apply", json!({ "hostId": h, "plan": plan })).await {
                        Ok(_) => {
                            c.ok(tr("Configuration appliquée, nginx rechargé.", "Configuration applied, nginx reloaded."));
                            after.run(());
                        }
                        Err(e) => c.handle_err(Some(&h), e),
                    }
                });
            },
        )
        .code(cmd),
    );
}

#[derive(Clone, PartialEq)]
enum NpmState {
    Absent,
    Connecting,
    NeedLogin(String),
    Ready,
    Error(String),
}

/// Which installation this server's Nginx app manages, remembered per server.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Source {
    Native,
    Npm,
    Both,
}

impl Source {
    fn key(host: &str) -> String {
        format!("sshos.nginx.{host}")
    }
    fn load(host: &str) -> Option<Self> {
        match storage_get(&Self::key(host)).as_deref() {
            Some("native") => Some(Self::Native),
            Some("npm") => Some(Self::Npm),
            Some("both") => Some(Self::Both),
            _ => None,
        }
    }
    fn save(self, host: &str) {
        storage_set(&Self::key(host), match self {
            Self::Native => "native",
            Self::Npm => "npm",
            Self::Both => "both",
        });
    }
    fn native(self) -> bool {
        self != Self::Npm
    }
    fn npm(self) -> bool {
        self != Self::Native
    }
}

/// The detection restricted to the chosen source.
fn restrict(mut d: NginxDetect, src: Source) -> NginxDetect {
    if !src.native() {
        d.native = None;
    }
    if !src.npm() {
        d.npm = None;
    }
    d
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Sites,
    Certs,
    Redirects,
    Streams,
}

#[derive(Clone)]
enum Sheet {
    None,
    NewNative,
    Quick(NginxSite),
    Proxy(Option<NpmProxyHost>),
    NewCert,
    NewRedirect,
    NewStream,
}

#[derive(Clone, Copy)]
struct N {
    c: Ctx,
    host: StoredValue<String>,
    det: RwSignal<Option<NginxDetect>>,
    /// Detection before applying the source choice.
    raw: RwSignal<Option<NginxDetect>>,
    source: RwSignal<Option<Source>>,
    sites: RwSignal<Vec<NginxSite>>,
    npm: RwSignal<NpmState>,
    loc: RwSignal<Option<NpmLocation>>,
    ov: RwSignal<NpmOverview>,
    sheet: RwSignal<Sheet>,
    loading: RwSignal<bool>,
}

impl N {
    fn h(&self) -> String {
        self.host.get_value()
    }

    fn refresh_native(self) {
        let Some(native) = self.det.with_untracked(|d| d.as_ref().and_then(|d| d.native.clone())) else { return };
        spawn_local(async move {
            match invoke::<Vec<NginxSite>>("nginx_sites", json!({ "hostId": self.h(), "native": native })).await {
                Ok(s) => self.sites.set(s),
                Err(e) => self.c.handle_err(Some(&self.h()), e),
            }
        });
    }

    fn refresh_npm(self) {
        spawn_local(async move {
            match invoke::<NpmOverview>("npm_overview", json!({ "hostId": self.h() })).await {
                Ok(o) => {
                    self.ov.set(o);
                    self.npm.set(NpmState::Ready);
                }
                Err(ErrorPayload::NpmAuthRequired { message }) => self.npm.set(NpmState::NeedLogin(message)),
                Err(e) => self.npm.set(NpmState::Error(e.message())),
            }
        });
    }

    fn init(self) {
        self.loading.set(true);
        spawn_local(async move {
            let d = invoke::<NginxDetect>("nginx_detect", json!({ "hostId": self.h() })).await;
            self.loading.set(false);
            let d = match d {
                Ok(d) => d,
                Err(e) => return self.c.handle_err(Some(&self.h()), e),
            };
            let (nat, npm) = (d.native.is_some(), d.npm.is_some());
            self.raw.set(Some(d));
            // Both installed: ask once (remembered). Only one: nothing to choose.
            let src = match (nat, npm) {
                (true, true) => Source::load(&self.h()),
                (true, false) => Some(Source::Native),
                (false, true) => Some(Source::Npm),
                (false, false) => Some(Source::Both),
            };
            self.source.set(src);
            if let Some(src) = src {
                self.apply(src);
            }
        });
    }

    /// The user picked (or changed) the installation to manage.
    fn choose(self, src: Source) {
        src.save(&self.h());
        self.source.set(Some(src));
        self.apply(src);
    }

    fn apply(self, src: Source) {
        let Some(d) = self.raw.get_untracked() else { return };
        let d = restrict(d, src);
        self.sites.set(vec![]);
        self.ov.set(NpmOverview::default());
        self.loc.set(None);
        self.npm.set(NpmState::Absent);
        let has_npm = d.npm.is_some();
        spawn_local(async move {
            self.det.set(Some(d));
            self.refresh_native();
            if !has_npm {
                self.npm.set(NpmState::Absent);
                return;
            }
            self.npm.set(NpmState::Connecting);
            match invoke::<NpmLocation>("npm_connect", json!({ "hostId": self.h() })).await {
                Ok(loc) => {
                    self.loc.set(Some(loc));
                    self.refresh_npm();
                }
                Err(e) => self.npm.set(NpmState::Error(e.message())),
            }
        });
    }

    fn native(&self) -> Option<sshos_shared::NginxNative> {
        self.det.with_untracked(|d| d.as_ref().and_then(|d| d.native.clone()))
    }

    fn after_native(self) -> Callback<()> {
        Callback::new(move |()| {
            self.sheet.set(Sheet::None);
            self.refresh_native();
        })
    }

    /// Wrap an NPM API call: refresh on success, show errors.
    fn npm_call(self, cmd: &'static str, args: Value, ok: &'static str) {
        spawn_local(async move {
            match invoke::<Value>(cmd, args).await {
                Ok(_) => {
                    self.c.ok(ok);
                    self.sheet.set(Sheet::None);
                    self.refresh_npm();
                }
                Err(ErrorPayload::NpmAuthRequired { message }) => self.npm.set(NpmState::NeedLogin(message)),
                Err(e) => self.c.handle_err(Some(&self.h()), e),
            }
        });
    }
}

fn cert_name_of(path: &str) -> Option<String> {
    path.strip_prefix("/etc/letsencrypt/live/")
        .and_then(|r| r.split('/').next())
        .filter(|s| !s.is_empty())
        .map(String::from)
}

fn days_badge(days: Option<i64>) -> AnyView {
    match days {
        None => view! { <span class="muted">"—"</span> }.into_any(),
        Some(d) => {
            let class = if d < 7 { "badge crit" } else if d < 21 { "badge warn" } else { "badge ok" };
            view! { <span class=class><Icon name="lock" size=11 /> {format!(" {d} j")}</span> }.into_any()
        }
    }
}

#[component]
pub fn Nginx(host: String) -> impl IntoView {
    let c = ctx();
    let n = N {
        c,
        host: StoredValue::new(host),
        det: RwSignal::new(None),
        raw: RwSignal::new(None),
        source: RwSignal::new(None),
        sites: RwSignal::new(vec![]),
        npm: RwSignal::new(NpmState::Absent),
        loc: RwSignal::new(None),
        ov: RwSignal::new(NpmOverview::default()),
        sheet: RwSignal::new(Sheet::None),
        loading: RwSignal::new(false),
    };
    let tab = RwSignal::new(Tab::Sites);
    let filter = RwSignal::new(String::new());
    n.init();

    let header = move || {
        n.det.get().map(|d| {
            let native = d.native.map(|nat| {
                format!(
                    "nginx {} · {}",
                    nat.version,
                    match nat.layout {
                        NginxLayout::SitesAvailable => "sites-available",
                        NginxLayout::ConfD => "conf.d",
                    }
                )
            });
            let npm = d.npm.map(|l| {
                format!("NPM · {}{}", l.target, l.container.map(|c| format!(" ({c})")).unwrap_or_default())
            });
            let root = if d.is_root { "root" } else { tr("sudo via terminal", "sudo via terminal") };
            [native, npm, Some(root.to_string())].into_iter().flatten().collect::<Vec<_>>().join("   ·   ")
        })
    };
    let has_native = move || n.det.with(|d| d.as_ref().map(|d| d.native.is_some()).unwrap_or(false));
    let npm_ready = move || n.npm.get() == NpmState::Ready;
    let nothing = move || {
        n.det.with(|d| d.as_ref().map(|d| d.native.is_none() && d.npm.is_none()).unwrap_or(false))
    };

    let logout = move |_| {
        spawn_local(async move {
            let _ = invoke_unit("npm_logout", json!({ "hostId": n.h() })).await;
            n.npm.set(NpmState::NeedLogin(String::new()));
        });
    };

    let has = move |native: bool| n.raw.with(|d| d.as_ref().map(|d| if native { d.native.is_some() } else { d.npm.is_some() }).unwrap_or(false));
    let pick = move |src: Source| {
        tab.set(Tab::Sites);
        n.choose(src);
    };
    app_menus(vec![
        MenuSection::new(
            tr("Présentation", "View"),
            vec![
                MenuItem::new("Sites", move || tab.set(Tab::Sites)).checked(move || tab.get() == Tab::Sites),
                MenuItem::new(tr("Certificats", "Certificates"), move || tab.set(Tab::Certs)).checked(move || tab.get() == Tab::Certs),
                MenuItem::new(tr("Redirections", "Redirections"), move || tab.set(Tab::Redirects)).checked(move || tab.get() == Tab::Redirects),
                MenuItem::new("Streams", move || tab.set(Tab::Streams)).checked(move || tab.get() == Tab::Streams),
            ],
        ),
        MenuSection::new(
            "Source",
            vec![
                MenuItem::new(tr("nginx (service)", "nginx (service)"), move || pick(Source::Native))
                    .checked(move || n.source.get() == Some(Source::Native))
                    .disabled(move || !has(true)),
                MenuItem::new(tr("Nginx Proxy Manager (Docker)", "Nginx Proxy Manager (Docker)"), move || pick(Source::Npm))
                    .checked(move || n.source.get() == Some(Source::Npm))
                    .disabled(move || !has(false)),
                MenuItem::new(tr("Les deux", "Both"), move || pick(Source::Both))
                    .checked(move || n.source.get() == Some(Source::Both))
                    .disabled(move || !(has(true) && has(false))),
            ],
        ),
    ]);
    let choosing = move || !n.loading.get() && n.raw.with(|d| d.is_some()) && n.source.get().is_none();

    view! {
        <div class="nginx" class:choosing=choosing>
            <div class="toolbar">
                <span class="muted small grow">{header}</span>
                <button on:click=move |_| n.init() title=tr("Actualiser", "Refresh")><Icon name="refresh" size=14 /></button>
                <Show when=has_native>
                    <button class="primary" on:click=move |_| n.sheet.set(Sheet::NewNative)>
                        <Icon name="plus" size=14 /> {tr("Site nginx", "Nginx site")}
                    </button>
                </Show>
                <Show when=npm_ready>
                    <button class="primary" on:click=move |_| n.sheet.set(Sheet::Proxy(None))>
                        <Icon name="plus" size=14 /> {tr("Proxy host NPM", "NPM proxy host")}
                    </button>
                    <button on:click=logout title=tr("Oublier les identifiants NPM", "Forget NPM credentials")>
                        {tr("Déconnexion NPM", "NPM logout")}
                    </button>
                </Show>
            </div>
            <div class="tabs">
                <button class:active=move || tab.get() == Tab::Sites on:click=move |_| tab.set(Tab::Sites)>{tr("Sites", "Sites")}</button>
                <Show when=npm_ready>
                    <button class:active=move || tab.get() == Tab::Certs on:click=move |_| tab.set(Tab::Certs)>{tr("Certificats", "Certificates")}</button>
                    <button class:active=move || tab.get() == Tab::Redirects on:click=move |_| tab.set(Tab::Redirects)>{tr("Redirections", "Redirections")}</button>
                    <button class:active=move || tab.get() == Tab::Streams on:click=move |_| tab.set(Tab::Streams)>"Streams"</button>
                </Show>
                <span class="spacer"></span>
                <input class="filter" placeholder=tr("Filtrer", "Filter") bind:value=filter />
            </div>
            <Show when=move || n.loading.get()>
                <div class="empty">{tr("Détection de nginx…", "Detecting nginx…")}</div>
            </Show>
            <Show when=nothing>
                <div class="empty">
                    {tr(
                        "Ni nginx ni Nginx Proxy Manager n'ont été trouvés sur ce serveur. Si NPM tourne ailleurs, indiquez l'adresse de son API dans la connexion.",
                        "Neither nginx nor Nginx Proxy Manager was found on this server. If NPM runs elsewhere, set its API address in the connection.",
                    )}
                </div>
            </Show>
            <Show when=choosing>{move || chooser(n, pick)}</Show>
            {move || npm_status(n)}
            <div class="nginx-body">
                {move || match tab.get() {
                    Tab::Sites => sites_table(n, filter).into_any(),
                    Tab::Certs => certs_table(n, filter).into_any(),
                    Tab::Redirects => redirects_table(n, filter).into_any(),
                    Tab::Streams => streams_table(n).into_any(),
                }}
            </div>
            {move || sheet_view(n)}
        </div>
    }
}

/// First opening on a server running both nginx and NPM: which one to manage?
fn chooser(n: N, pick: impl Fn(Source) + Copy + Send + Sync + 'static) -> impl IntoView {
    let d = n.raw.get_untracked().unwrap_or(NginxDetect { native: None, npm: None, is_root: false });
    let nat = d
        .native
        .map(|x| format!("nginx {} · {}", x.version, match x.layout {
            NginxLayout::SitesAvailable => "sites-available",
            NginxLayout::ConfD => "conf.d",
        }))
        .unwrap_or_default();
    let npm = d
        .npm
        .map(|l| format!("{}{}", l.container.map(|c| format!("{c} · ")).unwrap_or_default(), l.target))
        .unwrap_or_default();
    view! {
        <div class="nginx-choose">
            <h3>{tr("Quelle installation gérer ?", "Which installation should sshOs manage?")}</h3>
            <p class="muted small">
                {tr(
                    "Ce serveur fait tourner nginx en service et Nginx Proxy Manager dans Docker. Votre choix est mémorisé pour ce serveur ; vous pourrez le changer dans le menu Source.",
                    "This server runs nginx as a service and Nginx Proxy Manager in Docker. Your choice is remembered for this server; change it later from the Source menu.",
                )}
            </p>
            <div class="choose-grid">
                <button class="choose-card" on:click=move |_| pick(Source::Native)>
                    <span class="dock-tile t-globe"><Icon name="globe" size=22 /></span>
                    <b>{tr("nginx (service)", "nginx (service)")}</b>
                    <span class="muted small">{nat}</span>
                    <span class="small">{tr("Fichiers de configuration, nginx -t avant chaque rechargement", "Config files, nginx -t before every reload")}</span>
                </button>
                <button class="choose-card" on:click=move |_| pick(Source::Npm)>
                    <span class="dock-tile t-box"><Icon name="box" size=22 /></span>
                    <b>"Nginx Proxy Manager"</b>
                    <span class="muted small">{npm}</span>
                    <span class="small">{tr("Proxy hosts, certificats et redirections via son API", "Proxy hosts, certificates and redirections through its API")}</span>
                </button>
            </div>
            <button class="link" on:click=move |_| pick(Source::Both)>{tr("Gérer les deux dans une seule liste", "Manage both in a single list")}</button>
        </div>
    }
}

fn npm_status(n: N) -> AnyView {
    match n.npm.get() {
        NpmState::Absent | NpmState::Ready => ().into_any(),
        NpmState::Connecting => {
            view! { <div class="info-banner">{tr("Connexion à Nginx Proxy Manager via le tunnel SSH…", "Connecting to Nginx Proxy Manager through the SSH tunnel…")}</div> }
                .into_any()
        }
        NpmState::Error(e) => view! { <div class="error-banner">"NPM : " {e}</div> }.into_any(),
        NpmState::NeedLogin(msg) => npm_login(n, msg).into_any(),
    }
}

fn npm_login(n: N, msg: String) -> impl IntoView {
    let identity = RwSignal::new(String::new());
    let secret = RwSignal::new(String::new());
    let remember = RwSignal::new(true);
    let busy = RwSignal::new(false);
    let submit = move |e: leptos::ev::SubmitEvent| {
        e.prevent_default();
        busy.set(true);
        spawn_local(async move {
            let r = invoke_unit(
                "npm_login",
                json!({ "req": {
                    "host_id": n.h(),
                    "identity": identity.get_untracked(),
                    "secret": secret.get_untracked(),
                    "remember": remember.get_untracked(),
                } }),
            )
            .await;
            let _ = busy.try_set(false);
            match r {
                Ok(()) => n.refresh_npm(),
                Err(e) => n.c.error(e.message()),
            }
        });
    };
    view! {
        <form class="card npm-login" on:submit=submit>
            <h4>{tr("Connexion à Nginx Proxy Manager", "Sign in to Nginx Proxy Manager")}</h4>
            {(!msg.is_empty()).then(|| view! { <p class="muted small">{msg}</p> })}
            <div class="row">
                <input class="grow" placeholder="admin@example.com" bind:value=identity />
                <input class="grow" type="password" placeholder=tr("Mot de passe", "Password") bind:value=secret />
                <button class="primary" type="submit" disabled=move || busy.get()>{tr("Se connecter", "Sign in")}</button>
            </div>
            <label class="check">
                <input type="checkbox" bind:checked=remember />
                {tr("Mémoriser dans le trousseau", "Remember in the keychain")}
            </label>
        </form>
    }
}

// ---------------------------------------------------------------- sites (native + NPM proxy hosts)

fn matches(q: &str, fields: &[&str]) -> bool {
    q.is_empty() || fields.iter().any(|f| f.to_lowercase().contains(q))
}

fn sites_table(n: N, filter: RwSignal<String>) -> impl IntoView {
    let c = n.c;
    let native_rows = move || {
        let q = filter.get().to_lowercase();
        let all = n.sites.get();
        all.clone()
            .into_iter()
            .filter(|s| {
                let domains = s.servers.iter().flat_map(|b| b.server_names.clone()).collect::<Vec<_>>().join(" ");
                matches(&q, &[&s.name, &domains])
            })
            .map(|s| {
                let domains: Vec<String> = {
                    let mut d: Vec<String> = s.servers.iter().flat_map(|b| b.server_names.clone()).filter(|d| d != "_").collect();
                    d.dedup();
                    d
                };
                let target = s
                    .servers
                    .iter()
                    .find_map(|b| b.proxy_pass.first().cloned().or_else(|| b.root.clone()))
                    .unwrap_or_default();
                let enabled = s.enabled;
                let (s1, s2, s3, s4) = (s.clone(), s.clone(), s.clone(), s.clone());
                let doms = domains.clone();
                let all = all.clone();
                let toggle = move |_| {
                    let Some(native) = n.native() else { return };
                    let name = s1.name.clone();
                    spawn_local(async move {
                        match invoke::<NginxPlan>("nginx_plan_toggle", json!({ "native": native, "name": name, "enable": !enabled })).await {
                            Ok(plan) => apply_plan(c, n.h(), plan, n.after_native()),
                            Err(e) => c.handle_err(Some(&n.h()), e),
                        }
                    });
                };
                let https = move |_| {
                    let d = doms.clone();
                    spawn_local(async move {
                        match invoke::<String>("nginx_certbot_command", json!({ "domains": d })).await {
                            Ok(cmd) => c.run_in_terminal(&n.h(), &cmd),
                            Err(e) => c.handle_err(Some(&n.h()), e),
                        }
                    });
                };
                let remove = move |_| {
                    let Some(native) = n.native() else { return };
                    // Drop the Let's Encrypt certificate too when no other site uses it.
                    let cert = s2
                        .servers
                        .iter()
                        .find_map(|b| b.ssl_certificate.as_deref().and_then(cert_name_of))
                        .filter(|cn| {
                            !all.iter().any(|o| {
                                o.path != s2.path
                                    && o.servers.iter().any(|b| b.ssl_certificate.as_deref().and_then(cert_name_of).as_deref() == Some(cn))
                            })
                        });
                    let name = s2.name.clone();
                    spawn_local(async move {
                        match invoke::<NginxPlan>("nginx_plan_remove", json!({ "native": native, "name": name, "certName": cert })).await {
                            Ok(plan) => apply_plan(c, n.h(), plan, n.after_native()),
                            Err(e) => c.handle_err(Some(&n.h()), e),
                        }
                    });
                };
                view! {
                    <tr class:disabled=!enabled>
                        <td><span class="badge native">"nginx"</span></td>
                        <td>
                            <div class="strong">{s.name.clone()}</div>
                            <div class="muted small">{domains.join(", ")}</div>
                        </td>
                        <td class="mono small ellipsis">{target}</td>
                        <td>{days_badge(s.cert_days_left)}</td>
                        <td>
                            <button class="switch" class:on=enabled on:click=toggle title=if enabled { tr("Désactiver", "Disable") } else { tr("Activer", "Enable") }>
                                <span></span>
                            </button>
                        </td>
                        <td class="actions">
                            <button class="small" on:click=move |_| c.open(AppKind::Editor { path: s3.path.clone(), nginx: true }, Some(n.h()))>
                                {tr("Éditer", "Edit")}
                            </button>
                            <button class="small" on:click=move |_| n.sheet.set(Sheet::Quick(s4.clone()))>{tr("Rapide", "Quick")}</button>
                            <button class="small" on:click=https>"HTTPS"</button>
                            <button class="small danger" on:click=remove><Icon name="trash" size=12 /></button>
                        </td>
                    </tr>
                }
            })
            .collect_view()
    };

    let npm_rows = move || {
        let q = filter.get().to_lowercase();
        let ov = n.ov.get();
        let certs = ov.certificates.clone();
        ov.proxy_hosts
            .into_iter()
            .filter(|h| matches(&q, &[&h.domain_names.join(" "), &h.forward_host]))
            .map(|h| {
                let cert_days = h
                    .certificate
                    .as_ref()
                    .map(|c| c.expires_on.clone())
                    .or_else(|| {
                        let id = h.certificate_id.as_i64()?;
                        certs.iter().find(|c| c.id == id).map(|c| c.expires_on.clone())
                    })
                    .and_then(|e| days_until(&e));
                let (id, enabled) = (h.id, h.enabled);
                let h_edit = h.clone();
                let toggle = move |_| {
                    n.npm_call(
                        "npm_toggle",
                        json!({ "hostId": n.h(), "kind": "proxy-hosts", "id": id, "enabled": !enabled }),
                        if enabled { tr("Proxy host désactivé", "Proxy host disabled") } else { tr("Proxy host activé", "Proxy host enabled") },
                    )
                };
                let label = h.domain_names.join(", ");
                let label2 = label.clone();
                let remove = move |_| {
                    c.show(
                        Dialog::confirm(
                            tr("Supprimer le proxy host", "Delete proxy host"),
                            format!("{} {label2} ?", tr("Supprimer dans Nginx Proxy Manager :", "Delete in Nginx Proxy Manager:")),
                            tr("Supprimer", "Delete"),
                            move || {
                                n.npm_call(
                                    "npm_delete",
                                    json!({ "hostId": n.h(), "kind": "proxy-hosts", "id": id }),
                                    tr("Proxy host supprimé", "Proxy host deleted"),
                                )
                            },
                        )
                        .danger(),
                    )
                };
                let view_conf = move |_| {
                    let Some(container) = n.loc.get_untracked().and_then(|l| l.container) else {
                        c.error(tr("Conteneur NPM inconnu : impossible de lire la conf générée.", "Unknown NPM container: cannot read the generated config."));
                        return;
                    };
                    spawn_local(async move {
                        match invoke::<String>(
                            "npm_conf",
                            json!({ "hostId": n.h(), "container": container, "kind": "proxy-hosts", "id": id }),
                        )
                        .await
                        {
                            Ok(conf) => c.show(
                                Dialog::info(
                                    format!("proxy_host/{id}.conf"),
                                    tr(
                                        "Configuration générée par NPM (lecture seule : modifiez via NPM).",
                                        "Config generated by NPM (read-only: change it through NPM).",
                                    ),
                                )
                                .code(conf),
                            ),
                            Err(e) => c.handle_err(Some(&n.h()), e),
                        }
                    });
                };
                view! {
                    <tr class:disabled=!enabled>
                        <td><span class="badge npm">"NPM"</span></td>
                        <td>
                            <div class="strong">{label}</div>
                            <div class="muted small">
                                {[
                                    h.ssl_forced.then_some("SSL forcé"),
                                    h.allow_websocket_upgrade.then_some("WebSocket"),
                                    h.http2_support.then_some("HTTP/2"),
                                    (h.access_list_id > 0).then_some("Access list"),
                                ]
                                    .into_iter()
                                    .flatten()
                                    .collect::<Vec<_>>()
                                    .join(" · ")}
                            </div>
                        </td>
                        <td class="mono small ellipsis">{format!("{}://{}:{}", h.forward_scheme, h.forward_host, h.forward_port)}</td>
                        <td>{days_badge(cert_days)}</td>
                        <td>
                            <button class="switch" class:on=enabled on:click=toggle><span></span></button>
                        </td>
                        <td class="actions">
                            <button class="small" on:click=move |_| n.sheet.set(Sheet::Proxy(Some(h_edit.clone())))>{tr("Modifier", "Edit")}</button>
                            <button class="small" on:click=view_conf title=tr("Voir la conf nginx générée", "View generated nginx config")>
                                <Icon name="eye" size=12 />
                            </button>
                            <button class="small danger" on:click=remove><Icon name="trash" size=12 /></button>
                        </td>
                    </tr>
                }
            })
            .collect_view()
    };

    view! {
        <div class="table-wrap">
            <table class="site-table">
                <thead>
                    <tr>
                        <th></th>
                        <th>{tr("Site / domaines", "Site / domains")}</th>
                        <th>{tr("Cible", "Target")}</th>
                        <th>"TLS"</th>
                        <th>{tr("Actif", "On")}</th>
                        <th></th>
                    </tr>
                </thead>
                <tbody>
                    {native_rows}
                    {npm_rows}
                </tbody>
            </table>
        </div>
    }
}

// ---------------------------------------------------------------- NPM tabs

fn certs_table(n: N, filter: RwSignal<String>) -> impl IntoView {
    let c = n.c;
    let rows = move || {
        let q = filter.get().to_lowercase();
        n.ov
            .get()
            .certificates
            .into_iter()
            .filter(|ct| matches(&q, &[&ct.nice_name, &ct.domain_names.join(" ")]))
            .map(|ct| {
                let id = ct.id;
                let name = if ct.nice_name.is_empty() { ct.domain_names.join(", ") } else { ct.nice_name.clone() };
                let name2 = name.clone();
                let renew = move |_| n.npm_call("npm_cert_renew", json!({ "hostId": n.h(), "id": id }), tr("Certificat renouvelé", "Certificate renewed"));
                let remove = move |_| {
                    c.show(
                        Dialog::confirm(
                            tr("Supprimer le certificat", "Delete certificate"),
                            name2.clone(),
                            tr("Supprimer", "Delete"),
                            move || n.npm_call("npm_cert_delete", json!({ "hostId": n.h(), "id": id }), tr("Certificat supprimé", "Certificate deleted")),
                        )
                        .danger(),
                    )
                };
                let le = ct.provider == "letsencrypt";
                view! {
                    <tr>
                        <td>
                            <div class="strong">{name}</div>
                            <div class="muted small">{ct.domain_names.join(", ")}</div>
                        </td>
                        <td>{ct.provider.clone()}</td>
                        <td class="small">{ct.expires_on.clone()}</td>
                        <td>{days_badge(days_until(&ct.expires_on))}</td>
                        <td class="actions">
                            <Show when=move || le>
                                <button class="small" on:click=renew>{tr("Renouveler", "Renew")}</button>
                            </Show>
                            <button class="small danger" on:click=remove><Icon name="trash" size=12 /></button>
                        </td>
                    </tr>
                }
            })
            .collect_view()
    };
    view! {
        <div class="toolbar">
            <span class="grow"></span>
            <button class="primary" on:click=move |_| n.sheet.set(Sheet::NewCert)>
                <Icon name="plus" size=14 /> {tr("Certificat Let's Encrypt", "Let's Encrypt certificate")}
            </button>
        </div>
        <div class="table-wrap">
            <table>
                <thead>
                    <tr>
                        <th>{tr("Certificat", "Certificate")}</th>
                        <th>{tr("Fournisseur", "Provider")}</th>
                        <th>{tr("Expire le", "Expires on")}</th>
                        <th></th>
                        <th></th>
                    </tr>
                </thead>
                <tbody>{rows}</tbody>
            </table>
        </div>
    }
}

fn redirects_table(n: N, filter: RwSignal<String>) -> impl IntoView {
    let c = n.c;
    let rows = move || {
        let q = filter.get().to_lowercase();
        n.ov
            .get()
            .redirection_hosts
            .into_iter()
            .filter(|r| matches(&q, &[&r.domain_names.join(" "), &r.forward_domain_name]))
            .map(|r| {
                let (id, enabled) = (r.id, r.enabled);
                let label = r.domain_names.join(", ");
                let toggle = move |_| {
                    n.npm_call(
                        "npm_toggle",
                        json!({ "hostId": n.h(), "kind": "redirection-hosts", "id": id, "enabled": !enabled }),
                        tr("Redirection mise à jour", "Redirection updated"),
                    )
                };
                let label2 = label.clone();
                let remove = move |_| {
                    c.show(
                        Dialog::confirm(tr("Supprimer la redirection", "Delete redirection"), label2.clone(), tr("Supprimer", "Delete"), move || {
                            n.npm_call(
                                "npm_delete",
                                json!({ "hostId": n.h(), "kind": "redirection-hosts", "id": id }),
                                tr("Redirection supprimée", "Redirection deleted"),
                            )
                        })
                        .danger(),
                    )
                };
                view! {
                    <tr class:disabled=!enabled>
                        <td class="strong">{label}</td>
                        <td class="mono small">{format!("{} {}://{}", r.forward_http_code, r.forward_scheme, r.forward_domain_name)}</td>
                        <td><button class="switch" class:on=enabled on:click=toggle><span></span></button></td>
                        <td class="actions"><button class="small danger" on:click=remove><Icon name="trash" size=12 /></button></td>
                    </tr>
                }
            })
            .collect_view()
    };
    view! {
        <div class="toolbar">
            <span class="grow"></span>
            <button class="primary" on:click=move |_| n.sheet.set(Sheet::NewRedirect)><Icon name="plus" size=14 /> {tr("Redirection", "Redirection")}</button>
        </div>
        <div class="table-wrap">
            <table>
                <thead><tr><th>{tr("Domaines", "Domains")}</th><th>{tr("Vers", "To")}</th><th>{tr("Actif", "On")}</th><th></th></tr></thead>
                <tbody>{rows}</tbody>
            </table>
        </div>
    }
}

fn streams_table(n: N) -> impl IntoView {
    let c = n.c;
    let rows = move || {
        n.ov
            .get()
            .streams
            .into_iter()
            .map(|s| {
                let (id, enabled) = (s.id, s.enabled);
                let toggle = move |_| {
                    n.npm_call(
                        "npm_toggle",
                        json!({ "hostId": n.h(), "kind": "streams", "id": id, "enabled": !enabled }),
                        tr("Stream mis à jour", "Stream updated"),
                    )
                };
                let port = s.incoming_port;
                let remove = move |_| {
                    c.show(
                        Dialog::confirm(tr("Supprimer le stream", "Delete stream"), format!(":{port}"), tr("Supprimer", "Delete"), move || {
                            n.npm_call("npm_delete", json!({ "hostId": n.h(), "kind": "streams", "id": id }), tr("Stream supprimé", "Stream deleted"))
                        })
                        .danger(),
                    )
                };
                let proto = [s.tcp_forwarding.then_some("TCP"), s.udp_forwarding.then_some("UDP")]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join("/");
                view! {
                    <tr class:disabled=!enabled>
                        <td class="mono">{format!(":{}", s.incoming_port)}</td>
                        <td class="mono small">{format!("{}:{}", s.forwarding_host, s.forwarding_port)}</td>
                        <td>{proto}</td>
                        <td><button class="switch" class:on=enabled on:click=toggle><span></span></button></td>
                        <td class="actions"><button class="small danger" on:click=remove><Icon name="trash" size=12 /></button></td>
                    </tr>
                }
            })
            .collect_view()
    };
    view! {
        <div class="toolbar">
            <span class="grow"></span>
            <button class="primary" on:click=move |_| n.sheet.set(Sheet::NewStream)><Icon name="plus" size=14 /> "Stream"</button>
        </div>
        <div class="table-wrap">
            <table>
                <thead><tr><th>{tr("Port entrant", "Incoming port")}</th><th>{tr("Vers", "To")}</th><th>{tr("Protocole", "Protocol")}</th><th>{tr("Actif", "On")}</th><th></th></tr></thead>
                <tbody>{rows}</tbody>
            </table>
        </div>
    }
}

// ---------------------------------------------------------------- sheets (side forms)

fn sheet_view(n: N) -> AnyView {
    let body = match n.sheet.get() {
        Sheet::None => return ().into_any(),
        Sheet::NewNative => new_native(n).into_any(),
        Sheet::Quick(site) => quick(n, site).into_any(),
        Sheet::Proxy(h) => proxy_form(n, h).into_any(),
        Sheet::NewCert => new_cert(n).into_any(),
        Sheet::NewRedirect => new_redirect(n).into_any(),
        Sheet::NewStream => new_stream(n).into_any(),
    };
    view! {
        <div class="sheet-overlay" on:click=move |_| n.sheet.set(Sheet::None)>
            <div class="sheet" on:click=|e| e.stop_propagation()>
                <button class="icon-btn sheet-close" on:click=move |_| n.sheet.set(Sheet::None)><Icon name="x" /></button>
                {body}
            </div>
        </div>
    }
    .into_any()
}

fn new_native(n: N) -> impl IntoView {
    let c = n.c;
    let name = RwSignal::new(String::new());
    let domains = RwSignal::new(String::new());
    let kind = RwSignal::new("proxy".to_string());
    let target = RwSignal::new("http://127.0.0.1:3000".to_string());
    let websocket = RwSignal::new(true);
    let php_socket = RwSignal::new("/run/php/php-fpm.sock".to_string());
    let preview = RwSignal::new(String::new());
    let preview_err = RwSignal::new(None::<String>);

    let req = move || {
        let d = split_list(&domains.get());
        let nm = name.get();
        NewSiteRequest {
            name: if nm.trim().is_empty() { d.first().cloned().unwrap_or_default() } else { nm.trim().to_string() },
            domains: d,
            kind: match kind.get().as_str() {
                "static" => NewSiteKind::Static,
                "php" => NewSiteKind::Php,
                _ => NewSiteKind::Proxy,
            },
            target: target.get().trim().to_string(),
            websocket: websocket.get(),
            php_socket: Some(php_socket.get()).filter(|_| kind.get() == "php"),
        }
    };
    Effect::new(move |_| {
        let r = req();
        spawn_local(async move {
            match invoke::<String>("nginx_preview", json!({ "req": r })).await {
                Ok(p) => {
                    let _ = preview.try_set(p);
                    let _ = preview_err.try_set(None);
                }
                Err(e) => {
                    let _ = preview_err.try_set(Some(e.message()));
                }
            }
        });
    });
    let create = move |e: leptos::ev::SubmitEvent| {
        e.prevent_default();
        let Some(native) = n.native() else { return };
        let r = req.clone()();
        spawn_local(async move {
            match invoke::<NginxPlan>("nginx_plan_create", json!({ "hostId": n.h(), "native": native, "req": r })).await {
                Ok(plan) => apply_plan(c, n.h(), plan, n.after_native()),
                Err(e) => c.handle_err(Some(&n.h()), e),
            }
        });
    };
    view! {
        <form class="form" on:submit=create>
            <h3>{tr("Nouveau site nginx", "New nginx site")}</h3>
            <label>{tr("Domaines", "Domains")}<input bind:value=domains placeholder="app.example.com www.app.example.com" /></label>
            <label>{tr("Nom du fichier", "File name")}<input bind:value=name placeholder=tr("par défaut : premier domaine", "default: first domain") /></label>
            <label>
                "Type"
                <select prop:value=move || kind.get() on:change=move |e| {
                    let v = event_target_value(&e);
                    target.set(match v.as_str() {
                        "proxy" => "http://127.0.0.1:3000".into(),
                        _ => "/var/www/html".into(),
                    });
                    kind.set(v);
                }>
                    <option value="proxy">{tr("Reverse proxy", "Reverse proxy")}</option>
                    <option value="static">{tr("Site statique", "Static site")}</option>
                    <option value="php">"PHP-FPM"</option>
                </select>
            </label>
            <label>
                {move || if kind.get() == "proxy" { tr("URL de l'upstream", "Upstream URL") } else { tr("Racine des documents", "Document root") }}
                <input bind:value=target />
            </label>
            <Show when=move || kind.get() == "proxy">
                <label class="check"><input type="checkbox" bind:checked=websocket />"WebSocket"</label>
            </Show>
            <Show when=move || kind.get() == "php">
                <label>{tr("Socket php-fpm", "php-fpm socket")}<input bind:value=php_socket /></label>
            </Show>
            <div class="muted small">{tr("Aperçu", "Preview")}</div>
            {move || match preview_err.get() {
                Some(e) => view! { <div class="error-banner">{e}</div> }.into_any(),
                None => view! { <pre class="code preview">{move || preview.get()}</pre> }.into_any(),
            }}
            <p class="muted small">
                {tr(
                    "HTTPS : une fois le site créé, utilisez le bouton HTTPS (certbot dans un terminal).",
                    "HTTPS: once created, use the HTTPS button (certbot in a terminal).",
                )}
            </p>
            <div class="form-actions">
                <button class="primary" type="submit" disabled=move || preview_err.get().is_some()>{tr("Créer…", "Create…")}</button>
            </div>
        </form>
    }
}

fn quick(n: N, site: NginxSite) -> impl IntoView {
    let c = n.c;
    let add = RwSignal::new(String::new());
    let proxy = RwSignal::new(site.servers.iter().find_map(|b| b.proxy_pass.first().cloned()).unwrap_or_default());
    let body = RwSignal::new(String::new());
    let path = site.path.clone();
    let has_proxy = !proxy.get_untracked().is_empty();
    let submit = move |e: leptos::ev::SubmitEvent| {
        e.prevent_default();
        let path = path.clone();
        spawn_local(async move {
            let args = json!({
                "hostId": n.h(),
                "path": path,
                "addDomains": split_list(&add.get_untracked()),
                "proxy": Some(proxy.get_untracked()).filter(|p| has_proxy && !p.trim().is_empty()),
                "bodySize": Some(body.get_untracked()).filter(|b| !b.trim().is_empty()),
            });
            match invoke::<NginxPlan>("nginx_plan_quick", args).await {
                Ok(plan) => apply_plan(c, n.h(), plan, n.after_native()),
                Err(e) => c.handle_err(Some(&n.h()), e),
            }
        });
    };
    view! {
        <form class="form" on:submit=submit>
            <h3>{format!("{} — {}", tr("Modification rapide", "Quick change"), site.name)}</h3>
            <label>{tr("Ajouter des domaines", "Add domains")}<input bind:value=add placeholder="new.example.com" /></label>
            <Show when=move || has_proxy>
                <label>"proxy_pass"<input bind:value=proxy /></label>
            </Show>
            <label>"client_max_body_size"<input bind:value=body placeholder="100m" /></label>
            <p class="muted small">{tr(
                "Le fichier est modifié, validé par nginx -t puis rechargé ; la sauvegarde est restaurée en cas d'échec.",
                "The file is edited, validated with nginx -t then reloaded; the backup is restored on failure.",
            )}</p>
            <div class="form-actions"><button class="primary" type="submit">{tr("Continuer…", "Continue…")}</button></div>
        </form>
    }
}

fn proxy_form(n: N, existing: Option<NpmProxyHost>) -> impl IntoView {
    let base = existing.clone().unwrap_or_else(|| NpmProxyHost {
        forward_scheme: "http".into(),
        block_exploits: true,
        allow_websocket_upgrade: true,
        enabled: true,
        certificate_id: Value::from(0),
        ..Default::default()
    });
    let is_new = existing.is_none();
    let domains = RwSignal::new(base.domain_names.join(" "));
    let scheme = RwSignal::new(if base.forward_scheme.is_empty() { "http".into() } else { base.forward_scheme.clone() });
    let fhost = RwSignal::new(base.forward_host.clone());
    let fport = RwSignal::new(if base.forward_port == 0 { String::new() } else { base.forward_port.to_string() });
    let cert = RwSignal::new(match &base.certificate_id {
        Value::Number(x) => x.to_string(),
        Value::String(s) => s.clone(),
        _ => "0".into(),
    });
    let email = RwSignal::new(base.meta.get("letsencrypt_email").and_then(Value::as_str).unwrap_or_default().to_string());
    let ssl_forced = RwSignal::new(base.ssl_forced);
    let http2 = RwSignal::new(base.http2_support);
    let hsts = RwSignal::new(base.hsts_enabled);
    let exploits = RwSignal::new(base.block_exploits);
    let ws = RwSignal::new(base.allow_websocket_upgrade);
    let caching = RwSignal::new(base.caching_enabled);
    let access = RwSignal::new(base.access_list_id.to_string());
    let advanced = RwSignal::new(base.advanced_config.clone());
    let base = StoredValue::new(base);

    let submit = move |e: leptos::ev::SubmitEvent| {
        e.prevent_default();
        let mut h = base.get_value();
        h.domain_names = split_list(&domains.get_untracked());
        h.forward_scheme = scheme.get_untracked();
        h.forward_host = fhost.get_untracked().trim().to_string();
        h.forward_port = fport.get_untracked().trim().parse().unwrap_or(0);
        if h.domain_names.is_empty() || h.forward_host.is_empty() || h.forward_port == 0 {
            n.c.error(tr("Domaines, hôte et port de destination requis.", "Domains, forward host and port are required."));
            return;
        }
        let cid = cert.get_untracked();
        h.certificate_id = if cid == "new" { Value::from("new") } else { Value::from(cid.parse::<i64>().unwrap_or(0)) };
        if cid == "new" {
            let mut meta = h.meta.as_object().cloned().unwrap_or_default();
            meta.insert("letsencrypt_email".into(), Value::from(email.get_untracked()));
            meta.insert("letsencrypt_agree".into(), Value::from(true));
            meta.insert("dns_challenge".into(), Value::from(false));
            h.meta = Value::Object(meta);
        }
        h.ssl_forced = ssl_forced.get_untracked();
        h.http2_support = http2.get_untracked();
        h.hsts_enabled = hsts.get_untracked();
        h.block_exploits = exploits.get_untracked();
        h.allow_websocket_upgrade = ws.get_untracked();
        h.caching_enabled = caching.get_untracked();
        h.access_list_id = access.get_untracked().parse().unwrap_or(0);
        h.advanced_config = advanced.get_untracked();
        n.npm_call(
            "npm_save_proxy_host",
            json!({ "hostId": n.h(), "host": h }),
            tr("Proxy host enregistré dans NPM", "Proxy host saved in NPM"),
        );
    };

    view! {
        <form class="form" on:submit=submit>
            <h3>{if is_new { tr("Nouveau proxy host (NPM)", "New proxy host (NPM)") } else { tr("Modifier le proxy host (NPM)", "Edit proxy host (NPM)") }}</h3>
            <label>{tr("Domaines", "Domains")}<input bind:value=domains placeholder="app.example.com" /></label>
            <div class="row">
                <label class="w80">
                    {tr("Schéma", "Scheme")}
                    <select prop:value=move || scheme.get() on:change=move |e| scheme.set(event_target_value(&e))>
                        <option value="http">"http"</option>
                        <option value="https">"https"</option>
                    </select>
                </label>
                <label class="grow">{tr("Hôte de destination", "Forward host")}<input bind:value=fhost placeholder="10.0.0.5 / container" /></label>
                <label class="w80">"Port"<input bind:value=fport placeholder="3000" /></label>
            </div>
            <div class="checks">
                <label class="check"><input type="checkbox" bind:checked=exploits />{tr("Bloquer les exploits courants", "Block common exploits")}</label>
                <label class="check"><input type="checkbox" bind:checked=ws />"WebSocket"</label>
                <label class="check"><input type="checkbox" bind:checked=caching />{tr("Cache des assets", "Cache assets")}</label>
            </div>
            <label>
                {tr("Liste d'accès", "Access list")}
                <select prop:value=move || access.get() on:change=move |e| access.set(event_target_value(&e))>
                    <option value="0">{tr("Publique", "Publicly accessible")}</option>
                    {move || {
                        n.ov
                            .get()
                            .access_lists
                            .into_iter()
                            .map(|a| {
                                let v = a.id.to_string();
                                let sel = access.get_untracked() == v;
                                view! { <option value=v selected=sel>{a.name}</option> }
                            })
                            .collect_view()
                    }}
                </select>
            </label>
            <h4>"SSL"</h4>
            <label>
                {tr("Certificat", "Certificate")}
                <select prop:value=move || cert.get() on:change=move |e| cert.set(event_target_value(&e))>
                    <option value="0">{tr("Aucun", "None")}</option>
                    <option value="new">{tr("Nouveau certificat Let's Encrypt", "Request a new Let's Encrypt certificate")}</option>
                    {move || {
                        n.ov
                            .get()
                            .certificates
                            .into_iter()
                            .map(|ct| {
                                let v = ct.id.to_string();
                                let sel = cert.get_untracked() == v;
                                let label = if ct.nice_name.is_empty() { ct.domain_names.join(", ") } else { ct.nice_name };
                                view! { <option value=v selected=sel>{label}</option> }
                            })
                            .collect_view()
                    }}
                </select>
            </label>
            <Show when=move || cert.get() == "new">
                <label>{tr("E-mail Let's Encrypt", "Let's Encrypt e-mail")}<input bind:value=email placeholder="admin@example.com" /></label>
                <p class="muted small">{tr(
                    "En demandant un certificat, vous acceptez les conditions de Let's Encrypt.",
                    "Requesting a certificate means you agree to the Let's Encrypt terms.",
                )}</p>
            </Show>
            <div class="checks">
                <label class="check"><input type="checkbox" bind:checked=ssl_forced />{tr("Forcer SSL", "Force SSL")}</label>
                <label class="check"><input type="checkbox" bind:checked=http2 />"HTTP/2"</label>
                <label class="check"><input type="checkbox" bind:checked=hsts />"HSTS"</label>
            </div>
            <label>{tr("Configuration avancée (nginx)", "Advanced (nginx) config")}<textarea rows="4" class="mono" bind:value=advanced></textarea></label>
            <p class="muted small">{tr(
                "Enregistré via l'API de Nginx Proxy Manager, qui régénère et recharge nginx lui-même.",
                "Saved through the Nginx Proxy Manager API, which regenerates and reloads nginx itself.",
            )}</p>
            <div class="form-actions"><button class="primary" type="submit"><Icon name="save" size=14 /> {tr("Enregistrer", "Save")}</button></div>
        </form>
    }
}

fn new_cert(n: N) -> impl IntoView {
    let domains = RwSignal::new(String::new());
    let email = RwSignal::new(String::new());
    let submit = move |e: leptos::ev::SubmitEvent| {
        e.prevent_default();
        n.c.info_toast(tr("Demande du certificat à Let's Encrypt…", "Requesting the certificate from Let's Encrypt…"));
        n.npm_call(
            "npm_cert_create",
            json!({ "hostId": n.h(), "domains": split_list(&domains.get_untracked()), "email": email.get_untracked() }),
            tr("Certificat créé", "Certificate created"),
        );
    };
    view! {
        <form class="form" on:submit=submit>
            <h3>{tr("Nouveau certificat Let's Encrypt", "New Let's Encrypt certificate")}</h3>
            <label>{tr("Domaines", "Domains")}<input bind:value=domains placeholder="app.example.com" /></label>
            <label>"E-mail"<input bind:value=email placeholder="admin@example.com" /></label>
            <p class="muted small">{tr(
                "Les domaines doivent pointer vers ce serveur (challenge HTTP).",
                "Domains must point to this server (HTTP challenge).",
            )}</p>
            <div class="form-actions"><button class="primary" type="submit">{tr("Demander", "Request")}</button></div>
        </form>
    }
}

fn new_redirect(n: N) -> impl IntoView {
    let domains = RwSignal::new(String::new());
    let scheme = RwSignal::new("auto".to_string());
    let target = RwSignal::new(String::new());
    let code = RwSignal::new("301".to_string());
    let preserve = RwSignal::new(true);
    let submit = move |e: leptos::ev::SubmitEvent| {
        e.prevent_default();
        let body = json!({
            "domain_names": split_list(&domains.get_untracked()),
            "forward_scheme": scheme.get_untracked(),
            "forward_domain_name": target.get_untracked().trim(),
            "forward_http_code": code.get_untracked().parse::<u16>().unwrap_or(301),
            "preserve_path": preserve.get_untracked(),
            "certificate_id": 0,
            "ssl_forced": false,
            "hsts_enabled": false,
            "hsts_subdomains": false,
            "http2_support": false,
            "block_exploits": true,
            "advanced_config": "",
            "meta": {},
        });
        n.npm_call(
            "npm_save",
            json!({ "hostId": n.h(), "kind": "redirection-hosts", "id": null, "body": body }),
            tr("Redirection créée", "Redirection created"),
        );
    };
    view! {
        <form class="form" on:submit=submit>
            <h3>{tr("Nouvelle redirection (NPM)", "New redirection (NPM)")}</h3>
            <label>{tr("Domaines", "Domains")}<input bind:value=domains placeholder="old.example.com" /></label>
            <div class="row">
                <label class="w80">
                    {tr("Schéma", "Scheme")}
                    <select prop:value=move || scheme.get() on:change=move |e| scheme.set(event_target_value(&e))>
                        <option value="auto">"auto"</option>
                        <option value="http">"http"</option>
                        <option value="https">"https"</option>
                    </select>
                </label>
                <label class="grow">{tr("Domaine cible", "Target domain")}<input bind:value=target placeholder="new.example.com" /></label>
                <label class="w80">
                    "Code"
                    <select prop:value=move || code.get() on:change=move |e| code.set(event_target_value(&e))>
                        <option value="301">"301"</option>
                        <option value="302">"302"</option>
                        <option value="307">"307"</option>
                        <option value="308">"308"</option>
                    </select>
                </label>
            </div>
            <label class="check"><input type="checkbox" bind:checked=preserve />{tr("Conserver le chemin", "Preserve path")}</label>
            <div class="form-actions"><button class="primary" type="submit">{tr("Créer", "Create")}</button></div>
        </form>
    }
}

fn new_stream(n: N) -> impl IntoView {
    let incoming = RwSignal::new(String::new());
    let fhost = RwSignal::new(String::new());
    let fport = RwSignal::new(String::new());
    let tcp = RwSignal::new(true);
    let udp = RwSignal::new(false);
    let submit = move |e: leptos::ev::SubmitEvent| {
        e.prevent_default();
        let body = json!({
            "incoming_port": incoming.get_untracked().trim().parse::<u16>().unwrap_or(0),
            "forwarding_host": fhost.get_untracked().trim(),
            "forwarding_port": fport.get_untracked().trim().parse::<u16>().unwrap_or(0),
            "tcp_forwarding": tcp.get_untracked(),
            "udp_forwarding": udp.get_untracked(),
            "meta": {},
        });
        n.npm_call(
            "npm_save",
            json!({ "hostId": n.h(), "kind": "streams", "id": null, "body": body }),
            tr("Stream créé", "Stream created"),
        );
    };
    view! {
        <form class="form" on:submit=submit>
            <h3>{tr("Nouveau stream TCP/UDP (NPM)", "New TCP/UDP stream (NPM)")}</h3>
            <label>{tr("Port entrant", "Incoming port")}<input bind:value=incoming placeholder="5432" /></label>
            <div class="row">
                <label class="grow">{tr("Hôte de destination", "Forward host")}<input bind:value=fhost /></label>
                <label class="w80">"Port"<input bind:value=fport /></label>
            </div>
            <div class="checks">
                <label class="check"><input type="checkbox" bind:checked=tcp />"TCP"</label>
                <label class="check"><input type="checkbox" bind:checked=udp />"UDP"</label>
            </div>
            <div class="form-actions"><button class="primary" type="submit">{tr("Créer", "Create")}</button></div>
        </form>
    }
}
