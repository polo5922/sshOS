//! Disks: volumes (df), a drill-down space analyser (du) and cleanup suggestions.
//! Everything here only reads; cleanups open a terminal with the command typed in (Davish rule).

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::apps::files::shell_quote;
use crate::icons::Icon;
use crate::state::*;
use crate::tauri::exec;

#[derive(Clone, Debug, PartialEq)]
pub struct Volume {
    pub device: String,
    pub fs: String,
    pub size: u64,
    pub used: u64,
    pub avail: u64,
    pub mount: String,
}

impl Volume {
    fn pct(&self) -> f64 {
        if self.size == 0 {
            0.0
        } else {
            self.used as f64 * 100.0 / self.size as f64
        }
    }
}

/// `df -B1 -P -T` output (header skipped). Mount points may contain spaces.
pub fn parse_df(out: &str) -> Vec<Volume> {
    out.lines()
        .skip_while(|l| l.starts_with("Filesystem") || l.starts_with("Sys. de fichiers"))
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 7 {
                return None;
            }
            Some(Volume {
                device: f[0].into(),
                fs: f[1].into(),
                size: f[2].parse().ok()?,
                used: f[3].parse().ok()?,
                avail: f[4].parse().ok()?,
                mount: f[6..].join(" "),
            })
        })
        .collect()
}

/// `du -x -B1 --max-depth=1 DIR | sort -rn` output: (bytes, path), biggest first.
pub fn parse_du(out: &str) -> Vec<(u64, String)> {
    out.lines()
        .filter_map(|l| {
            let (n, p) = l.split_once(char::is_whitespace)?;
            Some((n.trim().parse().ok()?, p.trim().to_string()))
        })
        .collect()
}

