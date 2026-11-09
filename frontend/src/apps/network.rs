//! Network: interfaces and addresses, routes, listening ports, established connections, and
//! diagnostic tools run *from the server* (ping, traceroute, DNS lookup, TCP port test).

use leptos::prelude::*;

use crate::apps::files::shell_quote;
use crate::apps::firewall::{parse_listen, Listen};
use crate::apps::kit;
use crate::icons::Icon;
use crate::state::*;

const SCRIPT: &str = r#"PATH=$PATH:/usr/sbin:/sbin
echo @@addr; ip -br addr 2>/dev/null; echo @@route; ip route 2>/dev/null; echo @@dns; grep -E '^nameserver' /etc/resolv.conf 2>/dev/null
echo @@listen; ss -ltnuHp 2>/dev/null || ss -ltnuH 2>/dev/null; echo @@conn; ss -tunH state established 2>/dev/null | head -400"#;

#[derive(Clone, Debug, PartialEq)]
pub struct Iface {
    pub name: String,
    pub state: String,
    pub addrs: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Conn {
    pub proto: String,
    pub local: String,
    pub peer: String,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Data {
    pub ifaces: Vec<Iface>,
    pub routes: Vec<String>,
    pub dns: Vec<String>,
    pub listen: Vec<Listen>,
    pub conns: Vec<Conn>,
}

pub fn parse(out: &str) -> Data {
    let mut d = Data::default();
    let mut section = "";
    let mut listen = String::new();
    for line in out.lines() {
        if let Some(s) = line.strip_prefix("@@") {
            section = match s {
                "addr" => "addr",
                "route" => "route",
                "dns" => "dns",
                "listen" => "listen",
                "conn" => "conn",
                _ => "",
            };
            continue;
        }
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.is_empty() {
            continue;
        }
        match section {
            "addr" if f.len() >= 2 => d.ifaces.push(Iface {
                name: f[0].split('@').next().unwrap_or(f[0]).to_string(),
                state: f[1].to_string(),
                addrs: f[2..].iter().filter(|a| !a.starts_with("fe80")).map(|s| s.to_string()).collect(),
            }),
            "route" => d.routes.push(line.trim().to_string()),
            "dns" if f.len() >= 2 => d.dns.push(f[1].to_string()),
            "listen" => {
                listen.push_str(line);
                listen.push('\n');
            }
            "conn" if f.len() >= 5 => d.conns.push(Conn { proto: f[0].into(), local: f[3].into(), peer: f[4].into() }),
            _ => {}
        }
    }
    d.listen = parse_listen(&listen);
    d
}

/// Host name or IP typed by the user: a conservative character set, so it is safe to pass on.
pub fn valid_target(s: &str) -> bool {
    !s.is_empty() && s.len() <= 253 && !s.starts_with('-') && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '_'))
}

#[derive(Clone, Copy, PartialEq)]
pub enum Tool {
    Ping,
    Trace,
    Dns,
    Port,
}

/// Diagnostic command run on the server, or None if the input is invalid.
pub fn tool_command(tool: Tool, target: &str, port: &str) -> Option<String> {
    let t = target.trim();
    if !valid_target(t) {
        return None;
    }
    let q = shell_quote(t);
    Some(match tool {
        Tool::Ping => format!("ping -c 4 -W 2 {q} 2>&1"),
        Tool::Trace => format!("PATH=$PATH:/usr/sbin; (traceroute -m 20 -w 2 {q} || tracepath -m 20 {q}) 2>&1"),
        Tool::Dns => format!("(command -v dig >/dev/null && dig +noall +answer {q} A {q} AAAA) || getent ahosts {q} || host {q}"),
        Tool::Port => {
            let p: u16 = port.trim().parse().ok().filter(|p| *p > 0)?;
            format!("if timeout 5 bash -c \"</dev/tcp/{t}/{p}\" 2>/dev/null; then echo '{t}:{p} OPEN'; else echo '{t}:{p} CLOSED / FILTERED'; fi")
        }
    })
}

#[derive(Clone, Copy, PartialEq)]
enum View {
    Interfaces,
    Ports,
    Connections,
    Tools,
}

