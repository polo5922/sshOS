//! TextEdit: open any text file of the server by path (or from the recent list) in the editor.

use leptos::prelude::*;

use crate::apps::editor::Editor;
use crate::icons::Icon;
use crate::state::*;

const RECENT_KEY: &str = "sshos.recent_files";
const RECENT_MAX: usize = 15;

/// Recent files, stored as "host_id\tpath" lines, newest first.
fn recent() -> Vec<(String, String)> {
    storage_get(RECENT_KEY)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('\t').map(|(h, p)| (h.to_string(), p.to_string())))
        .collect()
}

pub fn remember(host: &str, path: &str) {
    let mut v = recent();
    v.retain(|(h, p)| !(h == host && p == path));
    v.insert(0, (host.to_string(), path.to_string()));
    v.truncate(RECENT_MAX);
    let s: Vec<String> = v.into_iter().map(|(h, p)| format!("{h}\t{p}")).collect();
    storage_set(RECENT_KEY, &s.join("\n"));
}

#[component]
pub fn TextEdit(host: String, start: Option<String>, title: RwSignal<String>) -> impl IntoView {
    let c = ctx();
    let host = StoredValue::new(host);
    let current = RwSignal::new(start.clone());
    let input = RwSignal::new(start.unwrap_or_default());
    let base_title = StoredValue::new(title.get_untracked());

    let open = move |p: String| {
        let p = p.trim().to_string();
        if p.is_empty() {
            return;
        }
        remember(&host.get_value(), &p);
        input.set(p.clone());
        current.set(Some(p));
    };
    if let Some(p) = current.get_untracked() {
        remember(&host.get_value(), &p);
    }

    app_menus(vec![MenuSection::new(
        tr("Fichier", "File"),
        vec![
            MenuItem::new(tr("Ouvrir…", "Open…"), move || {
                current.set(None);
                title.set(base_title.get_value());
            })
            .key("Ctrl+O"),
            MenuItem::new(tr("Afficher dans Fichiers", "Show in Files"), move || {
                if let Some(p) = current.get_untracked() {
                    let dir = p.rsplit_once('/').map(|(d, _)| if d.is_empty() { "/".to_string() } else { d.to_string() });
                    c.open(AppKind::Files { path: dir }, Some(host.get_value()));
                }
            })
            .disabled(move || current.get().is_none()),
        ],
    )]);

    let picker = move || {
        let h = host.get_value();
        let items: Vec<String> = recent().into_iter().filter(|(rh, _)| *rh == h).map(|(_, p)| p).collect();
        view! {
            <div class="textedit-open">
                <Icon name="file" size=40 />
                <h3>{tr("Ouvrir un fichier texte", "Open a text file")}</h3>
                <form
                    class="textedit-form"
                    on:submit=move |e| {
                        e.prevent_default();
                        open(input.get_untracked());
                    }
                >
                    <input class="grow mono" placeholder="/etc/hosts" bind:value=input autofocus />
                    <button class="primary" type="submit" disabled=move || input.get().trim().is_empty()>
                        {tr("Ouvrir", "Open")}
                    </button>
                </form>
                <Show when={
                    let empty = items.is_empty();
                    move || !empty
                }>
                    <div class="side-title">{tr("Récents", "Recent")}</div>
                </Show>
                <div class="textedit-recent">
                    {items
                        .clone()
                        .into_iter()
                        .map(|p| {
                            let p2 = p.clone();
                            view! {
                                <button class="recent-item" on:click=move |_| open(p2.clone())>
                                    <Icon name="file" size=14 />
                                    <span class="ellipsis mono">{p}</span>
                                </button>
                            }
                        })
                        .collect_view()}
                </div>
                <p class="muted small">
                    {tr(
                        "Astuce : dans Fichiers, double-cliquez un fichier texte pour l'ouvrir dans l'éditeur.",
                        "Tip: in Files, double-click a text file to open it in the editor.",
                    )}
                </p>
            </div>
        }
    };

    view! {
        <div class="textedit">
            {move || match current.get() {
                None => picker().into_any(),
                Some(p) => {
                    view! {
                        <div class="textedit-bar">
                            <button
                                class="icon-btn"
                                title=tr("Ouvrir un autre fichier", "Open another file")
                                on:click=move |_| {
                                    current.set(None);
                                    title.set(base_title.get_value());
                                }
                            >
                                <Icon name="left" size=14 />
                            </button>
                            <span class="mono ellipsis grow">{p.clone()}</span>
                        </div>
                        <div class="textedit-body">
                            <Editor host=host.get_value() path=p nginx=false title=title />
                        </div>
                    }
                        .into_any()
                }
            }}
        </div>
    }
}
