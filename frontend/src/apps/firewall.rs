//! Firewall section of the Security app: ufw rules (or the nftables ruleset, read-only), opening /
//! closing a port with the exact command previewed, and the listening ports cross-checked against
//! the rules. Every change is root-only, so it goes through `kit::privileged`.

use leptos::prelude::*;

use crate::apps::kit::{self, SBIN};
use crate::icons::Icon;
use crate::state::*;

#[derive(Clone, Debug, PartialEq)]
pub struct UfwRule {
    pub num: u32,
    pub to: String,
    pub action: String,
    pub from: String,
    pub v6: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Ufw {
    Missing,
    NeedsRoot,
    Inactive,
    Active(Vec<UfwRule>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Listen {
    pub proto: String,
    pub addr: String,
    pub port: u16,
    pub process: String,
}

impl Listen {
    /// Bound to loopback only: unreachable from outside whatever the firewall says.
    pub fn local_only(&self) -> bool {
        self.addr.starts_with("127.") || self.addr == "::1" || self.addr == "[::1]" || self.addr.contains("%lo")
    }
}

#[derive(Clone, Debug, Default)]
struct Fw {
    ufw: Option<Ufw>,
    nft: Option<String>,
    ports: Vec<Listen>,
    ssh_port: u16,
}

const SCRIPT: &str = r#"echo "@@ssh ${SSH_CONNECTION##* }"
if command -v ufw >/dev/null 2>&1; then echo @@ufw; ufw status numbered 2>&1; fi
if command -v nft >/dev/null 2>&1; then echo @@nft; nft list ruleset 2>&1 | head -400; fi
echo @@ports; ss -ltnuHp 2>/dev/null || ss -ltnuH 2>/dev/null"#;

/// `ufw status numbered` output.
pub fn parse_ufw(out: &str) -> Ufw {
    let l = out.to_lowercase();
    if l.contains("need to be root") || l.contains("permission denied") {
        return Ufw::NeedsRoot;
    }
    if l.contains("status: inactive") {
        return Ufw::Inactive;
    }
    let rules = out
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let rest = line.strip_prefix('[')?;
            let (num, rest) = rest.split_once(']')?;
            let cols: Vec<&str> = rest.trim().split("  ").map(str::trim).filter(|s| !s.is_empty()).collect();
            if cols.len() < 3 {
                return None;
            }
            let to = cols[0].to_string();
            Some(UfwRule {
                num: num.trim().parse().ok()?,
                v6: to.contains("(v6)"),
                to,
                action: cols[1].to_string(),
                from: cols[2..].join(" "),
            })
        })
        .collect();
    Ufw::Active(rules)
}

/// `ss -ltnuH[p]` output, one entry per (proto, port, address).
pub fn parse_listen(out: &str) -> Vec<Listen> {
    let mut v: Vec<Listen> = out
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 5 {
                return None;
            }
            let (addr, port) = f[4].rsplit_once(':')?;
            let process = f
                .get(6)
                .and_then(|p| p.split('"').nth(1))
                .unwrap_or_default()
                .to_string();
            Some(Listen { proto: f[0].to_string(), addr: addr.trim_matches(['[', ']']).to_string(), port: port.parse().ok()?, process })
        })
        .collect();
    v.sort_by(|a, b| (a.port, &a.proto, &a.addr).cmp(&(b.port, &b.proto, &b.addr)));
    v.dedup();
    v
}

/// Ports named by a well-known ufw application profile.
fn profile_ports(name: &str) -> Option<Vec<u16>> {
    Some(match name {
        "OpenSSH" => vec![22],
        "Nginx Full" | "Apache Full" | "WWW Full" => vec![80, 443],
        "Nginx HTTP" | "Apache" | "WWW" => vec![80],
        "Nginx HTTPS" | "Apache Secure" | "WWW Secure" => vec![443],
        _ => return None,
    })
}

