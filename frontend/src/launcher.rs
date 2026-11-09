//! App catalog and the launcher (Launchpad-like): every app grouped by category, a search field,
//! and a pin on each app to add it to the Dock or remove it.

use leptos::ev;
use leptos::prelude::*;

use crate::icons::Icon;
use crate::state::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Category {
    Files,
    Monitoring,
    Admin,
    Services,
    Security,
    Dev,
    System,
}

impl Category {
    pub const ALL: [Category; 7] = [
        Category::Files,
        Category::Monitoring,
        Category::Admin,
        Category::Services,
        Category::Security,
        Category::Dev,
        Category::System,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Category::Files => tr("Fichiers et terminal", "Files & terminal"),
            Category::Monitoring => tr("Surveillance", "Monitoring"),
            Category::Admin => tr("Administration système", "System administration"),
            Category::Services => tr("Web, réseau et conteneurs", "Web, network & containers"),
            Category::Security => tr("Sécurité et accès", "Security & access"),
            Category::Dev => tr("Développement", "Development"),
            Category::System => "sshOs",
        }
    }
}

pub struct AppEntry {
    /// Stable id, stored in the preferences (Dock content).
    pub id: &'static str,
    pub make: fn() -> AppKind,
    pub cat: Category,
    pub desc: fn() -> &'static str,
}

/// Every app the launcher shows, in display order.
pub const APPS: &[AppEntry] = &[
    AppEntry { id: "terminal", make: || AppKind::Terminal { input: None }, cat: Category::Files, desc: || tr("Shell distant", "Remote shell") },
    AppEntry { id: "files", make: || AppKind::Files { path: None }, cat: Category::Files, desc: || tr("Explorateur SFTP", "SFTP browser") },
    AppEntry { id: "textedit", make: || AppKind::TextEdit { path: None }, cat: Category::Files, desc: || tr("Éditeur de texte", "Text editor") },
    AppEntry { id: "transfers", make: || AppKind::Transfers, cat: Category::Files, desc: || tr("Envoi de fichiers", "File uploads") },
    AppEntry { id: "monitor", make: || AppKind::Monitor, cat: Category::Monitoring, desc: || tr("CPU, mémoire, processus", "CPU, memory, processes") },
    AppEntry { id: "disks", make: || AppKind::Disks, cat: Category::Monitoring, desc: || tr("Espace disque", "Disk space") },
    AppEntry { id: "logs", make: || AppKind::Logs, cat: Category::Monitoring, desc: || tr("journalctl et /var/log", "journalctl and /var/log") },
    AppEntry { id: "fleet", make: || AppKind::Fleet, cat: Category::Monitoring, desc: || tr("Tous les serveurs d'un coup d'œil", "Every server at a glance") },
    AppEntry { id: "services", make: || AppKind::Services, cat: Category::Admin, desc: || tr("Services systemd", "systemd services") },
    AppEntry { id: "updates", make: || AppKind::Updates, cat: Category::Admin, desc: || tr("Paquets à mettre à jour", "Pending package updates") },
    AppEntry { id: "cron", make: || AppKind::Cron, cat: Category::Admin, desc: || tr("crontab et timers systemd", "crontab and systemd timers") },
    AppEntry { id: "backups", make: || AppKind::Backups, cat: Category::Admin, desc: || tr("Archives, restic et borg", "Archives, restic and borg") },
    AppEntry { id: "scripts", make: || AppKind::Scripts, cat: Category::Admin, desc: || tr("Commandes sur plusieurs serveurs", "Commands on several servers") },
    AppEntry { id: "cmdlog", make: || AppKind::CmdLog, cat: Category::Monitoring, desc: || tr("Commandes lancées par sshOs", "Commands run by sshOs") },
    AppEntry { id: "docker", make: || AppKind::Docker, cat: Category::Services, desc: || tr("Conteneurs, images, volumes", "Containers, images, volumes") },
    AppEntry { id: "nginx", make: || AppKind::Nginx, cat: Category::Services, desc: || tr("Sites et Nginx Proxy Manager", "Sites and Nginx Proxy Manager") },
    AppEntry { id: "browser", make: || AppKind::Browser { url: None }, cat: Category::Services, desc: || tr("Pages localhost du serveur", "Server localhost pages") },
    AppEntry { id: "network", make: || AppKind::Network, cat: Category::Services, desc: || tr("Interfaces, ports, ping, DNS", "Interfaces, ports, ping, DNS") },
    AppEntry { id: "tunnels", make: || AppKind::Tunnels, cat: Category::Services, desc: || tr("Redirections de ports SSH", "SSH port forwards") },
    AppEntry { id: "databases", make: || AppKind::Databases, cat: Category::Services, desc: || tr("Postgres, MySQL, Redis en Docker", "Postgres, MySQL, Redis in Docker") },
    AppEntry { id: "security", make: || AppKind::Security, cat: Category::Security, desc: || tr("Tentatives SSH, fail2ban, pare-feu", "SSH attempts, fail2ban, firewall") },
    AppEntry { id: "certs", make: || AppKind::Certs, cat: Category::Security, desc: || tr("Expiration des certificats TLS", "TLS certificate expiry") },
    AppEntry { id: "users", make: || AppKind::Users, cat: Category::Security, desc: || tr("Comptes et clés SSH autorisées", "Accounts and authorized SSH keys") },
    AppEntry { id: "connect", make: || AppKind::Connect, cat: Category::Security, desc: || tr("Serveurs enregistrés", "Saved servers") },
    AppEntry { id: "git", make: || AppKind::Git, cat: Category::Dev, desc: || tr("Dépôts du serveur", "Server repositories") },
    AppEntry { id: "settings", make: || AppKind::Settings, cat: Category::System, desc: || tr("Apparence, sécurité", "Appearance, security") },
    AppEntry { id: "about", make: || AppKind::About, cat: Category::System, desc: || tr("Version et licence", "Version and licence") },
];

