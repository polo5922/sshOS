//! Services: systemd units with their state, start / stop / restart, enable at boot, status and
//! journal. Reading is unprivileged; every change is root-only and goes through `kit::privileged`.

use leptos::prelude::*;

use crate::apps::files::shell_quote;
use crate::apps::kit;
use crate::icons::Icon;
use crate::state::*;

#[derive(Clone, Debug, PartialEq)]
pub struct Unit {
    pub name: String,
    pub load: String,
    pub active: String,
    pub sub: String,
    pub desc: String,
    /// enabled / disabled / static / masked… (from list-unit-files).
    pub boot: String,
}

#[derive(Clone, Copy, PartialEq)]
enum Filter {
    All,
    Running,
    Failed,
    Stopped,
    Enabled,
}

impl Filter {
    fn title(self) -> &'static str {
        match self {
            Filter::All => tr("Tous les services", "All services"),
            Filter::Running => tr("En cours", "Running"),
            Filter::Failed => tr("En échec", "Failed"),
            Filter::Stopped => tr("Arrêtés", "Stopped"),
            Filter::Enabled => tr("Lancés au démarrage", "Started at boot"),
        }
    }
    fn keep(self, u: &Unit) -> bool {
        match self {
            Filter::All => true,
            Filter::Running => u.active == "active",
            Filter::Failed => u.active == "failed",
            Filter::Stopped => u.active == "inactive",
            Filter::Enabled => u.boot == "enabled",
        }
    }
}

const SCRIPT: &str = "systemctl list-units --type=service --all --no-legend --plain --no-pager 2>/dev/null; echo @@files; systemctl list-unit-files --type=service --no-legend --no-pager 2>/dev/null";

/// `list-units` lines followed by `@@files` and `list-unit-files` lines.
pub fn parse(out: &str) -> Vec<Unit> {
    let (units, files) = out.split_once("@@files").unwrap_or((out, ""));
    let boot: std::collections::HashMap<&str, &str> = files
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            Some((f.next()?, f.next()?))
        })
        .collect();
    let mut v: Vec<Unit> = units
        .lines()
        .filter_map(|l| {
            let f = kit::fields(l.trim_start_matches('●').trim(), 5);
            if f.len() < 4 || !f[0].ends_with(".service") {
                return None;
            }
            Some(Unit {
                boot: boot.get(f[0].as_str()).unwrap_or(&"").to_string(),
                name: f[0].clone(),
                load: f[1].clone(),
                active: f[2].clone(),
                sub: f[3].clone(),
                desc: f.get(4).cloned().unwrap_or_default(),
            })
        })
        .filter(|u| u.load != "not-found")
        .collect();
    v.sort_by(|a, b| a.name.cmp(&b.name));
    v
}

