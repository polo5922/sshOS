//! Browser: opens the server's local web apps (`localhost:3000`, a container's IP…) through an
//! SSH tunnel, pings URLs from the server itself and lists its listening ports.

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::json;
use sshos_shared::{ListeningPort, PingResult};

use crate::icons::Icon;
use crate::state::*;
use crate::tauri::invoke;

/// `localhost:3000` → `http://localhost:3000/`.
pub fn normalize(input: &str) -> Option<String> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    // Without a scheme, ports ending in 443 (443, 8443, 9443…) are assumed to be TLS.
    let scheme = |hostport: &str| {
        let port = hostport.split('/').next().unwrap_or("").rsplit_once(':').map(|x| x.1).unwrap_or("");
        if port.ends_with("443") { "https" } else { "http" }
    };
    let s = if s.starts_with("http://") || s.starts_with("https://") {
        s.to_string()
    } else if s.chars().all(|c| c.is_ascii_digit()) {
        format!("{}://localhost:{s}", scheme(&format!(":{s}")))
    } else {
        format!("{}://{s}", scheme(s))
    };
    let after = s.split_once("://").map(|x| x.1).unwrap_or("");
    Some(if after.contains('/') { s } else { format!("{s}/") })
}

#[derive(Clone, PartialEq)]
enum Ping {
    Idle,
    Busy,
    Done(PingResult),
    Failed(String),
}

