//! Updates: pending package upgrades (apt or dnf/yum), security ones highlighted, "reboot required"
//! flag. Refresh, upgrade and reboot are root commands: always pre-typed in a Terminal.

use leptos::prelude::*;

use crate::apps::kit;
use crate::icons::Icon;
use crate::state::*;

#[derive(Clone, Debug, PartialEq)]
pub struct Pkg {
    pub name: String,
    pub version: String,
    pub from: String,
    pub origin: String,
    pub security: bool,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Report {
    /// "apt", "dnf", "yum" or "" when none was found.
    pub pm: String,
    pub pkgs: Vec<Pkg>,
    pub reboot: bool,
    pub reboot_pkgs: Vec<String>,
}

const SCRIPT: &str = r#"if command -v apt-get >/dev/null 2>&1; then echo "@@pm apt"; apt list --upgradable 2>/dev/null;
elif command -v dnf >/dev/null 2>&1; then echo "@@pm dnf"; dnf -q check-update 2>/dev/null; echo @@sec; dnf -q updateinfo list --security 2>/dev/null;
elif command -v yum >/dev/null 2>&1; then echo "@@pm yum"; yum -q check-update 2>/dev/null; echo @@sec; yum -q updateinfo list security 2>/dev/null; fi
echo @@reboot; if [ -f /var/run/reboot-required ]; then echo REQUIRED; cat /var/run/reboot-required.pkgs 2>/dev/null; fi
if command -v needs-restarting >/dev/null 2>&1; then needs-restarting -r >/dev/null 2>&1 || echo REQUIRED; fi"#;

pub fn parse(out: &str) -> Report {
    let mut r = Report::default();
    let mut section = "";
    let mut sec_names: Vec<String> = Vec::new();
    for line in out.lines() {
        let l = line.trim();
        if let Some(pm) = l.strip_prefix("@@pm ") {
            r.pm = pm.to_string();
            section = "pkgs";
            continue;
        }
        match l {
            "@@sec" => {
                section = "sec";
                continue;
            }
            "@@reboot" => {
                section = "reboot";
                continue;
            }
            _ => {}
        }
        if l.is_empty() {
            continue;
        }
        match section {
            "pkgs" if r.pm == "apt" => {
                // nginx/jammy-updates,jammy-security 1.18.0-6ubuntu14.4 amd64 [upgradable from: 1.18.0-6ubuntu14.3]
                let Some((name, rest)) = l.split_once('/') else { continue };
                let f: Vec<&str> = rest.split_whitespace().collect();
                if f.len() < 2 {
                    continue;
                }
                let from = l.split("from: ").nth(1).map(|s| s.trim_end_matches(']').to_string()).unwrap_or_default();
                r.pkgs.push(Pkg { name: name.to_string(), version: f[1].to_string(), from, origin: f[0].to_string(), security: f[0].contains("-security") });
            }
            "pkgs" => {
                // kernel.x86_64   5.14.0-362.el9   baseos
                let f: Vec<&str> = l.split_whitespace().collect();
                if f.len() != 3 || !f[0].contains('.') || l.starts_with("Obsoleting") || l.starts_with("Last metadata") {
                    continue;
                }
                let name = f[0].rsplit_once('.').map(|(n, _)| n).unwrap_or(f[0]);
                r.pkgs.push(Pkg { name: name.to_string(), version: f[1].to_string(), from: String::new(), origin: f[2].to_string(), security: false });
            }
            "sec" => {
                // RHSA-2023:1234 Important/Sec. kernel-5.14.0-362.el9.x86_64
                if let Some(nevra) = l.split_whitespace().nth(2) {
                    sec_names.push(nevra.to_string());
                }
            }
            "reboot" => {
                if l == "REQUIRED" {
                    r.reboot = true;
                } else if !r.reboot_pkgs.iter().any(|p| p == l) {
                    r.reboot_pkgs.push(l.to_string());
                }
            }
            _ => {}
        }
    }
    for p in r.pkgs.iter_mut().filter(|p| !p.security) {
        let prefix = format!("{}-", p.name);
        p.security = sec_names.iter().any(|n| n.strip_prefix(&prefix).is_some_and(|v| v.starts_with(|c: char| c.is_ascii_digit())));
    }
    r.pkgs.sort_by(|a, b| b.security.cmp(&a.security).then(a.name.cmp(&b.name)));
    r
}

/// Commands per package manager: (refresh, upgrade all, security only).
fn commands(pm: &str, sec: &[String]) -> (String, String, String) {
    match pm {
        "apt" => (
            "apt-get update".into(),
            "apt-get update && apt-get upgrade".into(),
            if sec.is_empty() { String::new() } else { format!("apt-get install --only-upgrade {}", sec.join(" ")) },
        ),
        pm => (format!("{pm} makecache"), format!("{pm} upgrade"), format!("{pm} upgrade --security")),
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Filter {
    All,
    Security,
}

#[component]
pub fn Updates(host: String) -> impl IntoView {
    let host = StoredValue::new(host);
    let report = RwSignal::new(None::<Report>);
    let filter = RwSignal::new(Filter::All);
    let search = RwSignal::new(String::new());
    let loading = RwSignal::new(false);
    let h = move || host.get_value();

    let load = move || {
        loading.set(true);
        kit::query(host.get_value(), SCRIPT, move |r| {
            let _ = loading.try_set(false);
            let _ = report.try_set(Some(parse(&r.stdout)));
        });
    };
    load();

    let pm = move || report.with(|r| r.as_ref().map(|r| r.pm.clone()).unwrap_or_default());
    let sec_list = move || report.with(|r| r.as_ref().map(|r| r.pkgs.iter().filter(|p| p.security).map(|p| p.name.clone()).collect::<Vec<_>>()).unwrap_or_default());
    let refresh_cmd = move || {
        let pm = pm();
        if !pm.is_empty() {
            kit::in_terminal(&h(), &commands(&pm, &[]).0)
        }
    };
    let upgrade_all = move || {
        let pm = pm();
        if !pm.is_empty() {
            kit::in_terminal(&h(), &commands(&pm, &[]).1)
        }
    };
    let upgrade_sec = move || {
        let pm = pm();
        let cmd = commands(&pm, &sec_list()).2;
        if !pm.is_empty() && !cmd.is_empty() {
            kit::in_terminal(&h(), &cmd)
        }
    };
    let reboot = move || kit::in_terminal(&h(), "reboot");
    let upgrade_one = move |name: String| {
        let pm = pm();
        let cmd = if pm == "apt" { format!("apt-get install --only-upgrade {name}") } else { format!("{pm} upgrade {name}") };
        kit::in_terminal(&h(), &cmd);
    };

    let count = move |f: Filter| {
        move || report.with(|r| r.as_ref().map(|r| r.pkgs.iter().filter(|p| f == Filter::All || p.security).count()).unwrap_or(0))
    };
    let side_item = move |f: Filter, label: &'static str, icon: &'static str| {
        view! {
            <button class="side-item" class:active=move || filter.get() == f on:click=move |_| filter.set(f)>
                <Icon name=icon size=15 />
                <span class="grow">{label}</span>
                <span class="side-count" class:crit={move || f == Filter::Security && count(f)() > 0}>{count(f)}</span>
            </button>
        }
    };

    let body = move || {
        let Some(r) = report.get() else { return kit::loading_view().into_any() };
        if r.pm.is_empty() {
            return kit::empty_view(tr("Aucun gestionnaire de paquets reconnu (apt, dnf, yum).", "No supported package manager (apt, dnf, yum).")).into_any();
        }
        let reboot_banner = r.reboot.then(|| {
            let pkgs = r.reboot_pkgs.join(", ");
            view! {
                <div class="banner warn">
                    <Icon name="rotate" size=15 />
                    <span class="grow">
                        {tr("Redémarrage nécessaire pour terminer des mises à jour.", "A reboot is required to finish updates.")}
                        {(!pkgs.is_empty()).then(|| view! { <span class="muted small">{format!(" ({pkgs})")}</span> })}
                    </span>
                    <button class="btn small" on:click=move |_| reboot()>{tr("Redémarrer…", "Reboot…")}</button>
                </div>
            }
        });
        let q = search.get().to_lowercase();
        let f = filter.get();
        let rows: Vec<Pkg> = r.pkgs.into_iter().filter(|p| (f == Filter::All || p.security) && (q.is_empty() || p.name.to_lowercase().contains(&q))).collect();
        let table = if rows.is_empty() {
            kit::empty_view(if f == Filter::Security { tr("Aucune mise à jour de sécurité.", "No security updates.") } else { tr("Le système est à jour.", "The system is up to date.") })
                .into_any()
        } else {
            view! {
                <div class="table-card">
                    <table class="table data-table">
                        <thead><tr><th>{tr("Paquet", "Package")}</th><th>{tr("Version installée", "Installed")}</th><th>{tr("Nouvelle version", "New version")}</th><th>{tr("Source", "Source")}</th><th></th></tr></thead>
                        <tbody>
                            {rows
                                .into_iter()
                                .map(|p| {
                                    let n = p.name.clone();
                                    view! {
                                        <tr>
                                            <td class="name-cell">
                                                <div class="cell-flex">
                                                    <Icon name="package" size=14 />
                                                    <span class="ellipsis">{p.name.clone()}</span>
                                                    {p.security.then(|| view! { <span class="pill crit">{tr("sécurité", "security")}</span> })}
                                                </div>
                                            </td>
                                            <td class="mono muted">{if p.from.is_empty() { "—".to_string() } else { p.from.clone() }}</td>
                                            <td class="mono">{p.version.clone()}</td>
                                            <td class="muted small ellipsis">{p.origin.clone()}</td>
                                            <td class="row-actions">
                                                <button class="icon-btn" title=tr("Mettre à jour ce paquet…", "Upgrade this package…") on:click=move |_| upgrade_one(n.clone())>
                                                    <Icon name="download" size=14 />
                                                </button>
                                            </td>
                                        </tr>
                                    }
                                })
                                .collect_view()}
                        </tbody>
                    </table>
                </div>
            }
            .into_any()
        };
        view! { {reboot_banner} {table} }.into_any()
    };

    app_menus(vec![MenuSection::new(tr("Mises à jour", "Updates"), vec![
        MenuItem::new(tr("Actualiser la liste", "Refresh list"), load),
        MenuItem::new(tr("Rechercher les mises à jour sur les dépôts…", "Check repositories for updates…"), refresh_cmd),
        None,
        MenuItem::new(tr("Tout mettre à jour…", "Upgrade everything…"), upgrade_all),
        MenuItem::new(tr("Mises à jour de sécurité seulement…", "Security updates only…"), upgrade_sec),
        None,
        MenuItem::new(tr("Redémarrer le serveur…", "Reboot the server…"), reboot),
    ])]);

    view! {
        <div class="split-app updates-app">
            <aside class="side drag-region">
                <div class="side-top"></div>
                <div class="side-title">{move || match pm().as_str() { "" => tr("Paquets", "Packages").to_string(), pm => pm.to_string() }}</div>
                {side_item(Filter::All, tr("Toutes", "All"), "package")}
                {side_item(Filter::Security, tr("Sécurité", "Security"), "shield")}
                <div class="side-title">{tr("Actions", "Actions")}</div>
                <button class="side-item" on:click=move |_| refresh_cmd()><Icon name="search" size=15 /><span class="grow">{tr("Rechercher…", "Check…")}</span></button>
                <button class="side-item" on:click=move |_| upgrade_all()><Icon name="download" size=15 /><span class="grow">{tr("Tout mettre à jour…", "Upgrade everything…")}</span></button>
                <button class="side-item" on:click=move |_| upgrade_sec()><Icon name="shield-check" size=15 /><span class="grow">{tr("Sécurité seulement…", "Security only…")}</span></button>
                <button class="side-item" on:click=move |_| reboot()><Icon name="rotate" size=15 /><span class="grow">{tr("Redémarrer…", "Reboot…")}</span></button>
            </aside>
            <section class="main">
                <header class="main-head drag-region">
                    <h2 class="grow ellipsis">{move || if filter.get() == Filter::All { tr("Mises à jour disponibles", "Available updates") } else { tr("Mises à jour de sécurité", "Security updates") }}</h2>
                    <label class="search-field no-drag">
                        <Icon name="search" size=13 />
                        <input placeholder=tr("Rechercher", "Search") bind:value=search />
                    </label>
                    <button class="icon-btn" class:spin=move || loading.get() title=tr("Actualiser", "Refresh") on:click=move |_| load()>
                        <Icon name="refresh" size=15 />
                    </button>
                </header>
                <div class="main-body">
                    <p class="muted small hint">{tr("Les mises à jour s'exécutent dans un Terminal : relisez la commande puis appuyez sur Entrée.", "Upgrades run in a Terminal: read the command, then press Enter.")}</p>
                    {body}
                </div>
            </section>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_apt() {
        let out = "@@pm apt\nListing...\nnginx/jammy-updates,jammy-security 1.18.0-6ubuntu14.4 amd64 [upgradable from: 1.18.0-6ubuntu14.3]\ncurl/jammy-updates 7.81.0-1ubuntu1.16 amd64 [upgradable from: 7.81.0-1ubuntu1.15]\n@@reboot\nREQUIRED\nlinux-image-generic\n";
        let r = parse(out);
        assert_eq!(r.pm, "apt");
        assert_eq!(r.pkgs.len(), 2);
        assert_eq!(r.pkgs[0].name, "nginx");
        assert!(r.pkgs[0].security);
        assert_eq!(r.pkgs[1].from, "7.81.0-1ubuntu1.15");
        assert!(r.reboot);
        assert_eq!(r.reboot_pkgs, vec!["linux-image-generic"]);
        assert_eq!(commands("apt", &["nginx".into()]).2, "apt-get install --only-upgrade nginx");
    }

    #[test]
    fn parse_dnf() {
        let out = "@@pm dnf\n\nkernel.x86_64    5.14.0-362.el9    baseos\nvim-common.x86_64  2:8.2.2637-20.el9  appstream\n@@sec\nRHSA-2023:1 Important/Sec. kernel-5.14.0-362.el9.x86_64\n@@reboot\n";
        let r = parse(out);
        assert_eq!(r.pkgs.len(), 2);
        assert_eq!(r.pkgs[0].name, "kernel");
        assert!(r.pkgs[0].security);
        assert!(!r.pkgs[1].security);
        assert!(!r.reboot);
    }
}
