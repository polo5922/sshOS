//! Multi-server view: one card per saved server; connected ones show load, memory, disk, uptime,
//! pending updates and failed services, refreshed every 30 s with one read-only script per host.

use std::collections::HashMap;

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::icons::Icon;
use crate::state::*;
use crate::tauri::exec;

const SCRIPT: &str = r#"echo "load $(cut -d' ' -f1-3 /proc/loadavg)"; echo "cpus $(nproc 2>/dev/null)"
free -b 2>/dev/null | awk '/^Mem:/{print "mem",$2,$7}'
df -P -B1 / 2>/dev/null | awk 'NR==2{print "disk",$2,$3}'
echo "up $(cut -d' ' -f1 /proc/uptime)"
echo "failed $(systemctl --failed --no-legend --plain 2>/dev/null | grep -c .)"
if command -v apt-get >/dev/null 2>&1; then n=$(apt-get -s upgrade 2>/dev/null | grep -c '^Inst'); elif command -v dnf >/dev/null 2>&1; then n=$(dnf -C -q check-update 2>/dev/null | grep -c '^[a-zA-Z0-9]'); fi; echo "upd ${n:-}"
[ -f /var/run/reboot-required ] && echo reboot"#;

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Stats {
    pub load: [f64; 3],
    pub cpus: u32,
    pub mem_total: u64,
    pub mem_avail: u64,
    pub disk_total: u64,
    pub disk_used: u64,
    pub uptime: u64,
    pub failed: u32,
    pub updates: Option<u32>,
    pub reboot: bool,
}

impl Stats {
    pub fn mem_pct(&self) -> f64 {
        if self.mem_total == 0 { 0.0 } else { 100.0 * (self.mem_total - self.mem_avail.min(self.mem_total)) as f64 / self.mem_total as f64 }
    }
    pub fn disk_pct(&self) -> f64 {
        if self.disk_total == 0 { 0.0 } else { 100.0 * self.disk_used as f64 / self.disk_total as f64 }
    }
    /// Load per CPU, in %.
    pub fn load_pct(&self) -> f64 {
        100.0 * self.load[0] / self.cpus.max(1) as f64
    }
}

pub fn parse(out: &str) -> Stats {
    let mut s = Stats::default();
    for l in out.lines() {
        let f: Vec<&str> = l.split_whitespace().collect();
        let num = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
        match f.first().copied() {
            Some("load") => s.load = [num(1), num(2), num(3)],
            Some("cpus") => s.cpus = num(1) as u32,
            Some("mem") => (s.mem_total, s.mem_avail) = (num(1) as u64, num(2) as u64),
            Some("disk") => (s.disk_total, s.disk_used) = (num(1) as u64, num(2) as u64),
            Some("up") => s.uptime = num(1) as u64,
            Some("failed") => s.failed = num(1) as u32,
            Some("upd") => s.updates = f.get(1).and_then(|v| v.parse().ok()),
            Some("reboot") => s.reboot = true,
            _ => {}
        }
    }
    s
}

pub fn fmt_uptime(secs: u64) -> String {
    let (d, h, m) = (secs / 86400, secs % 86400 / 3600, secs % 3600 / 60);
    if d > 0 { format!("{d} {} {h} h", tr("j", "d")) } else if h > 0 { format!("{h} h {m} min") } else { format!("{m} min") }
}

fn level(pct: f64) -> &'static str {
    if pct >= 90.0 { "crit" } else if pct >= 75.0 { "warn" } else { "ok" }
}

fn meter(label: &'static str, pct: f64, detail: String) -> impl IntoView {
    view! {
        <div class="fleet-meter">
            <div class="cell-flex small"><span class="muted grow">{label}</span><span>{detail}</span></div>
            <div class=format!("bar {}", level(pct))><div style=format!("width:{:.0}%", pct.clamp(0.0, 100.0))></div></div>
        </div>
    }
}

