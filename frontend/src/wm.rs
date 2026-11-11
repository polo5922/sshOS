//! Desktop: menu bar, floating windows, dock, Spotlight, dialogs, toasts, transfers.

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::{json, Value};
use sshos_shared::{HostConfig, MonitorSnapshot, ServerInfo, TransferProgress};
use wasm_bindgen::JsCast;

use crate::apps;
use crate::icons::Icon;
use crate::launcher;
use crate::state::*;
use crate::tauri::{invoke, listen};

#[component]
pub fn App() -> impl IntoView {
    let c = Ctx::new();
    provide_context(c);

    Effect::new(move |_| {
        let dark = c.dark.get();
        if let Some(root) = document().document_element() {
            let _ = root.set_attribute("data-theme", if dark { "dark" } else { "light" });
        }
    });
    // Appearance preferences become data-* attributes on <html> (see app.css).
    Effect::new(move |_| {
        let attrs = c.prefs.with(|p| p.attributes());
        if let Some(root) = document().document_element() {
            for (k, v) in attrs {
                let _ = root.set_attribute(k, v);
            }
        }
    });
    // "Auto" theme: follow the OS while sshOs is open.
    if let Some(mq) = window().match_media("(prefers-color-scheme: dark)").ok().flatten() {
        let cb = wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::Event)>::new(move |_: web_sys::Event| {
            let d = crate::state::os_prefers_dark();
            let _ = c.os_dark.try_set(d);
            if let Some(p) = c.prefs.try_get_untracked() {
                let _ = c.dark.try_set(p.is_dark(d));
            }
        });
        let _ = mq.add_event_listener_with_callback("change", cb.as_ref().unchecked_ref());
        cb.forget(); // lives as long as the page
    }
    idle_watch(c);
    // Alt+F4 / taskbar close go through the same confirmation as the sshOs menu.
    crate::tauri::on_close_requested(move || quit(c));

    // Startup: hosts, sessions still alive in the backend (after a UI reload), global listeners.
    c.reload_hosts();
    spawn_local(async move {
        let _ = c.fullscreen.try_set(crate::tauri::is_fullscreen().await);
    });
    spawn_local(async move {
        if let Ok(ids) = invoke::<Vec<String>>("connected", json!({})).await {
            for id in ids {
                if let Ok(info) = invoke::<ServerInfo>("server_info", json!({ "hostId": id })).await {
                    c.sessions.update(|s| s.push((id.clone(), info)));
                    if c.active.get_untracked().is_none() {
                        c.active.set(Some(id));
                    }
                }
            }
        }
        // First launch: the tour (it offers to add a server). Otherwise reconnect the last active server.
        if storage_get(ONBOARDED_KEY).is_none() {
            c.onboarding.set(true);
            return;
        }
        if c.sessions.with_untracked(|s| s.is_empty()) {
            let hosts = invoke::<Vec<HostConfig>>("hosts_list", json!({})).await.unwrap_or_default();
            let auto = c.prefs.with_untracked(|p| p.auto_reconnect);
            let last = storage_get(LAST_HOST_KEY).filter(|id| auto && hosts.iter().any(|h| &h.id == id));
            match last {
                Some(id) => {
                    c.connect_async(id, None).await;
                }
                None => c.open(AppKind::Connect, None),
            }
        }
    });
    // Remember the active server for the next launch.
    Effect::new(move |_| {
        if let Some(h) = c.active.get() {
            storage_set(LAST_HOST_KEY, &h);
        }
    });
    let listeners = StoredValue::new_local(Vec::new());
    spawn_local(async move {
        let l1 = listen("transfer-progress", move |v: Value| {
            let Ok(p) = serde_json::from_value::<TransferProgress>(v) else { return };
            let (id, finished, err) = (p.id.clone(), p.finished, p.error.clone());
            c.transfers.update(|t| match t.iter_mut().find(|x| x.id == p.id) {
                Some(x) => *x = p,
                None => t.push(p),
            });
            if finished {
                if let Some(e) = err {
                    c.error(e);
                }
                c.fs_tick.update(|n| *n += 1);
                set_timeout(
                    move || {
                        let _ = c.transfers.try_update(|t| t.retain(|x| x.id != id));
                    },
                    std::time::Duration::from_secs(4),
                );
            }
        })
        .await;
        let l2 = listen("tauri://drag-drop", move |v: Value| {
            highlight_drop(None);
            on_os_drop(c, v)
        })
        .await;
        // While files are dragged over the window, outline the drop target under the cursor.
        let l3 = listen("tauri://drag-over", move |v: Value| highlight_drop(Some(&v))).await;
        let l4 = listen("tauri://drag-leave", move |_| highlight_drop(None)).await;
        listeners.update_value(|v: &mut Vec<_>| {
            v.push(l1);
            v.push(l2);
            v.push(l3);
            v.push(l4);
        });
    });

    let _keys = window_event_listener(ev::keydown, move |e| {
        let k = e.key().to_lowercase();
        if k == "f11" {
            e.prevent_default();
            c.toggle_fullscreen();
        } else if (e.ctrl_key() || e.meta_key()) && k == "k" {
            e.prevent_default();
            c.spotlight.update(|s| *s = !*s);
        } else if k == "f4" {
            e.prevent_default();
            c.launcher.update(|s| *s = !*s);
        } else if (e.ctrl_key() || e.meta_key()) && k == "," {
            e.prevent_default();
            c.open(AppKind::Settings, None);
        } else if k == "escape" {
            if c.dialog.get_untracked().is_some() {
                c.dialog.set(None);
            } else if c.spotlight.get_untracked() {
                c.spotlight.set(false);
            }
        }
    });

    view! {
        <div class="desktop">
            <MenuBar />
            <div class="windows">
                <For each=move || c.windows.get() key=|w| w.id children=move |w| view! { <Window win=w /> } />
                <Show when=move || c.windows.with(|w| w.is_empty())>
                    <div class="wallpaper-hint">
                        <div class="logo-big">"sshOs"</div>
                        <div>{tr("Ctrl+K pour lancer une app", "Ctrl+K to launch an app")}</div>
                    </div>
                </Show>
            </div>
            <Dock />
            <Spotlight />
            <Show when=move || c.launcher.get()>
                <crate::launcher::Launcher />
            </Show>
            <Transfers />
            <Toasts />
            <DialogView />
            <Show when=move || c.onboarding.get()>
                <crate::onboarding::Onboarding />
            </Show>
            <Show when=move || c.tour.get().is_some()>
                <crate::tour::Tour />
            </Show>
        </div>
    }
}

/// Element accepting OS file drops under a `tauri://drag-*` event position.
fn drop_target_at(v: &Value) -> Option<web_sys::Element> {
    let dpr = window().device_pixel_ratio();
    let x = v["position"]["x"].as_f64().unwrap_or(-1.0) / dpr;
    let y = v["position"]["y"].as_f64().unwrap_or(-1.0) / dpr;
    document().element_from_point(x as f32, y as f32).and_then(|e| e.closest("[data-drop-dir]").ok().flatten())
}

/// Outline the drop target under the cursor (`None`: clear every outline).
fn highlight_drop(v: Option<&Value>) {
    let target = v.and_then(drop_target_at);
    while let Ok(Some(el)) = document().query_selector(".drop-target") {
        let _ = el.class_list().remove_1("drop-target");
    }
    if let Some(t) = target {
        let _ = t.class_list().add_1("drop-target");
    }
}

