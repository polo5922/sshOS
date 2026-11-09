//! Tunnels: SSH local port forwards (`ssh -L`). A port on this computer (127.0.0.1 only) is
//! forwarded to `host:port` as seen from the server: a database, an admin UI, a container…

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::json;
use sshos_shared::TunnelInfo;

use crate::apps::firewall::parse_listen;
use crate::apps::kit;
use crate::icons::Icon;
use crate::state::*;
use crate::tauri::{copy_text, invoke, invoke_unit};

/// `host:port` with a conservative host charset; IPv6 as `[::1]:port`.
pub fn valid_remote(s: &str) -> bool {
    let Some((h, p)) = s.trim().rsplit_once(':') else { return false };
    let h = h.trim_start_matches('[').trim_end_matches(']');
    !h.is_empty()
        && h.len() <= 253
        && h.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '_'))
        && p.parse::<u16>().is_ok_and(|p| p > 0)
}

const COMMON: [(u16, &str); 8] = [
    (5432, "PostgreSQL"),
    (3306, "MySQL"),
    (6379, "Redis"),
    (27017, "MongoDB"),
    (9090, "Prometheus"),
    (3000, "Grafana / Node"),
    (8080, "HTTP"),
    (81, "NPM admin"),
];

#[component]
pub fn Tunnels(host: String) -> impl IntoView {
    let host = StoredValue::new(host);
    let tunnels = RwSignal::new(Vec::<TunnelInfo>::new());
    let listening = RwSignal::new(Vec::<(u16, String)>::new());
    let remote = RwSignal::new("localhost:".to_string());
    let local_port = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let h = move || host.get_value();

    let refresh = move || {
        spawn_local(async move {
            if let Ok(v) = invoke::<Vec<TunnelInfo>>("tunnel_list", json!({})).await {
                let hid = host.get_value();
                let _ = tunnels.try_set(v.into_iter().filter(|t| t.host_id == hid).collect());
            }
        });
    };
    refresh();
    kit::query(h(), "ss -ltnuHp 2>/dev/null || ss -ltnuH 2>/dev/null", move |r| {
        let mut v: Vec<(u16, String)> = parse_listen(&r.stdout).into_iter().filter(|l| l.proto == "tcp").map(|l| (l.port, l.process)).collect();
        v.sort();
        v.dedup_by_key(|x| x.0);
        let _ = listening.try_set(v);
    });

    let open = move || {
        let r = remote.get_untracked().trim().to_string();
        if !valid_remote(&r) {
            ctx().error(tr("Destination invalide : hôte:port (ex. localhost:5432).", "Invalid destination: host:port (e.g. localhost:5432)."));
            return;
        }
        let lp: u16 = local_port.get_untracked().trim().parse().unwrap_or(0);
        busy.set(true);
        let (c, hid) = (ctx(), host.get_value());
        spawn_local(async move {
            match invoke::<TunnelInfo>("tunnel_open", json!({ "hostId": hid, "remote": r, "localPort": lp })).await {
                Ok(t) => {
                    let addr = format!("127.0.0.1:{}", t.local_port);
                    c.ok(format!("{} {addr} → {}", tr("Tunnel ouvert :", "Tunnel open:"), t.remote));
                    let _ = local_port.try_set(String::new());
                }
                Err(e) => c.handle_err(Some(&hid), e),
            }
            let _ = busy.try_set(false);
            refresh();
        });
    };
    let close = move |id: u32| {
        let c = ctx();
        spawn_local(async move {
            if let Err(e) = invoke_unit("tunnel_close", json!({ "id": id })).await {
                c.handle_err(None, e);
            }
            refresh();
        });
    };
    let pick = move |port: u16| remote.set(format!("localhost:{port}"));

    app_menus(vec![MenuSection::new(tr("Tunnels", "Tunnels"), vec![
        MenuItem::new(tr("Actualiser", "Refresh"), refresh),
        MenuItem::new(tr("Fermer tous les tunnels", "Close all tunnels"), move || {
            for t in tunnels.get_untracked() {
                close(t.id)
            }
        }),
    ])]);

    view! {
        <div class="split-app tunnels-app">
            <section class="main">
                <header class="main-head drag-region">
                    <h2 class="grow ellipsis">{tr("Tunnels SSH", "SSH tunnels")}</h2>
                    <button class="icon-btn" title=tr("Actualiser", "Refresh") on:click=move |_| refresh()><Icon name="refresh" size=15 /></button>
                </header>
                <div class="main-body">
                    <p class="muted small hint">
                        {tr(
                            "Un port de cet ordinateur (127.0.0.1 uniquement) est relié à une adresse vue depuis le serveur, comme ssh -L. Les tunnels se ferment à la déconnexion.",
                            "A port on this computer (127.0.0.1 only) is linked to an address seen from the server, like ssh -L. Tunnels close on disconnect.",
                        )}
                    </p>
                    <div class="fw-form">
                        <div class="fw-row">
                            <label class="grow">
                                <span class="muted small">{tr("Destination (depuis le serveur)", "Destination (from the server)")}</span>
                                <input class="mono" placeholder="localhost:5432" bind:value=remote on:keydown=move |e| {
                                    if e.key() == "Enter" {
                                        open()
                                    }
                                } />
                            </label>
                            <label>
                                <span class="muted small">{tr("Port local (vide = auto)", "Local port (empty = auto)")}</span>
                                <input class="mono fw-port" placeholder="auto" bind:value=local_port />
                            </label>
                            <button class="primary" disabled=move || busy.get() || !valid_remote(&remote.get()) on:click=move |_| open()>{tr("Ouvrir", "Open")}</button>
                        </div>
                        <div class="chips">
                            {move || {
                                let l = listening.get();
                                if l.is_empty() {
                                    COMMON.iter().map(|(p, n)| { let p = *p; view! { <button class="chip" on:click=move |_| pick(p)>{format!("{p} · {n}")}</button> } }).collect_view().into_any()
                                } else {
                                    l.into_iter()
                                        .map(|(p, proc_)| {
                                            let name = COMMON.iter().find(|(c, _)| *c == p).map(|(_, n)| n.to_string()).unwrap_or(proc_);
                                            let label = if name.is_empty() { p.to_string() } else { format!("{p} · {name}") };
                                            view! { <button class="chip" on:click=move |_| pick(p)>{label}</button> }
                                        })
                                        .collect_view()
                                        .into_any()
                                }
                            }}
                        </div>
                    </div>
                    {move || {
                        let v = tunnels.get();
                        if v.is_empty() {
                            return kit::empty_view(tr("Aucun tunnel ouvert.", "No open tunnels.")).into_any();
                        }
                        view! {
                            <div class="table-card">
                                <table class="table data-table">
                                    <thead><tr><th>{tr("Sur cet ordinateur", "On this computer")}</th><th></th><th>{tr("Vers (depuis le serveur)", "To (from the server)")}</th><th>{tr("État", "State")}</th><th></th></tr></thead>
                                    <tbody>
                                        {v.into_iter()
                                            .map(|t| {
                                                let addr = format!("127.0.0.1:{}", t.local_port);
                                                let a2 = addr.clone();
                                                let id = t.id;
                                                view! {
                                                    <tr>
                                                        <td class="mono"><b>{addr.clone()}</b></td>
                                                        <td class="muted">"→"</td>
                                                        <td class="mono">{t.remote.clone()}</td>
                                                        <td>{if t.active { view! { <span class="pill ok">{tr("actif", "active")}</span> }.into_any() } else { view! { <span class="pill off">{tr("fermé", "closed")}</span> }.into_any() }}</td>
                                                        <td class="row-actions">
                                                            <button class="icon-btn" title=tr("Copier l'adresse", "Copy address") on:click=move |_| {
                                                                copy_text(&a2);
                                                                ctx().info_toast(tr("Adresse copiée.", "Address copied."));
                                                            }><Icon name="link" size=14 /></button>
                                                            <button class="icon-btn danger" title=tr("Fermer le tunnel", "Close tunnel") on:click=move |_| close(id)><Icon name="x" size=14 /></button>
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
    fn remotes() {
        assert!(valid_remote("localhost:5432"));
        assert!(valid_remote("172.17.0.2:80"));
        assert!(valid_remote("[::1]:6379"));
        assert!(!valid_remote("localhost"));
        assert!(!valid_remote("localhost:0"));
        assert!(!valid_remote("a b:80"));
    }
}