#[component]
pub fn Fleet() -> impl IntoView {
    let c = ctx();
    let stats = RwSignal::new(HashMap::<String, Stats>::new());
    let loading = RwSignal::new(false);

    let refresh = move || {
        let ids: Vec<String> = c.sessions.get_untracked().into_iter().map(|(h, _)| h).collect();
        loading.set(!ids.is_empty());
        let pending = StoredValue::new(ids.len());
        for id in ids {
            spawn_local(async move {
                if let Ok(r) = exec(&id, SCRIPT, true).await {
                    let s = parse(&r.stdout);
                    let _ = stats.try_update(|m| {
                        m.insert(id.clone(), s);
                    });
                }
                if pending.try_update_value(|p| {
                    *p -= 1;
                    *p
                }) == Some(0)
                {
                    let _ = loading.try_set(false);
                }
            });
        }
    };
    // Refresh when a server connects or disconnects, then every 30 s.
    Effect::new(move |_| {
        c.sessions.track();
        refresh();
    });
    if let Ok(h) = set_interval_with_handle(refresh, std::time::Duration::from_secs(30)) {
        on_cleanup(move || h.clear());
    }

    app_menus(vec![MenuSection::new(tr("Serveurs", "Servers"), vec![
        MenuItem::new(tr("Actualiser", "Refresh"), refresh),
        MenuItem::new(tr("Connecter tous les serveurs", "Connect all servers"), move || {
            for h in c.hosts.get_untracked() {
                if !c.is_connected(&h.id) {
                    c.connect(h.id, None);
                }
            }
        }),
        MenuItem::new(tr("Nouveau serveur…", "New server…"), move || c.open(AppKind::Connect, None)),
    ])]);

    view! {
        <div class="split-app fleet-app">
            <section class="main">
                <header class="main-head drag-region">
                    <h2 class="grow ellipsis">{tr("Multi-serveurs", "Fleet")}</h2>
                    <button class="icon-btn" class:spin=move || loading.get() title=tr("Actualiser", "Refresh") on:click=move |_| refresh()>
                        <Icon name="refresh" size=15 />
                    </button>
                </header>
                <div class="main-body">
                    {move || {
                        let hosts = c.hosts.get();
                        if hosts.is_empty() {
                            return view! {
                                <div class="empty">
                                    <p class="muted">{tr("Aucun serveur enregistré.", "No saved servers.")}</p>
                                    <button class="primary" on:click=move |_| c.open(AppKind::Connect, None)>{tr("Ajouter un serveur…", "Add a server…")}</button>
                                </div>
                            }
                            .into_any();
                        }
                        view! {
                            <div class="fleet-grid">
                                {hosts
                                    .into_iter()
                                    .map(|hc| {
                                        let id = StoredValue::new(hc.id.clone());
                                        let connected = move || c.is_connected(&id.get_value());
                                        let addr = format!("{}@{}{}", hc.user, hc.host, if hc.port == 22 { String::new() } else { format!(":{}", hc.port) });
                                        view! {
                                            <div class="card fleet-card" class:offline=move || !connected()>
                                                <div class="cell-flex">
                                                    <span class="state-dot" class:ok=connected></span>
                                                    <Icon name="server" size=16 />
                                                    <div class="grow">
                                                        <b class="ellipsis">{hc.name.clone()}</b>
                                                        <div class="muted small mono ellipsis">{addr}</div>
                                                    </div>
                                                </div>
                                                {move || {
                                                    if !connected() {
                                                        return view! {
                                                            <div class="fleet-body">
                                                                <p class="muted small">{tr("Non connecté", "Not connected")}</p>
                                                                <button class="small" on:click=move |_| c.connect(id.get_value(), None)>{tr("Connecter", "Connect")}</button>
                                                            </div>
                                                        }
                                                        .into_any();
                                                    }
                                                    let Some(s) = stats.with(|m| m.get(&id.get_value()).cloned()) else {
                                                        return view! { <div class="fleet-body"><p class="muted small">{tr("Chargement…", "Loading…")}</p></div> }.into_any();
                                                    };
                                                    let os = c.info(&id.get_value()).map(|i| i.os).unwrap_or_default();
                                                    view! {
                                                        <div class="fleet-body">
                                                            <div class="muted small ellipsis">{os}" · "{tr("allumé depuis", "up")}" "{fmt_uptime(s.uptime)}</div>
                                                            {meter(tr("Charge", "Load"), s.load_pct(), format!("{:.2} · {} CPU", s.load[0], s.cpus))}
                                                            {meter(tr("Mémoire", "Memory"), s.mem_pct(), format!("{} / {}", human_bytes((s.mem_total - s.mem_avail.min(s.mem_total)) as f64), human_bytes(s.mem_total as f64)))}
                                                            {meter(tr("Disque /", "Disk /"), s.disk_pct(), format!("{} / {}", human_bytes(s.disk_used as f64), human_bytes(s.disk_total as f64)))}
                                                            <div class="chips">
                                                                {(s.failed > 0).then(|| view! {
                                                                    <button class="pill crit" on:click=move |_| c.open(AppKind::Services, Some(id.get_value()))>
                                                                        {format!("{} {}", s.failed, tr("service(s) en échec", "failed service(s)"))}
                                                                    </button>
                                                                })}
                                                                {match s.updates {
                                                                    Some(n) if n > 0 => Some(view! {
                                                                        <button class="pill warn" on:click=move |_| c.open(AppKind::Updates, Some(id.get_value()))>
                                                                            {format!("{n} {}", tr("mise(s) à jour", "update(s)"))}
                                                                        </button>
                                                                    }),
                                                                    _ => None,
                                                                }}
                                                                {s.reboot.then(|| view! { <span class="pill warn">{tr("redémarrage requis", "reboot required")}</span> })}
                                                                {(s.failed == 0 && s.updates.unwrap_or(0) == 0 && !s.reboot).then(|| view! { <span class="pill ok">{tr("tout va bien", "all good")}</span> })}
                                                            </div>
                                                            <div class="form-actions">
                                                                <button class="small" on:click=move |_| c.open(AppKind::Monitor, Some(id.get_value()))>{tr("Moniteur", "Monitor")}</button>
                                                                <button class="small" on:click=move |_| c.open(AppKind::Terminal { input: None }, Some(id.get_value()))>{tr("Terminal", "Terminal")}</button>
                                                            </div>
                                                        </div>
                                                    }
                                                    .into_any()
                                                }}
                                            </div>
                                        }
                                    })
                                    .collect_view()}
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
    fn parse_stats() {
        let s = parse("load 0.50 0.40 0.30\ncpus 2\nmem 4000 1000\ndisk 100 90\nup 90061.5\nfailed 1\nupd 7\nreboot\n");
        assert_eq!(s.load, [0.5, 0.4, 0.3]);
        assert_eq!(s.load_pct(), 25.0);
        assert_eq!(s.mem_pct(), 75.0);
        assert_eq!(s.disk_pct(), 90.0);
        assert_eq!((s.failed, s.updates, s.reboot), (1, Some(7), true));
        assert_eq!(fmt_uptime(s.uptime), "1 j 1 h");
        assert_eq!(parse("upd \n").updates, None);
    }
}
