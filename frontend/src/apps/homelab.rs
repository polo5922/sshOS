//! Homelab: a live, interactive diagram of the server — hardware, disks and what lives on them,
//! Docker networks and containers, the nginx / Nginx Proxy Manager routes and the open ports.
//!
//! Hovering anything lights up what it is linked to (a container → its disks, networks, domains
//! and ports; a disk → the containers storing data on it…). Clicking a container opens its card.
//! Everything is collected read-only by the `homelab_snapshot` command.

use std::collections::HashSet;

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::json;
use sshos_shared::{HomelabContainer, HomelabSnapshot};

use crate::icons::Icon;
use crate::state::*;
use crate::tauri::{copy_text, invoke};

/// What the pointer (or the selection) is on.
#[derive(Clone, PartialEq, Debug)]
pub enum Focus {
    Ct(String),
    Fs(String),
    Net(String),
    Port(u16, String),
    Proxy(usize),
}

/// Everything linked to the focus; the rest of the diagram is dimmed.
#[derive(Clone, PartialEq, Default, Debug)]
pub struct Lit {
    pub cts: HashSet<String>,
    pub fss: HashSet<String>,
    pub nets: HashSet<String>,
    pub ports: HashSet<(u16, String)>,
    pub proxies: HashSet<usize>,
}

pub fn lit(s: &HomelabSnapshot, f: &Focus) -> Lit {
    let mut l = Lit::default();
    match f {
        Focus::Ct(n) => {
            l.cts.insert(n.clone());
        }
        Focus::Fs(m) => {
            l.fss.insert(m.clone());
            for fs in s.disks.iter().flat_map(|d| &d.filesystems).filter(|fs| &fs.mount == m) {
                l.cts.extend(fs.containers.iter().cloned());
            }
        }
        Focus::Net(n) => {
            l.nets.insert(n.clone());
            l.cts.extend(s.containers.iter().filter(|c| c.networks.iter().any(|(net, _)| net == n)).map(|c| c.name.clone()));
        }
        Focus::Port(p, proto) => {
            l.ports.insert((*p, proto.clone()));
            l.cts.extend(s.open_ports.iter().filter(|o| o.port == *p && &o.proto == proto).map(|o| o.service.clone()));
        }
        Focus::Proxy(i) => {
            l.proxies.insert(*i);
            l.cts.extend(s.proxies.get(*i).and_then(|p| p.container.clone()));
        }
    }
    // Spread from the containers to whatever they touch.
    let single_ct = matches!(f, Focus::Ct(_) | Focus::Proxy(_) | Focus::Port(..));
    for c in s.containers.iter().filter(|c| l.cts.contains(&c.name)) {
        if single_ct {
            l.nets.extend(c.networks.iter().map(|(n, _)| n.clone()));
            l.fss.extend(c.mounts.iter().map(|m| m.fs.clone()).filter(|m| !m.is_empty()));
        }
    }
    for (i, p) in s.proxies.iter().enumerate() {
        if p.container.as_ref().is_some_and(|c| l.cts.contains(c)) {
            l.proxies.insert(i);
        }
    }
    for o in &s.open_ports {
        if l.cts.contains(&o.service) {
            l.ports.insert((o.port, o.proto.clone()));
        }
    }
    l
}

/// Diagram columns, as in a hand-drawn homelab map.
pub fn category(c: &HomelabContainer) -> &'static str {
    let img = c.image.to_lowercase();
    let name = c.name.to_lowercase();
    let has = |keys: &[&str]| keys.iter().any(|k| img.contains(k) || name.contains(k));
    if has(&["nginx", "traefik", "caddy", "haproxy", "proxy", "authelia", "authentik", "keycloak", "crowdsec", "fail2ban", "vaultwarden", "bitwarden", "wireguard", "wg-easy", "cloudflared", "pihole", "pi-hole", "adguard", "tailscale", "headscale", "certbot"]) {
        return tr("Proxy et sécurité", "Edge & security");
    }
    if has(&["postgres", "mysql", "mariadb", "mongo", "redis", "valkey", "clickhouse", "elasticsearch", "opensearch", "memcached", "couchdb", "rabbitmq", "kafka", "minio", "phpmyadmin", "adminer", "pgadmin"]) {
        return tr("Bases de données", "Databases");
    }
    if has(&["grafana", "prometheus", "loki", "promtail", "alloy", "uptime-kuma", "uptime", "netdata", "exporter", "cadvisor", "portainer", "dozzle", "glances", "influx", "alertmanager", "beszel", "speedtest", "scrutiny", "gotify", "ntfy", "watchtower", "diun"]) {
        return tr("Surveillance", "Monitoring");
    }
    if has(&["jellyfin", "plex", "emby", "sonarr", "radarr", "prowlarr", "lidarr", "bazarr", "readarr", "seerr", "qbittorrent", "transmission", "deluge", "sabnzbd", "immich", "photoprism", "navidrome", "tautulli", "metube", "audiobookshelf", "kavita", "komga"]) {
        return tr("Médias et photos", "Media & photos");
    }
    if has(&["gitea", "forgejo", "gitlab", "jenkins", "drone", "woodpecker", "code-server", "registry", "sonarqube", "runner", "act_runner", "harbor", "verdaccio", "nexus"]) {
        return "Dev";
    }
    if has(&["matrix", "synapse", "element", "mattermost", "rocket", "mautrix", "conduit", "dendrite", "zulip", "revolt"]) {
        return "Chat";
    }
    if has(&["minecraft", "pelican", "pterodactyl", "wings", "valheim", "factorio", "satisfactory", "steam", "palworld", "terraria"]) {
        return tr("Jeux", "Gaming");
    }
    if has(&["homeassistant", "home-assistant", "zigbee", "mosquitto", "mqtt", "node-red", "esphome", "frigate", "scrypted"]) {
        return tr("Domotique", "Home automation");
    }
    if has(&["nextcloud", "paperless", "bookstack", "wiki", "outline", "vikunja", "actual", "firefly", "stirling", "mealie", "tandoor", "homepage", "heimdall", "homarr", "dashy", "n8n", "linkwarden", "freshrss", "joplin", "onlyoffice", "collabora", "penpot", "open-webui", "ollama", "seafile", "syncthing", "filebrowser", "wordpress", "ghost"]) {
        return tr("Productivité", "Productivity");
    }
    tr("Autres", "Other")
}

