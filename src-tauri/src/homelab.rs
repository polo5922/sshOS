//! Homelab diagram: one read-only pass over the host, its disks, Docker and the nginx config.
//!
//! Nothing here needs root: block devices come from lsblk, usage from df, listening sockets from
//! ss, Docker from its CLI (when this user may use it) and the proxy routes from the nginx files
//! readable by this user and from the Nginx Proxy Manager container's generated configs.

use std::collections::{BTreeMap, HashMap};

use sshos_shared::{
    HomelabContainer, HomelabDisk, HomelabFs, HomelabMount, HomelabNetwork, HomelabPort, HomelabProxy, HomelabSnapshot,
};

use crate::error::AppResult;
use crate::nginx::files::parse_config;
use crate::ssh::Session;

const SCRIPT: &str = r###"export LC_ALL=C
echo '@@host'
echo "hostname=$(hostname 2>/dev/null)"
[ -r /etc/os-release ] && . /etc/os-release; echo "os=${PRETTY_NAME:-$(uname -s)}"
echo "kernel=$(uname -r)"
cpu=$(grep -m1 -E '^(model name|Hardware|Processor)' /proc/cpuinfo 2>/dev/null | cut -d: -f2-)
[ -n "$cpu" ] || cpu=$(lscpu 2>/dev/null | sed -n 's/^Model name:[[:space:]]*//p' | head -1)
echo "cpu=$cpu"
echo "cores=$(nproc 2>/dev/null)"
echo "mem=$(awk '/^MemTotal/ { print $2 * 1024 }' /proc/meminfo 2>/dev/null)"
echo "ips=$(hostname -I 2>/dev/null)"
echo "date=$(date +%F)"
echo '@@lsblk'
lsblk -P -b -o NAME,KNAME,PKNAME,TYPE,SIZE,ROTA,TRAN,MODEL 2>/dev/null
echo '@@df'
df -PTB1 2>/dev/null | tail -n +2
echo '@@listen'
ss -ltnuH 2>/dev/null || netstat -ltnu 2>/dev/null | tail -n +3
echo '@@nginx'
for f in /etc/nginx/sites-enabled/* /etc/nginx/conf.d/*.conf; do [ -f "$f" ] && [ -r "$f" ] && { echo "## file $f"; cat "$f"; echo; }; done
command -v docker >/dev/null 2>&1 || exit 0
echo '@@docker'
docker info --format '{{.ServerVersion}}{{"\t"}}{{.DockerRootDir}}' 2>/dev/null
echo '@@containers'
ids=$(docker ps -aq --no-trunc 2>/dev/null)
[ -n "$ids" ] && docker inspect --format '{{.Name}}{{"\t"}}{{.Config.Image}}{{"\t"}}{{.State.Status}}{{"\t"}}{{index .Config.Labels "com.docker.compose.project"}}{{"\t"}}{{index .Config.Labels "com.docker.compose.service"}}{{"\t"}}{{range $k, $v := .NetworkSettings.Networks}}{{$k}}|{{$v.IPAddress}}|{{range $v.Aliases}}{{.}},{{end}} {{end}}{{"\t"}}{{range $p, $b := .HostConfig.PortBindings}}{{$p}}={{range $b}}{{.HostIp}}/{{.HostPort}},{{end}} {{end}}{{"\t"}}{{range $p, $v := .Config.ExposedPorts}}{{$p}} {{end}}{{"\t"}}{{range .Mounts}}{{.Type}}|{{if eq .Type "volume"}}{{.Name}}{{end}}|{{.Source}}|{{.Destination}};{{end}}{{"\t"}}{{.HostConfig.NetworkMode}}' $ids 2>/dev/null
echo '@@networks'
nets=$(docker network ls -q 2>/dev/null)
[ -n "$nets" ] && docker network inspect --format '{{.Name}}{{"\t"}}{{.Driver}}{{"\t"}}{{range .IPAM.Config}}{{.Subnet}} {{end}}{{"\t"}}{{range .Containers}}{{.Name}} {{end}}' $nets 2>/dev/null
npm=$(docker ps --format '{{.ID}} {{.Image}}' 2>/dev/null | awk '$2 ~ /nginx-proxy-manager/ { print $1; exit }')
[ -n "$npm" ] || exit 0
echo '@@npm'
docker exec $npm sh -c 'cd /data/nginx/proxy_host 2>/dev/null && for f in *.conf; do echo "## conf ${f%.conf}"; grep -hE "^[[:space:]]*(server_name|set .forward_scheme|set .server|set .port|ssl_certificate)[[:space:]]" "$f"; done' 2>/dev/null
true"###;

pub async fn snapshot(session: &Session) -> AppResult<HomelabSnapshot> {
    Ok(parse(&session.exec_read(SCRIPT).await?.stdout))
}

fn sections(out: &str) -> HashMap<&str, Vec<&str>> {
    let mut map: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut cur = "";
    for l in out.lines() {
        if let Some(name) = l.strip_prefix("@@") {
            cur = name.trim();
            map.entry(cur).or_default();
        } else if !cur.is_empty() {
            map.entry(cur).or_default().push(l);
        }
    }
    map
}

/// `KEY="value" KEY2="value 2"` (lsblk -P).
fn pairs(l: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    let mut rest = l.trim();
    while let Some((k, v)) = rest.split_once("=\"") {
        let Some(end) = v.find('"') else { break };
        m.insert(k.trim().to_string(), v[..end].trim().to_string());
        rest = &v[end + 1..];
    }
    m
}

struct Ct {
    name: String,
    service: String,
    aliases: Vec<String>,
    ips: Vec<String>,
    /// (container port/proto, host ip, host port)
    published: Vec<(String, String, u16)>,
    host_net: bool,
}

fn is_loopback(addr: &str) -> bool {
    let a = addr.trim_start_matches("::ffff:");
    a.starts_with("127.") || a == "::1" || a == "localhost"
}

/// `0.0.0.0:22`, `[::]:22`, `*:22`, `127.0.0.53%lo:53` → (address, port).
fn split_addr(s: &str) -> Option<(String, u16)> {
    let (a, p) = s.rsplit_once(':')?;
    let port = p.parse().ok()?;
    let a = a.trim_start_matches('[').trim_end_matches(']');
    let a = a.split('%').next().unwrap_or(a);
    Some((a.to_string(), port))
}

fn well_known(port: u16) -> &'static str {
    match port {
        21 => "ftp",
        22 => "ssh",
        25 | 465 | 587 => "smtp",
        53 => "dns",
        80 => "http",
        111 => "rpcbind",
        143 | 993 => "imap",
        443 => "https",
        445 => "smb",
        1194 => "openvpn",
        2049 => "nfs",
        3306 => "mysql",
        3389 => "rdp",
        5432 => "postgresql",
        6379 => "redis",
        9100 => "node-exporter",
        27017 => "mongodb",
        51820 => "wireguard",
        _ => "",
    }
}

/// `http://127.0.0.1:8080/api` → ("127.0.0.1", 8080).
fn upstream(target: &str) -> (String, u16) {
    let (scheme, rest) = target.split_once("://").unwrap_or(("http", target));
    let auth = rest.split('/').next().unwrap_or(rest);
    let default = if scheme == "https" { 443 } else { 80 };
    match auth.rsplit_once(':') {
        Some((h, p)) if !h.ends_with(']') || h.starts_with('[') => match p.parse() {
            Ok(p) => (h.trim_matches(['[', ']']).to_lowercase(), p),
            Err(_) => (auth.to_lowercase(), default),
        },
        _ => (auth.to_lowercase(), default),
    }
}

/// Longest mount point that contains `path`.
fn fs_of<'a>(mounts: &'a [String], path: &str) -> Option<&'a String> {
    mounts
        .iter()
        .filter(|m| m.as_str() == "/" || path == m.as_str() || path.starts_with(&format!("{m}/")))
        .max_by_key(|m| m.len())
}

pub fn parse(out: &str) -> HomelabSnapshot {
    let sec = sections(out);
    let get = |k: &str| sec.get(k).cloned().unwrap_or_default();
    let mut snap = HomelabSnapshot::default();

    for l in get("host") {
        let Some((k, v)) = l.split_once('=') else { continue };
        let v = v.trim();
        match k {
            "hostname" => snap.hostname = v.into(),
            "os" => snap.os = v.into(),
            "kernel" => snap.kernel = v.into(),
            "cpu" => snap.cpu = v.split_whitespace().collect::<Vec<_>>().join(" "),
            "cores" => snap.cores = v.parse().unwrap_or(0),
            "mem" => snap.mem_total = v.parse::<f64>().map(|m| m as u64).unwrap_or(0),
            "ips" => snap.ips = v.split_whitespace().filter(|ip| !ip.contains(':') || !ip.starts_with("fe80")).map(String::from).collect(),
            "date" => snap.date = v.into(),
            _ => {}
        }
    }

    if let Some(l) = get("docker").first() {
        let mut f = l.split('\t');
        snap.docker_version = f.next().unwrap_or("").trim().into();
        snap.docker_root = f.next().unwrap_or("").trim().into();
    }

    // ---- Containers ----
    let mut cts: Vec<Ct> = Vec::new();
    for l in get("containers") {
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() < 10 {
            continue;
        }
        let name = f[0].trim().trim_start_matches('/').to_string();
        let mut c = HomelabContainer {
            name: name.clone(),
            image: f[1].trim().into(),
            state: f[2].trim().into(),
            project: f[3].trim().trim_start_matches("<no value>").into(),
            ..Default::default()
        };
        let mut ct = Ct {
            name,
            service: f[4].trim().trim_start_matches("<no value>").into(),
            aliases: Vec::new(),
            ips: Vec::new(),
            published: Vec::new(),
            host_net: f[9].trim() == "host",
        };
        for n in f[5].split_whitespace() {
            let mut p = n.split('|');
            let net = p.next().unwrap_or("").to_string();
            let ip = p.next().unwrap_or("").to_string();
            ct.aliases.extend(p.next().unwrap_or("").split(',').filter(|a| !a.is_empty()).map(|a| a.to_lowercase()));
            if !ip.is_empty() {
                ct.ips.push(ip.clone());
            }
            c.networks.push((net, ip));
        }
        let mut seen = Vec::new();
        for b in f[6].split_whitespace() {
            let Some((cport, binds)) = b.split_once('=') else { continue };
            for bind in binds.split(',').filter(|s| !s.is_empty()) {
                let (hip, hport) = bind.rsplit_once('/').unwrap_or(("", bind));
                let Ok(hport) = hport.parse::<u16>() else { continue };
                let label = format!("{hport}→{cport}");
                if !c.ports.contains(&label) {
                    c.ports.push(label);
                }
                if !is_loopback(hip) {
                    c.public = true;
                }
                ct.published.push((cport.to_string(), hip.to_string(), hport));
            }
            seen.push(cport.to_string());
        }
        for p in f[7].split_whitespace() {
            if !seen.iter().any(|s| s == p) {
                c.ports.push(p.to_string());
            }
        }
        for m in f[8].split(';').filter(|m| !m.is_empty()) {
            let p: Vec<&str> = m.split('|').collect();
            if p.len() < 4 || !(p[0] == "volume" || p[0] == "bind") {
                continue;
            }
            c.mounts.push(HomelabMount {
                kind: p[0].into(),
                source: if p[0] == "volume" && !p[1].is_empty() { p[1].into() } else { p[2].into() },
                destination: p[3].into(),
                // Temporarily the host path; replaced by its filesystem below.
                fs: p[2].into(),
            });
        }
        snap.containers.push(c);
        cts.push(ct);
    }

    for l in get("networks") {
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() < 4 {
            continue;
        }
        snap.networks.push(HomelabNetwork {
            name: f[0].trim().into(),
            driver: f[1].trim().into(),
            subnet: f[2].split_whitespace().collect::<Vec<_>>().join(", "),
            containers: f[3].split_whitespace().map(String::from).collect(),
        });
    }
    // Default networks without any container are noise on a diagram.
    snap.networks.retain(|n| !(matches!(n.name.as_str(), "host" | "none") || (n.name == "bridge" && n.containers.is_empty())));

    // ---- Proxy routes ----
    let host_ips: Vec<String> = snap.ips.clone();
    let resolve = |host: &str, port: u16| -> Option<usize> {
        let by_name = cts.iter().position(|c| {
            c.name.eq_ignore_ascii_case(host) || c.service.eq_ignore_ascii_case(host) || c.aliases.iter().any(|a| a == host) || c.ips.iter().any(|ip| ip == host)
        });
        if by_name.is_some() {
            return by_name;
        }
        let to_host = is_loopback(host)
            || host == "host.docker.internal"
            || host_ips.iter().any(|ip| ip == host)
            || host.ends_with(".1") // a Docker gateway
            || host.parse::<std::net::IpAddr>().is_err();
        if !to_host {
            return None;
        }
        cts.iter().position(|c| c.published.iter().any(|(_, _, hp)| *hp == port))
    };

    // NPM: "## conf <id>" blocks.
    let mut npm: BTreeMap<String, (Vec<String>, String, String, u16, bool)> = BTreeMap::new();
    let mut cur = String::new();
    for l in get("npm") {
        if let Some(id) = l.strip_prefix("## conf ") {
            cur = id.trim().to_string();
            npm.entry(cur.clone()).or_insert_with(|| (Vec::new(), "http".into(), String::new(), 0, false));
            continue;
        }
        let Some(h) = npm.get_mut(&cur) else { continue };
        let t = l.trim().trim_end_matches(';');
        if let Some(v) = t.strip_prefix("server_name") {
            h.0.extend(v.split_whitespace().filter(|n| *n != "_").map(String::from));
        } else if let Some(v) = t.strip_prefix("set $forward_scheme") {
            h.1 = v.trim().trim_matches('"').into();
        } else if let Some(v) = t.strip_prefix("set $server") {
            h.2 = v.trim().trim_matches('"').to_lowercase();
        } else if let Some(v) = t.strip_prefix("set $port") {
            h.3 = v.trim().parse().unwrap_or(0);
        } else if t.starts_with("ssl_certificate") {
            h.4 = true;
        }
    }
    for (domains, scheme, server, port, ssl) in npm.into_values() {
        if server.is_empty() || domains.is_empty() {
            continue;
        }
        let container = resolve(&server, port).map(|i| cts[i].name.clone());
        snap.proxies.push(HomelabProxy { domains, target: format!("{scheme}://{server}:{port}"), container, ssl, source: "npm".into() });
    }

    // Native nginx: "## file <path>" blocks.
    let mut files: Vec<(String, String)> = Vec::new();
    for l in get("nginx") {
        if let Some(p) = l.strip_prefix("## file ") {
            files.push((p.trim().into(), String::new()));
        } else if let Some((_, body)) = files.last_mut() {
            body.push_str(l);
            body.push('\n');
        }
    }
    for (_, body) in &files {
        let (blocks, _) = parse_config(body);
        for b in blocks {
            let domains: Vec<String> = b.server_names.iter().filter(|n| !matches!(n.as_str(), "_" | "localhost" | "")).cloned().collect();
            if domains.is_empty() {
                continue;
            }
            let ssl = b.ssl_certificate.is_some() || b.listen.iter().any(|l| l.contains("443") || l.contains("ssl"));
            let targets: Vec<String> = if b.proxy_pass.is_empty() {
                b.root.iter().map(|r| format!("root {r}")).collect()
            } else {
                b.proxy_pass.clone()
            };
            for t in targets {
                let container = if t.starts_with("root ") {
                    None
                } else {
                    let (h, p) = upstream(&t);
                    resolve(&h, p).map(|i| cts[i].name.clone())
                };
                let dup = snap.proxies.iter_mut().find(|p| p.source == "nginx" && p.target == t && p.container == container);
                match dup {
                    // http → https redirect blocks and the TLS block share the target.
                    Some(p) => {
                        for d in &domains {
                            if !p.domains.contains(d) {
                                p.domains.push(d.clone());
                            }
                        }
                        p.ssl |= ssl;
                    }
                    None => snap.proxies.push(HomelabProxy { domains: domains.clone(), target: t, container, ssl, source: "nginx".into() }),
                }
            }
        }
    }
    for p in &snap.proxies {
        let Some(name) = &p.container else { continue };
        if let Some(c) = snap.containers.iter_mut().find(|c| &c.name == name) {
            for d in &p.domains {
                if !c.domains.contains(d) {
                    c.domains.push(d.clone());
                }
            }
            c.public = true;
        }
    }

    // ---- Open ports ----
    let native_nginx = !files.is_empty();
    let mut ports: BTreeMap<(u16, String), String> = BTreeMap::new();
    for l in get("listen") {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() < 4 {
            continue;
        }
        // ss: Netid State Recv-Q Send-Q Local Peer…  netstat: Proto Recv-Q Send-Q Local Foreign [State]
        let (proto, local) = if f[1].parse::<u64>().is_ok() { (f[0], f[3]) } else if f.len() >= 5 { (f[0], f[4]) } else { continue };
        let proto = if proto.starts_with("udp") { "udp" } else { "tcp" };
        let Some((addr, port)) = split_addr(local) else { continue };
        if is_loopback(&addr) {
            continue;
        }
        let service = cts
            .iter()
            .find(|c| c.published.iter().any(|(cp, hip, hp)| *hp == port && cp.ends_with(proto) && !is_loopback(hip)))
            .map(|c| c.name.clone())
            .or_else(|| (native_nginx && (port == 80 || port == 443)).then(|| "nginx".to_string()))
            .unwrap_or_else(|| well_known(port).to_string());
        ports.entry((port, proto.to_string())).or_insert(service);
    }
    // Containers on the host network listen directly: name them when the port is theirs alone.
    let host_net: Vec<&Ct> = cts.iter().filter(|c| c.host_net).collect();
    if host_net.len() == 1 {
        for s in ports.values_mut().filter(|s| s.is_empty()) {
            *s = host_net[0].name.clone();
        }
    }
    snap.open_ports = ports.into_iter().map(|((port, proto), service)| HomelabPort { port, proto, service }).collect();

    // ---- Disks and filesystems ----
    struct Blk {
        name: String,
        pk: String,
        ty: String,
        size: u64,
        rota: String,
        tran: String,
        model: String,
    }
    let blks: Vec<Blk> = get("lsblk")
        .iter()
        .map(|l| pairs(l))
        .filter(|m| m.contains_key("NAME"))
        .map(|m| {
            let g = |k: &str| m.get(k).cloned().unwrap_or_default();
            Blk { name: g("NAME"), pk: g("PKNAME"), ty: g("TYPE"), size: g("SIZE").parse().unwrap_or(0), rota: g("ROTA"), tran: g("TRAN"), model: g("MODEL") }
        })
        .collect();
    let kname: HashMap<String, usize> = get("lsblk")
        .iter()
        .map(|l| pairs(l))
        .enumerate()
        .filter_map(|(i, m)| m.get("KNAME").cloned().map(|k| (k, i)))
        .collect();
    let disk_of = |dev: &str| -> Option<usize> {
        let base = dev.trim_start_matches("/dev/mapper/").trim_start_matches("/dev/");
        let mut i = blks.iter().position(|b| b.name == base).or_else(|| kname.get(base).copied())?;
        for _ in 0..8 {
            if blks[i].ty == "disk" {
                return Some(i);
            }
            i = kname.get(&blks[i].pk).copied().or_else(|| blks.iter().position(|b| b.name == blks[i].pk))?;
        }
        None
    };

    let mut fss: Vec<(Option<usize>, HomelabFs)> = Vec::new();
    let mut all_mounts: Vec<String> = Vec::new();
    for l in get("df") {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() < 7 {
            continue;
        }
        let (source, fstype, mount) = (f[0], f[1], f[6..].join(" "));
        let skip_type = matches!(fstype, "tmpfs" | "devtmpfs" | "overlay" | "squashfs" | "efivarfs" | "ramfs" | "nsfs" | "shm" | "devfs")
            || fstype.starts_with("fuse.lxcfs");
        let skip_mount = ["/run", "/snap", "/sys", "/proc", "/dev", "/boot", "/var/lib/docker/"].iter().any(|p| mount.starts_with(p));
        if skip_type || skip_mount {
            continue;
        }
        all_mounts.push(mount.clone());
        if fss.iter().any(|(_, fs)| fs.source == source) {
            continue;
        }
        let fs = HomelabFs {
            source: source.into(),
            mount: mount.clone(),
            fstype: fstype.into(),
            size: f[2].parse().unwrap_or(0),
            used: f[3].parse().unwrap_or(0),
            ..Default::default()
        };
        fss.push((if source.starts_with("/dev/") { disk_of(source) } else { None }, fs));
    }
    // A mount point → its filesystem (same source mounted twice counts once).
    let df_rows: Vec<(String, String)> = get("df")
        .iter()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            (f.len() >= 7).then(|| (f[6..].join(" "), f[0].to_string()))
        })
        .collect();
    let fs_index = |path: &str| -> Option<usize> {
        let m = fs_of(&all_mounts, path)?;
        let src = df_rows.iter().find(|(mp, _)| mp == m).map(|(_, s)| s.clone())?;
        fss.iter().position(|(_, fs)| fs.source == src)
    };
    let mut users: Vec<Vec<String>> = vec![Vec::new(); fss.len()];
    for c in &mut snap.containers {
        for m in &mut c.mounts {
            let host_path = std::mem::take(&mut m.fs);
            if let Some(i) = fs_index(&host_path) {
                m.fs = fss[i].1.mount.clone();
                if !users[i].contains(&c.name) {
                    users[i].push(c.name.clone());
                }
            }
        }
    }
    let root_fs = if snap.docker_root.is_empty() { None } else { fs_index(&snap.docker_root) };
    for (i, (_, fs)) in fss.iter_mut().enumerate() {
        fs.containers = std::mem::take(&mut users[i]);
        fs.docker_root = root_fs == Some(i);
    }

    for (i, b) in blks.iter().enumerate() {
        if b.ty != "disk" || b.size == 0 || b.name.starts_with("zram") || b.name.starts_with("loop") {
            continue;
        }
        let kind = if b.tran == "nvme" || b.name.starts_with("nvme") {
            "NVMe"
        } else if b.rota == "0" {
            "SSD"
        } else if b.rota == "1" {
            "HDD"
        } else {
            ""
        };
        snap.disks.push(HomelabDisk {
            name: b.name.clone(),
            model: b.model.clone(),
            kind: kind.into(),
            size: b.size,
            filesystems: fss.iter().filter(|(d, _)| *d == Some(i)).map(|(_, fs)| fs.clone()).collect(),
        });
    }
    // ZFS datasets, network shares, devices lsblk did not list: grouped by pool / server.
    for (d, fs) in fss.into_iter().filter(|(d, _)| d.is_none()) {
        let _ = d;
        let group = fs.source.split(['/', ':']).find(|s| !s.is_empty()).unwrap_or(&fs.source).to_string();
        match snap.disks.iter_mut().find(|d| d.size == 0 && d.name == group) {
            Some(d) => d.filesystems.push(fs),
            None => snap.disks.push(HomelabDisk { name: group, model: fs.fstype.to_uppercase(), kind: String::new(), size: 0, filesystems: vec![fs] }),
        }
    }
    snap
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUT: &str = "@@host
hostname=terra
os=Debian GNU/Linux 12 (bookworm)
kernel=6.1.0-18-amd64
cpu= Intel(R) Core(TM) i5-8500T CPU @ 2.10GHz
cores=6
mem=33554432000
ips=192.168.1.10 172.17.0.1 fe80::1
date=2026-11-11
@@lsblk
NAME=\"nvme0n1\" KNAME=\"nvme0n1\" PKNAME=\"\" TYPE=\"disk\" SIZE=\"512110190592\" ROTA=\"0\" TRAN=\"nvme\" MODEL=\"Samsung SSD 970 EVO\"
NAME=\"nvme0n1p1\" KNAME=\"nvme0n1p1\" PKNAME=\"nvme0n1\" TYPE=\"part\" SIZE=\"536870912\" ROTA=\"0\" TRAN=\"nvme\" MODEL=\"\"
NAME=\"nvme0n1p2\" KNAME=\"nvme0n1p2\" PKNAME=\"nvme0n1\" TYPE=\"part\" SIZE=\"511000000000\" ROTA=\"0\" TRAN=\"nvme\" MODEL=\"\"
NAME=\"vg-root\" KNAME=\"dm-0\" PKNAME=\"nvme0n1p2\" TYPE=\"lvm\" SIZE=\"511000000000\" ROTA=\"0\" TRAN=\"\" MODEL=\"\"
NAME=\"sda\" KNAME=\"sda\" PKNAME=\"\" TYPE=\"disk\" SIZE=\"4000787030016\" ROTA=\"1\" TRAN=\"sata\" MODEL=\"WDC WD40EFRX\"
NAME=\"sda1\" KNAME=\"sda1\" PKNAME=\"sda\" TYPE=\"part\" SIZE=\"4000785960960\" ROTA=\"1\" TRAN=\"sata\" MODEL=\"\"
@@df
/dev/mapper/vg-root ext4 502000000000 120000000000 360000000000 25% /
/dev/nvme0n1p1 vfat 536000000 6000000 530000000 2% /boot/efi
/dev/sda1 ext4 3936000000000 1900000000000 1836000000000 51% /srv/media
tmpfs tmpfs 1000 0 1000 0% /run
overlay overlay 502000000000 120000000000 360000000000 25% /var/lib/docker/overlay2/abc/merged
@@listen
tcp   LISTEN 0 128 0.0.0.0:22 0.0.0.0:*
tcp   LISTEN 0 511 0.0.0.0:80 0.0.0.0:*
tcp   LISTEN 0 511 [::]:443 [::]:*
tcp   LISTEN 0 4096 0.0.0.0:8096 0.0.0.0:*
tcp   LISTEN 0 4096 127.0.0.1:5432 0.0.0.0:*
udp   UNCONN 0 0 0.0.0.0:51820 0.0.0.0:*
@@nginx
## file /etc/nginx/sites-enabled/cloud
server {
  listen 443 ssl;
  server_name cloud.example.com;
  ssl_certificate /etc/letsencrypt/live/cloud/fullchain.pem;
  location / { proxy_pass http://127.0.0.1:8081; }
}
server {
  listen 80;
  server_name cloud.example.com;
  location / { proxy_pass http://127.0.0.1:8081; }
}
@@docker
27.3.1\t/var/lib/docker
@@containers
/jellyfin\tjellyfin/jellyfin:latest\trunning\tmedia\tjellyfin\tmedia_default|172.20.0.2|jellyfin,abc, \t8096/tcp=/8096, \t8096/tcp 1900/udp \tbind||/srv/media/movies|/media;volume|jellyfin_config|/var/lib/docker/volumes/jellyfin_config/_data|/config;\tmedia_default
/nextcloud\tnextcloud:29\trunning\tcloud\tapp\tcloud_default|172.21.0.3|app, \t80/tcp=127.0.0.1/8081, \t80/tcp \tvolume|cloud_data|/var/lib/docker/volumes/cloud_data/_data|/var/www/html;\tcloud_default
/npm\tjc21/nginx-proxy-manager:latest\trunning\t\t\tproxy|172.22.0.2| \t81/tcp=0.0.0.0/81, \t80/tcp 81/tcp 443/tcp \t\tproxy
/db\tpostgres:16\texited\tcloud\tdb\t\t\t5432/tcp \tvolume|cloud_db|/var/lib/docker/volumes/cloud_db/_data|/var/lib/postgresql/data;\tcloud_default
@@networks
bridge\tbridge\t172.17.0.0/16 \t
media_default\tbridge\t172.20.0.0/16 \tjellyfin
host\thost\t\t
@@npm
## conf 1
  server_name films.example.com;
  set $forward_scheme http;
  set $server \"jellyfin\";
  set $port 8096;
  ssl_certificate /etc/letsencrypt/live/npm-1/fullchain.pem;
";

    #[test]
    fn parses_snapshot() {
        let s = parse(OUT);
        assert_eq!(s.hostname, "terra");
        assert_eq!(s.cpu, "Intel(R) Core(TM) i5-8500T CPU @ 2.10GHz");
        assert_eq!(s.cores, 6);
        assert_eq!(s.ips, vec!["192.168.1.10", "172.17.0.1"]);
        assert_eq!(s.docker_version, "27.3.1");

        // Disks: LVM resolved to its NVMe, the HDD holds the media bind mount.
        assert_eq!(s.disks.len(), 2);
        let nvme = &s.disks[0];
        assert_eq!((nvme.name.as_str(), nvme.kind.as_str()), ("nvme0n1", "NVMe"));
        assert_eq!(nvme.filesystems.len(), 1);
        assert_eq!(nvme.filesystems[0].mount, "/");
        assert!(nvme.filesystems[0].docker_root);
        assert_eq!(nvme.filesystems[0].containers, vec!["jellyfin", "nextcloud", "db"]);
        let hdd = &s.disks[1];
        assert_eq!(hdd.kind, "HDD");
        assert_eq!(hdd.filesystems[0].containers, vec!["jellyfin"]);

        let jf = &s.containers[0];
        assert_eq!(jf.project, "media");
        assert_eq!(jf.ports, vec!["8096→8096/tcp", "1900/udp"]);
        assert_eq!(jf.mounts[0].fs, "/srv/media");
        assert_eq!(jf.mounts[1].source, "jellyfin_config");
        assert_eq!(jf.domains, vec!["films.example.com"]);
        assert!(jf.public);

        // Native nginx: both blocks merged, routed to the container publishing 8081 on loopback.
        let nc = &s.containers[1];
        assert_eq!(nc.domains, vec!["cloud.example.com"]);
        let p = s.proxies.iter().find(|p| p.source == "nginx").unwrap();
        assert!(p.ssl);
        assert_eq!(p.container.as_deref(), Some("nextcloud"));

        assert!(!s.containers[3].public);
        assert_eq!(s.networks.iter().map(|n| n.name.as_str()).collect::<Vec<_>>(), vec!["media_default"]);

        let ports: Vec<(u16, &str, &str)> = s.open_ports.iter().map(|p| (p.port, p.proto.as_str(), p.service.as_str())).collect();
        assert_eq!(ports, vec![(22, "tcp", "ssh"), (80, "tcp", "nginx"), (443, "tcp", "nginx"), (8096, "tcp", "jellyfin"), (51820, "udp", "wireguard")]);
    }

    #[test]
    fn upstreams() {
        assert_eq!(upstream("http://127.0.0.1:8080/api"), ("127.0.0.1".into(), 8080));
        assert_eq!(upstream("https://app"), ("app".into(), 443));
        assert_eq!(split_addr("[::]:443"), Some(("::".into(), 443)));
        assert_eq!(split_addr("127.0.0.53%lo:53"), Some(("127.0.0.53".into(), 53)));
    }
}

/// Parses a real capture: `HL_OUT=out.txt cargo test -p sshos homelab_real -- --ignored --nocapture`.
#[cfg(test)]
#[test]
#[ignore]
fn homelab_real() {
    let Ok(p) = std::env::var("HL_OUT") else { return };
    let s = parse(&std::fs::read_to_string(p).unwrap());
    for c in &s.containers {
        println!("{:28} {:10} nets={:?} ports={:?} domains={:?} public={} mounts={:?}", c.name, c.state, c.networks.iter().map(|n| &n.0).collect::<Vec<_>>(), c.ports, c.domains, c.public, c.mounts.iter().map(|m| (&m.source, &m.fs)).collect::<Vec<_>>());
    }
    for p in &s.proxies {
        println!("PROXY {:?} -> {} = {:?}", p.domains, p.target, p.container);
    }
    for d in &s.disks {
        println!("DISK {} {} {}", d.name, d.kind, d.size);
        for f in &d.filesystems {
            println!("   {} {} {:?}", f.mount, f.docker_root, f.containers);
        }
    }
    println!("PORTS {:?}", s.open_ports.iter().map(|p| format!("{}/{} {}", p.port, p.proto, p.service)).collect::<Vec<_>>());
}
