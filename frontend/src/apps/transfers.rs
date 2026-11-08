//! Transfers: send files from this computer to a server folder (button or drag and drop from the
//! OS file manager), fetch a remote file, and follow every transfer in progress.

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::json;

use crate::icons::Icon;
use crate::state::*;
use crate::tauri::{invoke, invoke_unit, pick_files, pick_save};

#[component]
pub fn Transfers() -> impl IntoView {
    let c = ctx();
    // Destination host: the active one, can be changed here.
    let host = RwSignal::new(c.active.get_untracked().unwrap_or_default());
    let dir = RwSignal::new(String::new());
    let remote_file = RwSignal::new(String::new());

    // Default folder: the home directory of the SSH user.
    let load_home = move || {
        let h = host.get_untracked();
        if h.is_empty() {
            return;
        }
        let home = c.info(&h).map(|i| i.home).filter(|h| !h.is_empty()).unwrap_or_else(|| "/tmp".into());
        dir.set(home);
    };
    load_home();

    let upload = move || {
        let (h, d) = (host.get_untracked(), dir.get_untracked());
        if h.is_empty() || d.trim().is_empty() {
            c.error(tr("Choisissez un serveur et un dossier de destination.", "Pick a server and a destination folder."));
            return;
        }
        spawn_local(async move {
            for local in pick_files().await {
                let (h, d) = (h.clone(), d.clone());
                spawn_local(async move {
                    if let Err(e) = invoke::<String>("upload", json!({ "hostId": h, "local": local, "remoteDir": d })).await {
                        c.handle_err(Some(&h), e);
                    }
                });
            }
        });
    };
    let download = move || {
        let (h, p) = (host.get_untracked(), remote_file.get_untracked().trim().to_string());
        if h.is_empty() || p.is_empty() {
            return;
        }
        spawn_local(async move {
            let name = p.rsplit('/').next().unwrap_or("file").to_string();
            let Some(local) = pick_save(&name).await else { return };
            match invoke_unit("download", json!({ "hostId": h, "remote": p, "local": local })).await {
                Ok(()) => c.ok(tr("Fichier téléchargé.", "File downloaded.")),
                Err(e) => c.handle_err(Some(&h), e),
            }
        });
    };

    app_menus(vec![MenuSection::new(
        tr("Fichier", "File"),
        vec![
            MenuItem::new(tr("Envoyer des fichiers…", "Upload files…"), upload).key("Ctrl+U"),
            MenuItem::new(tr("Effacer les transferts terminés", "Clear finished transfers"), move || {
                c.transfers.update(|t| t.retain(|x| !x.finished))
            }),
        ],
    )]);

    let hosts = move || c.sessions.with(|s| s.iter().map(|(h, _)| h.clone()).collect::<Vec<_>>());
    let list = move || {
        let t = c.transfers.get();
        if t.is_empty() {
            return view! { <div class="empty muted">{tr("Aucun transfert en cours.", "No transfer in progress.")}</div> }.into_any();
        }
        t.into_iter()
            .map(|t| {
                let pct = if t.total > 0 { t.done as f64 * 100.0 / t.total as f64 } else { 0.0 };
                let status = match (&t.error, t.finished) {
                    (Some(e), _) => format!("{} : {e}", tr("échec", "failed")),
                    (None, true) => tr("terminé", "done").to_string(),
                    _ => format!("{} / {} · {pct:.0} %", human_bytes(t.done as f64), human_bytes(t.total as f64)),
                };
                view! {
                    <div class="set-card xfer-row" class:failed=t.error.is_some()>
                        <Icon name=if t.finished && t.error.is_none() { "check" } else { "arrows" } size=16 />
                        <div class="grow">
                            <div class="ellipsis">{t.name}</div>
                            <div class="bar"><div style=format!("width:{pct}%")></div></div>
                        </div>
                        <span class="muted small xfer-status">{status}</span>
                    </div>
                }
            })
            .collect_view()
            .into_any()
    };

    view! {
        <div class="split-app transfers-app">
            <aside class="side drag-region">
                <div class="side-top"></div>
                <div class="side-title">{tr("Serveur", "Server")}</div>
                {move || {
                    let hs = hosts();
                    if hs.is_empty() {
                        return view! { <div class="muted small side-note">{tr("Aucun serveur connecté.", "No server connected.")}</div> }.into_any();
                    }
                    hs.into_iter()
                        .map(|h| {
                            let h2 = h.clone();
                            let h3 = h.clone();
                            view! {
                                <button
                                    class="side-item"
                                    class:active=move || host.get() == h2
                                    on:click=move |_| {
                                        host.set(h3.clone());
                                        dir.set(String::new());
                                        load_home();
                                    }
                                >
                                    <Icon name="server" size=15 />
                                    <span class="grow ellipsis">{c.host_name(&h)}</span>
                                </button>
                            }
                        })
                        .collect_view()
                        .into_any()
                }}
            </aside>
            <section class="main">
                <header class="main-head drag-region">
                    <h2 class="grow">{tr("Transferts", "Transfers")}</h2>
                    <button class="icon-btn" title=tr("Effacer les transferts terminés", "Clear finished transfers") on:click=move |_| c.transfers.update(|t| t.retain(|x| !x.finished))>
                        <Icon name="trash" size=15 />
                    </button>
                </header>
                <div class="main-body">
                    <div
                        class="dropzone"
                        attr:data-drop-dir=move || dir.get()
                        attr:data-drop-host=move || host.get()
                    >
                        <Icon name="upload" size=30 />
                        <div class="dz-title">{tr("Glissez des fichiers ici depuis l'explorateur", "Drag files here from your file manager")}</div>
                        <div class="dz-row">
                            <span class="muted small">{tr("Vers", "To")}</span>
                            <input class="mono grow" bind:value=dir placeholder="/home/user" />
                            <button class="primary" disabled=move || host.get().is_empty() on:click=move |_| upload()>
                                <Icon name="upload" size=13 />
                                {tr("Envoyer des fichiers…", "Upload files…")}
                            </button>
                        </div>
                        <div class="muted small">
                            {tr(
                                "Vous pouvez aussi déposer des fichiers sur une fenêtre Fichiers : ils y sont envoyés dans le dossier affiché.",
                                "You can also drop files on a Files window: they go to the folder it shows.",
                            )}
                        </div>
                    </div>
                    <div class="set-card dl-row">
                        <Icon name="download" size=16 />
                        <input class="mono grow" bind:value=remote_file placeholder="/var/log/syslog" />
                        <button disabled=move || remote_file.get().trim().is_empty() || host.get().is_empty() on:click=move |_| download()>
                            {tr("Télécharger…", "Download…")}
                        </button>
                    </div>
                    <div class="msection">{tr("Transferts", "Transfers")}</div>
                    {list}
                </div>
            </section>
        </div>
    }
}
