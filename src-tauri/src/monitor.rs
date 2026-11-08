//! System monitor: one shell round-trip per refresh, parsed locally.

use std::collections::HashMap;
use std::time::Instant;

use sshos_shared::{DiskUsage, GpuInfo, MonitorSnapshot, NetRate, ProcessInfo, SystemInfo};
use tokio::sync::Mutex;

use crate::error::AppResult;
use crate::ssh::Session;

const SCRIPT: &str = r#"LC_ALL=C; export LC_ALL
echo '@@stat'; grep '^cpu' /proc/stat
echo '@@cores'; grep -c '^processor' /proc/cpuinfo
echo '@@load'; cat /proc/loadavg
echo '@@mem'; cat /proc/meminfo
echo '@@uptime'; cat /proc/uptime
echo '@@net'; cat /proc/net/dev
echo '@@diskstats'; cat /proc/diskstats 2>/dev/null
echo '@@df'; df -P -B1 -x tmpfs -x devtmpfs -x squashfs -x overlay 2>/dev/null || df -P -k
echo '@@ps'; ps -eo pid=,user=,pcpu=,pmem=,args= --sort=-pcpu 2>/dev/null | head -40
echo '@@gpu'; command -v nvidia-smi >/dev/null 2>&1 && nvidia-smi --query-gpu=name,utilization.gpu,memory.used,memory.total,temperature.gpu --format=csv,noheader,nounits 2>/dev/null
echo '@@info'; hostname 2>/dev/null || cat /etc/hostname; (. /etc/os-release 2>/dev/null; echo "${PRETTY_NAME:-$(uname -s)}"); uname -r; uname -m; grep -m1 'model name' /proc/cpuinfo 2>/dev/null | cut -d: -f2-
true"#;

#[derive(Clone)]
struct Prev {
    /// `cpu`, `cpu0`… → the first 8 jiffy counters of /proc/stat.
    cpu: HashMap<String, Vec<u64>>,
    net: HashMap<String, (u64, u64)>,
    /// Sectors read / written, summed over physical disks.
    disk: (u64, u64),
    at: Instant,
}

#[derive(Default)]
pub struct Monitor {
    prev: Mutex<HashMap<String, Prev>>,
}

impl Monitor {
    pub async fn snapshot(&self, host_id: &str, session: &Session) -> AppResult<MonitorSnapshot> {
        let out = session.exec_quiet(SCRIPT).await?.stdout;
        let sections = split_sections(&out);
        let mut prev_map = self.prev.lock().await;
        let prev = prev_map.get(host_id).cloned();
        let (snap, next) = parse(&sections, prev.as_ref());
        prev_map.insert(host_id.to_string(), next);
        Ok(snap)
    }
}

fn split_sections(out: &str) -> HashMap<&str, Vec<&str>> {
    let mut map: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut cur = "";
    for line in out.lines() {
        if let Some(name) = line.strip_prefix("@@") {
            cur = name.trim();
            map.entry(cur).or_default();
        } else if !cur.is_empty() {
            map.entry(cur).or_default().push(line);
        }
    }
    map
}

/// Whole physical disks only (no partitions, loop, ram or device-mapper devices).
fn is_whole_disk(name: &str) -> bool {
    let tail_digits = |prefix: &str| name.strip_prefix(prefix).is_some_and(|r| !r.is_empty() && r.chars().all(|c| c.is_ascii_lowercase()));
    if tail_digits("sd") || tail_digits("vd") || tail_digits("xvd") || tail_digits("hd") {
        return true;
    }
    // nvme0n1 (not nvme0n1p1), mmcblk0 (not mmcblk0p1)
    if let Some(r) = name.strip_prefix("nvme") {
        return r.contains('n') && !r.contains('p');
    }
    if let Some(r) = name.strip_prefix("mmcblk") {
        return !r.is_empty() && r.chars().all(|c| c.is_ascii_digit());
    }
    false
}

