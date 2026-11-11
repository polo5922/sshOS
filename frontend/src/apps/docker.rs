//! Docker: containers grouped by compose project, images, volumes, live CPU/RAM.
//! Every action shows the exact docker command before it runs.

use std::collections::{BTreeMap, HashSet};

use leptos::html::Div;
use wasm_bindgen::JsCast;
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::json;
use sshos_shared::{DockerAction, DockerContainer, DockerOverview, DockerRequest, DockerStat, DockerVisitors, ErrorPayload};

use crate::icons::Icon;
use crate::state::*;
use crate::tauri::invoke;

#[derive(Clone, Copy, PartialEq)]
enum View {
    All,
    Running,
    Restarting,
    Stopped,
    Images,
    UnusedImages,
    Volumes,
    UnusedVolumes,
}

impl View {
    fn containers(self) -> bool {
        matches!(self, View::All | View::Running | View::Restarting | View::Stopped)
    }
    fn keep(self, c: &DockerContainer) -> bool {
        match self {
            View::Running => c.state == "running",
            View::Restarting => c.state == "restarting",
            View::Stopped => !matches!(c.state.as_str(), "running" | "restarting" | "paused"),
            _ => true,
        }
    }
    fn title(self) -> &'static str {
        match self {
            View::All => tr("Tous les conteneurs", "All containers"),
            View::Running => tr("En cours", "Running"),
            View::Restarting => tr("Boucle de redémarrage", "Restart loop"),
            View::Stopped => tr("Arrêtés", "Stopped"),
            View::Images => "Images",
            View::UnusedImages => tr("Images inutilisées", "Unused images"),
            View::Volumes => "Volumes",
            View::UnusedVolumes => tr("Volumes inutilisés", "Unused volumes"),
        }
    }
}

#[derive(Clone)]
enum Menu {
    Container(DockerContainer),
    Project(String, Vec<String>),
}

#[derive(Clone)]
enum Sheet {
    None,
    Text { title: String, body: Option<String>, tail: bool },
}

#[derive(Clone, Copy)]
struct D {
    c: Ctx,
    host: StoredValue<String>,
    data: RwSignal<Option<DockerOverview>>,
    stats: RwSignal<Vec<DockerStat>>,
    /// Current visitors of the containers that serve the web.
    visitors: RwSignal<Vec<DockerVisitors>>,
    error: RwSignal<Option<String>>,
    fix: RwSignal<Option<String>>,
    view: RwSignal<View>,
    search: RwSignal<String>,
    expanded: RwSignal<HashSet<String>>,
    menu: RwSignal<Option<(f64, f64, Menu)>>,
    sheet: RwSignal<Sheet>,
    /// Embedded `docker exec` shells: (tab id, container name), the open tab and the panel height.
    shells: RwSignal<Vec<(u32, String)>>,
    shell_tab: RwSignal<Option<u32>>,
    shell_h: RwSignal<f64>,
    busy: StoredValue<bool>,
    root: NodeRef<Div>,
}

fn stat_of<'a>(stats: &'a [DockerStat], id: &str) -> Option<&'a DockerStat> {
    if id.is_empty() {
        return None;
    }
    stats.iter().find(|s| s.id.starts_with(id) || id.starts_with(&s.id))
}

/// Visitors cell: distinct client IPs right now; "—" for containers that do not serve the web.
fn visitors_cell(v: Option<&DockerVisitors>) -> AnyView {
    let Some(v) = v else { return view! { <span class="muted">"—"</span> }.into_any() };
    let mut tip = format!(
        "{} {} · {} {} · {} {}",
        v.visitors,
        tr("visiteurs (adresses IP distinctes)", "visitors (distinct IP addresses)"),
        v.direct,
        tr("connectés directement", "connected directly"),
        v.proxied,
        tr("via Nginx Proxy Manager (5 dernières minutes)", "through Nginx Proxy Manager (last 5 minutes)"),
    );
    if !v.ports.is_empty() {
        tip += &format!("\n{} {}", tr("Ports web :", "Web ports:"), v.ports.iter().map(u16::to_string).collect::<Vec<_>>().join(", "));
    }
    if !v.sites.is_empty() {
        tip += &format!("\n{} {}", tr("Sites :", "Sites:"), v.sites.join(", "));
    }
    view! {
        <span class="visitors" class:active={v.visitors > 0} title=tip>
            <Icon name="users" size=12 />
            {v.visitors}
        </span>
    }
    .into_any()
}

/// Published TCP ports as (host, container): "0.0.0.0:8080->80/tcp, [::]:8080->80/tcp" -> [(8080, 80)].
pub fn published_ports(p: &str) -> Vec<(u16, u16)> {
    let mut out = Vec::new();
    for part in p.split(',').map(str::trim) {
        let Some((h, c)) = part.split_once("->") else { continue };
        if c.ends_with("/udp") {
            continue;
        }
        let hp = h.rsplit(':').next().unwrap_or(h).split('-').next().unwrap_or("");
        let cp = c.trim_end_matches("/tcp").split('-').next().unwrap_or("");
        if let (Ok(hp), Ok(cp)) = (hp.parse(), cp.parse()) {
            if !out.contains(&(hp, cp)) {
                out.push((hp, cp));
            }
        }
    }
    out
}

