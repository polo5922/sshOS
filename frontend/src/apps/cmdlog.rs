//! Command log: every command sshOs ran on a server (and every command it typed in a terminal),
//! newest first. Lets you check what the apps do, copy a command or replay it in a terminal.

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::json;
use sshos_shared::{CmdLogEntry, CmdLogKind};

use crate::icons::Icon;
use crate::state::*;
use crate::tauri::{copy_text, invoke, invoke_unit};

#[derive(Clone, Copy, PartialEq)]
enum Filter {
    All,
    Exec,
    Read,
    Terminal,
}

fn kind_label(k: CmdLogKind) -> (&'static str, &'static str) {
    match k {
        CmdLogKind::Exec => ("k-exec", tr("action", "action")),
        CmdLogKind::Read => ("k-read", tr("lecture", "read")),
        CmdLogKind::Terminal => ("k-term", tr("terminal", "terminal")),
    }
}

fn clock(ms: u64) -> String {
    let d = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(ms as f64));
    format!("{:02}:{:02}:{:02}", d.get_hours(), d.get_minutes(), d.get_seconds())
}

#[component]
pub fn CmdLog() -> impl IntoView {
    let c = ctx();
    let entries = RwSignal::new(Vec::<CmdLogEntry>::new());
    let filter = RwSignal::new(Filter::All);
    let host = RwSignal::new(None::<String>);
    let search = RwSignal::new(String::new());
    let paused = RwSignal::new(false);

    let load = move || {
        spawn_local(async move {
            if let Ok(v) = invoke::<Vec<CmdLogEntry>>("cmdlog_list", json!({})).await {
                let _ = entries.try_set(v);
            }
        });
    };
    load();
    if let Ok(h) = set_interval_with_handle(
        move || {
            if !paused.get_untracked() {
                load()
            }
        },
        std::time::Duration::from_secs(2),
    ) {
        on_cleanup(move || h.clear());
    }
    let clear = move || {
        c.show(
            Dialog::confirm(
                tr("Vider l'historique", "Clear the log"),
                tr("Toutes les entrées seront effacées de la mémoire de sshOs.", "Every entry is removed from sshOs memory."),
                tr("Vider", "Clear"),
                move || {
                    spawn_local(async move {
                        let _ = invoke_unit("cmdlog_clear", json!({})).await;
                        load();
                    })
                },
            )
            .danger(),
        )
    };

    app_menus(vec![MenuSection::new(
        tr("Historique", "Log"),
        vec![
            MenuItem::new(tr("Pause", "Pause"), move || paused.update(|p| *p = !*p)).checked(move || paused.get()),
            MenuItem::new(tr("Actualiser", "Refresh"), load),
            None,
            MenuItem::new(tr("Vider l'historique…", "Clear the log…"), clear),
        ],
    )]);

    let visible = move || {
        let f = filter.get();
        let h = host.get();
        let q = search.get().to_lowercase();
        entries
            .get()
            .into_iter()
            .filter(|e| match f {
                Filter::All => true,
                Filter::Exec => e.kind == CmdLogKind::Exec,
                Filter::Read => e.kind == CmdLogKind::Read,
                Filter::Terminal => e.kind == CmdLogKind::Terminal,
            })
            .filter(|e| h.as_ref().map(|h| *h == e.host_id).unwrap_or(true))
            .filter(|e| q.is_empty() || e.command.to_lowercase().contains(&q))
            .collect::<Vec<_>>()
    };
    let side = move |f: Filter, icon: &'static str, label: &'static str| {
        let count = move || {
            entries.with(|v| {
                v.iter()
                    .filter(|e| match f {
                        Filter::All => true,
                        Filter::Exec => e.kind == CmdLogKind::Exec,
                        Filter::Read => e.kind == CmdLogKind::Read,
                        Filter::Terminal => e.kind == CmdLogKind::Terminal,
                    })
                    .count()
            })
        };
        view! {
            <button class="side-item" class:active=move || filter.get() == f on:click=move |_| filter.set(f)>
                <Icon name=icon size=15 />
                <span class="grow">{label}</span>
                <span class="side-count">{count}</span>
            </button>
        }
    };
    let hosts = move || {
        let mut v: Vec<(String, String)> = entries.with(|e| e.iter().map(|e| (e.host_id.clone(), e.host_name.clone())).collect());
        v.sort();
        v.dedup();
        v
    };

    view! {
        <div class="split-app cmdlog-app">
            <aside class="side drag-region">
                <div class="side-top"></div>
                {side(Filter::All, "list", tr("Toutes", "All"))}
                {side(Filter::Exec, "zap", tr("Actions", "Actions"))}
                {side(Filter::Read, "eye", tr("Lectures", "Reads"))}
                {side(Filter::Terminal, "terminal", tr("Terminal", "Terminal"))}
                <div class="side-title">{tr("Serveurs", "Servers")}</div>
                <button class="side-item" class:active=move || host.get().is_none() on:click=move |_| host.set(None)>
                    <Icon name="server" size=15 />
                    <span class="grow">{tr("Tous", "All")}</span>
                </button>
                {move || {
                    hosts()
                        .into_iter()
                        .map(|(id, name)| {
                            let (i1, i2) = (id.clone(), id.clone());
                            view! {
                                <button class="side-item" class:active=move || host.get().as_deref() == Some(&i1) on:click=move |_| host.set(Some(i2.clone()))>
                                    <Icon name="server" size=15 />
                                    <span class="grow ellipsis">{if name.is_empty() { id } else { name }}</span>
                                </button>
                            }
                        })
                        .collect_view()
                }}
            </aside>
            <section class="main">
                <header class="main-head drag-region">
                    <h2 class="grow">{tr("Historique des commandes", "Command log")}</h2>
                    <label class="search-field no-drag">
                        <Icon name="search" size=13 />
                        <input placeholder=tr("Rechercher", "Search") bind:value=search />
                    </label>
                    <button class="icon-btn" class:on=move || paused.get() title=tr("Pause", "Pause") on:click=move |_| paused.update(|p| *p = !*p)>
                        <Icon name="pause" size=15 />
                    </button>
                    <button class="icon-btn" title=tr("Vider l'historique", "Clear the log") on:click=move |_| clear()>
                        <Icon name="trash" size=15 />
                    </button>
                </header>
                <div class="main-body cmdlog-body">
                    <p class="muted small">
                        {tr(
                            "Les requêtes périodiques (moniteur, notifications, statistiques) ne sont pas listées. Conservé en mémoire jusqu'à la fermeture de sshOs.",
                            "Periodic polling (monitor, notifications, stats) is not listed. Kept in memory until sshOs quits.",
                        )}
                    </p>
                    {move || {
                        let v = visible();
                        if v.is_empty() {
                            return view! { <div class="empty muted">{tr("Aucune commande.", "No command.")}</div> }.into_any();
                        }
                        view! {
                            <table class="table cmdlog">
                                <tbody>
                                    {v.into_iter()
                                        .map(|e| {
                                            let (cls, lab) = kind_label(e.kind);
                                            let ok = e.code.map(|c| c == 0);
                                            let (cmd1, cmd2) = (e.command.clone(), e.command.clone());
                                            let host_id = e.host_id.clone();
                                            view! {
                                                <tr>
                                                    <td class="muted num">{clock(e.at_ms)}</td>
                                                    <td class="ellipsis cl-host">{e.host_name.clone()}</td>
                                                    <td><span class=format!("kind {cls}")>{lab}</span></td>
                                                    <td class="mono cl-cmd" title=e.command.clone()>{e.command.clone()}</td>
                                                    <td class="num">
                                                        {match ok {
                                                            Some(true) => view! { <span class="ok">"0"</span> }.into_any(),
                                                            Some(false) => view! { <span class="crit">{e.code.unwrap_or(0).to_string()}</span> }.into_any(),
                                                            None => view! { <span class="muted">"—"</span> }.into_any(),
                                                        }}
                                                    </td>
                                                    <td class="muted num">{if e.duration_ms > 0 { format!("{} ms", e.duration_ms) } else { String::new() }}</td>
                                                    <td class="cl-actions">
                                                        <button class="icon-btn" title=tr("Copier", "Copy") on:click=move |_| {
                                                            copy_text(&cmd1);
                                                            c.ok(tr("Commande copiée.", "Command copied."));
                                                        }>
                                                            <Icon name="layers" size=13 />
                                                        </button>
                                                        <button
                                                            class="icon-btn"
                                                            title=tr("Retaper dans un terminal (sans l'exécuter)", "Type again in a terminal (not run)")
                                                            on:click=move |_| c.open(AppKind::Terminal { input: Some(cmd2.clone()) }, Some(host_id.clone()))
                                                        >
                                                            <Icon name="terminal" size=13 />
                                                        </button>
                                                    </td>
                                                </tr>
                                            }
                                        })
                                        .collect_view()}
                                </tbody>
                            </table>
                        }
                            .into_any()
                    }}
                </div>
            </section>
        </div>
    }
}
