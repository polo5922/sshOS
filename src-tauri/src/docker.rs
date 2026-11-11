//! Docker over SSH: containers (grouped by compose project), images, volumes, live usage.
//! Everything goes through the `docker` CLI on the server; no API socket is exposed.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use serde_json::Value;
use sshos_shared::{DockerAction, DockerContainer, DockerImage, DockerOverview, DockerRequest, DockerStat, DockerVisitors, DockerVolume};

use crate::error::{AppError, AppResult};
use crate::ssh::{sh_quote, Session};

const OVERVIEW: &str = r#"command -v docker >/dev/null 2>&1 || { echo '@@missing'; exit 0; }
echo '@@version'; docker version --format '{{.Server.Version}}' 2>&1
echo '@@ps'; docker ps -a --no-trunc --format '{{json .}}' 2>/dev/null
echo '@@images'; docker images --no-trunc --format '{{json .}}' 2>/dev/null
echo '@@used'; ids=$(docker ps -aq 2>/dev/null); [ -n "$ids" ] && docker inspect --format '{{.Image}}' $ids 2>/dev/null
echo '@@volumes'; docker volume ls --format '{{json .}}' 2>/dev/null
echo '@@dangling'; docker volume ls -qf dangling=true 2>/dev/null
true"#;

fn sections(out: &str) -> std::collections::HashMap<&str, Vec<&str>> {
    let mut map = std::collections::HashMap::new();
    let mut cur = "";
    for l in out.lines() {
        if let Some(name) = l.strip_prefix("@@") {
            cur = name.trim();
            map.entry(cur).or_insert_with(Vec::new);
        } else if !l.trim().is_empty() {
            map.entry(cur).or_insert_with(Vec::new).push(l);
        }
    }
    map
}

fn s(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or_default().to_string()
}

/// `Labels` from `docker ps` is "k=v,k2=v2" (values may contain commas, keys never contain '=').
fn label<'a>(labels: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("{key}=");
    let start = if labels.starts_with(&needle) {
        0
    } else {
        labels.find(&format!(",{needle}"))? + 1
    };
    let rest = &labels[start + needle.len()..];
    // Value ends at the next ",something=" .
    let mut end = rest.len();
    let mut i = 0;
    while let Some(p) = rest[i..].find(',') {
        let cand = &rest[i + p + 1..];
        let key_end = cand.find('=').unwrap_or(usize::MAX);
        let comma = cand.find(',').unwrap_or(usize::MAX);
        if key_end < comma {
            end = i + p;
            break;
        }
        i += p + 1;
    }
    Some(&rest[..end]).filter(|v| !v.is_empty())
}

pub fn parse_overview(out: &str) -> AppResult<DockerOverview> {
    let sec = sections(out);
    if sec.contains_key("missing") {
        return Err(AppError::msg("Docker is not installed on this server"));
    }
    let version = sec.get("version").and_then(|v| v.first()).map(|s| s.trim()).unwrap_or("");
    if version.contains("permission denied") {
        return Err(AppError::needs_terminal("sudo usermod -aG docker \"$USER\"  # then reconnect"));
    }
    if version.contains("Cannot connect") {
        return Err(AppError::msg(format!("Docker daemon not reachable: {version}")));
    }
    let json = |name: &str| -> Vec<Value> {
        sec.get(name)
            .map(|v| v.iter().filter_map(|l| serde_json::from_str(l).ok()).collect())
            .unwrap_or_default()
    };

    let mut containers: Vec<DockerContainer> = json("ps")
        .iter()
        .map(|v| {
            let labels = s(v, "Labels");
            let status = s(v, "Status");
            let mut state = s(v, "State");
            if state.is_empty() {
                state = if status.starts_with("Up") { "running".into() } else { "exited".into() };
            }
            DockerContainer {
                id: s(v, "ID"),
                name: s(v, "Names").split(',').next().unwrap_or_default().to_string(),
                image: s(v, "Image"),
                state,
                status,
                ports: s(v, "Ports"),
                created: s(v, "CreatedAt"),
                project: label(&labels, "com.docker.compose.project").map(String::from),
                service: label(&labels, "com.docker.compose.service").map(String::from),
                cpu: None,
                mem: None,
            }
        })
        .collect();
    containers.sort_by(|a, b| (&a.project, &a.name).cmp(&(&b.project, &b.name)));

    let used: HashSet<&str> = sec.get("used").map(|v| v.iter().map(|l| l.trim()).collect()).unwrap_or_default();
    let names_used: HashSet<&str> = containers.iter().map(|c| c.image.as_str()).collect();
    let images = json("images")
        .iter()
        .map(|v| {
            let (id, repo, tag) = (s(v, "ID"), s(v, "Repository"), s(v, "Tag"));
            let in_use = used.contains(id.as_str())
                || names_used.contains(format!("{repo}:{tag}").as_str())
                || (tag == "latest" && names_used.contains(repo.as_str()));
            DockerImage { id, repository: repo, tag, size: s(v, "Size"), created: s(v, "CreatedSince"), in_use }
        })
        .collect();

    let dangling: HashSet<&str> = sec.get("dangling").map(|v| v.iter().map(|l| l.trim()).collect()).unwrap_or_default();
    let volumes = json("volumes")
        .iter()
        .map(|v| {
            let name = s(v, "Name");
            DockerVolume { in_use: !dangling.contains(name.as_str()), driver: s(v, "Driver"), name }
        })
        .collect();

    Ok(DockerOverview { containers, images, volumes, version: version.to_string() })
}