/// From `docker inspect` JSON: the container's first network IP and its first exposed TCP port.
pub fn inspect_target(json_text: &str) -> Option<(String, u16)> {
    let v: serde_json::Value = serde_json::from_str(json_text).ok()?;
    let o = v.get(0).unwrap_or(&v);
    let ip = o["NetworkSettings"]["Networks"]
        .as_object()?
        .values()
        .filter_map(|n| n["IPAddress"].as_str())
        .find(|ip| !ip.is_empty())?
        .to_string();
    let mut ports: Vec<u16> = o["Config"]["ExposedPorts"]
        .as_object()
        .map(|m| m.keys().filter(|k| k.ends_with("/tcp")).filter_map(|k| k.trim_end_matches("/tcp").parse().ok()).collect())
        .unwrap_or_default();
    ports.sort();
    // Prefer the usual web ports when a container exposes several.
    let port = [80, 8080, 3000, 8000, 443].into_iter().find(|p| ports.contains(p)).or(ports.first().copied())?;
    Some((ip, port))
}

/// "0.0.0.0:8080->80/tcp, [::]:8080->80/tcp" -> [("8080→80", 8080)]: label and host port.
fn port_labels(p: &str) -> Vec<(String, u16)> {
    let mut out: Vec<(String, u16)> = Vec::new();
    for (hp, cp) in published_ports(p) {
        let s = if hp == cp { hp.to_string() } else { format!("{hp}→{cp}") };
        if !out.iter().any(|(l, _)| *l == s) {
            out.push((s, hp));
        }
    }
    out
}

/// Published ports; on a running container each one is a button opening it in the Browser.
fn port_cell(d: D, ports: &str, running: bool) -> AnyView {
    let labels = port_labels(ports);
    if labels.is_empty() {
        return view! { <span class="muted">"—"</span> }.into_any();
    }
    let more = labels.len().saturating_sub(3);
    view! {
        <div class="port-links">
            {labels
                .into_iter()
                .take(3)
                .map(|(label, hp)| {
                    if running {
                        view! {
                            <button
                                class="port-link"
                                title=format!("{} localhost:{hp}", tr("Ouvrir dans le navigateur :", "Open in the browser:"))
                                on:click=move |e| {
                                    e.stop_propagation();
                                    d.open_port(hp);
                                }
                            >
                                <Icon name="compass" size=11 />
                                {label}
                            </button>
                        }
                            .into_any()
                    } else {
                        view! { <span class="port-off">{label}</span> }.into_any()
                    }
                })
                .collect_view()}
            {(more > 0).then(|| view! { <span class="muted">{format!("+{more}")}</span> })}
        </div>
    }
    .into_any()
}

fn state_class(state: &str) -> &'static str {
    match state {
        "running" => "ok",
        "restarting" => "warn",
        "paused" => "info",
        _ => "off",
    }
}

fn state_word(state: &str) -> String {
    match state {
        "running" => tr("En cours", "Running").into(),
        "restarting" => tr("Redémarrage", "Restarting").into(),
        "paused" => tr("En pause", "Paused").into(),
        "exited" => tr("Arrêté", "Exited").into(),
        "created" => tr("Créé", "Created").into(),
        "dead" => tr("Mort", "Dead").into(),
        other => other.to_string(),
    }
}

fn action_word(a: DockerAction) -> &'static str {
    match a {
        DockerAction::Start => tr("Démarrer", "Start"),
        DockerAction::Stop => tr("Arrêter", "Stop"),
        DockerAction::Restart => tr("Redémarrer", "Restart"),
        DockerAction::Pause => "Pause",
        DockerAction::Unpause => tr("Reprendre", "Resume"),
        DockerAction::Remove | DockerAction::RemoveImage | DockerAction::RemoveVolume => tr("Supprimer", "Remove"),
    }
}

impl D {
    fn h(&self) -> String {
        self.host.get_value()
    }

    fn reload(self) {
        if self.busy.get_value() {
            return;
        }
        self.busy.set_value(true);
        let h = self.h();
        spawn_local(async move {
            match invoke::<DockerOverview>("docker_overview", json!({ "hostId": h })).await {
                Ok(o) => {
                    let _ = self.data.try_set(Some(o));
                    let _ = self.error.try_set(None);
                    let _ = self.fix.try_set(None);
                }
                Err(ErrorPayload::NeedsTerminal { command }) => {
                    let _ = self.error.try_set(Some(
                        tr(
                            "Votre utilisateur n'a pas accès à Docker. Ajoutez-le au groupe docker puis reconnectez-vous.",
                            "Your user cannot access Docker. Add it to the docker group, then reconnect.",
                        )
                        .to_string(),
                    ));
                    let _ = self.fix.try_set(Some(command));
                }
                Err(e) => {
                    let _ = self.error.try_set(Some(e.message()));
                }
            }
            if let Ok(v) = invoke::<Vec<DockerVisitors>>("docker_visitors", json!({ "hostId": h })).await {
                let _ = self.visitors.try_set(v);
            }
            if let Ok(s) = invoke::<Vec<DockerStat>>("docker_stats", json!({ "hostId": h })).await {
                let _ = self.stats.try_set(s);
            }
            let _ = self.busy.try_set_value(false);
        });
    }