/// Dock content on a fresh install.
pub const DEFAULT_DOCK: [&str; 9] = ["terminal", "files", "monitor", "disks", "docker", "nginx", "security", "git", "browser"];

pub fn app(id: &str) -> Option<&'static AppEntry> {
    APPS.iter().find(|a| a.id == id)
}

/// Catalog entry of an open window's app (the nginx/file Editor counts as TextEdit).
pub fn app_of(kind: &AppKind) -> Option<&'static AppEntry> {
    if matches!(kind, AppKind::Editor { .. }) {
        return app("textedit");
    }
    let d = std::mem::discriminant(kind);
    APPS.iter().find(|a| std::mem::discriminant(&(a.make)()) == d)
}

impl Ctx {
    pub fn in_dock(&self, id: &str) -> bool {
        self.prefs.with(|p| p.dock.iter().any(|d| d == id))
    }

    pub fn in_dock_untracked(&self, id: &str) -> bool {
        self.prefs.with_untracked(|p| p.dock.iter().any(|d| d == id))
    }

    pub fn toggle_dock(&self, id: &str) {
        let id = id.to_string();
        self.set_prefs(move |p| {
            if let Some(i) = p.dock.iter().position(|d| *d == id) {
                p.dock.remove(i);
            } else {
                p.dock.push(id);
            }
        });
    }
}