/// Files dropped from the OS: find the Files window under the cursor and upload there.
fn on_os_drop(c: Ctx, v: Value) {
    let paths: Vec<String> = v["paths"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    if paths.is_empty() {
        return;
    }
    let Some(t) = drop_target_at(&v) else {
        c.info_toast(tr(
            "Déposez les fichiers sur une fenêtre Fichiers ou Transferts.",
            "Drop files on a Files or Transfers window.",
        ));
        return;
    };
    let (Some(dir), Some(host)) = (t.get_attribute("data-drop-dir"), t.get_attribute("data-drop-host")) else { return };
    for p in paths {
        let (dir, host) = (dir.clone(), host.clone());
        spawn_local(async move {
            if let Err(e) = invoke::<String>("upload", json!({ "hostId": host, "local": p, "remoteDir": dir })).await {
                c.handle_err(Some(&host), e);
            }
        });
    }
}

// ---------------------------------------------------------------- menu bar

#[component]
fn MenuBar() -> impl IntoView {
    let c = ctx();
    let clock = RwSignal::new(String::new());
    let tick = move || {
        let d = js_sys::Date::new_0();
        let days_fr = ["Dim", "Lun", "Mar", "Mer", "Jeu", "Ven", "Sam"];
        let days_en = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
        let day = d.get_day() as usize % 7;
        let secs = if c.prefs.with_untracked(|p| p.clock_seconds) { format!(":{:02}", d.get_seconds()) } else { String::new() };
        let days_de = ["So", "Mo", "Di", "Mi", "Do", "Fr", "Sa"];
        let days_es = ["Dom", "Lun", "Mar", "Mié", "Jue", "Vie", "Sáb"];
        let (dd, mm, hh, mi) = (d.get_date(), d.get_month() + 1, d.get_hours(), d.get_minutes());
        let next = match c.lang.get_untracked() {
            Lang::Fr => format!("{} {dd:02}/{mm:02} {hh:02}:{mi:02}{secs}", days_fr[day]),
            Lang::De => format!("{} {dd:02}.{mm:02}. {hh:02}:{mi:02}{secs}", days_de[day]),
            Lang::Es => format!("{} {dd:02}/{mm:02} {hh:02}:{mi:02}{secs}", days_es[day]),
            Lang::En => format!("{} {mm:02}/{dd:02} {hh:02}:{mi:02}{secs}", days_en[day]),
        };
        if clock.get_untracked() != next {
            clock.set(next);
        }
    };
    tick();
    Effect::new(move |_| {
        c.prefs.with(|p| p.clock_seconds);
        tick();
    });
    if let Ok(h) = set_interval_with_handle(tick, std::time::Duration::from_secs(1)) {
        on_cleanup(move || h.clear());
    }

    // Live usage of the active host, shared with the apps through `c.live`.
    let busy = StoredValue::new(false);
    let poll = move || {
        let Some(h) = c.active.get_untracked() else {
            c.live.set(None);
            return;
        };
        if busy.get_value() || !c.sessions.with_untracked(|s| s.iter().any(|(x, _)| *x == h)) {
            return;
        }
        busy.set_value(true);
        spawn_local(async move {
            let r = invoke::<MonitorSnapshot>("monitor_snapshot", json!({ "hostId": h })).await;
            let _ = busy.try_set_value(false);
            if let Ok(s) = r {
                if c.active.get_untracked().as_deref() == Some(h.as_str()) {
                    c.live.set(Some((h, s)));
                }
            }
        });
    };
    Effect::new(move |_| {
        let active = c.active.get();
        let _ = c.sessions.with(|s| s.len());
        if c.live.with_untracked(|l| l.as_ref().map(|(h, _)| h.clone())) != active {
            c.live.set(None);
        }
        poll();
    });
    if let Ok(h) = set_interval_with_handle(poll, std::time::Duration::from_secs(3)) {
        on_cleanup(move || h.clear());
    }

    // Cycles EN → FR → DE → ES.
    let toggle_lang = move |_| {
        let cur = c.lang.get_untracked();
        let i = Lang::ALL.iter().position(|l| *l == cur).unwrap_or(0);
        Lang::ALL[(i + 1) % Lang::ALL.len()].apply();
    };
    let toggle_theme = move |_| c.toggle_theme();

    view! {
        <div class="menubar">
            <MenuTitles />
            <span class="menu-info">
                {move || {
                    c.active
                        .get()
                        .and_then(|h| c.sessions.with(|s| s.iter().find(|(x, _)| *x == h).map(|(_, i)| i.clone())))
                        .map(|i| format!("{}{}", i.os, if i.is_root { " · root" } else { "" }))
                        .unwrap_or_default()
                }}
            </span>
            <span class="spacer"></span>
            {move || c.live.get().filter(|_| c.prefs.with(|p| p.menu_stats)).map(|(_, s)| live_stats(&s))}
            <Notifications />
            <button class="menu-btn" data-tour="spotlight" title="Spotlight (Ctrl+K)" on:click=move |_| c.spotlight.set(true)>
                <Icon name="search" size=14 />
            </button>
            <button class="menu-btn lang-btn" data-tour="lang" title=move || c.lang.get().native_name() on:click=toggle_lang>
                {move || c.lang.get().code().to_uppercase()}
            </button>
            <button class="menu-btn" on:click=toggle_theme>
                {move || if c.dark.get() { view! { <Icon name="sun" size=14 /> } } else { view! { <Icon name="moon" size=14 /> } }}
            </button>
            <span class="clock">{move || clock.get()}</span>
        </div>
    }
}

/// macOS-style menus: the sshOs menu, the front app's own menus, then "Window".
#[component]
fn MenuTitles() -> impl IntoView {
    let c = ctx();
    let open = RwSignal::new(None::<usize>);
    // Outside click / Escape close the open menu (no overlay: it would sit under blurred layers).
    let h1 = window_event_listener(ev::pointerdown, move |e| {
        let inside = e
            .target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
            .and_then(|t| t.closest(".mb-menus").ok().flatten())
            .is_some();
        if !inside {
            open.set(None);
        }
    });
    let h2 = window_event_listener(ev::keydown, move |e| {
        if e.key() == "Escape" {
            open.set(None);
        }
    });
    on_cleanup(move || {
        h1.remove();
        h2.remove();
    });

    let menus = move || {
        let f = c.focused();
        let front = c.windows.with(|ws| ws.iter().find(|w| Some(w.id) == f).map(|w| (w.id, w.app.clone(), w.host_id.clone())));
        let mut v = vec![];
        match &front {
            Some((id, app, host)) => {
                let (id, app2, host2) = (*id, app.clone(), host.clone());
                let is_editor = matches!(app, AppKind::Editor { .. });
                v.push(MenuSection::new(
                    app.label(),
                    vec![
                        MenuItem::new(tr("Nouvelle fenêtre", "New window"), move || c.open(fresh(&app2), host2.clone()))
                            .disabled(move || is_editor),
                        None,
                        MenuItem::new(tr("Fermer la fenêtre", "Close window"), move || c.close_animated(id)),
                    ],
                ));
                v.extend(c.app_menus.with(|m| m.iter().find(|(w, _)| *w == id).map(|(_, s)| s.clone()).unwrap_or_default()));
            }
            None => v.push(MenuSection::new(
                tr("Bureau", "Desktop"),
                vec![
                    MenuItem::new(tr("Nouveau terminal", "New terminal"), move || c.open(AppKind::Terminal { input: None }, None)),
                    MenuItem::new(tr("Fichiers", "Files"), move || c.open(AppKind::Files { path: None }, None)),
                    MenuItem::new(tr("Navigateur", "Browser"), move || c.open(AppKind::Browser { url: None }, None)),
                    MenuItem::new(tr("Connexions…", "Connections…"), move || c.open(AppKind::Connect, None)),
                ],
            )),
        }
        v.push(window_menu(c, front.map(|f| f.0)));
        v
    };

    // Menu indices: 0 = sshOs, 1 = server picker, 2.. = front app menus (hovering moves between them).
    view! {
        <div class="mb-menus" data-tour="menus">
            {mb_menu(0, view! { <Icon name="server" size=14 /> }.into_any(), false, sshos_menu(c).items, open)}
            <HostPicker open=open />
            {move || {
                menus()
                    .into_iter()
                    .enumerate()
                    .map(|(i, m)| mb_menu(i + 2, m.title.clone().into_any(), i == 0, m.items, open))
                    .collect_view()
            }}
        </div>
    }
}

fn mb_menu(i: usize, title: AnyView, bold: bool, items: Vec<Option<MenuItem>>, open: RwSignal<Option<usize>>) -> impl IntoView {
    let is_open = move || open.get() == Some(i);
    view! {
        <div class="mb-menu">
            <button
                class="mb-title"
                class:open=is_open
                class:bold=bold
                on:pointerdown=move |e| {
                    e.prevent_default();
                    open.update(|o| *o = if *o == Some(i) { None } else { Some(i) });
                }
                on:pointerenter=move |_| {
                    if open.get_untracked().is_some() {
                        open.set(Some(i));
                    }
                }
            >
                {title}
            </button>
            <Show when=is_open>
                <div class="mb-dropdown">
                    {items
                        .clone()
                        .into_iter()
                        .map(|it| match it {
                            None => view! { <div class="mb-sep"></div> }.into_any(),
                            Some(it) => menu_entry(it, open).into_any(),
                        })
                        .collect_view()}
                </div>
            </Show>
        </div>
    }
}

/// Server picker, right after the sshOs icon: every saved server (connect or switch), then
/// "Connections…" (the Connections app is no longer in the Dock).
#[component]
fn HostPicker(open: RwSignal<Option<usize>>) -> impl IntoView {
    const I: usize = 1;
    let c = ctx();
    let is_open = move || open.get() == Some(I);
    let busy = move || !c.connecting.with(|v| v.is_empty());
    let label = move || match c.active.get() {
        Some(h) if c.is_connected(&h) => c.host_name(&h),
        _ if busy() => tr("Connexion…", "Connecting…").to_string(),
        _ => tr("Aucun serveur", "No server").to_string(),
    };
    let connected = move || c.active.get().map(|h| c.is_connected(&h)).unwrap_or(false);
    let pick = move |id: String| {
        open.set(None);
        if c.is_connected(&id) {
            c.active.set(Some(id));
        } else {
            c.connect(id, None);
        }
    };
    let rows = move || {
        let active = c.active.get();
        c.hosts
            .get()
            .into_iter()
            .map(|h: HostConfig| {
                let id = h.id.clone();
                let on = c.is_connected(&id);
                let wait = c.connecting.with(|v| v.contains(&id));
                let here = on && active.as_deref() == Some(id.as_str());
                let sub = format!("{}@{}", h.user, h.host);
                let id2 = id.clone();
                view! {
                    <button class="mb-item host-row" on:click=move |_| pick(id2.clone())>
                        <span class="mb-check">{if here { "✓" } else { "" }}</span>
                        <span class="status-dot" class:on=on class:busy=wait></span>
                        <span class="mb-label">
                            <span class="host-row-name">{h.name.clone()}</span>
                            <span class="host-row-sub addr">{sub}</span>
                        </span>
                        <span class="mb-key">
                            {if wait {
                                tr("connexion…", "connecting…")
                            } else if on {
                                ""
                            } else {
                                tr("Connecter", "Connect")
                            }}
                        </span>
                    </button>
                }
            })
            .collect_view()
    };
    let disconnect = move |_| {
        open.set(None);
        if let Some(h) = c.active.get_untracked() {
            c.disconnect(h);
        }
    };
    view! {
        <div class="mb-menu">
            <button
                class="mb-title host-pill"
                data-tour="host"
                class:open=is_open
                title=tr("Serveur actif", "Active server")
                on:pointerdown=move |e| {
                    e.prevent_default();
                    open.update(|o| *o = if *o == Some(I) { None } else { Some(I) });
                }
                on:pointerenter=move |_| {
                    if open.get_untracked().is_some() {
                        open.set(Some(I));
                    }
                }
            >
                <span class="status-dot" class:on=connected class:busy=busy></span>
                <span class="ellipsis">{label}</span>
                <Icon name="chev-down" size=11 />
            </button>
            <Show when=is_open>
                <div class="mb-dropdown host-menu">
                    <div class="dm-title">{tr("Serveurs", "Servers")}</div>
                    {rows}
                    <Show when=move || c.hosts.with(|h| h.is_empty())>
                        <div class="host-empty muted small">{tr("Aucun serveur enregistré.", "No saved server.")}</div>
                    </Show>
                    <div class="mb-sep"></div>
                    <button class="mb-item" on:click=move |_| {
                        open.set(None);
                        c.open(AppKind::Connect, None);
                    }>
                        <span class="mb-check"></span>
                        <span class="mb-label">{tr("Gérer les connexions…", "Manage connections…")}</span>
                        <span class="mb-key"></span>
                    </button>
                    <Show when=connected>
                        <button class="mb-item" on:click=disconnect>
                            <span class="mb-check"></span>
                            <span class="mb-label">
                                {move || format!("{} {}", tr("Se déconnecter de", "Disconnect from"), c.active.get().map(|h| c.host_name(&h)).unwrap_or_default())}
                            </span>
                            <span class="mb-key"></span>
                        </button>
                    </Show>
                </div>
            </Show>
        </div>
    }
}

fn menu_entry(it: MenuItem, open: RwSignal<Option<usize>>) -> impl IntoView {
    let dis = it.disabled.clone();
    let disabled = move || dis.as_ref().map(|f| f()).unwrap_or(false);
    let chk = it.checked.clone();
    let checked = move || chk.as_ref().map(|f| f()).unwrap_or(false);
    let run = it.run.clone();
    view! {
        <button
            class="mb-item"
            disabled=disabled
            on:click=move |_| {
                open.set(None);
                let r = run.clone();
                // Next tick: the action may close the window that registered this menu.
                set_timeout(move || r(), std::time::Duration::ZERO);
            }
        >
            <span class="mb-check">{move || if checked() { "✓" } else { "" }}</span>
            <span class="mb-label">{it.label.clone()}</span>
            <span class="mb-key">{it.shortcut.unwrap_or("")}</span>
        </button>
    }
}

/// A new instance of the same app (no pre-typed input, start folder…).
fn fresh(app: &AppKind) -> AppKind {
    match app {
        AppKind::Terminal { .. } => AppKind::Terminal { input: None },
        AppKind::Files { .. } => AppKind::Files { path: None },
        AppKind::Browser { .. } => AppKind::Browser { url: None },
        other => other.clone(),
    }
}

/// Quits sshOs, after a confirmation when the Settings ask for one.
pub fn quit(c: Ctx) {
    if !c.prefs.with_untracked(|p| p.confirm_quit) {
        spawn_local(crate::tauri::quit_app());
        return;
    }
    let n = c.sessions.with_untracked(|s| s.len());
    let body = if n > 0 {
        format!("{n} {}", tr("connexion(s) SSH seront fermées.", "SSH connection(s) will be closed."))
    } else {
        tr("Toutes les fenêtres seront fermées.", "Every window will be closed.").to_string()
    };
    c.dialog.set(Some(Dialog::confirm(tr("Quitter sshOs ?", "Quit sshOs?"), body, tr("Quitter", "Quit"), || {
        spawn_local(crate::tauri::quit_app())
    })));
}

/// Security setting: disconnect every server after N minutes without keyboard or mouse input.
fn idle_watch(c: Ctx) {
    let last = StoredValue::new(js_sys::Date::now());
    let bump = move || last.set_value(js_sys::Date::now());
    let h1 = window_event_listener(ev::pointermove, move |_| bump());
    let h2 = window_event_listener(ev::pointerdown, move |_| bump());
    let h3 = window_event_listener(ev::keydown, move |_| bump());
    let check = move || {
        let min = c.prefs.with_untracked(|p| p.idle_disconnect_min);
        if min == 0 || js_sys::Date::now() - last.get_value() < f64::from(min) * 60_000.0 {
            return;
        }
        let hosts: Vec<String> = c.sessions.with_untracked(|s| s.iter().map(|(h, _)| h.clone()).collect());
        if hosts.is_empty() {
            return;
        }
        // Not a deliberate choice of server: keep it for the next reconnect.
        let keep = storage_get(LAST_HOST_KEY);
        for h in hosts {
            c.disconnect(h);
        }
        if let Some(k) = keep {
            set_timeout(move || storage_set(LAST_HOST_KEY, &k), std::time::Duration::from_millis(1500));
        }
        c.toast(
            ToastKind::Info,
            format!("{} {min} min {}", tr("Serveurs déconnectés après", "Servers disconnected after"), tr("d'inactivité", "of inactivity")),
        );
        bump();
    };
    if let Ok(h) = set_interval_with_handle(check, std::time::Duration::from_secs(20)) {
        on_cleanup(move || h.clear());
    }
    on_cleanup(move || {
        h1.remove();
        h2.remove();
        h3.remove();
    });
}

fn sshos_menu(c: Ctx) -> MenuSection {
    MenuSection::new(
        "sshOs",
        vec![
            MenuItem::new(tr("À propos de sshOs", "About sshOs"), move || c.open(AppKind::About, None)),
            MenuItem::new(tr("Visite guidée…", "Quick tour…"), move || c.tour.set(Some(0))),
            MenuItem::new(tr("Assistant de bienvenue…", "Welcome assistant…"), move || c.onboarding.set(true)),
            None,
            MenuItem::new(tr("Réglages…", "Settings…"), move || c.open(AppKind::Settings, None)).key("Ctrl+,"),
            None,
            MenuItem::new(tr("Plein écran", "Full screen"), move || c.toggle_fullscreen())
                .checked(move || c.fullscreen.get())
                .key("F11"),
            MenuItem::new(tr("Thème sombre", "Dark theme"), move || c.toggle_theme()).checked(move || c.dark.get()),
            MenuItem::new("English", || Lang::En.apply()).checked(move || c.lang.get() == Lang::En),
            MenuItem::new("Français", || Lang::Fr.apply()).checked(move || c.lang.get() == Lang::Fr),
            MenuItem::new("Deutsch", || Lang::De.apply()).checked(move || c.lang.get() == Lang::De),
            MenuItem::new("Español", || Lang::Es.apply()).checked(move || c.lang.get() == Lang::Es),
            None,
            MenuItem::new(tr("Connexions…", "Connections…"), move || c.open(AppKind::Connect, None)),
            MenuItem::new(tr("Applications…", "Applications…"), move || c.launcher.set(true)).key("F4"),
            MenuItem::new("Spotlight", move || c.spotlight.set(true)).key("Ctrl+K"),
            None,
            MenuItem::new(tr("Réduire sshOs", "Minimize sshOs"), || spawn_local(crate::tauri::minimize_app())),
            MenuItem::new(tr("Quitter sshOs", "Quit sshOs"), move || quit(c)).key("Alt+F4"),
        ],
    )
}

fn window_menu(c: Ctx, front: Option<u32>) -> MenuSection {
    let with_front = move |f: fn(&Win)| {
        if let Some(id) = front {
            c.windows.with_untracked(|ws| ws.iter().find(|w| w.id == id).map(f));
        }
    };
    let none = move || front.is_none();
    let mut items = vec![
        MenuItem::new(tr("Réduire", "Minimize"), move || with_front(|w| w.minimized.set(true))).disabled(none),
        MenuItem::new(tr("Agrandir", "Zoom"), move || with_front(|w| w.maximized.update(|m| *m = !*m))).disabled(none),
        MenuItem::new(tr("Tout réduire", "Minimize all"), move || {
            c.windows.with_untracked(|ws| ws.iter().for_each(|w| w.minimized.set(true)))
        }),
    ];
    let wins: Vec<(u32, String)> = c.windows.with(|ws| ws.iter().map(|w| (w.id, w.title.get())).collect());
    if !wins.is_empty() {
        items.push(None);
    }
    for (id, t) in wins {
        items.push(MenuItem::new(t, move || c.focus(id)).checked(move || front == Some(id)));
    }
    MenuSection::new(tr("Fenêtre", "Window"), items)
}

/// Virtual interfaces (bridges, veth pairs, VPN) would double count traffic.
fn physical_iface(name: &str) -> bool {
    !["lo", "br-", "docker", "veth", "virbr", "cni", "flannel", "cali", "tailscale", "wg", "tun", "zt"]
        .iter()
        .any(|p| name.starts_with(p))
}

fn live_stats(s: &MonitorSnapshot) -> impl IntoView {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let cpu = s.cpu_percent.clamp(0.0, 100.0);
    let level = |p: f32| if p > 90.0 { "crit" } else if p > 70.0 { "warn" } else { "" };
    let (rx, tx) = s
        .net
        .iter()
        .filter(|n| physical_iface(&n.iface))
        .fold((0.0, 0.0), |(r, t), n| (r + n.rx_bytes_per_s, t + n.tx_bytes_per_s));
    let root = s.disks.iter().find(|d| d.mount == "/").or(s.disks.first());
    let disk = root.map(|d| if d.total > 0 { d.used as f32 * 100.0 / d.total as f32 } else { 0.0 }).unwrap_or(0.0);
    let disk_title = root
        .map(|d| format!("{} · {} / {}", d.mount, human_bytes(d.used as f64), human_bytes(d.total as f64)))
        .unwrap_or_default();
    let mem_pct = if s.mem_total > 0 { s.mem_used as f32 * 100.0 / s.mem_total as f32 } else { 0.0 };
    view! {
        <div class="live">
            <span class="live-item" title=format!("CPU · {} cores · load {:.2}", s.cores, s.load[0])>
                <Icon name="cpu" size=13 />
                <span class=format!("mini-bar {}", level(cpu))>
                    <span style=format!("width:{cpu:.0}%")></span>
                </span>
                <span class="num">{format!("{cpu:.0}%")}</span>
            </span>
            <span class="live-item" title=format!("RAM {mem_pct:.0}%")>
                <span class="live-label">"RAM"</span>
                <span class="num">{format!("{:.1} / {:.1} GB", s.mem_used as f64 / GB, s.mem_total as f64 / GB)}</span>
            </span>
            <span class="live-item" title=disk_title>
                <Icon name="hdd" size=13 />
                <span class=format!("num {}", level(disk))>{format!("{disk:.0}%")}</span>
            </span>
            <span class="live-item live-net" title=format!("↓ {}/s   ↑ {}/s", human_bytes(rx), human_bytes(tx))>
                <Icon name="download" size=12 />
                <span class="num net">{format!("{}/s", human_bytes(rx))}</span>
                <Icon name="upload" size=12 />
                <span class="num net">{format!("{}/s", human_bytes(tx))}</span>
            </span>
        </div>
    }
}

// ---------------------------------------------------------------- windows

#[derive(Clone, Copy)]
enum Drag {
    Move,
    Resize,
}

#[component]
fn Window(win: Win) -> impl IntoView {
    let c = ctx();
    let id = win.id;
    let (rect, z, minimized, maximized, title) = (win.rect, win.z, win.minimized, win.maximized, win.title);
    let drag = StoredValue::new(None::<(Drag, f64, f64, Rect)>);
    // Close / maximize animations: the class is set, the state changes after (or during) the CSS transition.
    let closing = win.closing;
    let zooming = RwSignal::new(false);
    let close_anim = move || c.close_animated(id);
    let toggle_max = move || {
        zooming.set(true);
        maximized.update(|m| *m = !*m);
        set_timeout(move || { zooming.try_set(false); }, std::time::Duration::from_millis(260));
    };

    let start = move |mode: Drag, e: ev::PointerEvent| {
        if e.button() != 0 || maximized.get_untracked() {
            return;
        }
        if let Some(t) = e.current_target().and_then(|t| t.dyn_into::<web_sys::Element>().ok()) {
            let _ = t.set_pointer_capture(e.pointer_id());
        }
        drag.set_value(Some((mode, e.client_x() as f64, e.client_y() as f64, rect.get_untracked())));
        e.prevent_default();
    };
    let on_move = move |e: ev::PointerEvent| {
        if let Some((mode, sx, sy, r)) = drag.get_value() {
            let (dx, dy) = (e.client_x() as f64 - sx, e.client_y() as f64 - sy);
            let (vw, vh) = viewport();
            rect.set(match mode {
                Drag::Move => Rect {
                    x: (r.x + dx).clamp(-r.w + 80.0, vw - 80.0),
                    y: (r.y + dy).clamp(30.0, vh - 40.0),
                    ..r
                },
                Drag::Resize => Rect { w: (r.w + dx).max(360.0), h: (r.h + dy).max(220.0), ..r },
            });
        }
    };
    let on_up = move |_e: ev::PointerEvent| drag.set_value(None);

    let style = move || {
        let r = rect.get();
        if maximized.get() {
            format!("left:0;top:30px;width:100%;height:calc(100% - 30px - 84px);z-index:{}", z.get())
        } else {
            format!("left:{}px;top:{}px;width:{}px;height:{}px;z-index:{}", r.x, r.y, r.w, r.h, z.get())
        }
    };
    let is_focused = move || c.focused() == Some(id);
    // Drag from any ".drag-region" (title bar, or sidebar/header of chromeless apps), except controls.
    let in_drag_region = |e: &ev::MouseEvent| {
        e.target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
            .map(|t| {
                t.closest(".drag-region").ok().flatten().is_some()
                    && t.closest("button, input, select, textarea, a, .no-drag").ok().flatten().is_none()
            })
            .unwrap_or(false)
    };

    let host = win.host_id.clone().unwrap_or_default();
    provide_context(WinId(id));
    let body = match win.app.clone() {
        AppKind::Connect => view! { <apps::connect::Connect /> }.into_any(),
        AppKind::About => view! { <About /> }.into_any(),
        AppKind::Settings => view! { <apps::settings::Settings /> }.into_any(),
        AppKind::Terminal { input } => view! { <apps::terminal::Terminal host=host input=input /> }.into_any(),
        AppKind::Files { path } => view! { <apps::files::Files host=host start=path /> }.into_any(),
        AppKind::Editor { path, nginx } => {
            view! { <apps::editor::Editor host=host path=path nginx=nginx title=title /> }.into_any()
        }
        AppKind::Monitor => view! { <apps::monitor::Monitor host=host /> }.into_any(),
        AppKind::Nginx => view! { <apps::nginx::Nginx host=host /> }.into_any(),
        AppKind::Docker => view! { <apps::docker::Docker host=host /> }.into_any(),
        AppKind::Security => view! { <apps::security::Security host=host /> }.into_any(),
        AppKind::Browser { url } => view! { <apps::browser::Browser host=host start=url /> }.into_any(),
        AppKind::Disks => view! { <apps::disks::Disks host=host /> }.into_any(),
        AppKind::TextEdit { path } => view! { <apps::textedit::TextEdit host=host start=path title=title /> }.into_any(),
        AppKind::Transfers => view! { <apps::transfers::Transfers /> }.into_any(),
        AppKind::Logs => view! { <apps::logs::Logs host=host /> }.into_any(),
        AppKind::Git => view! { <apps::git::Git host=host /> }.into_any(),
        AppKind::CmdLog => view! { <apps::cmdlog::CmdLog /> }.into_any(),
        AppKind::Services => view! { <apps::services::Services host=host /> }.into_any(),
        AppKind::Updates => view! { <apps::updates::Updates host=host /> }.into_any(),
        AppKind::Cron => view! { <apps::cron::Cron host=host /> }.into_any(),
        AppKind::Certs => view! { <apps::certs::Certs host=host /> }.into_any(),
        AppKind::Users => view! { <apps::users::Users host=host /> }.into_any(),
        AppKind::Network => view! { <apps::network::Network host=host /> }.into_any(),
        AppKind::Tunnels => view! { <apps::tunnels::Tunnels host=host /> }.into_any(),
        AppKind::Backups => view! { <apps::backups::Backups host=host /> }.into_any(),
        AppKind::Databases => view! { <apps::databases::Databases host=host /> }.into_any(),
        AppKind::Fleet => view! { <apps::fleet::Fleet /> }.into_any(),
        AppKind::Scripts => view! { <apps::scripts::Scripts /> }.into_any(),
        AppKind::Homelab => view! { <apps::homelab::Homelab host=host /> }.into_any(),
    };
    let chromeless = win.app.chromeless();
    let traffic = move || {
        view! {
            <div class="traffic">
                <button class="tl close" title=tr("Fermer", "Close") on:click=move |_| close_anim()>
                    <svg viewBox="0 0 12 12"><path d="M3.5 3.5l5 5M8.5 3.5l-5 5" /></svg>
                </button>
                <button class="tl min" title=tr("Réduire", "Minimize") on:click=move |_| minimized.set(true)>
                    <svg viewBox="0 0 12 12"><path d="M3 6h6" /></svg>
                </button>
                <button class="tl max" title=tr("Agrandir", "Maximize") on:click=move |_| toggle_max()>
                    <svg viewBox="0 0 12 12"><path class="fill" d="M3.4 3.4h3.9L3.4 7.3zM8.6 8.6H4.7l3.9-3.9z" /></svg>
                </button>
            </div>
        }
    };

    view! {
        <div
            class="window"
            class:focused=is_focused
            class:chromeless=chromeless
            class:maximized=move || maximized.get()
            class:minimized=move || minimized.get()
            class:closing=move || closing.get()
            class:zooming=move || zooming.get()
            style=style
            on:pointerdown=move |e| {
                c.focus(id);
                if in_drag_region(&e) {
                    start(Drag::Move, e);
                }
            }
            on:pointermove=on_move
            on:pointerup=on_up
            on:dblclick=move |e| {
                if in_drag_region(&e) {
                    toggle_max();
                }
            }
        >
            {if chromeless {
                view! { <div class="traffic-float">{traffic()}</div> }.into_any()
            } else {
                view! {
                    <div class="titlebar drag-region">
                        {traffic()}
                        <span class="title">{move || title.get()}</span>
                    </div>
                }
                    .into_any()
            }}
            <div class="window-body">{body}</div>
            <div
                class="resize"
                on:pointerdown=move |e| {
                    e.stop_propagation();
                    c.focus(id);
                    start(Drag::Resize, e)
                }
            ></div>
        </div>
    }
}

#[component]
fn About() -> impl IntoView {
    view! {
        <div class="about">
            <div class="logo-big">"sshOs"</div>
            <p>{tr(
                "Un bureau pour vos serveurs, sans agent : tout passe par SSH.",
                "A desktop for your servers, agentless: everything goes through SSH.",
            )}</p>
            <p class="muted">
                {tr("Portage Rust/Tauri inspiré de ", "Rust/Tauri port inspired by ")}
                "Davish (github.com/2hjaito/davish)" {tr(" — licence GPL-3.0.", " — GPL-3.0 license.")}
            </p>
        </div>
    }
}

// ---------------------------------------------------------------- dock

#[component]
fn Dock() -> impl IntoView {
    let c = ctx();
    // Id of the dock app whose context menu is open.
    let menu = RwSignal::new(None::<&'static str>);
    let h1 = window_event_listener(ev::pointerdown, move |e| {
        let inside = e
            .target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
            .and_then(|t| t.closest(".dock-menu").ok().flatten())
            .is_some();
        if !inside && menu.get_untracked().is_some() {
            menu.set(None);
        }
    });
    let h2 = window_event_listener(ev::keydown, move |e| {
        if e.key() == "Escape" {
            menu.set(None);
        }
    });
    on_cleanup(move || {
        h1.remove();
        h2.remove();
    });
    // Apps kept in the Dock, then the open apps that are not (after a separator), like macOS.
    let pinned = move || c.prefs.with(|p| p.dock.iter().filter_map(|id| launcher::app(id)).map(|a| a.id).collect::<Vec<_>>());
    let extra = move || {
        let pins = pinned();
        let mut v: Vec<&'static str> = vec![];
        c.windows.with(|ws| {
            for w in ws {
                if let Some(a) = launcher::app_of(&w.app) {
                    if !pins.contains(&a.id) && !v.contains(&a.id) {
                        v.push(a.id);
                    }
                }
            }
        });
        v
    };
    view! {
        <div class="dock" data-tour="dock">
            <div class="dock-slot" data-tour="launcher">
                <button class="dock-item" class:menu-open=move || c.launcher.get() on:click=move |_| c.launcher.update(|s| *s = !*s)>
                    <span class="dock-tile t-grid">
                        <Icon name="grid" size=24 />
                    </span>
                    <span class="dock-label">{tr("Applications", "Applications")}</span>
                </button>
            </div>
            <div class="dock-sep"></div>
            <For each=pinned key=|id| *id children=move |id| dock_item(c, id, menu) />
            <Show when=move || !extra().is_empty()>
                <div class="dock-sep"></div>
            </Show>
            <For each=extra key=|id| *id children=move |id| dock_item(c, id, menu) />
        </div>
    }
}

fn dock_item(c: Ctx, id: &'static str, menu: RwSignal<Option<&'static str>>) -> impl IntoView {
    let entry = launcher::app(id).expect("dock app in catalog");
    let make = entry.make;
    let app = make();
    let is_app = move |w: &Win| launcher::app_of(&w.app).map(|a| a.id) == Some(id);
    let running = move || c.windows.with(|ws| ws.iter().any(is_app));
    let label = app.label();
    let icon = app.icon();
    let click = move |_| {
        let app = make();
        let active = c.active.get_untracked();
        // Focus the most recent window of this kind for the active host, else open one.
        let existing = c.windows.with_untracked(|ws| {
            ws.iter()
                .filter(|w| is_app(w) && (!app.needs_host() || w.host_id == active))
                .max_by_key(|w| w.z.get_untracked())
                .map(|w| (w.id, w.minimized.get_untracked()))
        });
        match existing {
            Some((id, min)) if min || c.focused_untracked() != Some(id) => c.focus(id),
            Some((id, _)) => {
                c.windows.with_untracked(|ws| ws.iter().find(|w| w.id == id).map(|w| w.minimized.set(true)));
            }
            None => c.open(app, None),
        }
    };
    let is_open = move || menu.get() == Some(id);
    view! {
        <div class="dock-slot">
            <button
                class="dock-item"
                class:menu-open=is_open
                on:click=click
                on:contextmenu=move |e| {
                    e.prevent_default();
                    menu.set(if menu.get_untracked() == Some(id) { None } else { Some(id) });
                }
            >
                <span class=format!("dock-tile t-{icon}")>
                    <Icon name=icon size=24 />
                </span>
                <span class="dock-label">{label}</span>
                <span class="dot" class:on=running></span>
            </button>
            <Show when=is_open>{move || dock_menu(c, entry, label, menu)}</Show>
        </div>
    }
}

/// macOS-like dock menu: the app's windows, New window, Show all / Hide, Quit.
fn dock_menu(c: Ctx, entry: &'static launcher::AppEntry, label: &'static str, menu: RwSignal<Option<&'static str>>) -> impl IntoView {
    let make = entry.make;
    let id = entry.id;
    let wins: Vec<(u32, String, bool)> = c.windows.with_untracked(|ws| {
        let mut v: Vec<_> = ws
            .iter()
            .filter(|w| launcher::app_of(&w.app).map(|a| a.id) == Some(id))
            .map(|w| (w.id, w.title.get_untracked(), w.minimized.get_untracked(), w.z.get_untracked()))
            .collect();
        v.sort_by_key(|w| w.3);
        v.into_iter().map(|(id, t, m, _)| (id, t, m)).collect()
    });
    let ids: Vec<u32> = wins.iter().map(|w| w.0).collect();
    let front = c.focused_untracked();
    // Close the menu first, run the action on the next tick (it may remove windows).
    let act = move |f: Box<dyn Fn()>| {
        menu.set(None);
        set_timeout(move || f(), std::time::Duration::ZERO);
    };
    let has = !ids.is_empty();
    let (i1, i2, i3) = (ids.clone(), ids.clone(), ids);
    view! {
        <div class="dock-menu" on:contextmenu=|e| e.prevent_default()>
            <div class="dm-title">{label}</div>
            {wins
                .into_iter()
                .map(|(id, title, minimized)| {
                    view! {
                        <button on:click=move |_| act(Box::new(move || c.focus(id)))>
                            <span class="dm-check">{if front == Some(id) { "✓" } else if minimized { "◇" } else { "" }}</span>
                            <span class="ellipsis">{title}</span>
                        </button>
                    }
                })
                .collect_view()}
            {has.then(|| view! { <div class="mb-sep"></div> })}
            <button on:click=move |_| act(Box::new(move || c.open(make(), None)))>
                <span class="dm-check"></span>
                {if has { tr("Nouvelle fenêtre", "New window") } else { tr("Ouvrir", "Open") }}
            </button>
            <button on:click=move |_| act(Box::new(move || c.toggle_dock(id)))>
                <span class="dm-check"></span>
                {if c.in_dock_untracked(id) { tr("Retirer du Dock", "Remove from Dock") } else { tr("Garder dans le Dock", "Keep in Dock") }}
            </button>
            {has.then(|| {
                view! {
                    <button on:click=move |_| { let v = i1.clone(); act(Box::new(move || v.iter().for_each(|id| c.focus(*id)))) }>
                        <span class="dm-check"></span>{tr("Afficher toutes les fenêtres", "Show all windows")}
                    </button>
                    <button on:click=move |_| {
                        let v = i2.clone();
                        act(Box::new(move || c.windows.with_untracked(|ws| ws.iter().filter(|w| v.contains(&w.id)).for_each(|w| w.minimized.set(true)))))
                    }>
                        <span class="dm-check"></span>{tr("Masquer", "Hide")}
                    </button>
                    <div class="mb-sep"></div>
                    <button on:click=move |_| { let v = i3.clone(); act(Box::new(move || v.iter().for_each(|id| c.close_animated(*id)))) }>
                        <span class="dm-check"></span>{tr("Quitter", "Quit")}
                    </button>
                }
            })}
        </div>
    }
}

// ---------------------------------------------------------------- spotlight

#[derive(Clone)]
struct Action {
    label: String,
    hint: String,
    run: Callback<()>,
}

#[component]
fn Spotlight() -> impl IntoView {
    let c = ctx();
    let query = RwSignal::new(String::new());
    let sel = RwSignal::new(0usize);
    let input = NodeRef::<leptos::html::Input>::new();

    Effect::new(move |_| {
        if c.spotlight.get() {
            query.set(String::new());
            sel.set(0);
            if let Some(i) = input.get() {
                let _ = i.focus();
            }
        }
    });

    let actions = move || {
        let mut v: Vec<Action> = Vec::new();
        let host = c.active.get().map(|h| c.host_name(&h)).unwrap_or_default();
        v.push(Action {
            label: tr("Applications · lanceur", "Applications · launcher").into(),
            hint: "F4".into(),
            run: Callback::new(move |()| c.launcher.set(true)),
        });
        for (label, make) in [
            (tr("Nouveau terminal", "New terminal"), (|| AppKind::Terminal { input: None }) as fn() -> AppKind),
            (tr("Fichiers", "Files"), || AppKind::Files { path: None }),
            (tr("Moniteur", "Monitor"), || AppKind::Monitor),
            ("Nginx", || AppKind::Nginx),
            ("Docker", || AppKind::Docker),
            (tr("Sécurité · fail2ban, pare-feu", "Security · fail2ban, firewall"), || AppKind::Security),
            (tr("Navigateur", "Browser"), || AppKind::Browser { url: None }),
            (tr("Disques · espace disque", "Disks · disk space"), || AppKind::Disks),
            (tr("Texte · éditeur", "TextEdit · editor"), || AppKind::TextEdit { path: None }),
            (tr("Transferts · envoyer des fichiers", "Transfers · upload files"), || AppKind::Transfers),
            (tr("Journaux · logs", "Logs · journal"), || AppKind::Logs),
            ("Git", || AppKind::Git),
            (tr("Services · systemd", "Services · systemd"), || AppKind::Services),
            (tr("Mises à jour · paquets", "Updates · packages"), || AppKind::Updates),
            (tr("Tâches planifiées · cron", "Scheduled tasks · cron"), || AppKind::Cron),
            (tr("Certificats · expiration", "Certificates · expiry"), || AppKind::Certs),
            (tr("Utilisateurs et clés SSH", "Users & SSH keys"), || AppKind::Users),
            (tr("Réseau · ports, ping", "Network · ports, ping"), || AppKind::Network),
            (tr("Tunnels SSH", "SSH tunnels"), || AppKind::Tunnels),
            (tr("Sauvegardes · archives", "Backups · archives"), || AppKind::Backups),
            (tr("Bases de données", "Databases"), || AppKind::Databases),
            (tr("Multi-serveurs · vue d'ensemble", "All servers · overview"), || AppKind::Fleet),
            (tr("Scripts · commandes enregistrées", "Scripts · saved commands"), || AppKind::Scripts),
            (tr("Homelab · schéma du serveur", "Homelab · server diagram"), || AppKind::Homelab),
            (tr("Historique des commandes", "Command log"), || AppKind::CmdLog),
            (tr("Connexions", "Connections"), || AppKind::Connect),
            (tr("Réglages", "Settings"), || AppKind::Settings),
            (tr("À propos", "About"), || AppKind::About),
        ] {
            v.push(Action {
                label: label.into(),
                hint: if make().needs_host() { host.clone() } else { String::new() },
                run: Callback::new(move |()| c.open(make(), None)),
            });
        }
        for h in c.hosts.get() {
            let connected = c.is_connected(&h.id);
            let id = h.id.clone();
            v.push(Action {
                label: format!("{} {}", if connected { tr("Basculer vers", "Switch to") } else { tr("Se connecter à", "Connect to") }, h.name),
                hint: format!("{}@{}", h.user, h.host),
                run: Callback::new(move |()| {
                    if c.is_connected(&id) {
                        c.active.set(Some(id.clone()));
                    } else {
                        c.connect(id.clone(), None);
                    }
                }),
            });
        }
        v.push(Action {
            label: tr("Basculer le thème", "Toggle theme").into(),
            hint: String::new(),
            run: Callback::new(move |()| c.toggle_theme()),
        });
        let q = query.get().to_lowercase();
        v.retain(|a| q.is_empty() || a.label.to_lowercase().contains(&q) || a.hint.to_lowercase().contains(&q));
        v
    };
    let run = move |a: &Action| {
        c.spotlight.set(false);
        a.run.run(());
    };
    let on_key = move |e: ev::KeyboardEvent| {
        let list = actions();
        match e.key().as_str() {
            "ArrowDown" => sel.update(|s| *s = (*s + 1).min(list.len().saturating_sub(1))),
            "ArrowUp" => sel.update(|s| *s = s.saturating_sub(1)),
            "Enter" => {
                if let Some(a) = list.get(sel.get_untracked()) {
                    run(a);
                }
            }
            _ => return,
        }
        e.prevent_default();
    };

    view! {
        <Show when=move || c.spotlight.get()>
            <div class="overlay" on:click=move |_| c.spotlight.set(false)>
                <div class="spotlight" on:click=|e| e.stop_propagation()>
                    <input
                        node_ref=input
                        placeholder=tr("Rechercher une app, un serveur…", "Search an app, a server…")
                        bind:value=query
                        on:input=move |_| sel.set(0)
                        on:keydown=on_key
                    />
                    <div class="spot-list">
                        {move || {
                            actions()
                                .into_iter()
                                .enumerate()
                                .map(|(i, a)| {
                                    let a2 = a.clone();
                                    view! {
                                        <div class="spot-item" class:sel=move || sel.get() == i on:click=move |_| run(&a2)>
                                            <span>{a.label}</span>
                                            <span class="muted">{a.hint}</span>
                                        </div>
                                    }
                                })
                                .collect_view()
                        }}
                    </div>
                </div>
            </div>
        </Show>
    }
}

// ---------------------------------------------------------------- dialog / toasts / transfers

#[component]
fn DialogView() -> impl IntoView {
    let c = ctx();
    let ok = move || {
        let d = c.dialog.get_untracked();
        c.dialog.set(None);
        if let Some(cb) = d.and_then(|d| d.on_ok) {
            cb.run(());
        }
    };
    move || {
        c.dialog.get().map(|d| {
            let has_cancel = d.on_ok.is_some();
            let input = d.input;
            view! {
                <div class="overlay">
                    <div class="dialog" class:danger=d.danger>
                        <h3>{d.title}</h3>
                        <p class="dialog-body">{d.body}</p>
                        {d.code.map(|code| view! { <pre class="code">{code}</pre> })}
                        {input.map(|sig| {
                            view! {
                                <input
                                    class="dialog-input"
                                    bind:value=sig
                                    autofocus
                                    on:keydown=move |e: ev::KeyboardEvent| {
                                        if e.key() == "Enter" {
                                            ok();
                                        }
                                    }
                                />
                            }
                        })}
                        <div class="dialog-actions">
                            <Show when=move || has_cancel>
                                <button on:click=move |_| c.dialog.set(None)>{tr("Annuler", "Cancel")}</button>
                            </Show>
                            <button class="primary" class:danger=d.danger on:click=move |_| ok()>{d.ok_label}</button>
                        </div>
                    </div>
                </div>
            }
        })
    }
}

#[component]
fn Toasts() -> impl IntoView {
    let c = ctx();
    view! {
        <div class="toasts">
            {move || {
                c.toasts
                    .get()
                    .into_iter()
                    .map(|t| {
                        let id = t.id;
                        let class = match t.kind {
                            ToastKind::Info => "toast info",
                            ToastKind::Ok => "toast ok",
                            ToastKind::Error => "toast error",
                        };
                        view! {
                            <div class=class on:click=move |_| c.toasts.update(|v| v.retain(|x| x.id != id))>{t.text}</div>
                        }
                    })
                    .collect_view()
            }}
        </div>
    }
}

#[component]
fn Transfers() -> impl IntoView {
    let c = ctx();
    view! {
        <Show when=move || c.transfers.with(|t| !t.is_empty())>
            <div class="transfers">
                {move || {
                    c.transfers
                        .get()
                        .into_iter()
                        .map(|t| {
                            let pct = if t.total > 0 { t.done as f64 * 100.0 / t.total as f64 } else { 0.0 };
                            let status = match (&t.error, t.finished) {
                                (Some(_), _) => tr("échec", "failed").to_string(),
                                (None, true) => "✓".to_string(),
                                _ => format!("{pct:.0}%"),
                            };
                            view! {
                                <div class="transfer">
                                    <div class="transfer-row">
                                        <span class="ellipsis">{t.name}</span>
                                        <span class="muted">{status}</span>
                                    </div>
                                    <div class="bar"><div style=format!("width:{pct}%")></div></div>
                                </div>
                            }
                        })
                        .collect_view()
                }}
            </div>
        </Show>
    }
}

#[derive(Clone, PartialEq)]
struct Alert {
    host: String,
    svc: sshos_shared::FailedService,
}

impl Alert {
    fn key(&self) -> String {
        format!("{}|{}", self.host, self.svc.unit)
    }
}

/// Bell in the menu bar: failed systemd services on every connected host, polled every 30 s.
#[component]
fn Notifications() -> impl IntoView {
    let c = ctx();
    let alerts = RwSignal::new(Vec::<Alert>::new());
    // Dismissed alerts come back only if the service recovers and then fails again.
    let dismissed = RwSignal::new(Vec::<String>::new());
    let open = RwSignal::new(false);
    let busy = StoredValue::new(false);

    let poll = move || {
        if busy.get_value() {
            return;
        }
        let hosts: Vec<String> = c.sessions.with_untracked(|s| s.iter().map(|(h, _)| h.clone()).collect());
        busy.set_value(true);
        spawn_local(async move {
            let mut all = Vec::new();
            for h in hosts {
                if let Ok(v) = invoke::<Vec<sshos_shared::FailedService>>("failed_services", json!({ "hostId": h })).await {
                    all.extend(v.into_iter().map(|svc| Alert { host: h.clone(), svc }));
                }
            }
            let _ = busy.try_set_value(false);
            let Some(prev) = alerts.try_get_untracked() else { return };
            let keys: Vec<String> = all.iter().map(|a| a.key()).collect();
            // Toast the failures that just appeared.
            for a in all.iter().filter(|a| !prev.iter().any(|p| p.key() == a.key())) {
                if !dismissed.get_untracked().contains(&a.key()) {
                    c.toast(
                        ToastKind::Error,
                        format!("{} {} {}", unit_label(&a.svc.unit), tr("en échec sur", "failed on"), c.host_name(&a.host)),
                    );
                }
            }
            dismissed.update(|d| d.retain(|k| k.starts_with("cert:") || keys.contains(k)));
            alerts.set(all);
        });
    };
    // Certificates expiring within certs::WARN_DAYS days: checked on connect, then hourly.
    let certs_alerts = RwSignal::new(Vec::<(String, String, i64)>::new());
    let certs_busy = StoredValue::new(false);
    let poll_certs = move || {
        if certs_busy.get_value() {
            return;
        }
        let hosts: Vec<String> = c.sessions.with_untracked(|s| s.iter().map(|(h, _)| h.clone()).collect());
        certs_busy.set_value(true);
        spawn_local(async move {
            use crate::apps::certs;
            let now = certs::now_secs();
            let mut all = Vec::new();
            for h in hosts {
                if let Ok(r) = crate::tauri::exec(&h, &certs::script(&certs::manual_domains(&h)), true).await {
                    for cert in certs::parse(&r.stdout).certs {
                        if let Some(d) = cert.days_left(now).filter(|d| *d <= certs::WARN_DAYS) {
                            if !all.iter().any(|(x, n, _): &(String, String, i64)| *x == h && *n == cert.name) {
                                all.push((h.clone(), cert.name, d));
                            }
                        }
                    }
                }
            }
            let _ = certs_busy.try_set_value(false);
            let Some(prev) = certs_alerts.try_get_untracked() else { return };
            for (h, n, d) in all.iter().filter(|a| !prev.iter().any(|p| p.0 == a.0 && p.1 == a.1)) {
                if !dismissed.get_untracked().contains(&format!("cert:{h}:{n}")) {
                    c.toast(ToastKind::Error, cert_title(n, &c.host_name(h), *d));
                }
            }
            certs_alerts.set(all);
        });
    };
    Effect::new(move |_| {
        let _ = c.sessions.with(|s| s.len());
        poll();
        poll_certs();
    });
    if let Ok(h) = set_interval_with_handle(poll, std::time::Duration::from_secs(30)) {
        on_cleanup(move || h.clear());
    }
    if let Ok(h) = set_interval_with_handle(poll_certs, std::time::Duration::from_secs(3600)) {
        on_cleanup(move || h.clear());
    }

    // Close on any click outside the bell / panel, or on Escape. (A fixed overlay would not work:
    // the menu bar's backdrop-filter makes it the containing block of fixed children.)
    let h1 = window_event_listener(ev::pointerdown, move |e| {
        if !open.get_untracked() {
            return;
        }
        let inside = e
            .target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
            .map(|t| t.closest(".notif-panel, .bell").ok().flatten().is_some())
            .unwrap_or(false);
        if !inside {
            open.set(false);
        }
    });
    let h2 = window_event_listener(ev::keydown, move |e| {
        if e.key() == "Escape" {
            open.set(false);
        }
    });
    on_cleanup(move || {
        h1.remove();
        h2.remove();
    });

    let visible = move || {
        let d = dismissed.get();
        alerts.get().into_iter().filter(|a| !d.contains(&a.key())).collect::<Vec<_>>()
    };
    let visible_certs = move || {
        let d = dismissed.get();
        certs_alerts.get().into_iter().filter(|(h, n, _)| !d.contains(&format!("cert:{h}:{n}"))).collect::<Vec<_>>()
    };
    let count = move || visible().len() + visible_certs().len();

    view! {
        <button
            class="menu-btn bell"
            data-tour="bell"
            class:active=move || open.get()
            title=tr("Notifications", "Notifications")
            on:click=move |_| open.update(|o| *o = !*o)
        >
            <Icon name="bell" size=14 />
            {move || (count() > 0).then(|| view! { <span class="bell-badge">{count()}</span> })}
        </button>
        <Show when=move || open.get()>
            <div class="notif-panel">
                <div class="notif-head">
                    <span>"NOTIFICATIONS"</span>
                    <button class="link small" on:click=move |_| poll()>{tr("Actualiser", "Refresh")}</button>
                </div>
                {move || {
                    visible_certs()
                        .into_iter()
                        .map(|(h, name, days)| {
                            let key = format!("cert:{h}:{name}");
                            let title = cert_title(&name, &c.host_name(&h), days);
                            view! {
                                <div class="notif-item">
                                    <span class="notif-icon"><Icon name="cert" size=16 /></span>
                                    <div class="notif-text">
                                        <div class="notif-title">{title}</div>
                                        <div class="notif-body muted">
                                            {tr(
                                                "Renouvelez-le avant l'expiration pour éviter une erreur de sécurité dans les navigateurs.",
                                                "Renew it before it expires to avoid security errors in browsers.",
                                            )}
                                        </div>
                                        <div class="notif-actions">
                                            <button class="link" on:click=move |_| {
                                                open.set(false);
                                                c.open(AppKind::Certs, Some(h.clone()));
                                            }>{tr("Ouvrir Certificats", "Open Certificates")}</button>
                                        </div>
                                    </div>
                                    <button class="icon-btn notif-x" title=tr("Ignorer", "Dismiss") on:click=move |_| {
                                        let k = key.clone();
                                        dismissed.update(|d| d.push(k));
                                    }>
                                        <Icon name="x" size=12 />
                                    </button>
                                </div>
                            }
                        })
                        .collect_view()
                }}
                {move || {
                    let v = visible();
                    if v.is_empty() && !visible_certs().is_empty() {
                        return ().into_any();
                    }
                    if v.is_empty() {
                        return view! {
                            <div class="notif-empty"><Icon name="check" size=16 />{tr("Aucune alerte", "No alerts")}</div>
                        }
                            .into_any();
                    }
                    v.into_iter()
                        .map(|a| {
                            let key = a.key();
                            let (h1, h2) = (a.host.clone(), a.host.clone());
                            let unit = a.svc.unit.clone();
                            let q = crate::apps::files::shell_quote(&unit);
                            let logs = format!("journalctl -u {q} -n 100 --no-pager");
                            let restart = format!("sudo systemctl restart {q}");
                            let title = format!(
                                "{} {} {} {} {}",
                                tr("Le service", "Service"),
                                unit_label(&unit),
                                tr("sur", "on"),
                                c.host_name(&a.host),
                                tr("est en échec", "failed")
                            );
                            let desc = a.svc.description.clone();
                            view! {
                                <div class="notif-item">
                                    <span class="notif-icon"><Icon name="alert" size=16 /></span>
                                    <div class="notif-text">
                                        <div class="notif-title">{title}</div>
                                        <div class="notif-body muted">
                                            {(!desc.is_empty()).then(|| format!("{desc}. "))}
                                            {tr(
                                                "Le service s'est arrêté sur une erreur. Consultez ses logs pour en connaître la cause.",
                                                "The service stopped with an error. Check its log for the reason.",
                                            )}
                                        </div>
                                        <div class="notif-actions">
                                            <button class="link" on:click=move |_| {
                                                open.set(false);
                                                c.open(AppKind::Terminal { input: Some(logs.clone()) }, Some(h1.clone()));
                                            }>{tr("Voir les logs", "Show logs")}</button>
                                            <button class="link" on:click=move |_| {
                                                open.set(false);
                                                c.open(AppKind::Terminal { input: Some(restart.clone()) }, Some(h2.clone()));
                                            }>{tr("Redémarrer…", "Restart…")}</button>
                                        </div>
                                    </div>
                                    <button class="icon-btn notif-x" title=tr("Ignorer", "Dismiss") on:click=move |_| {
                                        let k = key.clone();
                                        dismissed.update(|d| d.push(k));
                                    }>
                                        <Icon name="x" size=12 />
                                    </button>
                                </div>
                            }
                        })
                        .collect_view()
                        .into_any()
                }}
            </div>
        </Show>
    }
}

fn cert_title(name: &str, host: &str, days: i64) -> String {
    if days < 0 {
        format!("{} {name} ({host}) {}", tr("Le certificat", "Certificate"), tr("a expiré", "has expired"))
    } else {
        format!("{} {name} ({host}) {} {days} {}", tr("Le certificat", "Certificate"), tr("expire dans", "expires in"), tr("jour(s)", "day(s)"))
    }
}

/// "teamspeak.service" -> "teamspeak".
fn unit_label(unit: &str) -> &str {
    unit.strip_suffix(".service").unwrap_or(unit)
}