fn parent(p: &str) -> String {
    let t = p.trim_end_matches('/');
    match t.rfind('/') {
        Some(0) | None => "/".into(),
        Some(i) => t[..i].to_string(),
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Volumes,
    Analyse,
    Cleanup,
}

struct Cleanup {
    title: &'static str,
    desc: &'static str,
    /// Prints a size in bytes (or a human line) on stdout.
    probe: &'static str,
    command: &'static str,
}

fn cleanups() -> Vec<Cleanup> {
    vec![
        Cleanup {
            title: tr("Cache APT", "APT cache"),
            desc: tr("Paquets .deb déjà installés, retéléchargeables.", "Downloaded .deb packages, can be fetched again."),
            probe: "du -sB1 /var/cache/apt/archives 2>/dev/null | cut -f1",
            command: "sudo apt-get clean",
        },
        Cleanup {
            title: tr("Journaux systemd", "systemd journal"),
            desc: tr("Conserve les 7 derniers jours de journaux.", "Keeps the last 7 days of logs."),
            probe: "journalctl --disk-usage 2>/dev/null | grep -o '[0-9.]*[KMGT]' | head -1",
            command: "sudo journalctl --vacuum-time=7d",
        },
        Cleanup {
            title: tr("Anciens journaux compressés", "Old compressed logs"),
            desc: tr("Fichiers .gz et rotations .1 de /var/log.", "Rotated .gz and .1 files in /var/log."),
            probe: "find /var/log -type f \\( -name '*.gz' -o -name '*.1' \\) -printf '%s\\n' 2>/dev/null | awk '{s+=$1} END {print s+0}'",
            command: "sudo find /var/log -type f \\( -name '*.gz' -o -name '*.1' \\) -delete",
        },
        Cleanup {
            title: tr("Fichiers temporaires", "Temporary files"),
            desc: tr("Fichiers de /tmp inutilisés depuis 7 jours.", "Files in /tmp unused for 7 days."),
            probe: "find /tmp -type f -atime +7 -printf '%s\\n' 2>/dev/null | awk '{s+=$1} END {print s+0}'",
            command: "sudo find /tmp -type f -atime +7 -delete",
        },
        Cleanup {
            title: tr("Docker", "Docker"),
            desc: tr(
                "Conteneurs arrêtés, réseaux inutilisés, images orphelines et cache de build.",
                "Stopped containers, unused networks, dangling images and build cache.",
            ),
            probe: "docker system df --format '{{.Type}}: {{.Reclaimable}}' 2>/dev/null | paste -sd ' ' -",
            command: "docker system prune",
        },
    ]
}

/// Probe output: plain bytes become a human size, anything else is shown as is.
fn probe_text(out: &str) -> String {
    let t = out.trim();
    match t.parse::<f64>() {
        Ok(b) => human_bytes(b),
        Err(_) if t.is_empty() => "—".into(),
        Err(_) => t.to_string(),
    }
}

#[component]
pub fn Disks(host: String) -> impl IntoView {
    let c = ctx();
    let host = StoredValue::new(host);
    let page = RwSignal::new(Page::Volumes);
    let volumes = RwSignal::new(None::<Vec<Volume>>);
    let dir = RwSignal::new("/".to_string());
    let entries = RwSignal::new(None::<Vec<(u64, String)>>);
    let loading = RwSignal::new(false);
    let probes = RwSignal::new(Vec::<String>::new());

    let load_volumes = move || {
        spawn_local(async move {
            let h = host.get_value();
            match exec(&h, "df -B1 -P -T -x tmpfs -x devtmpfs -x squashfs -x overlay -x efivarfs 2>/dev/null", true).await {
                Ok(r) => {
                    let _ = volumes.try_set(Some(parse_df(&r.stdout)));
                }
                Err(e) => c.handle_err(Some(&h), e),
            }
        });
    };
    let analyse = move |p: String| {
        dir.set(p.clone());
        page.set(Page::Analyse);
        entries.set(None);
        loading.set(true);
        spawn_local(async move {
            let h = host.get_value();
            let cmd = format!("du -x -B1 --max-depth=1 {} 2>/dev/null | sort -rn | head -60", shell_quote(&p));
            let r = exec(&h, &cmd, true).await;
            let _ = loading.try_set(false);
            match r {
                Ok(r) => {
                    let _ = entries.try_set(Some(parse_du(&r.stdout)));
                }
                Err(e) => c.handle_err(Some(&h), e),
            }
        });
    };
    let load_probes = move || {
        probes.set(vec![]);
        spawn_local(async move {
            let h = host.get_value();
            let mut out = vec![];
            for cl in cleanups() {
                out.push(match exec(&h, cl.probe, true).await {
                    Ok(r) => probe_text(&r.stdout),
                    Err(_) => "—".into(),
                });
                let _ = probes.try_set(out.clone());
            }
        });
    };
    load_volumes();

    let side = move |p: Page, icon: &'static str, label: &'static str| {
        view! {
            <button
                class="side-item"
                class:active=move || page.get() == p
                on:click=move |_| {
                    page.set(p);
                    match p {
                        Page::Analyse if entries.get_untracked().is_none() => analyse(dir.get_untracked()),
                        Page::Cleanup if probes.with_untracked(|v| v.is_empty()) => load_probes(),
                        _ => {}
                    }
                }
            >
                <Icon name=icon size=15 />
                <span>{label}</span>
            </button>
        }
    };

    let volumes_view = move || {
        let Some(v) = volumes.get() else { return view! { <div class="empty muted">{tr("Chargement…", "Loading…")}</div> }.into_any() };
        if v.is_empty() {
            return view! { <div class="empty muted">{tr("Aucun volume.", "No volume.")}</div> }.into_any();
        }
        v.into_iter()
            .map(|vol| {
                let pct = vol.pct();
                let level = if pct >= 90.0 { "crit" } else if pct >= 75.0 { "warn" } else { "" };
                let mount = vol.mount.clone();
                view! {
                    <div class="set-card disk-card">
                        <div class="disk-head">
                            <Icon name="hdd" size=22 />
                            <div class="grow">
                                <div class="disk-title">{vol.mount.clone()}</div>
                                <div class="muted small">{format!("{} · {}", vol.device, vol.fs)}</div>
                            </div>
                            <button class="btn" on:click=move |_| analyse(mount.clone())>
                                <Icon name="search" size=13 />
                                {tr("Analyser", "Analyse")}
                            </button>
                        </div>
                        <div class=format!("disk-bar {level}")><div style=format!("width:{pct:.1}%")></div></div>
                        <div class="disk-legend muted small">
                            <span>{format!("{} {}", human_bytes(vol.used as f64), tr("utilisés", "used"))}</span>
                            <span>{format!("{} {}", human_bytes(vol.avail as f64), tr("libres", "free"))}</span>
                            <span>{format!("{} · {pct:.0} %", human_bytes(vol.size as f64))}</span>
                        </div>
                    </div>
                }
            })
            .collect_view()
            .into_any()
    };

    let analyse_view = move || {
        let here = dir.get();
        let crumbs = {
            let mut acc = String::new();
            let mut v = vec![("/".to_string(), "/".to_string())];
            for part in here.split('/').filter(|s| !s.is_empty()) {
                acc.push('/');
                acc.push_str(part);
                v.push((acc.clone(), part.to_string()));
            }
            v.into_iter()
                .map(|(p, name)| view! { <button class="crumb" on:click=move |_| analyse(p.clone())>{name}</button> })
                .collect_view()
        };
        let list = move || {
            if loading.get() {
                return view! { <div class="empty muted"><span class="spinner"></span>{tr("Calcul des tailles…", "Measuring…")}</div> }.into_any();
            }
            let Some(rows) = entries.get() else { return ().into_any() };
            let here = dir.get_untracked();
            let total = rows.iter().find(|(_, p)| p.trim_end_matches('/') == here.trim_end_matches('/') || (p == "/" && here == "/"));
            let total = total.map(|t| t.0).unwrap_or_else(|| rows.iter().map(|r| r.0).sum());
            let children: Vec<_> = rows.into_iter().filter(|(_, p)| p.trim_end_matches('/') != here.trim_end_matches('/') && p != "/").collect();
            if children.is_empty() {
                return view! { <div class="empty muted">{tr("Aucun sous-dossier lisible.", "No readable sub-folder.")}</div> }.into_any();
            }
            view! {
                <div class="du-total muted small">{format!("{} : {}", tr("Total", "Total"), human_bytes(total as f64))}</div>
                {children
                    .into_iter()
                    .map(|(n, p)| {
                        let pct = if total > 0 { n as f64 * 100.0 / total as f64 } else { 0.0 };
                        let name = p.rsplit('/').next().unwrap_or(&p).to_string();
                        let (p1, p2) = (p.clone(), p.clone());
                        view! {
                            <div class="du-row" on:dblclick=move |_| analyse(p1.clone())>
                                <Icon name="folder" size=15 />
                                <span class="du-name ellipsis" title=p.clone()>{name}</span>
                                <div class="du-bar"><div style=format!("width:{pct:.1}%")></div></div>
                                <span class="du-size num">{human_bytes(n as f64)}</span>
                                <button class="icon-btn" title=tr("Ouvrir", "Open") on:click=move |_| analyse(p2.clone())>
                                    <Icon name="chev-right" size=14 />
                                </button>
                            </div>
                        }
                    })
                    .collect_view()}
            }
            .into_any()
        };
        view! {
            <div class="crumbs">
                <button class="icon-btn" title=tr("Dossier parent", "Parent folder") on:click=move |_| analyse(parent(&dir.get_untracked()))>
                    <Icon name="up" size=14 />
                </button>
                {crumbs}
                <span class="grow"></span>
                <button class="btn" on:click=move |_| c.open(AppKind::Files { path: Some(dir.get_untracked()) }, Some(host.get_value()))>
                    <Icon name="folder" size=13 />
                    {tr("Ouvrir dans Fichiers", "Open in Files")}
                </button>
            </div>
            <p class="muted small">
                {tr(
                    "Tailles sur ce volume uniquement. Sans sudo, les dossiers illisibles ne sont pas comptés. Double-cliquez pour descendre.",
                    "Sizes on this volume only. Without sudo, unreadable folders are not counted. Double-click to drill down.",
                )}
            </p>
            {list}
        }
    };

    let cleanup_view = move || {
        let p = probes.get();
        cleanups()
            .into_iter()
            .enumerate()
            .map(|(i, cl)| {
                let size = p.get(i).cloned();
                let cmd = cl.command;
                let title = cl.title;
                view! {
                    <div class="set-card clean-card">
                        <div class="grow">
                            <div class="disk-title">{title}</div>
                            <div class="muted small">{cl.desc}</div>
                            <code class="cmd-inline">{cmd}</code>
                        </div>
                        <div class="clean-size num">
                            {match size {
                                Some(s) => s.into_any(),
                                None => view! { <span class="spinner"></span> }.into_any(),
                            }}
                        </div>
                        <button
                            class="btn"
                            on:click=move |_| {
                                c.show(
                                    Dialog::confirm(
                                        title,
                                        tr(
                                            "La commande sera tapée dans un terminal, sans être lancée : relisez-la puis appuyez sur Entrée.",
                                            "The command will be typed in a terminal, not run: review it, then press Enter.",
                                        ),
                                        tr("Ouvrir le terminal", "Open terminal"),
                                        move || c.run_in_terminal(&host.get_value(), cmd),
                                    )
                                    .code(cmd)
                                    .danger(),
                                )
                            }
                        >
                            {tr("Nettoyer…", "Clean up…")}
                        </button>
                    </div>
                }
            })
            .collect_view()
    };

    let refresh = move || match page.get_untracked() {
        Page::Volumes => load_volumes(),
        Page::Analyse => analyse(dir.get_untracked()),
        Page::Cleanup => load_probes(),
    };

    app_menus(vec![MenuSection::new(
        tr("Présentation", "View"),
        vec![
            MenuItem::new(tr("Volumes", "Volumes"), move || page.set(Page::Volumes)).checked(move || page.get() == Page::Volumes),
            MenuItem::new(tr("Analyse", "Analysis"), move || analyse(dir.get_untracked())).checked(move || page.get() == Page::Analyse),
            MenuItem::new(tr("Nettoyage", "Cleanup"), move || {
                page.set(Page::Cleanup);
                load_probes()
            })
            .checked(move || page.get() == Page::Cleanup),
            None,
            MenuItem::new(tr("Actualiser", "Refresh"), refresh),
        ],
    )]);

    let host_name = c.host_name(&host.get_value());
    view! {
        <div class="split-app disks">
            <aside class="side drag-region">
                <div class="side-top"></div>
                <div class="side-host ellipsis">{host_name}</div>
                {side(Page::Volumes, "hdd", tr("Volumes", "Volumes"))}
                {side(Page::Analyse, "search", tr("Analyse", "Analysis"))}
                {side(Page::Cleanup, "trash", tr("Nettoyage", "Cleanup"))}
            </aside>
            <section class="main">
                <header class="main-head drag-region">
                    <h2 class="grow">
                        {move || match page.get() {
                            Page::Volumes => tr("Volumes", "Volumes"),
                            Page::Analyse => tr("Analyse de l'espace", "Space analysis"),
                            Page::Cleanup => tr("Nettoyage", "Cleanup"),
                        }}
                    </h2>
                    <button class="icon-btn" title=tr("Actualiser", "Refresh") on:click=move |_| refresh()>
                        <Icon name="refresh" size=15 />
                    </button>
                </header>
                <div class="main-body">
                    {move || match page.get() {
                        Page::Volumes => volumes_view().into_any(),
                        Page::Analyse => analyse_view().into_any(),
                        Page::Cleanup => {
                            view! {
                                <p class="muted small">
                                    {tr(
                                        "Chaque nettoyage ouvre un terminal avec la commande prête ; rien n'est supprimé sans votre validation.",
                                        "Each cleanup opens a terminal with the command ready; nothing is deleted until you confirm.",
                                    )}
                                </p>
                                {cleanup_view}
                            }
                                .into_any()
                        }
                    }}
                </div>
            </section>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn df_and_du() {
        let df = "Filesystem     Type 1B-blocks       Used  Available Capacity Mounted on\n/dev/sda1      ext4 100000 40000 60000 40% /\n/dev/sdb1 ext4 10 5 5 50% /mnt/my disk\n";
        let v = parse_df(df);
        assert_eq!(v.len(), 2);
        assert_eq!(v[1].mount, "/mnt/my disk");
        assert_eq!(v[0].pct(), 40.0);
        let du = parse_du("300\t/var\n200\t/var/lib\n100\t/var/log\n");
        assert_eq!(du[1], (200, "/var/lib".to_string()));
        assert_eq!(parent("/var/lib"), "/var");
        assert_eq!(parent("/var"), "/");
        assert_eq!(probe_text("2048\n"), "2.0 KB");
    }
}