#[component]
pub fn Launcher() -> impl IntoView {
    let c = ctx();
    let query = RwSignal::new(String::new());
    let cat = RwSignal::new(None::<Category>);
    let input = NodeRef::<leptos::html::Input>::new();
    Effect::new(move |_| {
        if let Some(i) = input.get() {
            let _ = i.focus();
        }
    });
    let close = move || c.launcher.set(false);
    let keys = window_event_listener(ev::keydown, move |e| {
        if e.key() == "Escape" {
            c.launcher.set(false);
        }
    });
    on_cleanup(move || keys.remove());

    let matches = move |a: &AppEntry| {
        let q = query.get().to_lowercase();
        let label = (a.make)().label().to_lowercase();
        (q.is_empty() || label.contains(&q) || (a.desc)().to_lowercase().contains(&q) || a.id.contains(&q))
            && cat.get().is_none_or(|k| k == a.cat)
    };

    let tile = move |a: &'static AppEntry| {
        let kind = (a.make)();
        let icon = kind.icon();
        let label = kind.label();
        let pinned = move || c.in_dock(a.id);
        view! {
            <div class="lp-app">
                <button
                    class="lp-open"
                    title=(a.desc)()
                    on:click=move |_| {
                        close();
                        c.open((a.make)(), None);
                    }
                >
                    <span class=format!("dock-tile t-{icon}")>
                        <Icon name=icon size=30 />
                    </span>
                    <span class="lp-name">{label}</span>
                    <span class="lp-desc">{(a.desc)()}</span>
                </button>
                <button
                    class="lp-pin"
                    class:on=pinned
                    title=move || if pinned() { tr("Retirer du Dock", "Remove from Dock") } else { tr("Ajouter au Dock", "Add to Dock") }
                    on:click=move |_| c.toggle_dock(a.id)
                >
                    {move || view! { <Icon name=if pinned() { "check" } else { "plus" } size=12 /> }}
                </button>
            </div>
        }
    };

    let sections = move || {
        Category::ALL
            .iter()
            .filter_map(|k| {
                let apps: Vec<&'static AppEntry> = APPS.iter().filter(|a| a.cat == *k && matches(a)).collect();
                (!apps.is_empty()).then(|| {
                    view! {
                        <section class="lp-section">
                            <h3>{k.label()}</h3>
                            <div class="lp-grid">{apps.into_iter().map(tile).collect_view()}</div>
                        </section>
                    }
                })
            })
            .collect_view()
    };
    let empty = move || !APPS.iter().any(|a| matches(a));

    view! {
        <div class="launchpad" on:click=move |_| close()>
            <div class="lp-inner" on:click=|e| e.stop_propagation()>
                <div class="lp-head">
                    <div class="lp-search">
                        <Icon name="search" size=14 />
                        <input
                            node_ref=input
                            placeholder=tr("Rechercher une app", "Search an app")
                            bind:value=query
                            on:keydown=move |e| {
                                if e.key() == "Enter" {
                                    if let Some(a) = APPS.iter().find(|a| matches(a)) {
                                        close();
                                        c.open((a.make)(), None);
                                    }
                                }
                            }
                        />
                    </div>
                    <div class="lp-cats seg">
                        <button class:on=move || cat.get().is_none() on:click=move |_| cat.set(None)>
                            {tr("Toutes", "All")}
                        </button>
                        {Category::ALL
                            .iter()
                            .map(|k| {
                                let k = *k;
                                view! {
                                    <button class:on=move || cat.get() == Some(k) on:click=move |_| cat.set(Some(k))>
                                        {k.label()}
                                    </button>
                                }
                            })
                            .collect_view()}
                    </div>
                </div>
                <div class="lp-body">
                    {sections}
                    <Show when=empty>
                        <p class="muted lp-empty">{tr("Aucune app ne correspond.", "No matching app.")}</p>
                    </Show>
                </div>
                <div class="lp-foot">
                    <span class="muted">
                        {tr(
                            "Cliquez sur une app pour l'ouvrir, sur + pour l'ajouter au Dock.",
                            "Click an app to open it, + to add it to the Dock.",
                        )}
                    </span>
                    <button on:click=move |_| c.set_prefs(|p| p.dock = DEFAULT_DOCK.iter().map(|s| s.to_string()).collect())>
                        {tr("Rétablir le Dock par défaut", "Reset the Dock")}
                    </button>
                </div>
            </div>
        </div>
    }
}