/// Does an ALLOW rule cover this port? `None` when a rule could not be understood (app profile…).
pub fn port_allowed(rules: &[UfwRule], port: u16, proto: &str) -> Option<bool> {
    let mut unknown = false;
    for r in rules.iter().filter(|r| r.action.starts_with("ALLOW")) {
        let to = r.to.replace("(v6)", "");
        let to = to.trim();
        if let Some(ports) = profile_ports(to) {
            if ports.contains(&port) {
                return Some(true);
            }
            continue;
        }
        let (spec, p) = match to.split_once('/') {
            Some((s, p)) => (s, Some(p.split_whitespace().next().unwrap_or(p))),
            None => (to.split_whitespace().next().unwrap_or(to), None),
        };
        if p.is_some_and(|p| p != proto) {
            continue;
        }
        for part in spec.split(',') {
            let hit = match part.split_once(':') {
                Some((a, b)) => match (a.parse::<u16>(), b.parse::<u16>()) {
                    (Ok(a), Ok(b)) => a <= port && port <= b,
                    _ => {
                        unknown = true;
                        false
                    }
                },
                None => match part.parse::<u16>() {
                    Ok(n) => n == port,
                    Err(_) => {
                        unknown = true;
                        false
                    }
                },
            };
            if hit {
                return Some(true);
            }
        }
    }
    if unknown { None } else { Some(false) }
}

/// ufw command opening (or denying) a port, optionally from one source address.
pub fn allow_command(action: &str, port: &str, proto: &str, from: &str) -> Option<String> {
    let port = port.trim();
    let valid = !port.is_empty() && port.split([',', ':']).all(|p| p.parse::<u16>().is_ok_and(|n| n > 0));
    if !valid {
        return None;
    }
    let from = from.trim();
    if from.is_empty() {
        Some(match proto {
            "tcp" | "udp" => format!("ufw {action} {port}/{proto}"),
            _ => format!("ufw {action} {port}"),
        })
    } else {
        if !from.chars().all(|c| c.is_ascii_hexdigit() || ".:/".contains(c)) {
            return None;
        }
        let proto = match proto {
            "tcp" | "udp" => format!(" proto {proto}"),
            _ => String::new(),
        };
        Some(format!("ufw {action} from {from} to any port {port}{proto}"))
    }
}

fn split(out: &str) -> Fw {
    let mut fw = Fw::default();
    let mut section = "";
    let mut buf = String::new();
    let flush = |section: &str, buf: &mut String, fw: &mut Fw| {
        match section {
            "ufw" => fw.ufw = Some(parse_ufw(buf)),
            "nft" => fw.nft = Some(buf.trim().to_string()),
            "ports" => fw.ports = parse_listen(buf),
            _ => {}
        }
        buf.clear();
    };
    for line in out.lines() {
        if let Some(p) = line.strip_prefix("@@ssh") {
            fw.ssh_port = p.trim().parse().unwrap_or(22);
        } else if let Some(s) = line.strip_prefix("@@") {
            flush(section, &mut buf, &mut fw);
            section = match s.trim() {
                "ufw" => "ufw",
                "nft" => "nft",
                "ports" => "ports",
                _ => "",
            };
        } else {
            buf.push_str(line);
            buf.push('\n');
        }
    }
    flush(section, &mut buf, &mut fw);
    if fw.ufw.is_none() {
        fw.ufw = Some(Ufw::Missing);
    }
    if fw.ssh_port == 0 {
        fw.ssh_port = 22;
    }
    fw
}

