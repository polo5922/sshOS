//! Monitor: Activity-Monitor-like app. Overview of CPU / memory / disk / network, one page per
//! resource with time charts (1, 5 or 15 minutes, can be paused), processes and system info.
//! Polled every 2 s; one shell round-trip per refresh (see the backend `monitor.rs`).

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::json;
use sshos_shared::{MonitorSnapshot, ProcessInfo};

use crate::icons::Icon;
use crate::state::*;
use crate::tauri::{invoke, invoke_unit};

const POLL_S: u64 = 2;
/// 15 minutes of samples, plus a little margin for the left edge of the chart.
const KEEP_S: f64 = 15.0 * 60.0 + 10.0;

#[derive(Clone, Copy, Default)]
struct Sample {
    t: f64,
    cpu: f64,
    mem: f64,
    rx: f64,
    tx: f64,
    rd: f64,
    wr: f64,
    load: f64,
    gpu: f64,
}

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Overview,
    Processes,
    Cpu,
    Memory,
    Gpu,
    Disk,
    Network,
    Info,
}

impl Page {
    fn title(self) -> &'static str {
        match self {
            Page::Overview => tr("Vue d'ensemble", "Overview"),
            Page::Processes => tr("Processus", "Processes"),
            Page::Cpu => "CPU",
            Page::Memory => tr("Mémoire", "Memory"),
            Page::Gpu => "GPU",
            Page::Disk => tr("Disque", "Disk"),
            Page::Network => tr("Réseau", "Network"),
            Page::Info => tr("Informations", "Info"),
        }
    }
    fn icon(self) -> &'static str {
        match self {
            Page::Overview => "grid",
            Page::Processes => "list",
            Page::Cpu => "cpu",
            Page::Memory => "layers",
            Page::Gpu => "zap",
            Page::Disk => "hdd",
            Page::Network => "arrows",
            Page::Info => "info",
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Sort {
    Cpu,
    Mem,
}

fn rate(b: f64) -> String {
    format!("{}/s", human_bytes(b))
}

fn pct_of(used: u64, total: u64) -> f64 {
    if total > 0 { used as f64 * 100.0 / total as f64 } else { 0.0 }
}

fn level(p: f64) -> &'static str {
    if p > 90.0 {
        "crit"
    } else if p > 70.0 {
        "warn"
    } else {
        ""
    }
}

// ---------------------------------------------------------------- charts

const W: f64 = 600.0;

/// SVG paths (line, filled area) of one series over [t0, t1].
fn paths(pts: &[(f64, f64)], t0: f64, t1: f64, ymax: f64, h: f64) -> (String, String) {
    let span = (t1 - t0).max(1.0);
    let ymax = ymax.max(1e-9);
    let xy: Vec<(f64, f64)> = pts
        .iter()
        .filter(|(t, _)| *t >= t0 - POLL_S as f64 * 2.0)
        .map(|(t, v)| (((t - t0) / span * W).max(0.0), h - (v / ymax).clamp(0.0, 1.0) * (h - 2.0)))
        .collect();
    if xy.len() < 2 {
        return (String::new(), String::new());
    }
    let line: String = xy
        .iter()
        .enumerate()
        .map(|(i, (x, y))| format!("{}{x:.1},{y:.1}", if i == 0 { "M" } else { " L" }))
        .collect();
    let area = format!("{line} L{:.1},{h} L{:.1},{h} Z", xy[xy.len() - 1].0, xy[0].0);
    (line, area)
}

/// Upper bound for a rate chart: the visible maximum plus some headroom.
fn auto_max(hist: &[Sample], t0: f64, f: impl Fn(&Sample) -> f64) -> f64 {
    hist.iter().filter(|s| s.t >= t0).map(f).fold(1024.0, f64::max) * 1.15
}