#[component]
pub fn Browser(host: String, start: Option<String>) -> impl IntoView {
    let host = StoredValue::new(host);
    let input = RwSignal::new(String::new());
    // Server-side URL currently shown, and its local tunnel URL.
    let current = RwSignal::new(None::<String>);
    let frame = RwSignal::new(None::<String>);
    let loading = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);
    let ports = RwSignal::new(Vec::<ListeningPort>::new());
    let ping = RwSignal::new(Ping::Idle);
    let history = RwSignal::new((Vec::<String>::new(), 0usize));
    // Bumped on reload so the iframe is recreated even when the src is unchanged.
    let nonce = RwSignal::new(0u32);
    let seq = StoredValue::new(0u32);

    let load_ports = move || {
        spawn_local(async move {
            if let Ok(v) = invoke::<Vec<ListeningPort>>("browser_ports", json!({ "hostId": host.get_value() })).await {
                let _ = ports.try_set(v);
            }
        });
    };

    // Shows `url` (already normalized) without touching the history.
    let show = move |url: String| {
        input.set(url.clone());
        current.set(Some(url.clone()));
        loading.set(true);
        error.set(None);
        ping.set(Ping::Idle);
        seq.update_value(|s| *s += 1);
        let my = seq.get_value();
        spawn_local(async move {
            let r = invoke::<String>("browser_open", json!({ "hostId": host.get_value(), "url": url })).await;
            if seq.try_get_value() != Some(my) {
                return;
            }
            let _ = loading.try_set(false);
            match r {
                Ok(local) => {
                    let _ = frame.try_set(Some(local));
                    let _ = nonce.try_update(|n| *n += 1);
                }
                Err(e) => {
                    let _ = frame.try_set(None);
                    let _ = error.try_set(Some(e.message()));
                }
            }
        });
    };
    let navigate = move |raw: String| {
        let Some(url) = normalize(&raw) else { return };
        history.update(|(v, i)| {
            v.truncate(*i);
            if v.last() != Some(&url) {
                v.push(url.clone());
            }
            *i = v.len();
        });
        show(url);
    };
    let step = move |d: isize| {
        let target = history.with_untracked(|(v, i)| {
            let j = *i as isize + d;
            (j >= 1 && j as usize <= v.len()).then(|| (j as usize, v[j as usize - 1].clone()))
        });
        if let Some((j, url)) = target {
            history.update(|h| h.1 = j);
            show(url);
        }
    };
    let can_back = move || history.with(|(_, i)| *i > 1);
    let can_fwd = move || history.with(|(v, i)| *i < v.len());
    let reload = move || {
        if let Some(u) = current.get_untracked() {
            show(u);
        }
    };
    let home = move || {
        current.set(None);
        frame.set(None);
        error.set(None);
        input.set(String::new());
        history.update(|h| h.1 = 0);
        load_ports();
    };
    let do_ping = move || {
        let Some(url) = normalize(&input.get_untracked()) else { return };
        ping.set(Ping::Busy);
        spawn_local(async move {
            let r = invoke::<PingResult>("browser_ping", json!({ "hostId": host.get_value(), "url": url })).await;
            let _ = ping.try_set(match r {
                Ok(p) => Ping::Done(p),
                Err(e) => Ping::Failed(e.message()),
            });
        });
    };
    let external = move || {
        let Some(local) = frame.get_untracked() else { return };
        let title = current.get_untracked().unwrap_or_default();
        let label = format!("web-{}", js_sys::Date::now() as u64);
        if !crate::tauri::open_webview_window(&label, &local, &title) {
            let _ = window().open_with_url_and_target(&local, "_blank");
        }
    };

    load_ports();
    match start {
        Some(u) => navigate(u),
        None => {}
    }

    app_menus(vec![
        MenuSection::new(
            tr("Navigation", "Navigation"),
            vec![
                MenuItem::new(tr("Précédent", "Back"), move || step(-1)).disabled(move || !can_back()),
                MenuItem::new(tr("Suivant", "Forward"), move || step(1)).disabled(move || !can_fwd()),
                MenuItem::new(tr("Recharger", "Reload"), reload).disabled(move || current.get().is_none()),
                MenuItem::new(tr("Accueil · ports du serveur", "Home · server ports"), home),
                None,
                MenuItem::new(tr("Pinger l'URL", "Ping URL"), do_ping).disabled(move || input.get().trim().is_empty()),
                MenuItem::new(tr("Ouvrir dans une fenêtre séparée", "Open in a separate window"), external)
                    .disabled(move || frame.get().is_none()),
            ],
        ),
    ]);

    let ping_pill = move || match ping.get() {
        Ping::Idle => None,
        Ping::Busy => Some(view! { <span class="pill info">"…"</span> }.into_any()),
        Ping::Done(p) => {
            let cls = match p.code {
                0 => "pill crit",
                200..=399 => "pill ok",
                400..=499 => "pill warn",
                _ => "pill crit",
            };
            let text = if p.code == 0 { tr("aucune réponse", "no answer").to_string() } else { format!("{} · {:.0} ms", p.code, p.ms) };
            Some(view! { <span class=cls title=p.error.unwrap_or_default()>{text}</span> }.into_any())
        }
        Ping::Failed(e) => Some(view! { <span class="pill crit" title=e>{tr("erreur", "error")}</span> }.into_any()),
    };

    let port_chips = move || {
        ports
            .get()
            .into_iter()
            .map(|p| {
                let url = format!("localhost:{}", p.port);
                let local_only = p.addr != "0.0.0.0";
                let title = format!("{}:{}{}", p.addr, p.port, p.process.as_ref().map(|x| format!(" · {x}")).unwrap_or_default());
                view! {
                    <button class="port-chip" class:local=local_only title=title on:click=move |_| navigate(url.clone())>
                        <span class="mono">{p.port}</span>
                        {p.process.clone().map(|x| view! { <span class="muted">{x}</span> })}
                    </button>
                }
            })
            .collect_view()
    };

    view! {
        <div class="browser">
            <header class="browser-bar">
                <button class="icon-btn" title=tr("Précédent", "Back") disabled=move || !can_back() on:click=move |_| step(-1)>
                    <Icon name="left" size=15 />
                </button>
                <button class="icon-btn" title=tr("Suivant", "Forward") disabled=move || !can_fwd() on:click=move |_| step(1)>
                    <Icon name="right" size=15 />
                </button>
                <button class="icon-btn" class:spin=move || loading.get() title=tr("Recharger", "Reload") disabled=move || current.get().is_none() on:click=move |_| reload()>
                    <Icon name="refresh" size=15 />
                </button>
                <button class="icon-btn" title=tr("Ports du serveur", "Server ports") on:click=move |_| home()>
                    <Icon name="home" size=15 />
                </button>
                <form class="url-form" on:submit=move |e| {
                    e.prevent_default();
                    navigate(input.get_untracked());
                }>
                    <Icon name="compass" size=14 />
                    <input
                        class="url-input mono"
                        placeholder=tr("localhost:3000, 172.17.0.2:8080…", "localhost:3000, 172.17.0.2:8080…")
                        spellcheck="false"
                        bind:value=input
                    />
                    {ping_pill}
                </form>
                <button class="btn" title=tr("Requête HTTP depuis le serveur (curl)", "HTTP request from the server (curl)") disabled=move || input.get().trim().is_empty() on:click=move |_| do_ping()>
                    <Icon name="zap" size=14 />"Ping"
                </button>
                <button class="icon-btn" title=tr("Ouvrir dans une fenêtre séparée", "Open in a separate window") disabled=move || frame.get().is_none() on:click=move |_| external()>
                    <Icon name="external" size=15 />
                </button>
            </header>
            <div class="browser-body">
                {move || {
                    if let Some(e) = error.get() {
                        return view! {
                            <div class="browser-empty">
                                <Icon name="alert" size=32 />
                                <h3>{tr("Impossible d'ouvrir la page", "Could not open the page")}</h3>
                                <p class="muted">{e}</p>
                                <button class="btn" on:click=move |_| reload()>{tr("Réessayer", "Retry")}</button>
                            </div>
                        }
                            .into_any();
                    }
                    match frame.get() {
                        Some(src) => {
                            let _ = nonce.get();
                            view! {
                                <iframe class="browser-frame" src=src title="page"></iframe>
                                <div class="browser-hint muted small">
                                    {tr("Page vide ? Certains sites refusent l'affichage intégré. ", "Blank page? Some sites refuse to be embedded. ")}
                                    <a href="#" on:click=move |e| { e.prevent_default(); external() }>{tr("Ouvrir dans une fenêtre séparée", "Open in a separate window")}</a>
                                </div>
                            }
                                .into_any()
                        }
                        None if loading.get() => view! { <div class="browser-empty"><div class="spinner"></div></div> }.into_any(),
                        None => view! {
                            <div class="browser-home">
                                <Icon name="compass" size=40 />
                                <h3>{tr("Navigateur du serveur", "Server browser")}</h3>
                                <p class="muted">
                                    {tr(
                                        "Les adresses sont ouvertes depuis le serveur via un tunnel SSH : localhost désigne le serveur.",
                                        "Addresses are opened from the server through an SSH tunnel: localhost means the server.",
                                    )}
                                </p>
                                <div class="side-title">{tr("Ports en écoute", "Listening ports")}</div>
                                <div class="port-chips">{port_chips}</div>
                            </div>
                        }
                            .into_any(),
                    }
                }}
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn urls() {
        assert_eq!(normalize("localhost:3000").as_deref(), Some("http://localhost:3000/"));
        assert_eq!(normalize("8080").as_deref(), Some("http://localhost:8080/"));
        assert_eq!(normalize("https://x/y").as_deref(), Some("https://x/y"));
        assert_eq!(normalize("  "), None);
        assert_eq!(normalize("localhost:9443").as_deref(), Some("https://localhost:9443/"));
        assert_eq!(normalize("443").as_deref(), Some("https://localhost:443/"));
        assert_eq!(normalize("http://localhost:9443").as_deref(), Some("http://localhost:9443/"));
    }
}