pub async fn overview(session: &Session) -> AppResult<DockerOverview> {
    parse_overview(&session.exec_read(OVERVIEW).await?.stdout)
}

/// "264.1MiB / 22.4GiB" -> bytes of the first part.
pub fn parse_size(s: &str) -> u64 {
    let s = s.split('/').next().unwrap_or("").trim();
    let idx = s.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(s.len());
    let (num, unit) = s.split_at(idx);
    let n: f64 = num.trim().parse().unwrap_or(0.0);
    let mul = match unit.trim() {
        "KiB" => 1024f64,
        "MiB" => 1024f64.powi(2),
        "GiB" => 1024f64.powi(3),
        "TiB" => 1024f64.powi(4),
        "kB" | "KB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        _ => 1.0,
    };
    (n * mul) as u64
}

pub fn parse_stats(out: &str) -> Vec<DockerStat> {
    out.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .map(|v| DockerStat {
            id: s(&v, "ID"),
            cpu: s(&v, "CPUPerc").trim_end_matches('%').parse().unwrap_or(0.0),
            mem: parse_size(&s(&v, "MemUsage")),
        })
        .collect()
}

/// `docker stats --no-stream` samples for ~2 s, so it is a separate call.
pub async fn stats(session: &Session) -> AppResult<Vec<DockerStat>> {
    let r = session.exec_quiet("docker stats --no-stream --no-trunc --format '{{json .}}'").await?;
    Ok(parse_stats(&r.stdout))
}

fn valid_target(t: &str) -> bool {
    !t.is_empty()
        && t.len() < 256
        && !t.starts_with('-')
        && t.chars().all(|c| c.is_ascii_alphanumeric() || "_.-:/@".contains(c))
}

/// The exact command an action runs (shown to the user before it runs).
pub fn command(req: &DockerRequest) -> AppResult<String> {
    if req.targets.is_empty() {
        return Err(AppError::msg("Nothing selected"));
    }
    if let Some(bad) = req.targets.iter().find(|t| !valid_target(t)) {
        return Err(AppError::msg(format!("Invalid docker name: {bad}")));
    }
    let verb = match req.action {
        DockerAction::Start => "start",
        DockerAction::Stop => "stop",
        DockerAction::Restart => "restart",
        DockerAction::Pause => "pause",
        DockerAction::Unpause => "unpause",
        DockerAction::Remove => "rm -f",
        DockerAction::RemoveImage => "rmi",
        DockerAction::RemoveVolume => "volume rm",
    };
    let targets: Vec<String> = req.targets.iter().map(|t| sh_quote(t)).collect();
    Ok(format!("docker {verb} {}", targets.join(" ")))
}

pub async fn run(session: &Session, req: &DockerRequest) -> AppResult<String> {
    let cmd = command(req)?;
    let r = session.exec(&cmd).await?;
    if r.ok() {
        Ok(r.stdout)
    } else if r.stderr.contains("permission denied") {
        Err(AppError::needs_terminal(format!("sudo {cmd}")))
    } else {
        Err(AppError::msg(r.stderr.trim().to_string()))
    }
}

pub async fn logs(session: &Session, id: &str, tail: u32) -> AppResult<String> {
    if !valid_target(id) {
        return Err(AppError::msg("Invalid container"));
    }
    let r = session
        .exec(&format!("docker logs --timestamps --tail {} {} 2>&1", tail.clamp(10, 5000), sh_quote(id)))
        .await?;
    Ok(r.stdout)
}