#[component]
pub fn Services(host: String) -> impl IntoView {
    let host = StoredValue::new(host);
    let units = RwSignal::new(None::<Vec<Unit>>);
    let filter = RwSignal::new(Filter::Running);
    let search = RwSignal::new(String::new());
    let sheet = RwSignal::new(None::<(String, String)>);
    let loading = RwSignal::new(false);

    let load = move || {
        loading.set(true);
        kit::query(host.get_value(), SCRIPT, move |r| {
            let _ = loading.try_set(false);
            let _ = units.try_set(Some(parse(&r.stdout)));
        });
    };
    load();
    let h = move || host.get_value();
    let act = move |verb: &'static str, unit: String| {
        let title = format!("systemctl {verb} {}", unit.trim_end_matches(".service"));
        kit::privileged(h(), title, format!("systemctl {verb} {}", shell_quote(&unit)), load);
    };
    let status = move |unit: String| {
        let q = shell_quote(&unit);
        kit::show_output(
            h(),
            unit.clone(),
            format!("systemctl status {q} --no-pager -n 0 2>&1; echo; echo '── journal ──'; journalctl -u {q} -n 150 --no-pager -o short-iso 2>&1"),
            sheet,
        );
    };
    let count = move |f: Filter| move || units.with(|u| u.as_ref().map(|u| u.iter().filter(|x| f.keep(x)).count()).unwrap_or(0));

    let side_item = move |f: Filter, icon: &'static str| {
        view! {
            <button class="side-item" class:active=move || filter.get() == f on:click=move |_| filter.set(f)>
                <Icon name=icon size=15 />
                <span class="grow">{f.title()}</span>
                <span class="side-count" class:crit={move || f == Filter::Failed && count(f)() > 0}>{count(f)}</span>
            </button>
        }
    };

    let table = move || {
        let Some(all) = units.get() else { return kit::loading_view().into_any() };
        let q = search.get().to_lowercase();
        let f = filter.get();
        let rows: Vec<Unit> = all
            .into_iter()
            .filter(|u| f.keep(u) && (q.is_empty() || u.name.to_lowercase().contains(&q) || u.desc.to_lowercase().contains(&q)))
            .collect();
        if rows.is_empty() {
            return kit::empty_view(tr("Aucun service", "No services")).into_any();
        }
        view! {
            <div class="table-card">
                <table class="table data-table">
                    <thead><tr><th>{tr("Service", "Service")}</th><th>{tr("État", "State")}</th><th>{tr("Au démarrage", "At boot")}</th><th></th></tr></thead>
                    <tbody>
                        {rows
                            .into_iter()
                            .map(|u| {
                                let cls = match u.active.as_str() {
                                    "active" => "pill ok",
                                    "failed" => "pill crit",
                                    "activating" | "deactivating" | "reloading" => "pill warn",
                                    _ => "pill off",
                                };
                                let running = u.active == "active";
                                let enabled = u.boot == "enabled";
                                let can_toggle = matches!(u.boot.as_str(), "enabled" | "disabled");
                                let (n1, n2, n3, n4) = (u.name.clone(), u.name.clone(), u.name.clone(), u.name.clone());
                                view! {
                                    <tr>
                                        <td class="name-cell">
                                            <div class="cell-flex">
                                                <Icon name="cog" size=14 />
                                                <div class="ellipsis">
                                                    <div class="ellipsis">{u.name.trim_end_matches(".service").to_string()}</div>
                                                    <div class="muted small ellipsis">{u.desc.clone()}</div>
                                                </div>
                                            </div>
                                        </td>
                                        <td><span class=cls>{format!("{} · {}", u.active, u.sub)}</span></td>
                                        <td>
                                            {if can_toggle {
                                                view! {
                                                    <button class="switch" class:on=enabled title=tr("Lancer au démarrage", "Start at boot") on:click=move |_| act(if enabled { "disable" } else { "enable" }, n1.clone())>
                                                        <span></span>
                                                    </button>
                                                }
                                                .into_any()
                                            } else {
                                                view! { <span class="muted small">{u.boot.clone()}</span> }.into_any()
                                            }}
                                        </td>
                                        <td class="row-actions">
                                            {if running {
                                                view! {
                                                    <button class="icon-btn" title=tr("Arrêter…", "Stop…") on:click=move |_| act("stop", n2.clone())><Icon name="stop" size=14 /></button>
                                                }
                                                .into_any()
                                            } else {
                                                view! {
                                                    <button class="icon-btn" title=tr("Démarrer…", "Start…") on:click=move |_| act("start", n2.clone())><Icon name="play" size=14 /></button>
                                                }
                                                .into_any()
                                            }}
                                            <button class="icon-btn" title=tr("Redémarrer…", "Restart…") on:click=move |_| act("restart", n3.clone())><Icon name="rotate" size=14 /></button>
                                            <button class="icon-btn" title=tr("État et journal", "Status and journal") on:click=move |_| status(n4.clone())><Icon name="logs" size=14 /></button>
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

    app_menus(vec![
        MenuSection::new(tr("Services", "Services"), vec![
            MenuItem::new(tr("Actualiser", "Refresh"), load),
            MenuItem::new(tr("Recharger la configuration systemd…", "Reload systemd configuration…"), move || {
                kit::privileged(h(), "systemctl daemon-reload", "systemctl daemon-reload", load)
            }),
            MenuItem::new(tr("Réinitialiser les échecs…", "Reset failed units…"), move || kit::privileged(h(), "systemctl reset-failed", "systemctl reset-failed", load)),
        ]),
        MenuSection::new(
            tr("Présentation", "View"),
            [Filter::All, Filter::Running, Filter::Failed, Filter::Stopped, Filter::Enabled]
                .into_iter()
                .map(|f| MenuItem::new(f.title(), move || filter.set(f)).checked(move || filter.get() == f))
                .collect(),
        ),
    ]);

    view! {
        <div class="split-app services-app">
            <aside class="side drag-region">
                <div class="side-top"></div>
                <div class="side-title">"systemd"</div>
                {side_item(Filter::Running, "play")}
                {side_item(Filter::Failed, "alert")}
                {side_item(Filter::Stopped, "stop")}
                {side_item(Filter::Enabled, "zap")}
                {side_item(Filter::All, "list")}
            </aside>
            <section class="main">
                <header class="main-head drag-region">
                    <h2 class="grow ellipsis">{move || filter.get().title()}</h2>
                    <label class="search-field no-drag">
                        <Icon name="search" size=13 />
                        <input placeholder=tr("Rechercher", "Search") bind:value=search />
                    </label>
                    <button class="icon-btn" class:spin=move || loading.get() title=tr("Actualiser", "Refresh") on:click=move |_| load()>
                        <Icon name="refresh" size=15 />
                    </button>
                </header>
                <div class="main-body">{table}</div>
            </section>
            {kit::text_sheet(sheet)}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_units() {
        let out = "nginx.service loaded active running A high performance web server\n● foo.service loaded failed failed Foo daemon\nbar.service not-found inactive dead bar.service\n@@files\nnginx.service enabled enabled\nfoo.service disabled enabled\n";
        let u = parse(out);
        assert_eq!(u.len(), 2);
        assert_eq!(u[0].name, "foo.service");
        assert_eq!(u[0].active, "failed");
        assert_eq!(u[0].desc, "Foo daemon");
        assert_eq!(u[1].boot, "enabled");
        assert_eq!(u[1].desc, "A high performance web server");
    }
}