#[component]
pub fn Firewall(host: String) -> impl IntoView {
    let c = ctx();
    let host = StoredValue::new(host);
    let data = RwSignal::new(None::<Fw>);
    let port = RwSignal::new(String::new());
    let proto = RwSignal::new(String::from("tcp"));
    let from = RwSignal::new(String::new());
    let action = RwSignal::new(String::from("allow"));

    let load = move || {
        kit::query(host.get_value(), format!("{SBIN}{SCRIPT}"), move |r| {
            let _ = data.try_set(Some(split(&r.stdout)));
        });
    };
    load();
    let reload = move || load();
    let h = move || host.get_value();
    let preview = move || allow_command(&action.get(), &port.get(), &proto.get(), &from.get());

    let read_sudo = move || c.run_in_terminal(&h(), "sudo ufw status numbered");
    let enable = move || {
        let ssh = data.with_untracked(|d| d.as_ref().map(|d| d.ssh_port).unwrap_or(22));
        kit::privileged(h(), tr("Activer le pare-feu", "Enable the firewall"), format!("ufw allow {ssh}/tcp && ufw --force enable"), reload);
    };
    let disable = move || kit::privileged(h(), tr("Désactiver le pare-feu", "Disable the firewall"), "ufw disable", reload);
    let apply = move || {
        if let Some(cmd) = preview() {
            kit::privileged(h(), tr("Règle de pare-feu", "Firewall rule"), cmd, reload);
            port.set(String::new());
        }
    };
    let delete = move |r: UfwRule| {
        let ssh = data.with_untracked(|d| d.as_ref().map(|d| d.ssh_port).unwrap_or(22));
        if port_allowed(std::slice::from_ref(&r), ssh, "tcp") == Some(true) {
            c.info_toast(tr(
                "Attention : cette règle autorise le port SSH. Sans autre règle, vous perdrez l'accès.",
                "Careful: this rule allows the SSH port. Without another rule you will lose access.",
            ));
        }
        kit::privileged(h(), format!("{} [{}] {}", tr("Supprimer la règle", "Delete rule"), r.num, r.to), format!("ufw --force delete {}", r.num), reload);
    };

    let form = move || {
        view! {
            <div class="card fw-form">
                <h4>{tr("Ouvrir ou bloquer un port", "Open or block a port")}</h4>
                <div class="fw-row">
                    <select prop:value=move || action.get() on:change=move |e| action.set(event_target_value(&e))>
                        <option value="allow">{tr("Autoriser", "Allow")}</option>
                        <option value="deny">{tr("Bloquer", "Deny")}</option>
                        <option value="limit">{tr("Limiter (anti brute-force)", "Limit (anti brute-force)")}</option>
                    </select>
                    <input class="fw-port" placeholder=tr("Port (80, 8000:8100)", "Port (80, 8000:8100)") bind:value=port />
                    <select prop:value=move || proto.get() on:change=move |e| proto.set(event_target_value(&e))>
                        <option value="tcp">"TCP"</option>
                        <option value="udp">"UDP"</option>
                        <option value="any">{tr("TCP + UDP", "TCP + UDP")}</option>
                    </select>
                    <input class="grow" placeholder=tr("Depuis (IP ou réseau, facultatif)", "From (IP or network, optional)") bind:value=from />
                    <button class="primary" disabled=move || preview().is_none() on:click=move |_| apply()>{tr("Appliquer…", "Apply…")}</button>
                </div>
                <pre class="code-preview">{move || preview().unwrap_or_else(|| tr("Saisissez un port valide.", "Enter a valid port.").into())}</pre>
            </div>
        }
    };

    let ufw_view = move |d: Fw| {
        let ufw = d.ufw.clone().unwrap_or(Ufw::Missing);
        let rules = match &ufw {
            Ufw::Active(r) => Some(r.clone()),
            _ => None,
        };
        let head = match &ufw {
            Ufw::Missing => view! {
                <div class="banner warn">
                    <Icon name="alert" size=16 />
                    <span class="grow">{tr("ufw n'est pas installé. Les règles nftables sont affichées en lecture seule.", "ufw is not installed. The nftables rules are shown read-only.")}</span>
                    <button class="small" on:click=move |_| kit::privileged(h(), tr("Installer ufw", "Install ufw"), "apt-get install -y ufw", reload)>
                        {tr("Installer ufw…", "Install ufw…")}
                    </button>
                </div>
            }
            .into_any(),
            Ufw::NeedsRoot => view! {
                <div class="banner warn">
                    <Icon name="lock" size=16 />
                    <span class="grow">{tr("Lire les règles du pare-feu demande les droits root.", "Reading the firewall rules needs root.")}</span>
                    <button class="small" on:click=move |_| read_sudo()>{tr("Lire avec sudo dans un terminal", "Read with sudo in a terminal")}</button>
                </div>
            }
            .into_any(),
            Ufw::Inactive => view! {
                <div class="banner warn">
                    <Icon name="alert" size=16 />
                    <span class="grow">{tr("Pare-feu ufw inactif : tous les ports en écoute sont joignables.", "ufw firewall inactive: every listening port is reachable.")}</span>
                    <button class="small primary" on:click=move |_| enable()>{tr("Activer…", "Enable…")}</button>
                </div>
            }
            .into_any(),
            Ufw::Active(r) => {
                let n = r.len();
                view! {
                    <div class="banner">
                        <Icon name="shield-check" size=16 />
                        <span class="grow">{format!("{} · {n} {}", tr("Pare-feu ufw actif", "ufw firewall active"), tr("règles", "rules"))}</span>
                        <button class="small" on:click=move |_| disable()>{tr("Désactiver…", "Disable…")}</button>
                    </div>
                }
                .into_any()
            }
        };
        let ports = d.ports.clone();
        let active = rules.is_some();
        let inactive = matches!(ufw, Ufw::Inactive);
        let rules_v = rules.clone().unwrap_or_default();
        view! {
            {head}
            {active.then(|| form())}
            {rules.map(|rules| {
                let empty = rules.is_empty();
                view! {
                    <div class="table-card">
                        <table class="table data-table">
                            <thead><tr><th>"#"</th><th>{tr("Vers", "To")}</th><th>{tr("Action", "Action")}</th><th>{tr("Depuis", "From")}</th><th></th></tr></thead>
                            <tbody>
                                {rules
                                    .into_iter()
                                    .map(|r| {
                                        let cls = if r.action.starts_with("ALLOW") { "pill ok" } else if r.action.starts_with("LIMIT") { "pill warn" } else { "pill crit" };
                                        let r2 = r.clone();
                                        view! {
                                            <tr>
                                                <td class="muted">{r.num}</td>
                                                <td class="mono">{r.to.clone()}</td>
                                                <td><span class=cls>{r.action.clone()}</span></td>
                                                <td class="mono">{r.from.clone()}</td>
                                                <td class="row-actions">
                                                    <button class="icon-btn danger" title=tr("Supprimer…", "Delete…") on:click=move |_| delete(r2.clone())>
                                                        <Icon name="trash" size=14 />
                                                    </button>
                                                </td>
                                            </tr>
                                        }
                                    })
                                    .collect_view()}
                            </tbody>
                        </table>
                        {empty.then(|| view! { <div class="empty">{tr("Aucune règle : tout est bloqué en entrée.", "No rules: all incoming traffic is blocked.")}</div> })}
                    </div>
                }
            })}
            <h3 class="section-title">{tr("Ports en écoute", "Listening ports")}</h3>
            <div class="table-card">
                <table class="table data-table">
                    <thead><tr><th>{tr("Port", "Port")}</th><th>{tr("Proto", "Proto")}</th><th>{tr("Adresse", "Address")}</th><th>{tr("Processus", "Process")}</th><th>{tr("Depuis Internet", "From the Internet")}</th><th></th></tr></thead>
                    <tbody>
                        {ports
                            .into_iter()
                            .map(|p| {
                                let pr = if p.proto.starts_with("udp") { "udp" } else { "tcp" };
                                let (cls, label) = if p.local_only() {
                                    ("pill off", tr("Local uniquement", "Local only"))
                                } else if inactive {
                                    ("pill warn", tr("Ouvert (pare-feu inactif)", "Open (firewall off)"))
                                } else if !active {
                                    ("pill off", "?")
                                } else {
                                    match port_allowed(&rules_v, p.port, pr) {
                                        Some(true) => ("pill ok", tr("Autorisé", "Allowed")),
                                        Some(false) => ("pill crit", tr("Bloqué", "Blocked")),
                                        None => ("pill off", "?"),
                                    }
                                };
                                let blocked = active && !p.local_only() && port_allowed(&rules_v, p.port, pr) == Some(false);
                                let port_s = p.port.to_string();
                                let proto_s = pr.to_string();
                                view! {
                                    <tr>
                                        <td class="mono">{p.port}</td>
                                        <td>{pr}</td>
                                        <td class="mono muted">{p.addr.clone()}</td>
                                        <td>{p.process.clone()}</td>
                                        <td><span class=cls>{label}</span></td>
                                        <td class="row-actions">
                                            {blocked.then(|| view! {
                                                <button class="small" on:click=move |_| {
                                                    action.set("allow".into());
                                                    port.set(port_s.clone());
                                                    proto.set(proto_s.clone());
                                                }>{tr("Ouvrir…", "Open…")}</button>
                                            })}
                                        </td>
                                    </tr>
                                }
                            })
                            .collect_view()}
                    </tbody>
                </table>
            </div>
            {d.nft.clone().filter(|_| !active).filter(|n| !n.is_empty()).map(|n| view! {
                <h3 class="section-title">"nftables"</h3>
                <pre class="logview fw-nft">{n}</pre>
            })}
        }
        .into_any()
    };

    view! {
        <div class="fw">
            {move || match data.get() {
                None => kit::loading_view().into_any(),
                Some(d) => ufw_view(d),
            }}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UFW: &str = "Status: active\n\n     To                         Action      From\n     --                         ------      ----\n[ 1] 22/tcp                     ALLOW IN    Anywhere\n[ 2] 80,443/tcp                 ALLOW IN    Anywhere\n[ 3] 8000:8100/tcp              DENY IN     10.0.0.0/8\n[ 4] Nginx Full                 ALLOW IN    Anywhere\n[ 5] 22/tcp (v6)                ALLOW IN    Anywhere (v6)\n";

    #[test]
    fn ufw_rules() {
        let Ufw::Active(r) = parse_ufw(UFW) else { panic!() };
        assert_eq!(r.len(), 5);
        assert_eq!(r[1].to, "80,443/tcp");
        assert_eq!(r[2].action, "DENY IN");
        assert_eq!(r[2].from, "10.0.0.0/8");
        assert!(r[4].v6);
        assert_eq!(port_allowed(&r, 443, "tcp"), Some(true));
        assert_eq!(port_allowed(&r, 22, "udp"), Some(false));
        assert_eq!(port_allowed(&r, 8050, "tcp"), Some(false));
        assert_eq!(parse_ufw("Status: inactive\n"), Ufw::Inactive);
        assert_eq!(parse_ufw("ERROR: You need to be root to run this script\n"), Ufw::NeedsRoot);
    }

    #[test]
    fn listen_and_commands() {
        let out = "tcp LISTEN 0 4096 0.0.0.0:22 0.0.0.0:* users:((\"sshd\",pid=1,fd=3))\ntcp LISTEN 0 511 127.0.0.1:3000 0.0.0.0:*\nudp UNCONN 0 0 127.0.0.53%lo:53 0.0.0.0:*\ntcp LISTEN 0 4096 [::]:22 [::]:*\n";
        let l = parse_listen(out);
        assert_eq!(l.len(), 4);
        assert_eq!(l[0].port, 22);
        assert_eq!(l[0].process, "sshd");
        assert!(l.iter().find(|p| p.port == 3000).unwrap().local_only());
        assert!(l.iter().find(|p| p.port == 53).unwrap().local_only());
        assert_eq!(allow_command("allow", "8080", "tcp", ""), Some("ufw allow 8080/tcp".into()));
        assert_eq!(allow_command("allow", "5432", "tcp", "10.0.0.0/8"), Some("ufw allow from 10.0.0.0/8 to any port 5432 proto tcp".into()));
        assert_eq!(allow_command("allow", "80; rm -rf /", "tcp", ""), None);
        assert_eq!(allow_command("allow", "80", "tcp", "1.2.3.4; reboot"), None);
        let fw = split("@@ssh 2222\n@@ufw\nStatus: inactive\n@@ports\n");
        assert_eq!(fw.ssh_port, 2222);
        assert_eq!(fw.ufw, Some(Ufw::Inactive));
    }
}