pub async fn inspect(session: &Session, id: &str) -> AppResult<String> {
    if !valid_target(id) {
        return Err(AppError::msg("Invalid target"));
    }
    let r = session.exec(&format!("docker inspect {}", sh_quote(id))).await?;
    if r.ok() {
        Ok(r.stdout)
    } else {
        Err(AppError::msg(r.stderr.trim().to_string()))
    }
}

// ---------------------------------------------------------------- visitors

/// Read-only. Running containers (pid, networks, published ports), the TCP table of each one
/// (from /proc on the host, or `docker exec` when it is not readable), and, when Nginx Proxy
/// Manager runs in Docker, its proxy hosts and the distinct client IPs of the last 5 minutes
/// of each access log (only the IPs leave the server, not the log lines).
const VISITORS: &str = r###"ids=$(docker ps -q --no-trunc 2>/dev/null); [ -n "$ids" ] || exit 0
echo '@@inspect'
docker inspect --format '{{.Id}}{{"\t"}}{{.State.Pid}}{{"\t"}}{{.Name}}{{"\t"}}{{.HostConfig.NetworkMode}}{{"\t"}}{{index .Config.Labels "com.docker.compose.service"}}{{"\t"}}{{range $k, $v := .NetworkSettings.Networks}}{{$v.IPAddress}}|{{$v.Gateway}}|{{join $v.Aliases ","}} {{end}}{{"\t"}}{{range $p, $b := .NetworkSettings.Ports}}{{range $b}}{{$p}}={{.HostPort}} {{end}}{{end}}' $ids 2>/dev/null
for l in $(docker inspect --format '{{.Id}}:{{.State.Pid}}' $ids 2>/dev/null); do
  id=${l%%:*}; pid=${l##*:}
  echo "@@tcp $id"
  if [ -r /proc/$pid/net/tcp ]; then cat /proc/$pid/net/tcp /proc/$pid/net/tcp6 2>/dev/null; else docker exec $id cat /proc/net/tcp /proc/net/tcp6 2>/dev/null; fi
done
npm=$(docker ps --no-trunc --format '{{.ID}} {{.Image}}' 2>/dev/null | awk '$2 ~ /nginx-proxy-manager/ { print $1; exit }')
[ -n "$npm" ] || exit 0
echo "@@npm $npm"
docker exec -e LC_ALL=C $npm sh -c 'cd /data/nginx/proxy_host 2>/dev/null && for f in *.conf; do echo "## conf ${f%.conf}"; grep -hE "^[[:space:]]*(server_name|set .server|set .port)[[:space:]]" "$f"; done
n=$(date +%s); p=""; for i in 0 1 2 3 4; do p="$p -e $(date -d @$((n - 60 * i)) +%d/%b/%Y:%H:%M)"; done
cd /data/logs 2>/dev/null && for f in proxy-host-*_access.log; do h=${f#proxy-host-}; echo "## log ${h%_access.log}"; tail -n 20000 "$f" | grep -F $p | sed -n "s/.*\[Client \([^]]*\)\].*/\1/p" | sort -u; done' 2>/dev/null
true"###;

/// Usual HTTP ports; any port published on the host or targeted by the proxy counts too.
const WEB_PORTS: &[u16] = &[80, 443, 81, 3000, 3001, 4000, 5000, 5173, 8000, 8008, 8080, 8081, 8443, 8888, 9000, 9443];
/// Never web: databases, mail, DNS, SSH, brokers, the Docker API.
const NOT_WEB: &[u16] = &[
    21, 22, 25, 53, 110, 143, 389, 465, 587, 636, 993, 995, 1433, 1521, 1883, 2375, 2376, 3306, 5432, 5672, 6379, 8883, 9042,
    11211, 27017, 27018,
];

struct Ct {
    id: String,
    name: String,
    service: String,
    host_net: bool,
    ips: Vec<IpAddr>,
    gateways: Vec<IpAddr>,
    aliases: Vec<String>,
    /// (container port, host port)
    published: Vec<(u16, u16)>,
}

fn parse_inspect_line(l: &str) -> Option<Ct> {
    let f: Vec<&str> = l.split('\t').collect();
    if f.len() < 7 {
        return None;
    }
    let (mut ips, mut gateways, mut aliases) = (Vec::new(), Vec::new(), Vec::new());
    for net in f[5].split_whitespace() {
        let mut p = net.split('|');
        if let Some(ip) = p.next().and_then(|s| s.parse().ok()) {
            ips.push(ip);
        }
        if let Some(gw) = p.next().and_then(|s| s.parse().ok()) {
            gateways.push(gw);
        }
        aliases.extend(p.next().unwrap_or("").split(',').filter(|a| !a.is_empty()).map(str::to_lowercase));
    }
    let published = f[6]
        .split_whitespace()
        .filter_map(|e| {
            let (cp, hp) = e.split_once('=')?;
            Some((cp.strip_suffix("/tcp")?.parse().ok()?, hp.parse().ok()?))
        })
        .collect();
    Some(Ct {
        id: f[0].to_string(),
        name: f[2].trim_start_matches('/').to_lowercase(),
        service: f[4].to_lowercase(),
        host_net: f[3] == "host",
        ips,
        gateways,
        aliases,
        published,
    })
}

/// "0100007F" (IPv4, little-endian) or 32 hex digits (IPv6, four little-endian words).
fn hex_ip(h: &str) -> Option<IpAddr> {
    match h.len() {
        8 => Some(IpAddr::V4(Ipv4Addr::from(u32::from_str_radix(h, 16).ok()?.to_le_bytes()))),
        32 => {
            let mut b = [0u8; 16];
            for i in 0..4 {
                b[i * 4..i * 4 + 4].copy_from_slice(&u32::from_str_radix(&h[i * 8..i * 8 + 8], 16).ok()?.to_le_bytes());
            }
            let v6 = Ipv6Addr::from(b);
            Some(v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(v6)))
        }
        _ => None,
    }
}

/// /proc/net/tcp{,6}: ports listening on a non-loopback address, and established connections
/// as (local port, remote IP).
fn parse_tcp(lines: &[&str]) -> (BTreeSet<u16>, Vec<(u16, IpAddr)>) {
    let (mut listen, mut est) = (BTreeSet::new(), Vec::new());
    for l in lines {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() < 4 || f[0] == "sl" {
            continue;
        }
        let addr = |s: &str| -> Option<(IpAddr, u16)> {
            let (ip, port) = s.split_once(':')?;
            Some((hex_ip(ip)?, u16::from_str_radix(port, 16).ok()?))
        };
        let (Some((lip, lport)), Some((rip, _))) = (addr(f[1]), addr(f[2])) else { continue };
        match f[3] {
            "0A" if !lip.is_loopback() => {
                listen.insert(lport);
            }
            "01" => est.push((lport, rip)),
            _ => {}
        }
    }
    (listen, est)
}

/// A proxy host of Nginx Proxy Manager: domains, forward host and port, recent client IPs.
#[derive(Default)]
struct ProxyHost {
    names: Vec<String>,
    server: String,
    port: u16,
    clients: Vec<IpAddr>,
}

fn parse_npm(lines: &[&str]) -> BTreeMap<String, ProxyHost> {
    let mut hosts: BTreeMap<String, ProxyHost> = BTreeMap::new();
    let mut cur: Option<(bool, String)> = None;
    for l in lines {
        if let Some(rest) = l.strip_prefix("## ") {
            cur = rest.split_once(' ').map(|(k, id)| (k == "log", id.trim().to_string()));
            continue;
        }
        let Some((is_log, id)) = &cur else { continue };
        let h = hosts.entry(id.clone()).or_default();
        let t = l.trim().trim_end_matches(';');
        if *is_log {
            if let Ok(ip) = t.parse::<IpAddr>() {
                h.clients.push(ip);
            }
        } else if let Some(v) = t.strip_prefix("server_name") {
            h.names.extend(v.split_whitespace().filter(|n| *n != "_").map(String::from));
        } else if let Some(v) = t.strip_prefix("set $server") {
            h.server = v.trim().trim_matches('"').to_lowercase();
        } else if let Some(v) = t.strip_prefix("set $port") {
            h.port = v.trim().parse().unwrap_or(0);
        }
    }
    hosts
}

pub fn parse_visitors(out: &str) -> Vec<DockerVisitors> {
    let sec = sections(out);
    let cts: Vec<Ct> = sec.get("inspect").map(|v| v.iter().filter_map(|l| parse_inspect_line(l)).collect()).unwrap_or_default();
    let npm = sec.iter().find(|(k, _)| k.starts_with("npm")).map(|(k, v)| (k[3..].trim().to_string(), parse_npm(v)));
    // Connections from another container or from the Docker host are not visitors (a proxy, an app calling an API).
    let internal: HashSet<IpAddr> = cts.iter().flat_map(|c| c.ips.iter().chain(&c.gateways).copied()).collect();

    // Proxy hosts → (container index, container port).
    let mut targets: Vec<(usize, u16, &ProxyHost)> = Vec::new();
    if let Some((_, hosts)) = &npm {
        for h in hosts.values().filter(|h| !h.server.is_empty()) {
            let by_name = cts.iter().position(|c| {
                c.name == h.server || c.service == h.server || c.aliases.contains(&h.server) || c.ips.iter().any(|ip| ip.to_string() == h.server)
            });
            match by_name {
                Some(i) => targets.push((i, h.port, h)),
                // Forwarded to the host (its IP, a gateway, host.docker.internal…): the container publishing that port.
                None => {
                    if let Some((i, cp)) = cts.iter().enumerate().find_map(|(i, c)| c.published.iter().find(|(_, hp)| *hp == h.port).map(|(cp, _)| (i, *cp))) {
                        targets.push((i, cp, h));
                    }
                }
            }
        }
    }

    let mut out = Vec::new();
    for (i, c) in cts.iter().enumerate() {
        let lines = sec.get(format!("tcp {}", c.id).as_str()).cloned().unwrap_or_default();
        let (listen, est) = if c.host_net { (BTreeSet::new(), Vec::new()) } else { parse_tcp(&lines) };
        let mine: Vec<&(usize, u16, &ProxyHost)> = targets.iter().filter(|t| t.0 == i).collect();
        let ports: Vec<u16> = listen
            .iter()
            .copied()
            .filter(|p| !NOT_WEB.contains(p))
            .filter(|p| WEB_PORTS.contains(p) || c.published.iter().any(|(cp, _)| cp == p) || mine.iter().any(|t| t.1 == *p))
            .collect();
        if ports.is_empty() && mine.is_empty() {
            continue;
        }
        let direct: BTreeSet<IpAddr> = est
            .iter()
            .filter(|(lp, ip)| ports.contains(lp) && !ip.is_loopback() && !internal.contains(ip))
            .map(|(_, ip)| *ip)
            .collect();
        let proxied: BTreeSet<IpAddr> = mine.iter().flat_map(|t| t.2.clients.iter().copied()).collect();
        let mut sites: Vec<String> = mine.iter().flat_map(|t| t.2.names.iter().cloned()).collect();
        sites.sort();
        sites.dedup();
        out.push(DockerVisitors {
            id: c.id.clone(),
            visitors: direct.union(&proxied).count() as u32,
            direct: direct.len() as u32,
            proxied: proxied.len() as u32,
            ports,
            sites,
        });
    }
    out
}

pub async fn visitors(session: &Session) -> AppResult<Vec<DockerVisitors>> {
    Ok(parse_visitors(&session.exec_quiet(VISITORS).await?.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        let l = "com.docker.compose.project=web,desc=a,b,com.docker.compose.service=app";
        assert_eq!(label(l, "com.docker.compose.project"), Some("web"));
        assert_eq!(label(l, "desc"), Some("a,b"));
        assert_eq!(label(l, "com.docker.compose.service"), Some("app"));
        assert_eq!(label(l, "nope"), None);
    }

    #[test]
    fn overview_parse() {
        let out = r#"@@version
29.8.0
@@ps
{"ID":"abc","Names":"web-app-1","Image":"nginx:latest","State":"running","Status":"Up 2 days","Ports":"0.0.0.0:80->80/tcp","Labels":"com.docker.compose.project=web,com.docker.compose.service=app","CreatedAt":"2026-01-01"}
{"ID":"def","Names":"lonely","Image":"redis","State":"exited","Status":"Exited (0) 3 hours ago","Ports":"","Labels":"","CreatedAt":"2026-01-02"}
@@images
{"ID":"sha256:111","Repository":"nginx","Tag":"latest","Size":"190MB","CreatedSince":"2 weeks ago"}
{"ID":"sha256:222","Repository":"redis","Tag":"latest","Size":"120MB","CreatedSince":"1 month ago"}
{"ID":"sha256:333","Repository":"old","Tag":"1.0","Size":"50MB","CreatedSince":"1 year ago"}
@@used
sha256:111
@@volumes
{"Name":"data","Driver":"local"}
{"Name":"junk","Driver":"local"}
@@dangling
junk
"#;
        let o = parse_overview(out).unwrap();
        assert_eq!(o.version, "29.8.0");
        assert_eq!(o.containers.len(), 2);
        let web = o.containers.iter().find(|c| c.id == "abc").unwrap();
        assert_eq!(web.project.as_deref(), Some("web"));
        assert_eq!(web.service.as_deref(), Some("app"));
        assert!(o.images[0].in_use && o.images[1].in_use && !o.images[2].in_use);
        assert!(o.volumes[0].in_use && !o.volumes[1].in_use);
    }

    #[test]
    fn sizes_and_stats() {
        assert_eq!(parse_size("264.1MiB / 22.4GiB"), (264.1 * 1048576.0) as u64);
        assert_eq!(parse_size("1.5GB"), 1_500_000_000);
        let st = parse_stats(r#"{"ID":"abc","CPUPerc":"1.25%","MemUsage":"10MiB / 1GiB"}"#);
        assert_eq!(st[0].cpu, 1.25);
        assert_eq!(st[0].mem, 10 * 1048576);
    }

    #[test]
    fn commands_are_quoted_and_validated() {
        let r = DockerRequest { action: DockerAction::Restart, targets: vec!["web-app-1".into()] };
        assert_eq!(command(&r).unwrap(), "docker restart web-app-1");
        let bad = DockerRequest { action: DockerAction::Remove, targets: vec!["x; rm -rf /".into()] };
        assert!(command(&bad).is_err());
        let opt = DockerRequest { action: DockerAction::Remove, targets: vec!["--all".into()] };
        assert!(command(&opt).is_err());
    }

    #[test]
    fn visitors() {
        // web: published 8080->80, two outside clients (one twice), one from the proxy container.
        // npm: proxy host 3 -> "app" (service name of web), proxy host 4 -> host port 8080.
        // db: listens on 5432 only.
        let out = "@@inspect\n\
web1\t100\t/shop-web-1\tshop_default\tapp\t172.18.0.5|172.18.0.1|shop-web-1,app \t80/tcp=8080 80/tcp=8080 \n\
npm1\t200\t/npm\tbridge\t\t172.18.0.9|172.18.0.1| \t80/tcp=80 443/tcp=443 81/tcp=81 \n\
db1\t300\t/shop-db-1\tshop_default\tdb\t172.18.0.6|172.18.0.1|db \t\n\
@@tcp web1\n\
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
   0: 00000000:0050 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 1\n\
   1: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 2\n\
   2: 050012AC:0050 0A01A8C0:D431 01 00000000:00000000 00:00000000 00000000     0        0 3\n\
   3: 050012AC:0050 0A01A8C0:D432 01 00000000:00000000 00:00000000 00000000     0        0 4\n\
   4: 050012AC:0050 090012AC:A000 01 00000000:00000000 00:00000000 00000000     0        0 5\n\
   5: 050012AC:9C40 060012AC:1538 01 00000000:00000000 00:00000000 00000000     0        0 6\n\
   0: 00000000000000000000000000000000:0050 00000000000000000000000000000000:0000 0A 0 0 0 0 0 7\n\
   1: 0000000000000000FFFF0000050012AC:0050 0000000000000000FFFF00000B01A8C0:D433 01 0 0 0 0 0 8\n\
@@tcp db1\n\
   0: 00000000:1538 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 9\n\
   1: 060012AC:1538 050012AC:9C40 01 00000000:00000000 00:00000000 00000000     0        0 10\n\
@@npm npm1\n\
## conf 3\n\
  server_name shop.example.com www.shop.example.com;\n\
  set $server         \"app\";\n\
  set $port           80;\n\
## conf 4\n\
  server_name old.example.com;\n\
  set $server         \"192.168.1.20\";\n\
  set $port           8080;\n\
## log 3\n\
203.0.113.7\n\
198.51.100.2\n\
## log 4\n\
203.0.113.7\n\
2001:db8::1\n";
        let v = parse_visitors(out);
        assert_eq!(v.len(), 1, "npm has no TCP table here, db is not web");
        let w = &v[0];
        assert_eq!(w.id, "web1");
        assert_eq!(w.ports, vec![80]);
        // 192.168.1.10, .11 directly (172.18.0.9 is the proxy; :40000 is outgoing to the db)
        assert_eq!(w.direct, 2);
        assert_eq!(w.proxied, 3);
        assert_eq!(w.visitors, 5);
        assert_eq!(w.sites, vec!["old.example.com", "shop.example.com", "www.shop.example.com"]);
        assert_eq!(hex_ip("0100007F"), Some("127.0.0.1".parse().unwrap()));
    }
}