/// Time chart. `series`: (CSS class, points). `grid`: draw the 50 % / 100 % lines.
fn chart(series: Vec<(&'static str, Vec<(f64, f64)>)>, t0: f64, t1: f64, ymax: f64, h: f64, grid: bool) -> impl IntoView {
    view! {
        <svg class="mchart" viewBox=format!("0 0 {W} {h}") preserveAspectRatio="none" style=format!("height:{h}px")>
            {grid
                .then(|| {
                    view! {
                        <line class="mgrid" x1="0" x2=W y1="1" y2="1" />
                        <line class="mgrid" x1="0" x2=W y1=h / 2.0 y2=h / 2.0 />
                    }
                })}
            {series
                .into_iter()
                .map(|(class, pts)| {
                    let (line, area) = paths(&pts, t0, t1, ymax, h);
                    view! {
                        <g class=class>
                            <path class="marea" d=area />
                            <path class="mline" d=line />
                        </g>
                    }
                })
                .collect_view()}
        </svg>
    }
}

fn range_label(range: u32) -> String {
    match range {
        60 => tr("il y a 1 minute", "1 minute ago").into(),
        300 => tr("il y a 5 minutes", "5 minutes ago").into(),
        _ => tr("il y a 15 minutes", "15 minutes ago").into(),
    }
}

fn axis(range: u32) -> impl IntoView {
    view! {
        <div class="maxis">
            <span>{range_label(range)}</span>
            <span>{tr("maintenant", "now")}</span>
        </div>
    }
}

fn stat(label: &'static str, value: String, class: &'static str) -> impl IntoView {
    view! {
        <div class="mstat">
            <div class="mstat-label">
                {(!class.is_empty()).then(|| view! { <span class=format!("dot {class}")></span> })}
                {label}
            </div>
            <div class="mstat-value">{value}</div>
        </div>
    }
}

// ---------------------------------------------------------------- app

#[component]
pub fn Monitor(host: String) -> impl IntoView {
    let c = ctx();
    let snap = RwSignal::new(None::<MonitorSnapshot>);
    let hist = RwSignal::new(Vec::<Sample>::new());
    let error = RwSignal::new(None::<String>);
    let page = RwSignal::new(Page::Overview);
    let range = RwSignal::new(300u32);
    let paused = RwSignal::new(false);
    let sort = RwSignal::new(Sort::Cpu);
    let filter = RwSignal::new(String::new());
    let busy = StoredValue::new(false);
    let host = StoredValue::new(host);

    let poll = move || {
        if busy.get_value() || paused.get_untracked() {
            return;
        }
        busy.set_value(true);
        spawn_local(async move {
            let r = invoke::<MonitorSnapshot>("monitor_snapshot", json!({ "hostId": host.get_value() })).await;
            let _ = busy.try_set_value(false);
            match r {
                Ok(s) => {
                    let t = js_sys::Date::now() / 1000.0;
                    let sample = Sample {
                        t,
                        cpu: s.cpu_percent as f64,
                        mem: pct_of(s.mem_used, s.mem_total),
                        rx: s.net.iter().map(|n| n.rx_bytes_per_s).sum(),
                        tx: s.net.iter().map(|n| n.tx_bytes_per_s).sum(),
                        rd: s.disk_read_bps,
                        wr: s.disk_write_bps,
                        load: s.load[0] as f64,
                        gpu: s.gpus.first().map(|g| g.util as f64).unwrap_or(0.0),
                    };
                    let _ = hist.try_update(|h| {
                        h.push(sample);
                        let cut = h.iter().position(|x| x.t >= t - KEEP_S).unwrap_or(0);
                        h.drain(..cut);
                    });
                    let _ = snap.try_set(Some(s));
                    let _ = error.try_set(None);
                }
                Err(e) => {
                    let _ = error.try_set(Some(e.message()));
                }
            }
        });
    };
    poll();
    if let Ok(h) = set_interval_with_handle(poll, std::time::Duration::from_secs(POLL_S)) {
        on_cleanup(move || h.clear());
    }

    // Visible window: [last sample - range, last sample] (frozen while paused).
    let window_t = move || {
        let t1 = hist.with(|h| h.last().map(|s| s.t)).unwrap_or_else(|| js_sys::Date::now() / 1000.0);
        (t1 - range.get() as f64, t1)
    };
    let series = move |f: fn(&Sample) -> f64| hist.with(|h| h.iter().map(|s| (s.t, f(s))).collect::<Vec<_>>());

    let kill = move |p: ProcessInfo, force: bool| {
        let sig = if force { "KILL" } else { "TERM" };
        c.show(
            Dialog::confirm(
                tr("Arrêter le processus", "Stop process"),
                format!("{} {} ({})", tr("Envoyer le signal au processus", "Send the signal to process"), p.pid, p.command),
                if force { "SIGKILL" } else { "SIGTERM" },
                move || {
                    let h = host.get_value();
                    spawn_local(async move {
                        match invoke_unit("kill_process", json!({ "hostId": h, "pid": p.pid, "force": force })).await {
                            Ok(()) => c.ok(format!("{} {}", tr("Signal envoyé à", "Signal sent to"), p.pid)),
                            Err(e) => c.handle_err(Some(&h), e),
                        }
                    });
                },
            )
            .code(format!("kill -s {sig} {}", p.pid))
            .danger(),
        );
    };

    // ------------------------------------------------------------ sidebar
    let side_value = move |p: Page| -> String {
        snap.with(|s| {
            let Some(s) = s else { return String::new() };
            match p {
                Page::Cpu => format!("{:.0} %", s.cpu_percent),
                Page::Memory => format!("{:.0} %", pct_of(s.mem_used, s.mem_total)),
                Page::Gpu => s.gpus.first().map(|g| format!("{:.0} %", g.util)).unwrap_or_else(|| "--".into()),
                Page::Disk => rate(s.disk_read_bps + s.disk_write_bps),
                Page::Network => rate(s.net.iter().map(|n| n.rx_bytes_per_s + n.tx_bytes_per_s).sum()),
                Page::Processes => s.processes.len().to_string(),
                _ => String::new(),
            }
        })
    };
    let item = move |p: Page| {
        view! {
            <button class="side-item" class:active=move || page.get() == p on:click=move |_| page.set(p)>
                <Icon name=p.icon() size=15 />
                <span class="grow">{p.title()}</span>
                <span class="side-count">{move || side_value(p)}</span>
            </button>
        }
    };
    let host_name = move || {
        snap.with(|s| s.as_ref().map(|s| s.info.hostname.clone()).filter(|h| !h.is_empty()))
            .unwrap_or_else(|| tr("Serveur", "Server").to_string())
    };

    // ------------------------------------------------------------ pages
    let card = move |p: Page, body: AnyView| {
        view! {
            <div class="mcard">
                <button class="mcard-head" on:click=move |_| page.set(p)>
                    <span class="grow">{p.title()}</span>
                    <Icon name="chev-right" size=13 />
                </button>
                {body}
            </div>
        }
    };

    let overview = move || {
        let Some(s) = snap.get() else { return ().into_any() };
        let (t0, t1) = window_t();
        let root = s.disks.iter().find(|d| d.mount == "/").or(s.disks.first()).cloned();
        let (rx, tx): (f64, f64) = s.net.iter().fold((0.0, 0.0), |(a, b), n| (a + n.rx_bytes_per_s, b + n.tx_bytes_per_s));
        let h = hist.get();
        let io_max = auto_max(&h, t0, |x| x.rd.max(x.wr));
        let net_max = auto_max(&h, t0, |x| x.rx.max(x.tx));
        view! {
            <div class="mgrid2">
                {card(
                    Page::Cpu,
                    view! {
                        <div class="mbig">{format!("{:.0} %", s.cpu_percent)}</div>
                        <div class="muted small">{format!("{} {}", s.cores, tr("cœurs", "cores"))}</div>
                        {chart(vec![("ch-cpu", series(|x| x.cpu))], t0, t1, 100.0, 90.0, false)}
                    }
                        .into_any(),
                )}
                {card(
                    Page::Memory,
                    view! {
                        <div class="mbig">{human_bytes(s.mem_used as f64)}</div>
                        <div class="muted small">
                            {format!("{} {}", tr("sur", "of"), human_bytes(s.mem_total as f64))}
                        </div>
                        {chart(vec![("ch-mem", series(|x| x.mem))], t0, t1, 100.0, 90.0, false)}
                    }
                        .into_any(),
                )}
                {card(
                    Page::Disk,
                    view! {
                        <div class="mbig">
                            {root.as_ref().map(|d| format!("{:.0} %", pct_of(d.used, d.total))).unwrap_or_else(|| "--".into())}
                        </div>
                        <div class="muted small">
                            {root
                                .as_ref()
                                .map(|d| format!("{} {} · {} {}", tr("Partition", "Partition"), d.mount, tr("sur", "of"), human_bytes(d.total as f64)))
                                .unwrap_or_default()}
                        </div>
                        {chart(vec![("ch-rd", series(|x| x.rd)), ("ch-wr", series(|x| x.wr))], t0, t1, io_max, 90.0, false)}
                    }
                        .into_any(),
                )}
                {card(
                    Page::Network,
                    view! {
                        <div class="mbig">{rate(rx + tx)}</div>
                        <div class="mlegend small">
                            <span><span class="dot ch-rx"></span>{tr("Reçu", "Received")}</span>
                            <span><span class="dot ch-tx"></span>{tr("Envoyé", "Sent")}</span>
                        </div>
                        {chart(vec![("ch-rx", series(|x| x.rx)), ("ch-tx", series(|x| x.tx))], t0, t1, net_max, 90.0, false)}
                    }
                        .into_any(),
                )}
            </div>
            <div class="mstrip">
                {stat(tr("Charge moyenne", "Load average"), format!("{:.2} · {:.2} · {:.2}", s.load[0], s.load[1], s.load[2]), "")}
                {stat(tr("Allumé depuis", "Uptime"), human_duration(s.uptime_s), "")}
                {stat("Swap", format!("{} / {}", human_bytes(s.swap_used as f64), human_bytes(s.swap_total as f64)), "")}
                {stat("GPU", s.gpus.first().map(|g| format!("{:.0} %", g.util)).unwrap_or_else(|| "--".into()), "")}
            </div>
        }
        .into_any()
    };

    let cpu_page = move || {
        let Some(s) = snap.get() else { return ().into_any() };
        let (t0, t1) = window_t();
        let r = range.get();
        let load_max = hist.with(|h| h.iter().filter(|x| x.t >= t0).map(|x| x.load).fold(s.cores as f64, f64::max)) * 1.1;
        view! {
            <div class="mhero">
                <div class="mbig xl">{format!("{:.0} %", s.cpu_percent)}</div>
                <div class="muted">{format!("{} · {} {}", tr("Utilisation", "Usage"), s.cores, tr("cœurs", "cores"))}</div>
            </div>
            <div class="mstats">
                {stat(tr("Utilisateur", "User"), format!("{:.0} %", s.cpu_user), "ch-cpu")}
                {stat(tr("Noyau", "Kernel"), format!("{:.0} %", s.cpu_system), "ch-sys")}
                {stat(tr("Attente E/S", "I/O wait"), format!("{:.0} %", s.cpu_iowait), "ch-rd")}
                {stat("Steal", format!("{:.0} %", s.cpu_steal), "")}
            </div>
            <div class="mpanel">
                <div class="mchart-wrap">
                    <span class="mgrid-label top">"100 %"</span>
                    <span class="mgrid-label mid">"50 %"</span>
                    {chart(vec![("ch-cpu", series(|x| x.cpu))], t0, t1, 100.0, 200.0, true)}
                </div>
                {axis(r)}
            </div>
            <h4 class="msection">{tr("Par cœur", "Per core")}</h4>
            <div class="mcores">
                {s.per_core
                    .iter()
                    .enumerate()
                    .map(|(i, p)| {
                        let p = *p as f64;
                        view! {
                            <div class="mcore">
                                <span class="muted small">{i}</span>
                                <div class=format!("bar {}", level(p))>
                                    <div style=format!("width:{p:.1}%")></div>
                                </div>
                                <span class="small num">{format!("{p:.0} %")}</span>
                            </div>
                        }
                    })
                    .collect_view()}
            </div>
            <h4 class="msection">{tr("Charge moyenne", "Load average")}</h4>
            <div class="mstats">
                {stat("1 min", format!("{:.2}", s.load[0]), "ch-load")}
                {stat("5 min", format!("{:.2}", s.load[1]), "")}
                {stat("15 min", format!("{:.2}", s.load[2]), "")}
            </div>
            <div class="mpanel">{chart(vec![("ch-load", series(|x| x.load))], t0, t1, load_max, 110.0, false)} {axis(r)}</div>
        }
        .into_any()
    };

    let memory_page = move || {
        let Some(s) = snap.get() else { return ().into_any() };
        let (t0, t1) = window_t();
        let total = s.mem_total.max(1) as f64;
        let app = s.mem_used as f64;
        let cache = (s.mem_cached + s.mem_buffers) as f64;
        let free = (total - app - cache).max(0.0);
        view! {
            <div class="mhero">
                <div class="mbig xl">{human_bytes(s.mem_used as f64)}</div>
                <div class="muted">
                    {format!("{} {} · {:.0} %", tr("sur", "of"), human_bytes(s.mem_total as f64), pct_of(s.mem_used, s.mem_total))}
                </div>
            </div>
            <div class="mstack">
                <span class="ch-mem" style=format!("width:{:.2}%", app / total * 100.0)></span>
                <span class="ch-cache" style=format!("width:{:.2}%", cache / total * 100.0)></span>
                <span style=format!("width:{:.2}%", free / total * 100.0)></span>
            </div>
            <div class="mstats">
                {stat(tr("Utilisée", "Used"), human_bytes(app), "ch-mem")}
                {stat(tr("Cache et tampons", "Cache & buffers"), human_bytes(cache), "ch-cache")}
                {stat(tr("Disponible", "Available"), human_bytes(s.mem_available as f64), "")}
                {stat("Swap", format!("{} / {}", human_bytes(s.swap_used as f64), human_bytes(s.swap_total as f64)), "")}
            </div>
            <div class="mpanel">
                <div class="mchart-wrap">
                    <span class="mgrid-label top">"100 %"</span>
                    <span class="mgrid-label mid">"50 %"</span>
                    {chart(vec![("ch-mem", series(|x| x.mem))], t0, t1, 100.0, 200.0, true)}
                </div>
                {axis(range.get())}
            </div>
        }
        .into_any()
    };

    let gpu_page = move || {
        let Some(s) = snap.get() else { return ().into_any() };
        if s.gpus.is_empty() {
            return view! {
                <div class="empty">
                    {tr(
                        "Aucun GPU NVIDIA détecté (nvidia-smi absent).",
                        "No NVIDIA GPU detected (nvidia-smi not found).",
                    )}
                </div>
            }
            .into_any();
        }
        let (t0, t1) = window_t();
        view! {
            {s.gpus
                .into_iter()
                .map(|g| {
                    view! {
                        <div class="mhero">
                            <div class="mbig xl">{format!("{:.0} %", g.util)}</div>
                            <div class="muted">{g.name}</div>
                        </div>
                        <div class="mstats">
                            {stat(tr("Mémoire", "Memory"), format!("{} / {} MB", g.mem_used_mb, g.mem_total_mb), "")}
                            {stat(tr("Température", "Temperature"), format!("{:.0} °C", g.temp_c), "")}
                        </div>
                    }
                })
                .collect_view()}
            <div class="mpanel">{chart(vec![("ch-gpu", series(|x| x.gpu))], t0, t1, 100.0, 180.0, true)} {axis(range.get())}</div>
        }
        .into_any()
    };

    let disk_page = move || {
        let Some(s) = snap.get() else { return ().into_any() };
        let (t0, t1) = window_t();
        let io_max = hist.with(|h| auto_max(h, t0, |x| x.rd.max(x.wr)));
        view! {
            <div class="mstats">
                {stat(tr("Lecture", "Read"), rate(s.disk_read_bps), "ch-rd")}
                {stat(tr("Écriture", "Write"), rate(s.disk_write_bps), "ch-wr")}
            </div>
            <div class="mpanel">
                {chart(vec![("ch-rd", series(|x| x.rd)), ("ch-wr", series(|x| x.wr))], t0, t1, io_max, 180.0, false)}
                {axis(range.get())}
            </div>
            <h4 class="msection">{tr("Partitions", "Partitions")}</h4>
            <div class="mpanel">
                {s.disks
                    .into_iter()
                    .map(|d| {
                        let pct = pct_of(d.used, d.total);
                        view! {
                            <div class="disk">
                                <div class="disk-row">
                                    <span class="mono">{d.mount}</span>
                                    <span class="muted small">
                                        {format!("{} / {} · {}", human_bytes(d.used as f64), human_bytes(d.total as f64), d.fs)}
                                    </span>
                                </div>
                                <div class=format!("bar {}", level(pct))>
                                    <div style=format!("width:{pct:.1}%")></div>
                                </div>
                            </div>
                        }
                    })
                    .collect_view()}
            </div>
            <button class="mlink" on:click=move |_| c.open(AppKind::Disks, None)>
                <Icon name="hdd" size=14 />
                {tr("Gérer l'espace disque…", "Manage disk space…")}
            </button>
        }
        .into_any()
    };

    let network_page = move || {
        let Some(s) = snap.get() else { return ().into_any() };
        let (t0, t1) = window_t();
        let (rx, tx): (f64, f64) = s.net.iter().fold((0.0, 0.0), |(a, b), n| (a + n.rx_bytes_per_s, b + n.tx_bytes_per_s));
        let net_max = hist.with(|h| auto_max(h, t0, |x| x.rx.max(x.tx)));
        view! {
            <div class="mstats">
                {stat(tr("Reçu", "Received"), rate(rx), "ch-rx")}
                {stat(tr("Envoyé", "Sent"), rate(tx), "ch-tx")}
            </div>
            <div class="mpanel">
                {chart(vec![("ch-rx", series(|x| x.rx)), ("ch-tx", series(|x| x.tx))], t0, t1, net_max, 200.0, false)}
                {axis(range.get())}
            </div>
            <h4 class="msection">{tr("Interfaces", "Interfaces")}</h4>
            <div class="mpanel">
                {s.net
                    .into_iter()
                    .map(|n| {
                        view! {
                            <div class="disk-row">
                                <span class="mono">{n.iface}</span>
                                <span class="muted small num">
                                    {format!("↓ {}   ↑ {}", rate(n.rx_bytes_per_s), rate(n.tx_bytes_per_s))}
                                </span>
                            </div>
                        }
                    })
                    .collect_view()}
            </div>
        }
        .into_any()
    };

    let info_page = move || {
        let Some(s) = snap.get() else { return ().into_any() };
        let rows = vec![
            (tr("Nom d'hôte", "Hostname"), s.info.hostname.clone()),
            (tr("Système", "System"), s.info.os.clone()),
            (tr("Noyau", "Kernel"), s.info.kernel.clone()),
            (tr("Architecture", "Architecture"), s.info.arch.clone()),
            (tr("Processeur", "Processor"), s.info.cpu_model.clone()),
            (tr("Cœurs", "Cores"), s.cores.to_string()),
            (tr("Mémoire", "Memory"), human_bytes(s.mem_total as f64)),
            ("Swap", human_bytes(s.swap_total as f64)),
            (tr("Allumé depuis", "Uptime"), human_duration(s.uptime_s)),
        ];
        view! {
            <div class="set-card minfo">
                {rows
                    .into_iter()
                    .map(|(k, v)| {
                        view! {
                            <div class="set-row">
                                <span class="muted grow">{k}</span>
                                <span class="mono selectable">{if v.is_empty() { "—".to_string() } else { v }}</span>
                            </div>
                        }
                    })
                    .collect_view()}
            </div>
        }
        .into_any()
    };

    let process_rows = move || {
        let mut list = snap.with(|s| s.as_ref().map(|s| s.processes.clone()).unwrap_or_default());
        let q = filter.get().to_lowercase();
        list.retain(|p| q.is_empty() || p.command.to_lowercase().contains(&q) || p.user.to_lowercase().contains(&q) || p.pid.to_string() == q);
        match sort.get() {
            Sort::Cpu => list.sort_by(|a, b| b.cpu.total_cmp(&a.cpu)),
            Sort::Mem => list.sort_by(|a, b| b.mem.total_cmp(&a.mem)),
        }
        list.into_iter()
            .take(150)
            .map(|p| {
                let (p1, p2) = (p.clone(), p.clone());
                view! {
                    <tr>
                        <td class="num">{p.pid}</td>
                        <td>{p.user.clone()}</td>
                        <td class="num">{format!("{:.1}", p.cpu)}</td>
                        <td class="num">{format!("{:.1}", p.mem)}</td>
                        <td class="mono ellipsis cmd" title=p.command.clone()>{p.command.clone()}</td>
                        <td class="actions">
                            <button class="small" on:click=move |_| kill(p1.clone(), false)>"TERM"</button>
                            <button class="small danger" on:click=move |_| kill(p2.clone(), true)>"KILL"</button>
                        </td>
                    </tr>
                }
            })
            .collect_view()
    };
    let processes_page = move || {
        view! {
            <div class="toolbar">
                <label class="search-field no-drag">
                    <Icon name="search" size=13 />
                    <input placeholder=tr("Filtrer", "Filter") bind:value=filter />
                </label>
            </div>
            <div class="table-wrap">
                <table class="proc-table">
                    <thead>
                        <tr>
                            <th class="num">"PID"</th>
                            <th>{tr("Utilisateur", "User")}</th>
                            <th class="num sortable" class:active=move || sort.get() == Sort::Cpu on:click=move |_| sort.set(Sort::Cpu)>
                                "CPU %"
                            </th>
                            <th class="num sortable" class:active=move || sort.get() == Sort::Mem on:click=move |_| sort.set(Sort::Mem)>
                                "MEM %"
                            </th>
                            <th>{tr("Commande", "Command")}</th>
                            <th></th>
                        </tr>
                    </thead>
                    <tbody>{process_rows}</tbody>
                </table>
            </div>
        }
    };

    app_menus(vec![
        MenuSection::new(
            tr("Présentation", "View"),
            [Page::Overview, Page::Processes, Page::Cpu, Page::Memory, Page::Gpu, Page::Disk, Page::Network, Page::Info]
                .into_iter()
                .map(|p| MenuItem::new(p.title(), move || page.set(p)).checked(move || page.get() == p))
                .chain([
                    None,
                    MenuItem::new(tr("1 minute", "1 minute"), move || range.set(60)).checked(move || range.get() == 60),
                    MenuItem::new(tr("5 minutes", "5 minutes"), move || range.set(300)).checked(move || range.get() == 300),
                    MenuItem::new(tr("15 minutes", "15 minutes"), move || range.set(900)).checked(move || range.get() == 900),
                    None,
                    MenuItem::new(tr("Pause", "Pause"), move || paused.update(|p| *p = !*p)).checked(move || paused.get()),
                ])
                .collect(),
        ),
        MenuSection::new(
            tr("Processus", "Processes"),
            vec![
                MenuItem::new(tr("Trier par CPU", "Sort by CPU"), move || sort.set(Sort::Cpu)).checked(move || sort.get() == Sort::Cpu),
                MenuItem::new(tr("Trier par mémoire", "Sort by memory"), move || sort.set(Sort::Mem))
                    .checked(move || sort.get() == Sort::Mem),
                None,
                MenuItem::new(tr("Actualiser", "Refresh"), move || poll()),
            ],
        ),
    ]);

    view! {
        <div class="split-app monitor2">
            <aside class="side drag-region">
                <div class="side-top"></div>
                <div class="side-host ellipsis" title=host_name>{host_name}</div>
                {item(Page::Overview)}
                {item(Page::Processes)}
                <div class="side-title">{tr("Matériel", "Hardware")}</div>
                {item(Page::Cpu)}
                {item(Page::Memory)}
                {item(Page::Gpu)}
                {item(Page::Disk)}
                {item(Page::Network)}
                <div class="side-title">{tr("Système", "System")}</div>
                {item(Page::Info)}
            </aside>
            <section class="main">
                <header class="main-head drag-region">
                    <h2 class="grow">{move || page.get().title()}</h2>
                    <div class="seg no-drag">
                        {[(60u32, "1m"), (300, "5m"), (900, "15m")]
                            .into_iter()
                            .map(|(v, l)| {
                                view! {
                                    <button class:on=move || range.get() == v on:click=move |_| range.set(v)>{l}</button>
                                }
                            })
                            .collect_view()}
                    </div>
                    <button
                        class="icon-btn no-drag"
                        class:on=move || paused.get()
                        title=move || if paused.get() { tr("Reprendre", "Resume") } else { tr("Pause", "Pause") }
                        on:click=move |_| {
                            paused.update(|p| *p = !*p);
                            if !paused.get_untracked() {
                                poll();
                            }
                        }
                    >
                        {move || if paused.get() { view! { <Icon name="play" size=14 /> } } else { view! { <Icon name="pause" size=14 /> } }}
                    </button>
                </header>
                <div class="main-body mbody" class:procs=move || page.get() == Page::Processes>
                    {move || error.get().map(|e| view! { <div class="error-banner">{e}</div> })}
                    <Show when=move || snap.with(|s| s.is_none()) && error.with(|e| e.is_none())>
                        <div class="empty">{tr("Lecture des métriques…", "Reading metrics…")}</div>
                    </Show>
                    {move || match page.get() {
                        Page::Overview => overview().into_any(),
                        Page::Processes => processes_page().into_any(),
                        Page::Cpu => cpu_page(),
                        Page::Memory => memory_page(),
                        Page::Gpu => gpu_page(),
                        Page::Disk => disk_page(),
                        Page::Network => network_page(),
                        Page::Info => info_page(),
                    }}
                </div>
            </section>
        </div>
    }
}