    /// Preview the command, confirm, run, reload.
    fn act(self, action: DockerAction, targets: Vec<String>, what: String) {
        let c = self.c;
        self.menu.set(None);
        let req = DockerRequest { action, targets };
        spawn_local(async move {
            let cmd = match invoke::<String>("docker_preview", json!({ "req": req })).await {
                Ok(cmd) => cmd,
                Err(e) => return c.error(e.message()),
            };
            let danger = matches!(action, DockerAction::Remove | DockerAction::RemoveImage | DockerAction::RemoveVolume);
            let word = action_word(action);
            let d = Dialog::confirm(format!("{word} · {what}"), tr("Commande exécutée sur le serveur :", "Command run on the server:"), word, move || {
                let (h, req, what) = (self.h(), req.clone(), what.clone());
                spawn_local(async move {
                    match invoke::<String>("docker_run", json!({ "hostId": h, "req": req })).await {
                        Ok(_) => {
                            c.ok(format!("{word} · {what}"));
                            self.reload();
                        }
                        Err(e) => c.handle_err(Some(&h), e),
                    }
                });
            })
            .code(cmd);
            c.show(if danger { d.danger() } else { d });
        });
    }

    fn text_sheet(self, title: String, cmd: &'static str, id: String) {
        self.menu.set(None);
        let tail = cmd == "docker_logs";
        self.sheet.set(Sheet::Text { title: title.clone(), body: None, tail });
        let h = self.h();
        spawn_local(async move {
            let args = if cmd == "docker_logs" {
                json!({ "hostId": h, "id": id, "tail": 500 })
            } else {
                json!({ "hostId": h, "id": id })
            };
            let body = match invoke::<String>(cmd, args).await {
                Ok(t) if t.trim().is_empty() => tr("(vide)", "(empty)").to_string(),
                Ok(t) => t,
                Err(e) => e.message(),
            };
            let _ = self.sheet.try_set(Sheet::Text { title, body: Some(body), tail });
        });
    }

    /// Open (or show) a shell on the container in the panel at the bottom of the app.
    fn shell(self, name: String) {
        self.menu.set(None);
        if let Some(id) = self.shells.with_untracked(|v| v.iter().find(|(_, n)| *n == name).map(|(i, _)| *i)) {
            self.shell_tab.set(Some(id));
            return;
        }
        let id = self.shells.with_untracked(|v| v.iter().map(|(i, _)| *i).max().unwrap_or(0) + 1);
        self.shells.update(|v| v.push((id, name)));
        self.shell_tab.set(Some(id));
    }

    /// Opens a published port of the server in the built-in Browser.
    fn open_port(self, port: u16) {
        self.c.open(AppKind::Browser { url: Some(format!("localhost:{port}")) }, Some(self.h()));
    }

    /// Opens the container's web page in the Browser: published port on the host, else the
    /// container's own IP (reachable from the host) and its exposed port.
    fn open_browser(self, ct: DockerContainer) {
        self.menu.set(None);
        let (c, h) = (self.c, self.h());
        if let Some((hp, _)) = published_ports(&ct.ports).first() {
            return c.open(AppKind::Browser { url: Some(format!("localhost:{hp}")) }, Some(h));
        }
        spawn_local(async move {
            match invoke::<String>("docker_inspect", json!({ "hostId": h, "id": ct.id })).await {
                Ok(t) => match inspect_target(&t) {
                    Some((ip, port)) => {
                        let host = if ip.contains(':') { format!("[{ip}]") } else { ip };
                        c.open(AppKind::Browser { url: Some(format!("{host}:{port}")) }, Some(h))
                    }
                    None => c.error(tr(
                        "Ce conteneur ne publie ni n'expose de port TCP joignable.",
                        "This container neither publishes nor exposes a reachable TCP port.",
                    )),
                },
                Err(e) => c.handle_err(Some(&h), e),
            }
        });
    }

    fn close_shell(self, id: u32) {
        self.shells.update(|v| v.retain(|(i, _)| *i != id));
        if self.shell_tab.get_untracked() == Some(id) {
            self.shell_tab.set(self.shells.with_untracked(|v| v.last().map(|(i, _)| *i)));
        }
    }

    fn open_menu(self, e: &leptos::ev::MouseEvent, m: Menu) {
        e.stop_propagation();
        let Some(root) = self.root.get_untracked() else { return };
        let r = root.get_bounding_client_rect();
        let x = (e.client_x() as f64 - r.left() - 180.0).clamp(4.0, (r.width() - 200.0).max(4.0));
        let y = (e.client_y() as f64 - r.top() + 6.0).min((r.height() - 230.0).max(4.0));
        self.menu.set(Some((x, y, m)));
    }
}