/// Colors of networks (like VLAN colors on a homelab map).
const NET_COLORS: [&str; 8] = ["#4c9bff", "#f0883e", "#b083f0", "#3fb950", "#e3b341", "#39c5cf", "#f85149", "#db61a2"];

/// Icon chip: two letters on a color derived from the image name.
fn chip(c: &HomelabContainer) -> (String, String) {
    let base = c.image.rsplit('/').next().unwrap_or(&c.image).split([':', '@']).next().unwrap_or("").to_string();
    let base = if base.is_empty() { c.name.clone() } else { base };
    let letters: String = {
        let parts: Vec<&str> = base.split(['-', '_', '.']).filter(|p| !p.is_empty()).collect();
        let s = if parts.len() >= 2 {
            format!("{}{}", parts[0].chars().next().unwrap_or(' '), parts[1].chars().next().unwrap_or(' '))
        } else {
            base.chars().take(2).collect()
        };
        s.to_uppercase()
    };
    let hue = base.bytes().fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32)) % 360;
    (letters, format!("hsl({hue} 55% 42%)"))
}

fn short_image(img: &str) -> String {
    let i = img.split('@').next().unwrap_or(img);
    i.strip_prefix("docker.io/").unwrap_or(i).strip_prefix("library/").unwrap_or(i).to_string()
}

fn last_octet(ip: &str) -> String {
    match ip.rsplit_once('.') {
        Some((_, o)) if !ip.contains(':') => format!(".{o}"),
        _ => String::new(),
    }
}

fn pct(used: u64, size: u64) -> f64 {
    if size == 0 { 0.0 } else { used as f64 * 100.0 / size as f64 }
}

#[derive(Clone, Copy, PartialEq)]
enum Group {
    Category,
    Project,
}

/// Whether an element is outside the lit set (so drawn faded).
fn dimmed(lights: Memo<Option<Lit>>, pick: impl Fn(&Lit) -> bool) -> bool {
    lights.with(|l| l.as_ref().is_some_and(|l| !pick(l)))
}

