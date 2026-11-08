//! Read-only end-to-end checks against a real server. Never run by default:
//!   SSHOS_LIVE_HOST=example.com SSHOS_LIVE_USER=me cargo test -p sshos live -- --ignored --nocapture
//! Optional: SSHOS_LIVE_KEY (default: Agent mode = SSH agent, then ~/.ssh/id_* files).
//! Optional NPM sign-in (read-only listing): SSHOS_NPM_USER / SSHOS_NPM_PASS.

use std::time::Duration;

use sshos_shared::{AuthMethod, HostConfig};

use crate::hosts::HostStore;
use crate::monitor::Monitor;
use crate::nginx::{self, npm::NpmClients};
use crate::sftp;
use crate::ssh::Pool;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

#[tokio::test]
#[ignore]
async fn live_read_only() {
    let Some(addr) = env("SSHOS_LIVE_HOST") else {
        eprintln!("SSHOS_LIVE_HOST not set, skipping");
        return;
    };
    let user = env("SSHOS_LIVE_USER").unwrap_or_else(|| "root".into());
    let keys = match env("SSHOS_LIVE_KEY") {
        Some(k) => vec![k],
        // No key given: Agent mode (agent, then default key files, like OpenSSH).
        None => vec![],
    };

    // ---- connect (first key that works)
    let pool = Pool::default();
    let mut store = None;
    let mut session = None;
    let auths: Vec<AuthMethod> = if keys.is_empty() {
        vec![AuthMethod::Agent]
    } else {
        keys.iter().map(|k| AuthMethod::Key { path: k.clone() }).collect()
    };
    for auth in auths {
        let key = format!("{auth:?}");
        let host = HostConfig {
            id: "live-test".into(),
            name: addr.clone(),
            host: addr.clone(),
            port: 22,
            user: user.clone(),
            auth,
            jump: None,
            npm_target: None,
        };
        let st = HostStore::in_memory(vec![host]);
        match pool.connect(&st, "live-test", None).await {
            Ok(s) => {
                println!("[ok] SSH connected with {key}");
                store = Some(st);
                session = Some(s);
                break;
            }
            Err(e) => println!("[..] {key}: {}", e.0.message()),
        }
    }
    let (store, s) = (store.expect("no key could connect"), session.unwrap());

    // ---- exec
    let r = s.exec("uname -sr; id -un").await.unwrap();
    println!("[ok] exec: {}", r.stdout.trim().replace('\n', " | "));
    assert!(r.ok());

    // ---- SFTP
    let sf = pool.sftp(&store, "live-test").await.expect("sftp");
    let home = s.exec_ok("printf %s \"$HOME\"").await.unwrap();
    let entries = sftp::list(&sf, &home).await.expect("sftp list");
    println!("[ok] sftp list {home}: {} entries", entries.len());
    let f = sftp::read_text(&sf, "/etc/os-release").await.expect("read");
    println!("[ok] read /etc/os-release: {}", f.text.lines().next().unwrap_or(""));

    // ---- monitor (2 samples so rates are computed)
    let mon = Monitor::default();
    mon.snapshot("live-test", &s).await.expect("monitor 1");
    tokio::time::sleep(Duration::from_secs(1)).await;
    let m = mon.snapshot("live-test", &s).await.expect("monitor 2");
    println!(
        "[ok] monitor: cpu {:.1}% / {} cores, mem {}/{} MB, {} disks, {} ifaces, {} procs, {} gpus, load {:?}",
        m.cpu_percent,
        m.cores,
        m.mem_used / 1_048_576,
        m.mem_total / 1_048_576,
        m.disks.len(),
        m.net.len(),
        m.processes.len(),
        m.gpus.len(),
        m.load
    );
    assert!(m.cores > 0 && m.mem_total > 0 && !m.processes.is_empty());

    // ---- docker (read-only)
    match crate::docker::overview(&s).await {
        Ok(d) => {
            let running = d.containers.iter().filter(|c| c.state == "running").count();
            let projects: std::collections::HashSet<_> = d.containers.iter().filter_map(|c| c.project.clone()).collect();
            println!(
                "[ok] docker {}: {} containers ({running} running, {} compose projects), {} images ({} unused), {} volumes ({} unused)",
                d.version,
                d.containers.len(),
                projects.len(),
                d.images.len(),
                d.images.iter().filter(|i| !i.in_use).count(),
                d.volumes.len(),
                d.volumes.iter().filter(|v| !v.in_use).count()
            );
            let st = crate::docker::stats(&s).await.expect("docker stats");
            let cpu: f32 = st.iter().map(|x| x.cpu).sum();
            let mem: u64 = st.iter().map(|x| x.mem).sum();
            println!("[ok] docker stats: {} rows, cpu {cpu:.1}%, mem {} MB", st.len(), mem / 1_048_576);
        }
        Err(e) => println!("[--] docker: {:?}", e.0),
    }

    // ---- security (read-only)
    let sr = crate::security::report(&s).await.expect("security");
    println!(
        "[ok] security from {:?} ({} -> {}): failed {}, invalid {}, accepted {}, probes {}, {} IPs, {} hours, {} events, hint {:?}",
        sr.source,
        sr.first,
        sr.last,
        sr.failed,
        sr.invalid_user,
        sr.accepted,
        sr.probes,
        sr.top_ips.len(),
        sr.hourly.len(),
        sr.events.len(),
        sr.hint
    );
    for ip in sr.top_ips.iter().take(5) {
        println!("     {} x{} banned={} users={:?}", ip.ip, ip.attempts, ip.banned, ip.users);
    }
    let f = &sr.fail2ban;
    println!("[ok] fail2ban installed={} running={} source={} hint={:?}", f.installed, f.running, f.source, f.hint);
    for j in &f.jails {
        println!(
            "     jail {} banned={} total={} failures={} maxretry={} findtime={} bantime={}",
            j.name,
            j.banned.len(),
            j.total_bans,
            j.failures,
            j.maxretry,
            j.findtime,
            j.bantime
        );
    }

    // ---- nginx detection
    let host = store.get("live-test").unwrap();
    let det = nginx::detect(&s, &host).await.expect("detect");
    println!("[ok] detect: native={:?} npm={:?} root={}", det.native, det.npm, det.is_root);
    if let Some(native) = &det.native {
        let sites = nginx::list_sites(&s, native).await.expect("sites");
        for site in &sites {
            println!("     site {} enabled={} servers={}", site.name, site.enabled, site.servers.len());
        }
    }

    // ---- NPM through the SSH tunnel
    if let Some(loc) = &det.npm {
        let port = pool.local_forward(&store, "live-test", &loc.target).await.expect("forward");
        let health: serde_json::Value = reqwest::get(format!("http://127.0.0.1:{port}/api/"))
            .await
            .expect("tunnel http")
            .json()
            .await
            .expect("json");
        println!("[ok] NPM via tunnel 127.0.0.1:{port} -> {}: {health}", loc.target);

        let npm = NpmClients::default();
        npm.set_base("live-test", format!("http://127.0.0.1:{port}")).await;
        match npm.overview("live-test").await {
            Err(e) => println!("[ok] NPM without credentials -> {:?}", e.0),
            Ok(_) => println!("[??] NPM overview worked without signing in"),
        }
        let creds = (env("SSHOS_NPM_USER"), env("SSHOS_NPM_PASS"));
        if creds.0.is_none() || creds.1.is_none() {
            println!(
                "[--] NPM sign-in skipped: SSHOS_NPM_USER {} / SSHOS_NPM_PASS {}",
                if creds.0.is_some() { "set" } else { "missing" },
                if creds.1.is_some() { "set" } else { "missing or empty" }
            );
        }
        if let (Some(u), Some(p)) = creds {
            npm.login("live-test", &u, &p, false).await.expect("npm login");
            let ov = npm.overview("live-test").await.expect("overview");
            println!(
                "[ok] NPM overview: {} proxy hosts, {} redirections, {} streams, {} certs, {} access lists",
                ov.proxy_hosts.len(),
                ov.redirection_hosts.len(),
                ov.streams.len(),
                ov.certificates.len(),
                ov.access_lists.len()
            );
            for h in &ov.proxy_hosts {
                println!("     #{} {:?} -> {}:{} enabled={}", h.id, h.domain_names, h.forward_host, h.forward_port, h.enabled);
            }
        }
        if let Some(c) = &loc.container {
            let r = s.exec(&format!("docker exec {c} ls /data/nginx/proxy_host")).await.unwrap();
            println!("[ok] generated confs in {c}: {}", r.stdout.split_whitespace().collect::<Vec<_>>().join(" "));
        }
    }
    pool.disconnect("live-test").await;
}