/// Percentages of a jiffy delta: (busy, user, system, iowait, steal).
fn cpu_split(now: &[u64], before: &[u64]) -> Option<(f32, f32, f32, f32, f32)> {
    let d = |i: usize| now.get(i).copied().unwrap_or(0).saturating_sub(before.get(i).copied().unwrap_or(0)) as f32;
    let total: f32 = (0..8).map(d).sum();
    if total <= 0.0 {
        return None;
    }
    let idle = d(3) + d(4);
    let pct = |v: f32| (100.0 * v / total).clamp(0.0, 100.0);
    Some((pct(total - idle), pct(d(0) + d(1)), pct(d(2) + d(5) + d(6)), pct(d(4)), pct(d(7))))
}

fn parse(s: &HashMap<&str, Vec<&str>>, prev: Option<&Prev>) -> (MonitorSnapshot, Prev) {
    let get = |k: &str| s.get(k).cloned().unwrap_or_default();
    let now = Instant::now();
    let mut snap = MonitorSnapshot::default();

    // CPU: aggregate line + one line per core.
    let mut cpu_now: HashMap<String, Vec<u64>> = HashMap::new();
    let mut core_names = vec![];
    for l in get("stat") {
        let mut it = l.split_whitespace();
        let Some(name) = it.next() else { continue };
        let v: Vec<u64> = it.take(8).filter_map(|x| x.parse().ok()).collect();
        if name != "cpu" {
            core_names.push(name.to_string());
        }
        cpu_now.insert(name.to_string(), v);
    }
    if let Some(p) = prev {
        if let (Some(a), Some(b)) = (cpu_now.get("cpu"), p.cpu.get("cpu")) {
            if let Some((busy, user, sys, io, steal)) = cpu_split(a, b) {
                snap.cpu_percent = busy;
                snap.cpu_user = user;
                snap.cpu_system = sys;
                snap.cpu_iowait = io;
                snap.cpu_steal = steal;
            }
        }
        core_names.sort_by_key(|n| n[3..].parse::<u32>().unwrap_or(0));
        snap.per_core = core_names
            .iter()
            .map(|n| match (cpu_now.get(n), p.cpu.get(n)) {
                (Some(a), Some(b)) => cpu_split(a, b).map(|x| x.0).unwrap_or(0.0),
                _ => 0.0,
            })
            .collect();
    }
    snap.cores = get("cores").first().and_then(|l| l.trim().parse().ok()).unwrap_or(1);

    if let Some(l) = get("load").first() {
        let v: Vec<f32> = l.split_whitespace().take(3).filter_map(|x| x.parse().ok()).collect();
        if v.len() == 3 {
            snap.load = [v[0], v[1], v[2]];
        }
    }

    // Memory (kB)
    let mut mem: HashMap<&str, u64> = HashMap::new();
    for l in get("mem") {
        if let Some((k, v)) = l.split_once(':') {
            let n = v.split_whitespace().next().and_then(|x| x.parse().ok()).unwrap_or(0);
            mem.insert(k.trim(), n * 1024);
        }
    }
    let m = |k: &str| mem.get(k).copied().unwrap_or(0);
    snap.mem_total = m("MemTotal");
    let avail = mem.get("MemAvailable").copied().unwrap_or_else(|| m("MemFree") + m("Buffers") + m("Cached"));
    snap.mem_used = snap.mem_total.saturating_sub(avail);
    snap.mem_available = avail;
    snap.mem_buffers = m("Buffers");
    snap.mem_cached = m("Cached") + m("SReclaimable");
    snap.swap_total = m("SwapTotal");
    snap.swap_used = snap.swap_total.saturating_sub(m("SwapFree"));

    snap.uptime_s = get("uptime")
        .first()
        .and_then(|l| l.split_whitespace().next())
        .and_then(|x| x.parse::<f64>().ok())
        .unwrap_or(0.0) as u64;

    let secs = prev.map(|p| now.duration_since(p.at).as_secs_f64().max(0.001));

    // Network
    let mut net_now = HashMap::new();
    for l in get("net").iter().skip(2) {
        let Some((iface, rest)) = l.split_once(':') else { continue };
        let iface = iface.trim().to_string();
        // Loopback and per-container veth pairs (dozens on a Docker host) are noise.
        if iface == "lo" || iface.starts_with("veth") {
            continue;
        }
        let v: Vec<u64> = rest.split_whitespace().filter_map(|x| x.parse().ok()).collect();
        if v.len() >= 9 {
            net_now.insert(iface, (v[0], v[8]));
        }
    }
    if let (Some(p), Some(secs)) = (prev, secs) {
        let mut rates: Vec<NetRate> = net_now
            .iter()
            .map(|(k, (rx, tx))| {
                let (prx, ptx) = p.net.get(k).copied().unwrap_or((*rx, *tx));
                NetRate {
                    iface: k.clone(),
                    rx_bytes_per_s: rx.saturating_sub(prx) as f64 / secs,
                    tx_bytes_per_s: tx.saturating_sub(ptx) as f64 / secs,
                }
            })
            .collect();
        rates.sort_by(|a, b| a.iface.cmp(&b.iface));
        snap.net = rates;
    }

    // Disk I/O: /proc/diskstats fields 6 (sectors read) and 10 (sectors written), 512-byte sectors.
    let mut disk_now = (0u64, 0u64);
    for l in get("diskstats") {
        let v: Vec<&str> = l.split_whitespace().collect();
        if v.len() < 10 || !is_whole_disk(v[2]) {
            continue;
        }
        disk_now.0 += v[5].parse::<u64>().unwrap_or(0);
        disk_now.1 += v[9].parse::<u64>().unwrap_or(0);
    }
    if let (Some(p), Some(secs)) = (prev, secs) {
        snap.disk_read_bps = disk_now.0.saturating_sub(p.disk.0) as f64 * 512.0 / secs;
        snap.disk_write_bps = disk_now.1.saturating_sub(p.disk.1) as f64 * 512.0 / secs;
    }

    // Disks
    let df = get("df");
    let kb = df.first().map(|h| h.contains("1024-blocks")).unwrap_or(false);
    for l in df.iter().skip(1) {
        let v: Vec<&str> = l.split_whitespace().collect();
        if v.len() < 6 {
            continue;
        }
        let mul = if kb { 1024 } else { 1 };
        snap.disks.push(DiskUsage {
            fs: v[0].into(),
            total: v[1].parse::<u64>().unwrap_or(0) * mul,
            used: v[2].parse::<u64>().unwrap_or(0) * mul,
            mount: v[5..].join(" "),
        });
    }

    // Processes
    for l in get("ps") {
        let mut it = l.split_whitespace();
        let (Some(pid), Some(user), Some(cpu), Some(memp)) = (it.next(), it.next(), it.next(), it.next()) else {
            continue;
        };
        snap.processes.push(ProcessInfo {
            pid: pid.parse().unwrap_or(0),
            user: user.into(),
            cpu: cpu.parse().unwrap_or(0.0),
            mem: memp.parse().unwrap_or(0.0),
            command: it.collect::<Vec<_>>().join(" "),
        });
    }

    for l in get("gpu") {
        let v: Vec<&str> = l.split(',').map(str::trim).collect();
        if v.len() == 5 {
            snap.gpus.push(GpuInfo {
                name: v[0].into(),
                util: v[1].parse().unwrap_or(0.0),
                mem_used_mb: v[2].parse().unwrap_or(0),
                mem_total_mb: v[3].parse().unwrap_or(0),
                temp_c: v[4].parse().unwrap_or(0.0),
            });
        }
    }

    let info = get("info");
    let line = |i: usize| info.get(i).map(|s| s.trim().to_string()).unwrap_or_default();
    snap.info = SystemInfo { hostname: line(0), os: line(1), kernel: line(2), arch: line(3), cpu_model: line(4) };

    (snap, Prev { cpu: cpu_now, net: net_now, disk: disk_now, at: now })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "@@stat\ncpu  100 0 100 700 100 0 0 0 0 0\ncpu0 50 0 50 350 50 0 0 0 0 0\ncpu1 50 0 50 350 50 0 0 0 0 0\n@@cores\n4\n@@load\n0.50 0.40 0.30 1/200 1234\n@@mem\nMemTotal:       8000000 kB\nMemFree:        1000000 kB\nMemAvailable:   6000000 kB\nBuffers:        100 kB\nCached:         2000 kB\nSReclaimable:   500 kB\nSwapTotal:      1000 kB\nSwapFree:       500 kB\n@@uptime\n3600.5 100.0\n@@net\nInter-|   Receive\n face |bytes\n  eth0: 1000 10 0 0 0 0 0 0 2000 20 0 0 0 0 0 0\n    lo: 5 0 0 0 0 0 0 0 5 0 0 0 0 0 0 0\n@@diskstats\n   8       0 sda 10 0 1000 0 20 0 2000 0 0 0 0\n   8       1 sda1 10 0 900 0 20 0 1900 0 0 0 0\n   7       0 loop0 1 0 99 0 0 0 0 0 0 0 0\n@@df\nFilesystem 1-byte-blocks Used Available Capacity Mounted on\n/dev/sda1 100000 40000 60000 40% /\n@@ps\n  1 root 0.5 0.1 /sbin/init splash\n@@gpu\n@@info\nweb1\nUbuntu 24.04 LTS\n6.8.0-45-generic\nx86_64\n Intel(R) Xeon(R) CPU\n";

    #[test]
    fn parses_sample() {
        let s = split_sections(SAMPLE);
        let (snap, prev) = parse(&s, None);
        assert_eq!(snap.cores, 4);
        assert_eq!(snap.mem_total, 8_000_000 * 1024);
        assert_eq!(snap.mem_used, 2_000_000 * 1024);
        assert_eq!(snap.mem_cached, 2500 * 1024);
        assert_eq!(snap.swap_used, 500 * 1024);
        assert_eq!(snap.uptime_s, 3600);
        assert_eq!(snap.disks[0].mount, "/");
        assert_eq!(snap.disks[0].used, 40000);
        assert_eq!(snap.processes[0].command, "/sbin/init splash");
        assert_eq!(snap.info.os, "Ubuntu 24.04 LTS");
        assert_eq!(snap.info.cpu_model, "Intel(R) Xeon(R) CPU");
        assert_eq!(prev.net.get("eth0"), Some(&(1000, 2000)));
        assert!(prev.net.get("lo").is_none());
        // Only sda counts (not the partition nor the loop device).
        assert_eq!(prev.disk, (1000, 2000));

        let zero = |n: usize| (n, vec![0u64; 8]);
        let mut before = HashMap::new();
        for (k, n) in [("cpu", 0), ("cpu0", 0), ("cpu1", 0)] {
            before.insert(k.to_string(), zero(n).1);
        }
        let (snap2, _) = parse(&s, Some(&Prev { cpu: before, disk: (0, 0), ..prev }));
        // 1000 total ticks, 800 idle (idle + iowait) -> 20 %, of which 10 % user, 10 % system.
        assert!((snap2.cpu_percent - 20.0).abs() < 0.01);
        assert!((snap2.cpu_user - 10.0).abs() < 0.01);
        assert!((snap2.cpu_iowait - 10.0).abs() < 0.01);
        assert_eq!(snap2.per_core.len(), 2);
        assert!((snap2.per_core[0] - 20.0).abs() < 0.01);
        assert!(snap2.disk_read_bps > 0.0);
    }

    #[test]
    fn whole_disks() {
        for d in ["sda", "vdb", "xvda", "nvme0n1", "mmcblk0"] {
            assert!(is_whole_disk(d), "{d}");
        }
        for d in ["sda1", "nvme0n1p2", "mmcblk0p1", "loop0", "dm-0", "ram0", "sr0"] {
            assert!(!is_whole_disk(d), "{d}");
        }
    }
}
