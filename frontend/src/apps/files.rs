//! Files: SFTP browser with uploads/downloads and shell file operations (command shown first).

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::json;
use sshos_shared::FileEntry;

use crate::icons::Icon;
use crate::state::*;
use crate::tauri::{invoke, invoke_unit, pick_files, pick_save};

fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

fn parent(p: &str) -> String {
    let t = p.trim_end_matches('/');
    match t.rfind('/') {
        Some(0) | None => "/".into(),
        Some(i) => t[..i].to_string(),
    }
}

pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[component]
pub fn Files(host: String, start: Option<String>) -> impl IntoView {
    let c = ctx();
    let home = c.info(&host).map(|i| i.home).filter(|h| !h.is_empty()).unwrap_or_else(|| "/".into());
    let path = RwSignal::new(start.unwrap_or_else(|| home.clone()));
    let path_input = RwSignal::new(path.get_untracked());
    let entries = RwSignal::new(Vec::<FileEntry>::new());
    let loading = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);
    let selected = RwSignal::new(None::<String>);
    let show_hidden = RwSignal::new(false);
    let filter = RwSignal::new(String::new());
    let history = StoredValue::new(Vec::<String>::new());
    let fwd = StoredValue::new(Vec::<String>::new());
    // (back, forward) stack sizes, to enable the arrows.
    let hist_len = RwSignal::new((0usize, 0usize));
    let editing_path = RwSignal::new(false);
    let sort = RwSignal::new((SortKey::Name, true));
    let host = StoredValue::new(host);

    let load = move || {
        let p = path.get_untracked();
        path_input.set(p.clone());
        loading.set(true);
        spawn_local(async move {
            let r = invoke::<Vec<FileEntry>>("sftp_list", json!({ "hostId": host.get_value(), "path": p })).await;
            loading.set(false);
            match r {
                Ok(mut v) => {
                    v.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
                    entries.set(v);
                    error.set(None);
                }
                Err(e) => error.set(Some(e.message())),
            }
        });
    };
    let sync_hist = move || hist_len.set((history.with_value(|h| h.len()), fwd.with_value(|f| f.len())));
    let go = move |p: String| {
        let cur = path.get_untracked();
        if cur != p {
            history.update_value(|h| h.push(cur));
            fwd.update_value(|f| f.clear());
            sync_hist();
        }
        selected.set(None);
        path.set(p);
        load();
    };
    let back = move || {
        let mut prev = None;
        history.update_value(|h| prev = h.pop());
        if let Some(p) = prev {
            fwd.update_value(|f| f.push(path.get_untracked()));
            sync_hist();
            path.set(p);
            selected.set(None);
            load();
        }
    };
    let forward = move || {
        let mut next = None;
        fwd.update_value(|f| next = f.pop());
        if let Some(p) = next {
            history.update_value(|h| h.push(path.get_untracked()));
            sync_hist();
            path.set(p);
            selected.set(None);
            load();
        }
    };

    // Initial load + refresh after uploads.
    Effect::new(move |_| {
        c.fs_tick.track();
        load();
    });

    let open_entry = move |e: FileEntry| {
        if e.is_dir {
            go(e.path);
        } else {
            let nginx = e.path.starts_with("/etc/nginx/");
            c.open(AppKind::Editor { path: e.path, nginx }, Some(host.get_value()));
        }
    };

    // Run a file operation after showing the exact command.
    let run_op = move |op: &'static str, p: String, target: Option<String>, danger: bool| {
        spawn_local(async move {
            let cmd = match invoke::<String>("file_op_preview", json!({ "op": op, "path": p, "target": target })).await {
                Ok(cmd) => cmd,
                Err(e) => return c.handle_err(None, e),
            };
            let exec = move || {
                let (p, target) = (p.clone(), target.clone());
                spawn_local(async move {
                    let h = host.get_value();
                    match invoke_unit("file_op", json!({ "hostId": h, "op": op, "path": p, "target": target })).await {
                        Ok(()) => load(),
                        Err(e) => c.handle_err(Some(&h), e),
                    }
                });
            };
            let mut d = Dialog::confirm(
                tr("Confirmer l'opération", "Confirm operation"),
                tr("Cette commande va être exécutée sur le serveur :", "This command will run on the server:"),
                tr("Exécuter", "Run"),
                exec,
            )
            .code(cmd);
            if danger {
                d = d.danger();
            }
            c.show(d);
        });
    };
    let prompt = move |title: &'static str, initial: String, then: Callback<String>| {
        let sig = c.root_signal(initial);
        c.show(
            Dialog::confirm(title, "", "OK", move || {
                let v = sig.get_untracked().trim().to_string();
                if !v.is_empty() && !v.contains('/') {
                    then.run(v);
                }
            })
            .with_input(sig),
        );
    };

    let new_folder = move || {
        prompt(
            tr("Nouveau dossier", "New folder"),
            String::new(),
            Callback::new(move |n: String| run_op("mkdir", join(&path.get_untracked(), &n), None, false)),
        )
    };
    let new_file = move || {
        prompt(
            tr("Nouveau fichier", "New file"),
            String::new(),
            Callback::new(move |n: String| run_op("touch", join(&path.get_untracked(), &n), None, false)),
        )
    };
    let rename = move |e: FileEntry| {
        let p = e.path.clone();
        prompt(
            tr("Renommer", "Rename"),
            e.name.clone(),
            Callback::new(move |n: String| run_op("rename", p.clone(), Some(join(&parent(&p), &n)), false)),
        )
    };
    let duplicate = move |e: FileEntry| {
        let p = e.path.clone();
        prompt(
            tr("Copier sous le nom", "Copy as"),
            format!("{}.copy", e.name),
            Callback::new(move |n: String| run_op("copy", p.clone(), Some(join(&parent(&p), &n)), false)),
        )
    };
    let delete = move |e: FileEntry| run_op("delete", e.path, None, true);
    let download = move |e: FileEntry| {
        spawn_local(async move {
            let Some(local) = pick_save(&e.name).await else { return };
            let h = host.get_value();
            if let Err(err) = invoke_unit("download", json!({ "hostId": h, "remote": e.path, "local": local })).await {
                c.handle_err(Some(&h), err);
            }
        });
    };
    let upload = move || {
        spawn_local(async move {
            let h = host.get_value();
            for local in pick_files().await {
                let dir = path.get_untracked();
                let h = h.clone();
                spawn_local(async move {
                    if let Err(err) = invoke::<String>("upload", json!({ "hostId": h, "local": local, "remoteDir": dir })).await {
                        c.handle_err(Some(&h), err);
                    }
                });
            }
        });
    };
    let terminal_here = move || {
        c.open(
            AppKind::Terminal { input: Some(format!("cd {}", shell_quote(&path.get_untracked()))) },
            Some(host.get_value()),
        )
    };

    let sel_entry = move || {
        let s = selected.get()?;
        entries.with(|v| v.iter().find(|e| e.path == s).cloned())
    };

    // Right-click menu, positioned inside the window.
    let menu = RwSignal::new(None::<(f64, f64, FileEntry)>);
    let root = NodeRef::<leptos::html::Div>::new();
    let open_menu = move |ev: leptos::ev::MouseEvent, e: FileEntry| {
        ev.prevent_default();
        ev.stop_propagation();
        selected.set(Some(e.path.clone()));
        let Some(r) = root.get_untracked() else { return };
        let b = r.get_bounding_client_rect();
        let x = (ev.client_x() as f64 - b.left()).min((b.width() - 200.0).max(4.0));
        let y = (ev.client_y() as f64 - b.top()).min((b.height() - 240.0).max(4.0));
        menu.set(Some((x, y, e)));
    };

    let sort_head = move |k: SortKey, label: &'static str, num: bool| {
        view! {
            <th class:num=num class="sortable" class:active=move || sort.get().0 == k on:click=move |_| {
                sort.update(|(key, asc)| if *key == k { *asc = !*asc } else { *key = k; *asc = true })
            }>
                {label}
                <span class="sort-arrow">{move || match sort.get() { (key, true) if key == k => "↑", (key, false) if key == k => "↓", _ => "" }}</span>
            </th>
        }
    };

    let rows = move || {
        let q = filter.get().to_lowercase();
        let hidden = show_hidden.get();
        let (key, asc) = sort.get();
        let mut list: Vec<FileEntry> = entries
            .get()
            .into_iter()
            .filter(|e| (hidden || !e.name.starts_with('.')) && (q.is_empty() || e.name.to_lowercase().contains(&q)))
            .collect();
        list.sort_by(|a, b| {
            let o = match key {
                SortKey::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                SortKey::Size => a.size.cmp(&b.size),
                SortKey::Modified => a.mtime.cmp(&b.mtime),
            };
            // Folders stay on top, whatever the order.
            b.is_dir.cmp(&a.is_dir).then(if asc { o } else { o.reverse() })
        });
        list.into_iter()
            .map(|e| {
                let p = e.path.clone();
                let p2 = e.path.clone();
                let (e2, e3) = (e.clone(), e.clone());
                let icon = if e.is_dir { "folder" } else if e.is_link { "link" } else { "file" };
                let tip = format!("{}  {}:{}", mode_string(e.mode, e.is_dir, e.is_link), e.user, e.group);
                view! {
                    <tr
                        class:sel=move || selected.get().as_deref() == Some(p.as_str())
                        class:dir=e.is_dir
                        title=tip
                        on:click=move |_| selected.set(Some(p2.clone()))
                        on:dblclick=move |_| open_entry(e2.clone())
                        on:contextmenu=move |ev| open_menu(ev, e3.clone())
                    >
                        <td class="name"><span class="f-icon"><Icon name=icon size=15 /></span><span class="ellipsis">{e.name.clone()}</span></td>
                        <td class="num muted">{if e.is_dir { "--".to_string() } else { human_bytes(e.size as f64) }}</td>
                        <td class="muted">{finder_time(e.mtime)}</td>
                    </tr>
                }
            })
            .collect_view()
    };

    let host_label = c.host_name(&host.get_value());
    let fav = move |icon: &'static str, label: String, target: String| {
        let t2 = target.clone();
        view! {
            <button class="side-item" class:active=move || path.get() == t2 on:click=move |_| go(target.clone())>
                <Icon name=icon size=15 />
                <span class="grow ellipsis">{label}</span>
            </button>
        }
    };
    let home_dir = home.clone();

    let crumbs = move || {
        let p = path.get();
        let mut acc = String::new();
        let mut parts = vec![("/".to_string(), host_label.clone(), "server")];
        for seg in p.split('/').filter(|s| !s.is_empty()) {
            acc = format!("{acc}/{seg}");
            parts.push((acc.clone(), seg.to_string(), "folder"));
        }
        let n = parts.len();
        parts
            .into_iter()
            .enumerate()
            .map(|(i, (target, label, icon))| {
                view! {
                    <button class="crumb" on:click=move |_| go(target.clone())>
                        <Icon name=icon size=12 />{label}
                    </button>
                    {(i + 1 < n).then(|| view! { <span class="crumb-sep">"›"</span> })}
                }
            })
            .collect_view()
    };

    let title = move || {
        let p = path.get();
        let t = p.trim_end_matches('/');
        match t.rfind('/') {
            Some(i) if i + 1 < t.len() => t[i + 1..].to_string(),
            _ => "/".to_string(),
        }
    };

    let home_m = home.clone();
    app_menus(vec![
        MenuSection::new(
            tr("Fichier", "File"),
            vec![
                MenuItem::new(tr("Nouveau dossier", "New folder"), new_folder),
                MenuItem::new(tr("Nouveau fichier", "New file"), new_file),
                MenuItem::new(tr("Téléverser…", "Upload…"), upload),
                None,
                MenuItem::new(tr("Terminal ici", "Terminal here"), terminal_here),
            ],
        ),
        MenuSection::new(
            tr("Présentation", "View"),
            vec![
                MenuItem::new(tr("Fichiers cachés", "Hidden files"), move || show_hidden.update(|h| *h = !*h))
                    .checked(move || show_hidden.get()),
                None,
                MenuItem::new(tr("Trier par nom", "Sort by name"), move || sort.set((SortKey::Name, true)))
                    .checked(move || sort.get().0 == SortKey::Name),
                MenuItem::new(tr("Trier par taille", "Sort by size"), move || sort.set((SortKey::Size, false)))
                    .checked(move || sort.get().0 == SortKey::Size),
                MenuItem::new(tr("Trier par date", "Sort by date"), move || sort.set((SortKey::Modified, false)))
                    .checked(move || sort.get().0 == SortKey::Modified),
                None,
                MenuItem::new(tr("Actualiser", "Refresh"), move || load()),
            ],
        ),
        MenuSection::new(
            tr("Aller", "Go"),
            vec![
                MenuItem::new(tr("Précédent", "Back"), move || back()).disabled(move || hist_len.get().0 == 0),
                MenuItem::new(tr("Suivant", "Forward"), move || forward()).disabled(move || hist_len.get().1 == 0),
                MenuItem::new(tr("Dossier parent", "Enclosing folder"), move || go(parent(&path.get_untracked()))),
                MenuItem::new(tr("Départ", "Home"), move || go(home_m.clone())),
                MenuItem::new(tr("Aller au dossier…", "Go to folder…"), move || editing_path.set(true)),
            ],
        ),
    ]);

    view! {
        <div class="split-app finder" node_ref=root attr:data-drop-dir=move || path.get() attr:data-drop-host=host.get_value()>
            <aside class="side drag-region">
                <div class="side-top"></div>
                <div class="side-title">{c.host_name(&host.get_value())}</div>
                {fav("home", tr("Départ", "Home").to_string(), home_dir.clone())}
                {fav("hdd", "Root (/)".to_string(), "/".into())}
                {fav("folder", "/etc".to_string(), "/etc".into())}
                {fav("folder", "/var/log".to_string(), "/var/log".into())}
                {fav("folder", "/opt".to_string(), "/opt".into())}
                {fav("folder", "/srv".to_string(), "/srv".into())}
                {fav("folder", "/tmp".to_string(), "/tmp".into())}
                <div class="side-title">{tr("Actions", "Actions")}</div>
                <button class="side-item" on:click=move |_| new_folder()><Icon name="plus" size=15 /><span class="grow">{tr("Nouveau dossier", "New folder")}</span></button>
                <button class="side-item" on:click=move |_| new_file()><Icon name="file" size=15 /><span class="grow">{tr("Nouveau fichier", "New file")}</span></button>
                <button class="side-item" on:click=move |_| terminal_here()><Icon name="terminal" size=15 /><span class="grow">{tr("Terminal ici", "Terminal here")}</span></button>
            </aside>
            <section class="main">
                <header class="main-head finder-head drag-region">
                    <button class="icon-btn" title=tr("Précédent", "Back") disabled=move || hist_len.get().0 == 0 on:click=move |_| back()>
                        <Icon name="left" size=16 />
                    </button>
                    <button class="icon-btn flip" title=tr("Suivant", "Forward") disabled=move || hist_len.get().1 == 0 on:click=move |_| forward()>
                        <Icon name="left" size=16 />
                    </button>
                    <button class="icon-btn" title=tr("Dossier parent", "Parent folder") on:click=move |_| go(parent(&path.get_untracked()))>
                        <Icon name="up" size=15 />
                    </button>
                    {move || if editing_path.get() {
                        view! {
                            <form class="grow no-drag" on:submit=move |e| {
                                e.prevent_default();
                                editing_path.set(false);
                                go(path_input.get_untracked());
                            }>
                                <input class="path-input mono" bind:value=path_input autofocus=true on:blur=move |_| editing_path.set(false) />
                            </form>
                        }
                            .into_any()
                    } else {
                        view! {
                            <h2 class="finder-title" title=tr("Cliquer pour saisir un chemin", "Click to type a path") on:click=move |_| {
                                path_input.set(path.get_untracked());
                                editing_path.set(true);
                            }>{title}</h2>
                            <div class="grow"></div>
                        }
                            .into_any()
                    }}
                    <button class="upload-btn no-drag" title=tr("Envoyer des fichiers de cet ordinateur dans ce dossier (ou glissez-les sur la fenêtre)", "Upload files from this computer to this folder (or drag them onto the window)") on:click=move |_| upload()>
                        <Icon name="upload" size=14 />
                        <span>{tr("Envoyer", "Upload")}</span>
                    </button>
                    <button class="icon-btn" title=tr("Télécharger", "Download") disabled=move || sel_entry().map(|e| e.is_dir).unwrap_or(true) on:click=move |_| {
                        if let Some(e) = sel_entry() { download(e) }
                    }><Icon name="download" size=15 /></button>
                    <button class="icon-btn" class:on=move || show_hidden.get() title=tr("Fichiers cachés", "Hidden files") on:click=move |_| show_hidden.update(|v| *v = !*v)>
                        <Icon name="eye" size=15 />
                    </button>
                    <button class="icon-btn" class:spin=move || loading.get() title=tr("Actualiser", "Refresh") on:click=move |_| load()><Icon name="refresh" size=15 /></button>
                    <label class="search-field no-drag">
                        <Icon name="search" size=13 />
                        <input placeholder=tr("Rechercher", "Search") bind:value=filter />
                    </label>
                </header>
                {move || error.get().map(|e| view! { <div class="banner crit finder-error"><Icon name="alert" size=16 /><span>{e}</span></div> })}
                <div class="table-wrap finder-list" class:loading=move || loading.get() on:click=move |ev| {
                    if ev.target() == ev.current_target() { selected.set(None) }
                }>
                    <table class="file-table">
                        <thead>
                            <tr>
                                {sort_head(SortKey::Name, tr("Nom", "Name"), false)}
                                {sort_head(SortKey::Size, tr("Taille", "Size"), true)}
                                {sort_head(SortKey::Modified, tr("Modifié", "Modified"), false)}
                            </tr>
                        </thead>
                        <tbody>{rows}</tbody>
                    </table>
                </div>
                <footer class="pathbar">
                    <div class="crumbs">{crumbs}</div>
                    <span class="muted small">
                        {move || {
                            let hidden = show_hidden.get();
                            let n = entries.with(|v| v.iter().filter(|e| hidden || !e.name.starts_with('.')).count());
                            format!("{n} {}", tr("éléments", "items"))
                        }}
                    </span>
                </footer>
            </section>
            {move || menu.get().map(|(x, y, e)| {
                let (e1, e2, e3, e4, e5) = (e.clone(), e.clone(), e.clone(), e.clone(), e.clone());
                let is_dir = e.is_dir;
                let close = move || menu.set(None);
                view! {
                    <div class="menu-overlay" on:click=move |_| close() on:contextmenu=move |ev| { ev.prevent_default(); close() }>
                        <div class="ctx-menu" style=format!("left:{x}px;top:{y}px") on:click=|ev| ev.stop_propagation()>
                            <button on:click=move |_| { close(); open_entry(e1.clone()) }>
                                <Icon name=if is_dir { "folder" } else { "edit" } size=14 />{tr("Ouvrir", "Open")}
                            </button>
                            {(!is_dir).then(|| view! {
                                <button on:click=move |_| { close(); download(e2.clone()) }><Icon name="download" size=14 />{tr("Télécharger…", "Download…")}</button>
                            })}
                            {is_dir.then(|| {
                                let p = e.path.clone();
                                view! {
                                    <button on:click=move |_| {
                                        close();
                                        c.open(AppKind::Terminal { input: Some(format!("cd {}", shell_quote(&p))) }, Some(host.get_value()))
                                    }><Icon name="terminal" size=14 />{tr("Terminal ici", "Terminal here")}</button>
                                }
                            })}
                            <div class="menu-sep"></div>
                            <button on:click=move |_| { close(); rename(e3.clone()) }><Icon name="edit" size=14 />{tr("Renommer…", "Rename…")}</button>
                            <button on:click=move |_| { close(); duplicate(e4.clone()) }><Icon name="layers" size=14 />{tr("Dupliquer…", "Duplicate…")}</button>
                            <div class="menu-sep"></div>
                            <button class="danger" on:click=move |_| { close(); delete(e5.clone()) }><Icon name="trash" size=14 />{tr("Supprimer…", "Delete…")}</button>
                        </div>
                    </div>
                }
            })}
        </div>
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SortKey {
    Name,
    Size,
    Modified,
}

/// Finder-style date: "Aujourd'hui, 12:16", "Hier, 08:02" or "14/06/2026".
fn finder_time(unix: u32) -> String {
    let d = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(unix as f64 * 1000.0));
    let now = js_sys::Date::new_0();
    let day = |x: &js_sys::Date| (x.get_full_year(), x.get_month(), x.get_date());
    let yesterday = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(now.get_time() - 86_400_000.0));
    let hm = format!("{:02}:{:02}", d.get_hours(), d.get_minutes());
    if day(&d) == day(&now) {
        format!("{}, {hm}", tr("Aujourd'hui", "Today"))
    } else if day(&d) == day(&yesterday) {
        format!("{}, {hm}", tr("Hier", "Yesterday"))
    } else {
        format!("{:02}/{:02}/{:04}", d.get_date(), d.get_month() + 1, d.get_full_year())
    }
}