#[component]
pub fn Homelab(host: String) -> impl IntoView {
    let c = ctx();
    let host = StoredValue::new(host);
    let snap = RwSignal::new(None::<HomelabSnapshot>);
    let loading = RwSignal::new(false);
    let hover = RwSignal::new(None::<Focus>);
    let selected = RwSignal::new(None::<String>);
    let group = RwSignal::new(Group::Category);
    let show_stopped = RwSignal::new(true);
    let query = RwSignal::new(String::new());
    let zoom = RwSignal::new(1.0_f64);
    let title = RwSignal::new(tr("Mon homelab", "My Homelab").to_string());

    let refresh = move || {
        if loading.get_untracked() {
            return;
        }
        loading.set(true);
        spawn_local(async move {
            let h = host.get_value();
            let r = invoke::<HomelabSnapshot>("homelab_snapshot", json!({ "hostId": h })).await;
            let _ = loading.try_set(false);
            match r {
                Ok(s) => {
                    let _ = snap.try_set(Some(s));
                }
                Err(e) => c.handle_err(Some(&h), e),
            }
        });
    };
    refresh();

    let set_zoom = move |z: f64| zoom.set((z * 10.0).round().clamp(4.0, 20.0) / 10.0);

    app_menus(vec![
        MenuSection::new(tr("Diagramme", "Diagram"), vec![MenuItem::new(tr("Actualiser", "Refresh"), move || refresh()).key("F5")]),
        MenuSection::new(tr("Présentation", "View"), vec![
            MenuItem::new(tr("Grouper par catégorie", "Group by category"), move || group.set(Group::Category)).checked(move || group.get() == Group::Category),
            MenuItem::new(tr("Grouper par projet compose", "Group by compose project"), move || group.set(Group::Project)).checked(move || group.get() == Group::Project),
            None,
            MenuItem::new(tr("Afficher les conteneurs arrêtés", "Show stopped containers"), move || show_stopped.update(|v| *v = !*v)).checked(move || show_stopped.get()),
            None,
            MenuItem::new(tr("Zoom avant", "Zoom in"), move || set_zoom(zoom.get_untracked() + 0.1)).key("Ctrl+molette"),
            MenuItem::new(tr("Zoom arrière", "Zoom out"), move || set_zoom(zoom.get_untracked() - 0.1)),
            MenuItem::new(tr("Taille réelle", "Actual size"), move || zoom.set(1.0)),
        ]),
    ]);

    // The focus: what is hovered, or else the selected container.
    let focus = Memo::new(move |_| hover.get().or_else(|| selected.get().map(Focus::Ct)));
    let lights = Memo::new(move |_| {
        let f = focus.get()?;
        snap.with(|s| s.as_ref().map(|s| lit(s, &f)))
    });
    let enter = move |f: Focus| hover.set(Some(f));
    let leave = move || hover.set(None);

    // Network colors, by name.
    let net_color = move |name: &str| -> &'static str {
        snap.with(|s| {
            s.as_ref()
                .and_then(|s| s.networks.iter().position(|n| n.name == name))
                .map(|i| NET_COLORS[i % NET_COLORS.len()])
                .unwrap_or("#6e7b91")
        })
    };

    // ---------- Side panels ----------
    let networks_panel = move || {
        let nets = snap.with(|s| s.as_ref().map(|s| s.networks.clone()).unwrap_or_default());
        view! {
            <div class="hl-panel">
                <h3>{tr("Réseaux Docker", "Docker networks")}</h3>
                {if nets.is_empty() {
                    view! { <div class="hl-muted">{tr("Aucun réseau utilisateur.", "No user network.")}</div> }.into_any()
                } else {
                    nets.into_iter()
                        .enumerate()
                        .map(|(i, n)| {
                            let color = NET_COLORS[i % NET_COLORS.len()];
                            let (n1, n2) = (n.name.clone(), n.name.clone());
                            let count = n.containers.len();
                            view! {
                                <div
                                    class="hl-net"
                                    style=format!("--net:{color}")
                                    class:dim=move || dimmed(lights, |l| l.nets.contains(&n2))
                                    on:mouseenter=move |_| enter(Focus::Net(n1.clone()))
                                    on:mouseleave=move |_| leave()
                                >
                                    <div class="hl-net-top">
                                        <span class="hl-net-tag">{n.driver.clone()}</span>
                                        <b class="ellipsis">{n.name.clone()}</b>
                                        <span class="grow"></span>
                                        <span class="mono">{n.subnet.clone()}</span>
                                    </div>
                                    <div class="hl-sub">
                                        {format!("{count} {}", if count > 1 { tr("conteneurs actifs", "running containers") } else { tr("conteneur actif", "running container") })}
                                    </div>
                                </div>
                            }
                        })
                        .collect_view()
                        .into_any()
                }}
            </div>
        }
    };

    let legend_panel = move || {
        view! {
            <div class="hl-panel hl-legend">
                <h3>{tr("Légende", "Legend")}</h3>
                <div><span class="hl-domain">app.example.com</span><span>{tr("domaine routé par nginx / NPM", "domain routed by nginx / NPM")}</span></div>
                <div><span class="hl-pill lan">"LAN"</span><span>{tr("non exposé à l'extérieur", "not exposed outside")}</span></div>
                <div><span class="hl-pill pub">"PUBLIC"</span><span>{tr("port publié ou derrière le proxy", "published port or behind the proxy")}</span></div>
                <div><span class="hl-dot run"></span><span>{tr("en cours · arrêté", "running · stopped")}<span class="hl-dot off" style="margin-left:6px"></span></span></div>
                <div><span class="hl-netbar"></span><span>{tr("couleur du réseau Docker", "Docker network color")}</span></div>
                <div><span class="hl-pill docker">"docker"</span><span>{tr("disque des images et couches Docker", "disk holding Docker images and layers")}</span></div>
                <p class="hl-muted">{tr("Survolez un élément pour voir ce qui y est relié ; cliquez un conteneur pour ses détails.", "Hover anything to see what it is linked to; click a container for details.")}</p>
            </div>
        }
    };

    let ports_panel = move || {
        let ports = snap.with(|s| s.as_ref().map(|s| s.open_ports.clone()).unwrap_or_default());
        view! {
            <div class="hl-panel">
                <h3>{tr("Ouvert sur le réseau", "Open to the network")}</h3>
                <div class="hl-sub">{tr("ports en écoute hors boucle locale (ss)", "ports listening outside loopback (ss)")}</div>
                {if ports.is_empty() {
                    view! { <div class="hl-muted">{tr("Aucun port détecté.", "No port found.")}</div> }.into_any()
                } else {
                    ports
                        .into_iter()
                        .map(|p| {
                            let key = (p.port, p.proto.clone());
                            let k2 = key.clone();
                            view! {
                                <div
                                    class="hl-port"
                                    class:dim=move || dimmed(lights, {
                                        let k = k2.clone();
                                        move |l: &Lit| l.ports.contains(&k)
                                    })
                                    on:mouseenter=move |_| enter(Focus::Port(key.0, key.1.clone()))
                                    on:mouseleave=move |_| leave()
                                >
                                    <span class="mono hl-port-n">{p.port}</span>
                                    <span class="grow ellipsis">{if p.service.is_empty() { "—".to_string() } else { p.service.clone() }}</span>
                                    <span class=format!("hl-pill {}", p.proto)>{p.proto.to_uppercase()}</span>
                                </div>
                            }
                        })
                        .collect_view()
                        .into_any()
                }}
            </div>
        }
    };

    let routes_panel = move || {
        let proxies = snap.with(|s| s.as_ref().map(|s| s.proxies.clone()).unwrap_or_default());
        view! {
            <div class="hl-panel">
                <h3>{tr("Routes du proxy", "Proxy routes")}</h3>
                <div class="hl-sub">{tr("nginx (sites activés) et Nginx Proxy Manager", "nginx (enabled sites) and Nginx Proxy Manager")}</div>
                {if proxies.is_empty() {
                    view! { <div class="hl-muted">{tr("Aucune route lisible.", "No readable route.")}</div> }.into_any()
                } else {
                    proxies
                        .into_iter()
                        .enumerate()
                        .map(|(i, p)| {
                            let ct = p.container.clone();
                            view! {
                                <div
                                    class="hl-route"
                                    class:dim=move || dimmed(lights, |l| l.proxies.contains(&i))
                                    on:mouseenter=move |_| enter(Focus::Proxy(i))
                                    on:mouseleave=move |_| leave()
                                    on:click=move |_| {
                                        if let Some(n) = &ct {
                                            selected.set(Some(n.clone()));
                                        }
                                    }
                                >
                                    <div class="hl-route-top">
                                        {p.ssl.then(|| view! { <Icon name="lock" size=11 /> })}
                                        <span class="hl-domain ellipsis">{p.domains.join(", ")}</span>
                                        <span class="grow"></span>
                                        <span class="hl-pill">{if p.source == "npm" { "NPM" } else { "nginx" }}</span>
                                    </div>
                                    <div class="hl-sub ellipsis">
                                        {format!("→ {}", p.target)}
                                        {p.container.clone().map(|c| view! { <b class="hl-route-ct">{format!(" · {c}")}</b> })}
                                    </div>
                                </div>
                            }
                        })
                        .collect_view()
                        .into_any()
                }}
            </div>
        }
    };

    // ---------- Server card ----------
    let hardware = move || {
        let Some(s) = snap.get() else { return ().into_any() };
        let row = |icon: &'static str, label: &'static str, value: String, sub: String| {
            view! {
                <div class="hl-hw">
                    <Icon name=icon size=16 />
                    <div class="grow">
                        <div class="hl-hw-label">{label}</div>
                        <div class="hl-hw-value ellipsis" title=value.clone()>{value.clone()}</div>
                        {(!sub.is_empty()).then(|| view! { <div class="hl-sub ellipsis">{sub.clone()}</div> })}
                    </div>
                </div>
            }
        };
        let cpu = s.cpu.replace("(R)", "").replace("(TM)", "").replace(" CPU", "");
        view! {
            <div class="hl-col-title">{tr("Matériel", "Hardware")}</div>
            {row("cpu", "CPU", if cpu.is_empty() { "—".into() } else { cpu }, format!("{} {}", s.cores, tr("cœurs", "cores")))}
            {row("activity", tr("Mémoire", "Memory"), human_bytes(s.mem_total as f64), String::new())}
            {row("server", tr("Système", "System"), s.os.clone(), format!("Linux {}", s.kernel))}
            {row("network", tr("Réseau", "Network"), s.ips.first().cloned().unwrap_or_default(), s.ips.iter().skip(1).take(3).cloned().collect::<Vec<_>>().join(" · "))}
            {(!s.docker_version.is_empty()).then(|| row("box", "Docker", s.docker_version.clone(), s.docker_root.clone()))}
            {s.disks
                .iter()
                .filter(|d| d.size > 0)
                .map(|d| {
                    let label = match d.kind.as_str() {
                        "NVMe" => "NVME",
                        "SSD" => "SSD",
                        "HDD" => "HDD",
                        _ => tr("DISQUE", "DISK"),
                    };
                    row("hdd", label, format!("{} {}", if d.model.is_empty() { d.name.clone() } else { d.model.clone() }, human_bytes(d.size as f64)), d.name.clone())
                })
                .collect_view()}
        }
        .into_any()
    };

    let storage = move || {
        let Some(s) = snap.get() else { return ().into_any() };
        view! {
            <div class="hl-col-title">{tr("Stockage utilisé par Docker", "Storage used by Docker")}</div>
            {s.disks
                .into_iter()
                .filter(|d| !d.filesystems.is_empty())
                .map(|d| {
                    let head = if d.model.is_empty() { d.name.clone() } else { format!("{} · {}", d.name, d.model) };
                    view! {
                        <div class="hl-disk">
                            <div class="hl-disk-head">
                                <Icon name="hdd" size=18 />
                                <b class="grow ellipsis" title=head.clone()>{head.clone()}</b>
                                {(!d.kind.is_empty()).then(|| view! { <span class="hl-pill">{d.kind.clone()}</span> })}
                                {(d.size > 0).then(|| view! { <span class="hl-sub">{human_bytes(d.size as f64)}</span> })}
                            </div>
                            {d.filesystems
                                .into_iter()
                                .map(|fs| {
                                    let p = pct(fs.used, fs.size);
                                    let level = if p >= 90.0 { "crit" } else if p >= 75.0 { "warn" } else { "" };
                                    let (m1, m2) = (fs.mount.clone(), fs.mount.clone());
                                    let users = fs.containers.clone();
                                    view! {
                                        <div
                                            class="hl-fs"
                                            class:dim=move || dimmed(lights, {
                                                let m = m2.clone();
                                                move |l: &Lit| l.fss.contains(&m)
                                            })
                                            on:mouseenter=move |_| enter(Focus::Fs(m1.clone()))
                                            on:mouseleave=move |_| leave()
                                        >
                                            <div class="hl-fs-top">
                                                <b class="mono">{fs.mount.clone()}</b>
                                                <span class="hl-sub">{format!("{} · {}", fs.source, fs.fstype)}</span>
                                                <span class="grow"></span>
                                                {fs.docker_root.then(|| view! { <span class="hl-pill docker">"docker"</span> })}
                                            </div>
                                            <div class=format!("hl-bar {level}")><div style=format!("width:{p:.1}%")></div></div>
                                            <div class="hl-sub">
                                                {format!(
                                                    "{p:.0} % {} · {} {}",
                                                    tr("utilisé", "used"),
                                                    human_bytes(fs.size.saturating_sub(fs.used) as f64),
                                                    tr("libres", "free")
                                                )}
                                            </div>
                                            {(!users.is_empty()).then(|| view! {
                                                <div class="hl-users">
                                                    {users
                                                        .into_iter()
                                                        .map(|u| {
                                                            let u2 = u.clone();
                                                            view! {
                                                                <button class="hl-user" on:click=move |e| {
                                                                    e.stop_propagation();
                                                                    selected.set(Some(u2.clone()));
                                                                }>{u}</button>
                                                            }
                                                        })
                                                        .collect_view()}
                                                </div>
                                            })}
                                        </div>
                                    }
                                })
                                .collect_view()}
                        </div>
                    }
                })
                .collect_view()}
        }
        .into_any()
    };

    let service = move |ct: HomelabContainer| {
        let (letters, color) = chip(&ct);
        let running = ct.state == "running";
        let ip = ct.networks.iter().map(|(_, ip)| last_octet(ip)).find(|s| !s.is_empty()).unwrap_or_default();
        let (n1, n2, n3, n4) = (ct.name.clone(), ct.name.clone(), ct.name.clone(), ct.name.clone());
        let nets = ct.networks.clone();
        let ports: Vec<String> = ct.ports.iter().filter(|p| p.contains('→')).take(3).cloned().collect();
        view! {
            <div
                class="hl-svc"
                class:stopped=!running
                class:sel=move || selected.with(|s| s.as_deref() == Some(n4.as_str()))
                class:dim=move || dimmed(lights, {
                    let n = n2.clone();
                    move |l: &Lit| l.cts.contains(&n)
                })
                on:mouseenter=move |_| enter(Focus::Ct(n1.clone()))
                on:mouseleave=move |_| leave()
                on:click=move |_| selected.update(|s| *s = if s.as_deref() == Some(n3.as_str()) { None } else { Some(n3.clone()) })
            >
                <span class="hl-ico" style=format!("background:{color}")>{letters}</span>
                <div class="hl-svc-main">
                    <div class="hl-svc-name">
                        <b class="ellipsis">{ct.name.clone()}</b>
                        {(!ct.public).then(|| view! { <span class="hl-pill lan">"LAN"</span> })}
                    </div>
                    <div class="hl-sub ellipsis" title=ct.image.clone()>{short_image(&ct.image)}</div>
                    {ct.domains.iter().map(|d| view! { <div class="hl-domain ellipsis">{d.clone()}</div> }).collect_view()}
                    {(!ports.is_empty()).then(|| view! { <div class="hl-svc-ports mono">{ports.join("  ")}</div> })}
                </div>
                <div class="hl-svc-side">
                    <span class="hl-tags">
                        {nets
                            .iter()
                            .map(|(n, _)| view! { <span class="hl-nettag" style=format!("background:{}", net_color(n)) title=n.clone()></span> })
                            .collect_view()}
                    </span>
                    <span class="mono hl-sub">{ip}</span>
                    <span class="hl-dot" class:run=running class:off=!running title=ct.state.clone()></span>
                </div>
            </div>
        }
    };

    let services = move || {
        let Some(s) = snap.get() else { return ().into_any() };
        if s.docker_version.is_empty() {
            return view! {
                <div class="hl-muted hl-nodocker">
                    {tr(
                        "Docker est absent ou illisible pour cet utilisateur (ajoutez-le au groupe docker). Les disques, ports et routes nginx restent affichés.",
                        "Docker is missing or not readable by this user (add it to the docker group). Disks, ports and nginx routes are still shown.",
                    )}
                </div>
            }
            .into_any();
        }
        let q = query.get().to_lowercase();
        let stopped = show_stopped.get();
        let by = group.get();
        let mut groups: Vec<(String, Vec<HomelabContainer>)> = Vec::new();
        for ct in s.containers.into_iter() {
            if !stopped && ct.state != "running" {
                continue;
            }
            if !q.is_empty()
                && !(ct.name.to_lowercase().contains(&q) || ct.image.to_lowercase().contains(&q) || ct.domains.iter().any(|d| d.contains(&q)) || ct.project.to_lowercase().contains(&q))
            {
                continue;
            }
            let key = match by {
                Group::Category => category(&ct).to_string(),
                Group::Project if ct.project.is_empty() => tr("Conteneurs autonomes", "Standalone containers").to_string(),
                Group::Project => ct.project.clone(),
            };
            match groups.iter_mut().find(|(k, _)| *k == key) {
                Some((_, v)) => v.push(ct),
                None => groups.push((key, vec![ct])),
            }
        }
        if groups.is_empty() {
            return view! { <div class="hl-muted">{tr("Aucun conteneur.", "No container.")}</div> }.into_any();
        }
        // Biggest groups first; "Other" / standalone last.
        let last = [tr("Autres", "Other"), tr("Conteneurs autonomes", "Standalone containers")];
        groups.sort_by(|a, b| last.contains(&a.0.as_str()).cmp(&last.contains(&b.0.as_str())).then(b.1.len().cmp(&a.1.len())));
        groups
            .into_iter()
            .map(|(k, v)| {
                view! {
                    <div class="hl-group">
                        <div class="hl-col-title">{format!("{k} · {}", v.len())}</div>
                        {v.into_iter().map(service).collect_view()}
                    </div>
                }
            })
            .collect_view()
            .into_any()
    };

    // ---------- Inspector ----------
    let inspector = move || {
        let name = selected.get()?;
        let s = snap.get()?;
        let ct = s.containers.iter().find(|c| c.name == name)?.clone();
        let h = host.get_value();
        let web = ct
            .ports
            .iter()
            .find_map(|p| p.split_once('→').filter(|(_, cp)| cp.ends_with("/tcp")).and_then(|(hp, _)| hp.parse::<u16>().ok()))
            .map(|hp| format!("localhost:{hp}"))
            .or_else(|| {
                let ip = ct.networks.iter().map(|(_, ip)| ip.clone()).find(|ip| !ip.is_empty() && !ip.contains(':'))?;
                let port = ct.ports.iter().filter(|p| !p.contains('→') && p.ends_with("/tcp")).find_map(|p| p.trim_end_matches("/tcp").parse::<u16>().ok())?;
                Some(format!("{ip}:{port}"))
            });
        let (h1, h2) = (h.clone(), h.clone());
        Some(view! {
            <aside class="hl-inspector">
                <div class="hl-insp-head">
                    <span class="hl-ico" style=format!("background:{}", chip(&ct).1)>{chip(&ct).0}</span>
                    <div class="grow">
                        <b>{ct.name.clone()}</b>
                        <div class="hl-sub">{format!("{} · {}", ct.state, category(&ct))}</div>
                    </div>
                    <button class="icon-btn" title=tr("Fermer", "Close") on:click=move |_| selected.set(None)>
                        <Icon name="x" size=14 />
                    </button>
                </div>
                <div class="hl-insp-body">
                    <div class="hl-kv"><span>"Image"</span><span class="mono">{ct.image.clone()}</span></div>
                    {(!ct.project.is_empty()).then(|| view! { <div class="hl-kv"><span>"Compose"</span><span>{ct.project.clone()}</span></div> })}
                    <div class="hl-kv">
                        <span>{tr("Réseaux", "Networks")}</span>
                        <span>
                            {ct.networks.iter().map(|(n, ip)| view! {
                                <div><span class="hl-nettag" style=format!("background:{}", net_color(n))></span>{format!(" {n} ")}<span class="mono hl-sub">{ip.clone()}</span></div>
                            }).collect_view()}
                        </span>
                    </div>
                    <div class="hl-kv"><span>"Ports"</span><span class="mono">{if ct.ports.is_empty() { "—".to_string() } else { ct.ports.join(", ") }}</span></div>
                    {(!ct.domains.is_empty()).then(|| view! {
                        <div class="hl-kv">
                            <span>{tr("Domaines", "Domains")}</span>
                            <span>
                                {ct.domains.iter().map(|d| {
                                    let d2 = d.clone();
                                    view! {
                                        <div class="hl-domain hl-copy" title=tr("Copier", "Copy") on:click=move |_| {
                                            copy_text(&d2);
                                            c.toast(ToastKind::Ok, tr("Domaine copié", "Domain copied"));
                                        }>{d.clone()}</div>
                                    }
                                }).collect_view()}
                            </span>
                        </div>
                    })}
                    <div class="hl-kv">
                        <span>{tr("Données", "Data")}</span>
                        <span>
                            {if ct.mounts.is_empty() {
                                view! { <span class="hl-sub">{tr("aucun volume", "no volume")}</span> }.into_any()
                            } else {
                                ct.mounts.iter().map(|m| {
                                    let path = (m.kind == "bind").then(|| m.source.clone());
                                    let hh = h.clone();
                                    view! {
                                        <div class="hl-mount">
                                            <span class="hl-pill">{m.kind.clone()}</span>
                                            {match path {
                                                Some(p) => view! {
                                                    <button class="link mono" title=tr("Ouvrir dans Fichiers", "Open in Files") on:click=move |_| c.open(AppKind::Files { path: Some(p.clone()) }, Some(hh.clone()))>{m.source.clone()}</button>
                                                }.into_any(),
                                                None => view! { <span class="mono">{m.source.clone()}</span> }.into_any(),
                                            }}
                                            <span class="hl-sub mono">{format!("→ {}", m.destination)}</span>
                                            {(!m.fs.is_empty()).then(|| view! { <span class="hl-sub">{format!("{} {}", tr("sur", "on"), m.fs)}</span> })}
                                        </div>
                                    }
                                }).collect_view().into_any()
                            }}
                        </span>
                    </div>
                </div>
                <div class="hl-insp-actions">
                    <button class="btn" on:click=move |_| c.open(AppKind::Docker, Some(h1.clone()))>
                        <Icon name="box" size=13 />
                        {tr("Ouvrir Docker", "Open Docker")}
                    </button>
                    {web.map(|u| view! {
                        <button class="btn" on:click=move |_| c.open(AppKind::Browser { url: Some(u.clone()) }, Some(h2.clone()))>
                            <Icon name="compass" size=13 />
                            {tr("Navigateur", "Browser")}
                        </button>
                    })}
                </div>
            </aside>
        })
    };

    // ---------- Pan (drag the background) and Ctrl+wheel zoom ----------
    let scroller = NodeRef::<leptos::html::Div>::new();
    let drag = StoredValue::new(None::<(i32, i32, i32, i32)>);
    let on_down = move |e: leptos::ev::MouseEvent| {
        let Some(el) = scroller.get() else { return };
        let on_bg = e
            .target()
            .and_then(|t| wasm_bindgen::JsCast::dyn_into::<web_sys::Element>(t).ok())
            .is_some_and(|t| t.class_list().contains("hl-scroll") || t.class_list().contains("hl-board") || t.class_list().contains("hl-grid"));
        if on_bg && e.button() == 0 {
            drag.set_value(Some((e.client_x(), e.client_y(), el.scroll_left(), el.scroll_top())));
        }
    };
    let on_move = move |e: leptos::ev::MouseEvent| {
        let Some((x, y, sl, st)) = drag.get_value() else { return };
        if let Some(el) = scroller.get() {
            el.set_scroll_left(sl - (e.client_x() - x));
            el.set_scroll_top(st - (e.client_y() - y));
        }
    };
    let on_up = move |_| drag.set_value(None);
    let on_wheel = move |e: leptos::ev::WheelEvent| {
        if e.ctrl_key() {
            e.prevent_default();
            set_zoom(zoom.get_untracked() + if e.delta_y() < 0.0 { 0.1 } else { -0.1 });
        }
    };

    view! {
        <div class="hl-app">
            <header class="main-head drag-region hl-head">
                <h2>"Homelab"</h2>
                <div class="seg">
                    <button class:active=move || group.get() == Group::Category on:click=move |_| group.set(Group::Category)>{tr("Catégories", "Categories")}</button>
                    <button class:active=move || group.get() == Group::Project on:click=move |_| group.set(Group::Project)>{tr("Projets compose", "Compose projects")}</button>
                </div>
                <label class="check small">
                    <input type="checkbox" prop:checked=move || show_stopped.get() on:change=move |_| show_stopped.update(|v| *v = !*v) />
                    {tr("Arrêtés", "Stopped")}
                </label>
                <span class="grow"></span>
                <div class="search-field">
                    <Icon name="search" size=13 />
                    <input placeholder=tr("Filtrer les services", "Filter services") prop:value=move || query.get() on:input=move |e| query.set(event_target_value(&e)) />
                </div>
                <div class="hl-zoom">
                    <button class="icon-btn" title=tr("Zoom arrière", "Zoom out") on:click=move |_| set_zoom(zoom.get_untracked() - 0.1)>"−"</button>
                    <button class="hl-zoom-v" title=tr("Taille réelle", "Actual size") on:click=move |_| zoom.set(1.0)>{move || format!("{:.0} %", zoom.get() * 100.0)}</button>
                    <button class="icon-btn" title=tr("Zoom avant", "Zoom in") on:click=move |_| set_zoom(zoom.get_untracked() + 0.1)>"+"</button>
                </div>
                <button class="icon-btn" class:spin=move || loading.get() title=tr("Actualiser", "Refresh") on:click=move |_| refresh()>
                    <Icon name="refresh" size=15 />
                </button>
            </header>
            <div class="hl-body">
                <div
                    class="hl-scroll"
                    node_ref=scroller
                    class:panning=move || drag.with_value(|d| d.is_some())
                    on:mousedown=on_down
                    on:mousemove=on_move
                    on:mouseup=on_up
                    on:mouseleave=on_up
                    on:wheel=on_wheel
                >
                    {move || {
                        if snap.with(|s| s.is_none()) {
                            return view! { <div class="hl-loading"><span class="spinner"></span>{tr("Lecture du serveur…", "Reading the server…")}</div> }.into_any();
                        }
                        let (host_name, date) = snap.with(|s| s.as_ref().map(|s| (s.hostname.clone(), s.date.clone())).unwrap_or_default());
                        view! {
                            <div class="hl-board" style=move || format!("zoom:{}", zoom.get())>
                                <div class="hl-title">
                                    <input class="hl-title-input" prop:value=move || title.get() on:input=move |e| title.set(event_target_value(&e)) spellcheck="false" />
                                    <span class="grow"></span>
                                    <div class="hl-author">
                                        <b>{host_name.clone()}</b>
                                        <div>{date}</div>
                                    </div>
                                </div>
                                <div class="hl-grid">
                                    <div class="hl-left">
                                        {networks_panel}
                                        {ports_panel}
                                        {routes_panel}
                                        {legend_panel}
                                    </div>
                                    <div class="hl-server">
                                        <div class="hl-tab">
                                            <b>{host_name}</b>
                                            " · "
                                            <span>{tr("serveur", "server")}</span>
                                        </div>
                                        <div class="hl-server-body">
                                            <div class="hl-hwcol">{hardware}</div>
                                            <div class="hl-storage">{storage}</div>
                                            <div class="hl-services">{services}</div>
                                        </div>
                                    </div>
                                </div>
                            </div>
                        }
                        .into_any()
                    }}
                </div>
                {inspector}
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sshos_shared::{HomelabDisk, HomelabFs, HomelabMount, HomelabNetwork, HomelabPort, HomelabProxy};

    fn snap() -> HomelabSnapshot {
        HomelabSnapshot {
            disks: vec![HomelabDisk {
                name: "sda".into(),
                filesystems: vec![HomelabFs { mount: "/srv".into(), containers: vec!["jellyfin".into()], ..Default::default() }],
                ..Default::default()
            }],
            networks: vec![HomelabNetwork { name: "media".into(), ..Default::default() }],
            containers: vec![
                HomelabContainer {
                    name: "jellyfin".into(),
                    image: "jellyfin/jellyfin:latest".into(),
                    networks: vec![("media".into(), "172.20.0.2".into())],
                    mounts: vec![HomelabMount { fs: "/srv".into(), ..Default::default() }],
                    ..Default::default()
                },
                HomelabContainer { name: "db".into(), image: "postgres:16".into(), ..Default::default() },
            ],
            proxies: vec![HomelabProxy { container: Some("jellyfin".into()), ..Default::default() }],
            open_ports: vec![HomelabPort { port: 8096, proto: "tcp".into(), service: "jellyfin".into() }],
            ..Default::default()
        }
    }

    #[test]
    fn container_lights_its_links() {
        let l = lit(&snap(), &Focus::Ct("jellyfin".into()));
        assert!(l.fss.contains("/srv") && l.nets.contains("media") && l.proxies.contains(&0));
        assert!(l.ports.contains(&(8096, "tcp".into())));
        assert!(!l.cts.contains("db"));
    }

    #[test]
    fn disk_lights_its_containers() {
        let l = lit(&snap(), &Focus::Fs("/srv".into()));
        assert_eq!(l.cts, HashSet::from(["jellyfin".to_string()]));
    }

    #[test]
    fn categories_and_chips() {
        let s = snap();
        assert_eq!(category(&s.containers[0]), "Médias et photos");
        assert_eq!(category(&s.containers[1]), "Bases de données");
        assert_eq!(chip(&s.containers[1]).0, "PO");
        assert_eq!(last_octet("172.20.0.2"), ".2");
    }
}