#[component]
pub fn Docker(host: String) -> impl IntoView {
    let c = ctx();
    let d = D {
        c,
        host: StoredValue::new(host),
        data: RwSignal::new(None),
        stats: RwSignal::new(Vec::new()),
        visitors: RwSignal::new(Vec::new()),
        error: RwSignal::new(None),
        fix: RwSignal::new(None),
        view: RwSignal::new(View::All),
        search: RwSignal::new(String::new()),
        expanded: RwSignal::new(HashSet::new()),
        menu: RwSignal::new(None),
        sheet: RwSignal::new(Sheet::None),
        shells: RwSignal::new(Vec::new()),
        shell_tab: RwSignal::new(None),
        shell_h: RwSignal::new(300.0),
        busy: StoredValue::new(false),
        root: NodeRef::new(),
    };
    d.reload();
    if let Ok(h) = set_interval_with_handle(move || d.reload(), std::time::Duration::from_secs(10)) {
        on_cleanup(move || h.clear());
    }

    let count = move |f: fn(&DockerOverview) -> usize| move || d.data.with(|o| o.as_ref().map(f).unwrap_or(0));
    let side_item = move |v: View, icon: &'static str, label: &'static str, n: Box<dyn Fn() -> usize + Send + Sync>| {
        view! {
            <button class="side-item" class:active=move || d.view.get() == v on:click=move |_| d.view.set(v)>
                <Icon name=icon size=15 />
                <span class="grow">{label}</span>
                <span class="side-count">{move || n()}</span>
            </button>
        }
    };

    let sidebar = view! {
        <aside class="side drag-region">
            <div class="side-top"></div>
            <div class="side-title">{tr("Conteneurs", "Containers")}</div>
            {side_item(View::All, "grid", tr("Tous", "All"), Box::new(count(|o| o.containers.len())))}
            {side_item(View::Running, "play", tr("En cours", "Running"), Box::new(count(|o| o.containers.iter().filter(|c| c.state == "running").count())))}
            {side_item(View::Restarting, "rotate", tr("Redémarrage en boucle", "Restart loop"), Box::new(count(|o| o.containers.iter().filter(|c| c.state == "restarting").count())))}
            {side_item(View::Stopped, "stop", tr("Arrêtés", "Stopped"), Box::new(count(|o| o.containers.iter().filter(|c| View::Stopped.keep(c)).count())))}
            <div class="side-title">"Images"</div>
            {side_item(View::Images, "layers", tr("Toutes", "All"), Box::new(count(|o| o.images.len())))}
            {side_item(View::UnusedImages, "image", tr("Inutilisées", "Unused"), Box::new(count(|o| o.images.iter().filter(|i| !i.in_use).count())))}
            <div class="side-title">"Volumes"</div>
            {side_item(View::Volumes, "database", tr("Tous", "All"), Box::new(count(|o| o.volumes.len())))}
            {side_item(View::UnusedVolumes, "hdd", tr("Inutilisés", "Unused"), Box::new(count(|o| o.volumes.iter().filter(|v| !v.in_use).count())))}
            <div class="grow"></div>
            <div class="side-foot muted small">{move || d.data.with(|o| o.as_ref().map(|o| format!("Docker {}", o.version)).unwrap_or_default())}</div>
        </aside>
    };

    let cards = move || {
        let (cpu, mem, running, total, projects) = d.data.with(|o| {
            let Some(o) = o else { return (0.0, 0u64, 0, 0, 0) };
            let st = d.stats.get();
            let mut cpu = 0.0f32;
            let mut mem = 0u64;
            for ct in &o.containers {
                if let Some(s) = stat_of(&st, &ct.id) {
                    cpu += s.cpu;
                    mem += s.mem;
                }
            }
            let projects: HashSet<_> = o.containers.iter().filter_map(|c| c.project.as_deref()).collect();
            (cpu, mem, o.containers.iter().filter(|c| c.state == "running").count(), o.containers.len(), projects.len())
        });
        let host = d.h();
        let live = c.live.with(|l| l.as_ref().filter(|(h, _)| *h == host).map(|(_, s)| (s.cores.max(1), s.mem_total)));
        let (cpu_pct, cpu_sub) = match live {
            Some((cores, _)) => (cpu / cores as f32, format!("{cpu:.0}% / {cores} {}", tr("cœurs", "cores"))),
            None => (cpu, String::new()),
        };
        let (mem_pct, mem_sub) = match live {
            Some((_, total)) if total > 0 => (mem as f32 * 100.0 / total as f32, format!("{} / {}", human_bytes(mem as f64), human_bytes(total as f64))),
            _ => (0.0, human_bytes(mem as f64)),
        };
        let gauge = |pct: f32| {
            let p = pct.clamp(0.0, 100.0);
            view! {
                <div class="card-bar" class:warn={p > 70.0} class:crit={p > 90.0}>
                    <span style=format!("width:{p:.1}%")></span>
                </div>
            }
        };
        view! {
            <div class="cards">
                <div class="card">
                    <div class="card-label"><Icon name="cpu" size=13 />{tr("CPU des conteneurs", "Containers CPU")}</div>
                    <div class="card-value">{format!("{cpu_pct:.1}%")}</div>
                    {gauge(cpu_pct)}
                    <div class="card-sub">{cpu_sub}</div>
                </div>
                <div class="card">
                    <div class="card-label"><Icon name="database" size=13 />{tr("RAM des conteneurs", "Containers RAM")}</div>
                    <div class="card-value">{human_bytes(mem as f64)}</div>
                    {gauge(mem_pct)}
                    <div class="card-sub">{mem_sub}</div>
                </div>
                <div class="card">
                    <div class="card-label"><Icon name="play" size=13 />{tr("En cours", "Running")}</div>
                    <div class="card-value">
                        {running}<span class="card-of">{format!(" / {total}")}</span>
                    </div>
                    {gauge(if total > 0 { running as f32 * 100.0 / total as f32 } else { 0.0 })}
                    <div class="card-sub">{format!("{projects} {}", tr("projets compose", "compose projects"))}</div>
                </div>
            </div>
        }
    };

    let container_row = move |ct: DockerContainer, nested: bool| {
        let st = d.stats.with(|s| stat_of(s, &ct.id).cloned());
        let vis = d.visitors.with(|v| v.iter().find(|v| !ct.id.is_empty() && (v.id.starts_with(&ct.id) || ct.id.starts_with(&v.id))).cloned());
        let running = ct.state == "running";
        let (ct1, ct2) = (ct.clone(), ct.clone());
        let label = if nested { ct.service.clone().unwrap_or_else(|| ct.name.clone()) } else { ct.name.clone() };
        let short_id: String = ct.id.chars().take(12).collect();
        view! {
            <tr class:nested=nested>
                <td class="name-cell"><div class="cell-flex">
                    <span class=format!("state-dot {}", state_class(&ct.state))></span>
                    <span class="ellipsis" title=ct.name.clone()>{label}</span>
                </div></td>
                <td class="mono muted">{short_id}</td>
                <td class="ellipsis image-cell" title=ct.image.clone()>{ct.image.clone()}</td>
                <td class="mono small ports-cell" title=ct.ports.clone()>{port_cell(d, &ct.ports, running)}</td>
                <td class="num">{if running { visitors_cell(vis.as_ref()) } else { visitors_cell(None) }}</td>
                <td class="num">{st.as_ref().map(|s| format!("{:.1}%", s.cpu)).unwrap_or_else(|| "—".into())}</td>
                <td class="num">{st.as_ref().map(|s| human_bytes(s.mem as f64)).unwrap_or_else(|| "—".into())}</td>
                <td>
                    <span class=format!("pill {}", state_class(&ct.state)) title=ct.status.clone()>{state_word(&ct.state)}</span>
                </td>
                <td class="row-actions">
                    {if running {
                        view! {
                            <button class="icon-btn" title=tr("Arrêter", "Stop") on:click=move |_| d.act(DockerAction::Stop, vec![ct1.name.clone()], ct1.name.clone())>
                                <Icon name="stop" size=14 />
                            </button>
                        }
                            .into_any()
                    } else {
                        view! {
                            <button class="icon-btn" title=tr("Démarrer", "Start") on:click=move |_| d.act(DockerAction::Start, vec![ct1.name.clone()], ct1.name.clone())>
                                <Icon name="play" size=14 />
                            </button>
                        }
                            .into_any()
                    }}
                    <button class="icon-btn" title=tr("Redémarrer", "Restart") on:click={
                        let n = ct.name.clone();
                        move |_| d.act(DockerAction::Restart, vec![n.clone()], n.clone())
                    }>
                        <Icon name="rotate" size=14 />
                    </button>
                    <button class="icon-btn" title=tr("Plus", "More") on:click=move |e| d.open_menu(&e, Menu::Container(ct2.clone()))>
                        <Icon name="more" size=14 />
                    </button>
                </td>
            </tr>
        }
    };

    let containers_table = move || {
        let v = d.view.get();
        let q = d.search.get().to_lowercase();
        let list: Vec<DockerContainer> = d.data.with(|o| {
            o.as_ref()
                .map(|o| {
                    o.containers
                        .iter()
                        .filter(|c| v.keep(c))
                        .filter(|c| {
                            q.is_empty()
                                || c.name.to_lowercase().contains(&q)
                                || c.image.to_lowercase().contains(&q)
                                || c.project.as_deref().unwrap_or("").to_lowercase().contains(&q)
                        })
                        .cloned()
                        .collect()
                })
                .unwrap_or_default()
        });
        let mut groups: BTreeMap<String, Vec<DockerContainer>> = BTreeMap::new();
        let mut single = Vec::new();
        for ct in list {
            match ct.project.clone() {
                Some(p) => groups.entry(p).or_default().push(ct),
                None => single.push(ct),
            }
        }
        let empty = groups.is_empty() && single.is_empty();
        let stats = d.stats.get();
        let visitors = d.visitors.get();
        let rows = groups
            .into_iter()
            .map(|(p, cts)| {
                let open = !q.is_empty() || d.expanded.with(|e| e.contains(&p));
                let run = cts.iter().filter(|c| c.state == "running").count();
                let cpu: f32 = cts.iter().filter_map(|c| stat_of(&stats, &c.id)).map(|s| s.cpu).sum();
                let mem: u64 = cts.iter().filter_map(|c| stat_of(&stats, &c.id)).map(|s| s.mem).sum();
                // Sum over the project's web services ("—" when none serves the web).
                let group_vis = cts
                    .iter()
                    .filter(|c| c.state == "running" && !c.id.is_empty())
                    .filter_map(|c| visitors.iter().find(|v| v.id.starts_with(&c.id) || c.id.starts_with(&v.id)))
                    .fold(None::<DockerVisitors>, |acc, v| {
                        let mut a = acc.unwrap_or_default();
                        a.visitors += v.visitors;
                        a.direct += v.direct;
                        a.proxied += v.proxied;
                        a.ports.extend(&v.ports);
                        a.sites.extend(v.sites.iter().cloned());
                        Some(a)
                    });
                let (cls, word) = if run == cts.len() {
                    ("ok", tr("En cours", "Running"))
                } else if run == 0 {
                    ("off", tr("Arrêté", "Stopped"))
                } else {
                    ("warn", tr("Partiel", "Partial"))
                };
                let names: Vec<String> = cts.iter().map(|c| c.name.clone()).collect();
                let (p1, p2) = (p.clone(), p.clone());
                let total = cts.len();
                view! {
                    <tr class="group-row" on:click=move |_| d.expanded.update(|e| { if !e.remove(&p1) { e.insert(p1.clone()); } })>
                        <td class="name-cell"><div class="cell-flex">
                            <span class="chev">
                                {if open { view! { <Icon name="chev-down" size=13 /> } } else { view! { <Icon name="chev-right" size=13 /> } }}
                            </span>
                            <Icon name="layers" size=14 />
                            <span class="ellipsis strong">{p.clone()}</span>
                        </div></td>
                        <td colspan="3" class="muted small">{format!("compose · {run}/{total} {}", tr("en cours", "running"))}</td>
                        <td class="num">{visitors_cell(group_vis.as_ref())}</td>
                        <td class="num">{format!("{cpu:.1}%")}</td>
                        <td class="num">{human_bytes(mem as f64)}</td>
                        <td><span class=format!("pill {cls}")>{word}</span></td>
                        <td class="row-actions">
                            <button class="icon-btn" title=tr("Plus", "More") on:click=move |e| d.open_menu(&e, Menu::Project(p2.clone(), names.clone()))>
                                <Icon name="more" size=14 />
                            </button>
                        </td>
                    </tr>
                    {open.then(|| cts.into_iter().map(|ct| container_row(ct, true)).collect_view())}
                }
            })
            .collect_view();
        view! {
            <table class="table data-table">
                <thead>
                    <tr>
                        <th>{tr("Nom", "Name")}</th>
                        <th>"ID"</th>
                        <th>"Image"</th>
                        <th>"Ports"</th>
                        <th class="num" title=tr("Visiteurs actuels des services web : adresses IP distinctes connectées, ou vues par Nginx Proxy Manager ces 5 dernières minutes", "Current visitors of web services: distinct IP addresses connected, or seen by Nginx Proxy Manager in the last 5 minutes")>{tr("Visiteurs", "Visitors")}</th>
                        <th class="num">"CPU"</th>
                        <th class="num">"RAM"</th>
                        <th>{tr("État", "Status")}</th>
                        <th class="right">"Actions"</th>
                    </tr>
                </thead>
                <tbody>
                    {rows}
                    {single.into_iter().map(|ct| container_row(ct, false)).collect_view()}
                </tbody>
            </table>
            {empty.then(|| view! { <div class="empty">{tr("Aucun conteneur", "No containers")}</div> })}
        }
    };

    let images_table = move || {
        let unused_only = d.view.get() == View::UnusedImages;
        let q = d.search.get().to_lowercase();
        let list: Vec<_> = d.data.with(|o| {
            o.as_ref()
                .map(|o| {
                    o.images
                        .iter()
                        .filter(|i| !unused_only || !i.in_use)
                        .filter(|i| q.is_empty() || i.repository.to_lowercase().contains(&q) || i.tag.to_lowercase().contains(&q))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default()
        });
        view! {
            <table class="table data-table">
                <thead>
                    <tr>
                        <th>{tr("Dépôt", "Repository")}</th>
                        <th>"Tag"</th>
                        <th>"ID"</th>
                        <th class="num">{tr("Taille", "Size")}</th>
                        <th>{tr("Créée", "Created")}</th>
                        <th>{tr("État", "Status")}</th>
                        <th class="right">"Actions"</th>
                    </tr>
                </thead>
                <tbody>
                    {list
                        .into_iter()
                        .map(|i| {
                            let id = i.id.trim_start_matches("sha256:").chars().take(12).collect::<String>();
                            let name = format!("{}:{}", i.repository, i.tag);
                            let target = if i.repository == "<none>" { id.clone() } else { name.clone() };
                            view! {
                                <tr>
                                    <td class="name-cell"><div class="cell-flex"><Icon name="image" size=14 /><span class="ellipsis">{i.repository.clone()}</span></div></td>
                                    <td class="mono small">{i.tag.clone()}</td>
                                    <td class="mono muted">{id}</td>
                                    <td class="num">{i.size.clone()}</td>
                                    <td class="muted small">{i.created.clone()}</td>
                                    <td>
                                        {if i.in_use {
                                            view! { <span class="pill ok">{tr("Utilisée", "In use")}</span> }
                                        } else {
                                            view! { <span class="pill off">{tr("Inutilisée", "Unused")}</span> }
                                        }}
                                    </td>
                                    <td class="row-actions">
                                        <button class="icon-btn danger" title=tr("Supprimer", "Remove") on:click=move |_| d.act(DockerAction::RemoveImage, vec![target.clone()], name.clone())>
                                            <Icon name="trash" size=14 />
                                        </button>
                                    </td>
                                </tr>
                            }
                        })
                        .collect_view()}
                </tbody>
            </table>
        }
    };

    let volumes_table = move || {
        let unused_only = d.view.get() == View::UnusedVolumes;
        let q = d.search.get().to_lowercase();
        let list: Vec<_> = d.data.with(|o| {
            o.as_ref()
                .map(|o| {
                    o.volumes
                        .iter()
                        .filter(|v| !unused_only || !v.in_use)
                        .filter(|v| q.is_empty() || v.name.to_lowercase().contains(&q))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default()
        });
        view! {
            <table class="table data-table">
                <thead>
                    <tr>
                        <th>{tr("Nom", "Name")}</th>
                        <th>"Driver"</th>
                        <th>{tr("État", "Status")}</th>
                        <th class="right">"Actions"</th>
                    </tr>
                </thead>
                <tbody>
                    {list
                        .into_iter()
                        .map(|v| {
                            let (n1, n2) = (v.name.clone(), v.name.clone());
                            view! {
                                <tr>
                                    <td class="name-cell"><div class="cell-flex"><Icon name="database" size=14 /><span class="ellipsis mono small" title=v.name.clone()>{v.name.clone()}</span></div></td>
                                    <td class="muted">{v.driver.clone()}</td>
                                    <td>
                                        {if v.in_use {
                                            view! { <span class="pill ok">{tr("Utilisé", "In use")}</span> }
                                        } else {
                                            view! { <span class="pill off">{tr("Inutilisé", "Unused")}</span> }
                                        }}
                                    </td>
                                    <td class="row-actions">
                                        <button class="icon-btn danger" title=tr("Supprimer", "Remove") on:click=move |_| d.act(DockerAction::RemoveVolume, vec![n1.clone()], n2.clone())>
                                            <Icon name="trash" size=14 />
                                        </button>
                                    </td>
                                </tr>
                            }
                        })
                        .collect_view()}
                </tbody>
            </table>
        }
    };

    let menu = move || {
        d.menu.get().map(|(x, y, m)| {
            let items = match m {
                Menu::Container(ct) => {
                    let paused = ct.state == "paused";
                    let (a, b, e, f, g) = (ct.clone(), ct.clone(), ct.clone(), ct.clone(), ct.clone());
                    let web = ct.clone();
                    let running = ct.state == "running";
                    view! {
                        <button on:click=move |_| d.text_sheet(format!("{} · logs", a.name), "docker_logs", a.id.clone())>
                            <Icon name="logs" size=14 />{tr("Journaux", "Logs")}
                        </button>
                        <button on:click=move |_| d.shell(b.name.clone())>
                            <Icon name="terminal" size=14 />"Shell"
                        </button>
                        <button disabled=!running on:click=move |_| d.open_browser(web.clone())>
                            <Icon name="compass" size=14 />{tr("Ouvrir dans le navigateur", "Open in browser")}
                        </button>
                        <button on:click=move |_| d.text_sheet(format!("{} · inspect", e.name), "docker_inspect", e.id.clone())>
                            <Icon name="eye" size=14 />{tr("Inspecter", "Inspect")}
                        </button>
                        <button on:click=move |_| {
                            let act = if paused { DockerAction::Unpause } else { DockerAction::Pause };
                            d.act(act, vec![f.name.clone()], f.name.clone())
                        }>
                            <Icon name=if paused { "play" } else { "pause" } size=14 />
                            {if paused { tr("Reprendre", "Resume") } else { "Pause" }}
                        </button>
                        <div class="menu-sep"></div>
                        <button class="danger" on:click=move |_| d.act(DockerAction::Remove, vec![g.name.clone()], g.name.clone())>
                            <Icon name="trash" size=14 />{tr("Supprimer", "Remove")}
                        </button>
                    }
                        .into_any()
                }
                Menu::Project(p, names) => {
                    let (n1, n2, n3) = (names.clone(), names.clone(), names);
                    let (p1, p2, p3) = (p.clone(), p.clone(), p);
                    view! {
                        <button on:click=move |_| d.act(DockerAction::Start, n1.clone(), p1.clone())>
                            <Icon name="play" size=14 />{tr("Tout démarrer", "Start all")}
                        </button>
                        <button on:click=move |_| d.act(DockerAction::Restart, n2.clone(), p2.clone())>
                            <Icon name="rotate" size=14 />{tr("Tout redémarrer", "Restart all")}
                        </button>
                        <button on:click=move |_| d.act(DockerAction::Stop, n3.clone(), p3.clone())>
                            <Icon name="stop" size=14 />{tr("Tout arrêter", "Stop all")}
                        </button>
                    }
                        .into_any()
                }
            };
            view! {
                <div class="menu-overlay" on:click=move |_| d.menu.set(None)>
                    <div class="ctx-menu" style=format!("left:{x}px;top:{y}px") on:click=|e| e.stop_propagation()>
                        {items}
                    </div>
                </div>
            }
        })
    };

    let sheet = move || match d.sheet.get() {
        Sheet::None => None,
        Sheet::Text { title, body, tail } => Some(view! {
            <div class="sheet-overlay" on:click=move |_| d.sheet.set(Sheet::None)>
                <div class="sheet wide" on:click=|e| e.stop_propagation()>
                    <button class="icon-btn sheet-close" on:click=move |_| d.sheet.set(Sheet::None)><Icon name="x" /></button>
                    <h3>{title}</h3>
                    {match body {
                        None => view! { <div class="empty">{tr("Chargement…", "Loading…")}</div> }.into_any(),
                        Some(b) => view! { <div class="log-view" class:tail=tail><pre>{b}</pre></div> }.into_any(),
                    }}
                </div>
            </div>
        }),
    };

    app_menus(vec![
        MenuSection::new(
            "Docker",
            vec![MenuItem::new(tr("Actualiser", "Refresh"), move || d.reload())],
        ),
        MenuSection::new(
            tr("Présentation", "View"),
            vec![
                MenuItem::new(tr("Tous les conteneurs", "All containers"), move || d.view.set(View::All)).checked(move || d.view.get() == View::All),
                MenuItem::new(tr("En cours", "Running"), move || d.view.set(View::Running)).checked(move || d.view.get() == View::Running),
                MenuItem::new(tr("Arrêtés", "Stopped"), move || d.view.set(View::Stopped)).checked(move || d.view.get() == View::Stopped),
                MenuItem::new("Images", move || d.view.set(View::Images)).checked(move || d.view.get() == View::Images),
                MenuItem::new("Volumes", move || d.view.set(View::Volumes)).checked(move || d.view.get() == View::Volumes),
            ],
        ),
    ]);

    view! {
        <div class="split-app" node_ref=d.root>
            {sidebar}
            <section class="main">
                <header class="main-head drag-region">
                    <h2>{move || d.view.get().title()}</h2>
                    <div class="grow"></div>
                    <label class="search-field">
                        <Icon name="search" size=13 />
                        <input placeholder=tr("Rechercher", "Search") bind:value=d.search />
                    </label>
                    <button class="icon-btn" title=tr("Actualiser", "Refresh") on:click=move |_| d.reload()>
                        <Icon name="refresh" size=15 />
                    </button>
                </header>
                <div class="main-body">
                    {move || {
                        d.error
                            .get()
                            .map(|e| {
                                let fix = d.fix.get();
                                view! {
                                    <div class="banner warn">
                                        <Icon name="alert" size=16 />
                                        <span class="grow">{e}</span>
                                        {fix.map(|cmd| {
                                            view! {
                                                <button class="small" on:click=move |_| c.run_in_terminal(&d.h(), &cmd)>
                                                    {tr("Corriger dans le terminal", "Fix in terminal")}
                                                </button>
                                            }
                                        })}
                                    </div>
                                }
                            })
                    }}
                    <Show when=move || d.data.with(|o| o.is_none()) && d.error.with(|e| e.is_none())>
                        <div class="empty">{tr("Lecture de Docker…", "Reading Docker…")}</div>
                    </Show>
                    <Show when=move || d.data.with(|o| o.is_some())>
                        {move || {
                            let v = d.view.get();
                            if v.containers() {
                                view! { {cards} <div class="table-card">{containers_table}</div> }.into_any()
                            } else if matches!(v, View::Images | View::UnusedImages) {
                                view! { <div class="table-card">{images_table}</div> }.into_any()
                            } else {
                                view! { <div class="table-card">{volumes_table}</div> }.into_any()
                            }
                        }}
                    </Show>
                </div>
                <ShellPanel d=d />
            </section>
            {menu}
            {sheet}
        </div>
    }
}

/// `docker exec` shells docked at the bottom of the Docker app, one tab per container.
#[component]
fn ShellPanel(d: D) -> impl IntoView {
    let drag = StoredValue::new(None::<(f64, f64)>);
    let tabs = move || {
        d.shells
            .get()
            .into_iter()
            .map(|(id, name)| {
                view! {
                    <div class="shell-tab" class:active=move || d.shell_tab.get() == Some(id) on:click=move |_| d.shell_tab.set(Some(id))>
                        <Icon name="terminal" size=12 />
                        <span class="ellipsis">{name}</span>
                        <button class="icon-btn tiny" title=tr("Fermer", "Close") on:click=move |e| {
                            e.stop_propagation();
                            d.close_shell(id);
                        }>
                            <Icon name="x" size=11 />
                        </button>
                    </div>
                }
            })
            .collect_view()
    };
    // Keyed by tab id: adding or closing a tab never restarts the other sessions.
    let term = move |(id, name): (u32, String)| {
        // Typed then run: `docker exec` needs no sudo, and the command stays visible.
        let cmd = format!(
            "exec docker exec -it {} sh -c 'command -v bash >/dev/null && exec bash || exec sh'",
            crate::apps::files::shell_quote(&name)
        ) + "\r";
        view! {
            <div class="shell-term" class:active=move || d.shell_tab.get() == Some(id)>
                <crate::apps::terminal::Terminal host=d.h() input=Some(cmd) />
            </div>
        }
    };
    view! {
        <Show when=move || d.shells.with(|v| !v.is_empty())>
            <div class="shell-panel" style=move || format!("height:{}px", d.shell_h.get())>
                <div
                    class="shell-resize"
                    on:pointerdown=move |e| {
                        if let Some(t) = e.current_target().and_then(|t| t.dyn_into::<web_sys::Element>().ok()) {
                            let _ = t.set_pointer_capture(e.pointer_id());
                        }
                        drag.set_value(Some((e.client_y() as f64, d.shell_h.get_untracked())));
                    }
                    on:pointermove=move |e| {
                        if let Some((y0, h0)) = drag.get_value() {
                            d.shell_h.set((h0 - (e.client_y() as f64 - y0)).clamp(140.0, 900.0));
                        }
                    }
                    on:pointerup=move |_| drag.set_value(None)
                ></div>
                <div class="shell-tabs">
                    {tabs}
                    <div class="grow"></div>
                    <button class="icon-btn tiny" title=tr("Fermer tous les shells", "Close all shells") on:click=move |_| {
                        d.shells.set(Vec::new());
                        d.shell_tab.set(None);
                    }>
                        <Icon name="chev-down" size=13 />
                    </button>
                </div>
                <div class="shell-body">
                    <For each=move || d.shells.get() key=|(id, _)| *id children=term />
                </div>
            </div>
        </Show>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports() {
        assert_eq!(published_ports("0.0.0.0:8001->80/tcp, [::]:8001->80/tcp"), vec![(8001, 80)]);
        assert_eq!(published_ports("127.0.0.1:5432->5432/tcp, 0.0.0.0:53->53/udp"), vec![(5432, 5432)]);
        assert_eq!(published_ports("80/tcp, 443/tcp"), vec![]);
        assert_eq!(published_ports(""), vec![]);
        assert_eq!(port_labels("0.0.0.0:8001->80/tcp, [::]:8001->80/tcp"), vec![("8001→80".to_string(), 8001)]);
        assert_eq!(port_labels("0.0.0.0:9443->9443/tcp"), vec![("9443".to_string(), 9443)]);
    }

    #[test]
    fn inspect() {
        let j = r#"[{"Id":"x","Config":{"ExposedPorts":{"9000/tcp":{},"80/tcp":{}}},
            "NetworkSettings":{"Networks":{"bridge":{"IPAddress":""},"web":{"IPAddress":"172.18.0.4"}}}}]"#;
        assert_eq!(inspect_target(j), Some(("172.18.0.4".into(), 80)));
        assert_eq!(inspect_target(r#"[{"Config":{},"NetworkSettings":{"Networks":{}}}]"#), None);
    }
}