#[component]
pub fn Network(host: String) -> impl IntoView {
    let host = StoredValue::new(host);
    let data = RwSignal::new(None::<Data>);
    let sel = RwSignal::new(View::Interfaces);
    let loading = RwSignal::new(false);
    let tool = RwSignal::new(Tool::Ping);
    let target = RwSignal::new(String::new());
    let port = RwSignal::new("443".to_string());
    let output = RwSignal::new(None::<(String, String)>);
    let running = RwSignal::new(false);
    let h = move || host.get_value();

    let load = move || {
        loading.set(true);
        kit::query(host.get_value(), SCRIPT, move |r| {
            let _ = loading.try_set(false);
            let _ = data.try_set(Some(parse(&r.stdout)));
        });
    };
    load();

    let preview = move || tool_command(tool.get(), &target.get(), &port.get());
    let run_tool = move || {
        let Some(cmd) = preview() else { return };
        running.set(true);
        output.set(Some((cmd.clone(), String::new())));
        kit::query(h(), cmd.clone(), move |r| {
            let _ = running.try_set(false);
            let _ = output.try_set(Some((cmd, format!("{}{}", r.stdout, r.stderr))));
        });
    };

    let interfaces = move || {
        let d = data.get().unwrap_or_default();
        view! {
            <div class="cards">
                {d.ifaces
                    .into_iter()
                    .map(|i| {
                        let cls = match i.state.as_str() {
                            "UP" => "pill ok",
                            "DOWN" => "pill crit",
                            _ => "pill off",
                        };
                        view! {
                            <div class="card net-card">
                                <div class="cell-flex"><Icon name="network" size=15 /><b class="grow">{i.name.clone()}</b><span class=cls>{i.state.clone()}</span></div>
                                {if i.addrs.is_empty() {
                                    view! { <div class="muted small">{tr("Aucune adresse", "No address")}</div> }.into_any()
                                } else {
                                    i.addrs.into_iter().map(|a| view! { <div class="mono small">{a}</div> }).collect_view().into_any()
                                }}
                            </div>
                        }
                    })
                    .collect_view()}
            </div>
            <div class="section-title">{tr("Routes", "Routes")}</div>
            <pre class="logview net-pre">{d.routes.join("\n")}</pre>
            <div class="section-title">{tr("Serveurs DNS", "DNS servers")}</div>
            <div class="mono small">{if d.dns.is_empty() { "—".to_string() } else { d.dns.join(", ") }}</div>
        }
    };

    let ports = move || {
        let d = data.get().unwrap_or_default();
        if d.listen.is_empty() {
            return kit::empty_view(tr("Aucun port en écoute.", "No listening ports.")).into_any();
        }
        view! {
            <div class="table-card">
                <table class="table data-table">
                    <thead><tr><th>{tr("Port", "Port")}</th><th>{tr("Protocole", "Protocol")}</th><th>{tr("Adresse", "Address")}</th><th>{tr("Processus", "Process")}</th><th>{tr("Exposition", "Exposure")}</th><th></th></tr></thead>
                    <tbody>
                        {d.listen
                            .into_iter()
                            .map(|l| {
                                let local = l.local_only();
                                let p = l.port;
                                let tcp = l.proto == "tcp";
                                view! {
                                    <tr>
                                        <td class="mono"><b>{l.port}</b></td>
                                        <td class="muted">{l.proto.clone()}</td>
                                        <td class="mono muted">{l.addr.clone()}</td>
                                        <td class="ellipsis">{if l.process.is_empty() { "—".to_string() } else { l.process.clone() }}</td>
                                        <td>{if local { view! { <span class="pill ok">{tr("local", "local")}</span> }.into_any() } else { view! { <span class="pill warn">{tr("toutes interfaces", "all interfaces")}</span> }.into_any() }}</td>
                                        <td class="row-actions">
                                            {tcp.then(|| view! {
                                                <button class="icon-btn" title=tr("Ouvrir dans le Navigateur", "Open in Browser") on:click=move |_| ctx().open(AppKind::Browser { url: Some(format!("http://localhost:{p}")) }, Some(h()))>
                                                    <Icon name="compass" size=14 />
                                                </button>
                                                <button class="icon-btn" title=tr("Créer un tunnel", "Create a tunnel") on:click=move |_| ctx().open(AppKind::Tunnels, Some(h()))>
                                                    <Icon name="tunnel" size=14 />
                                                </button>
                                            })}
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

    let connections = move || {
        let d = data.get().unwrap_or_default();
        if d.conns.is_empty() {
            return kit::empty_view(tr("Aucune connexion établie.", "No established connections.")).into_any();
        }
        // Group by remote IP: who talks to this server the most.
        let mut by_peer: Vec<(String, usize, Vec<String>)> = Vec::new();
        for c in &d.conns {
            let ip = c.peer.rsplit_once(':').map(|(a, _)| a).unwrap_or(&c.peer).trim_matches(['[', ']']).to_string();
            let lport = c.local.rsplit_once(':').map(|(_, p)| p.to_string()).unwrap_or_default();
            match by_peer.iter_mut().find(|(p, _, _)| *p == ip) {
                Some(e) => {
                    e.1 += 1;
                    if !e.2.contains(&lport) {
                        e.2.push(lport)
                    }
                }
                None => by_peer.push((ip, 1, vec![lport])),
            }
        }
        by_peer.sort_by(|a, b| b.1.cmp(&a.1));
        view! {
            <div class="table-card">
                <table class="table data-table">
                    <thead><tr><th>{tr("Adresse distante", "Remote address")}</th><th>{tr("Connexions", "Connections")}</th><th>{tr("Ports locaux", "Local ports")}</th></tr></thead>
                    <tbody>
                        {by_peer
                            .into_iter()
                            .map(|(ip, n, ports)| view! { <tr><td class="mono">{ip}</td><td>{n}</td><td class="mono muted ellipsis">{ports.join(", ")}</td></tr> })
                            .collect_view()}
                    </tbody>
                </table>
            </div>
        }
        .into_any()
    };

    let tools = move || {
        let tool_btn = move |t: Tool, label: &'static str| {
            view! { <button class:on=move || tool.get() == t on:click=move |_| tool.set(t)>{label}</button> }
        };
        view! {
            <div class="seg">
                {tool_btn(Tool::Ping, "Ping")}
                {tool_btn(Tool::Trace, "Traceroute")}
                {tool_btn(Tool::Dns, "DNS")}
                {tool_btn(Tool::Port, tr("Port TCP", "TCP port"))}
            </div>
            <div class="fw-row">
                <input class="grow mono" placeholder=tr("Hôte ou IP (ex. 1.1.1.1, exemple.fr)", "Host or IP (e.g. 1.1.1.1, example.com)") bind:value=target on:keydown=move |e| {
                    if e.key() == "Enter" {
                        run_tool()
                    }
                } />
                {move || (tool.get() == Tool::Port).then(|| view! { <input class="mono fw-port" placeholder="443" bind:value=port /> })}
                <button class="primary" disabled=move || preview().is_none() || running.get() on:click=move |_| run_tool()>{tr("Lancer", "Run")}</button>
            </div>
            <pre class="code-preview">{move || preview().unwrap_or_else(|| tr("Saisissez une cible valide.", "Enter a valid target.").to_string())}</pre>
            {move || {
                output.get().map(|(_, out)| {
                    view! {
                        <pre class="logview net-pre">{if running.get() { tr("En cours…", "Running…").to_string() } else { out }}</pre>
                    }
                })
            }}
        }
    };

    let title = move || match sel.get() {
        View::Interfaces => tr("Interfaces", "Interfaces"),
        View::Ports => tr("Ports en écoute", "Listening ports"),
        View::Connections => tr("Connexions établies", "Established connections"),
        View::Tools => tr("Outils de diagnostic", "Diagnostic tools"),
    };
    let side_item = move |v: View, label: &'static str, icon: &'static str, n: Option<fn(&Data) -> usize>| {
        view! {
            <button class="side-item" class:active=move || sel.get() == v on:click=move |_| sel.set(v)>
                <Icon name=icon size=15 />
                <span class="grow">{label}</span>
                {n.map(|n| view! { <span class="side-count">{move || data.with(|d| d.as_ref().map(n).unwrap_or(0))}</span> })}
            </button>
        }
    };

    app_menus(vec![
        MenuSection::new(tr("Réseau", "Network"), vec![MenuItem::new(tr("Actualiser", "Refresh"), load)]),
        MenuSection::new(tr("Présentation", "View"), vec![
            MenuItem::new(tr("Interfaces", "Interfaces"), move || sel.set(View::Interfaces)).checked(move || sel.get() == View::Interfaces),
            MenuItem::new(tr("Ports en écoute", "Listening ports"), move || sel.set(View::Ports)).checked(move || sel.get() == View::Ports),
            MenuItem::new(tr("Connexions établies", "Established connections"), move || sel.set(View::Connections)).checked(move || sel.get() == View::Connections),
            MenuItem::new(tr("Outils de diagnostic", "Diagnostic tools"), move || sel.set(View::Tools)).checked(move || sel.get() == View::Tools),
        ]),
    ]);

    view! {
        <div class="split-app network-app">
            <aside class="side drag-region">
                <div class="side-top"></div>
                <div class="side-title">{tr("Réseau", "Network")}</div>
                {side_item(View::Interfaces, tr("Interfaces", "Interfaces"), "network", Some(|d| d.ifaces.len()))}
                {side_item(View::Ports, tr("Ports en écoute", "Listening ports"), "plug", Some(|d| d.listen.len()))}
                {side_item(View::Connections, tr("Connexions", "Connections"), "arrows", Some(|d| d.conns.len()))}
                {side_item(View::Tools, tr("Outils", "Tools"), "activity", None)}
            </aside>
            <section class="main">
                <header class="main-head drag-region">
                    <h2 class="grow ellipsis">{title}</h2>
                    <button class="icon-btn" class:spin=move || loading.get() title=tr("Actualiser", "Refresh") on:click=move |_| load()>
                        <Icon name="refresh" size=15 />
                    </button>
                </header>
                <div class="main-body">
                    {move || {
                        if sel.get() == View::Tools {
                            return tools().into_any();
                        }
                        if data.with(|d| d.is_none()) {
                            return kit::loading_view().into_any();
                        }
                        match sel.get() {
                            View::Interfaces => interfaces().into_any(),
                            View::Ports => ports(),
                            _ => connections(),
                        }
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
    fn parse_all() {
        let out = "@@addr\nlo UNKNOWN 127.0.0.1/8 ::1/128\neth0@if5 UP 10.0.0.5/24 fe80::1/64\n@@route\ndefault via 10.0.0.1 dev eth0\n@@dns\nnameserver 1.1.1.1\n@@listen\ntcp LISTEN 0 511 0.0.0.0:80 0.0.0.0:* users:((\"nginx\",pid=1,fd=6))\n@@conn\ntcp 0 0 10.0.0.5:22 203.0.113.5:51234\ntcp 0 0 10.0.0.5:443 203.0.113.5:51235\n";
        let d = parse(out);
        assert_eq!(d.ifaces.len(), 2);
        assert_eq!(d.ifaces[1].name, "eth0");
        assert_eq!(d.ifaces[1].addrs, vec!["10.0.0.5/24"]);
        assert_eq!(d.routes, vec!["default via 10.0.0.1 dev eth0"]);
        assert_eq!(d.dns, vec!["1.1.1.1"]);
        assert_eq!(d.listen.len(), 1);
        assert_eq!(d.conns.len(), 2);
        assert_eq!(d.conns[0].peer, "203.0.113.5:51234");
    }

    #[test]
    fn tools() {
        assert_eq!(tool_command(Tool::Ping, "1.1.1.1", "").unwrap(), "ping -c 4 -W 2 '1.1.1.1' 2>&1");
        assert!(tool_command(Tool::Ping, "a;rm -rf", "").is_none());
        assert!(tool_command(Tool::Ping, "-f", "").is_none());
        assert!(tool_command(Tool::Port, "example.com", "99999").is_none());
        assert!(tool_command(Tool::Port, "example.com", "443").unwrap().contains("/dev/tcp/example.com/443"));
    }
}
