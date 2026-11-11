//! sshOs — your server as a desktop, over SSH. A Rust/Tauri port of Davish (GPL-3.0).

mod browser;
mod cmdlog;
mod docker;
mod error;
mod homelab;
mod hosts;
mod monitor;
mod nginx;
mod security;
mod services;
mod sftp;
mod ssh;
mod terminal;
#[cfg(test)]
mod live_test;

use std::sync::Arc;

use base64::Engine;
use serde_json::Value;
use sshos_shared::*;
use tauri::{AppHandle, State};

use error::{AppError, AppResult};
use hosts::HostStore;
use monitor::Monitor;
use nginx::npm::NpmClients;
use ssh::{sh_quote, Pool, Session};
use terminal::{TermCmd, Terminals};

type S<'a, T> = State<'a, T>;

async fn session(store: &HostStore, pool: &Pool, host_id: &str) -> AppResult<Arc<Session>> {
    pool.get(store, host_id).await
}

// ---------------------------------------------------------------- hosts

#[tauri::command]
fn hosts_list(store: S<'_, HostStore>) -> Vec<HostConfig> {
    store.list()
}

#[tauri::command]
fn hosts_save(store: S<'_, HostStore>, req: SaveHostRequest) -> AppResult<HostConfig> {
    let h = store.upsert(req.host)?;
    if let Some(secret) = req.secret {
        if secret.is_empty() {
            hosts::delete_secret(&hosts::host_secret_key(&h.id))?;
        } else {
            hosts::set_secret(&hosts::host_secret_key(&h.id), &secret)?;
        }
    }
    Ok(h)
}

#[tauri::command]
async fn hosts_delete(store: S<'_, HostStore>, pool: S<'_, Pool>, npm: S<'_, NpmClients>, id: String) -> AppResult<()> {
    pool.disconnect(&id).await;
    npm.forget(&id).await;
    store.remove(&id)
}

/// Import `~/.ssh/config`; returns the hosts that were added.
#[tauri::command]
fn hosts_import(store: S<'_, HostStore>) -> AppResult<Vec<HostConfig>> {
    let found = hosts::import_ssh_config(&store.list())?;
    found.into_iter().map(|h| store.upsert(h)).collect()
}

// ---------------------------------------------------------------- connection

const INFO: &str = r#"hostname; (. /etc/os-release 2>/dev/null && echo "$PRETTY_NAME") || uname -s; uname -r; id -un; id -u; echo "$HOME""#;

#[tauri::command]
async fn connect(store: S<'_, HostStore>, pool: S<'_, Pool>, req: ConnectRequest) -> AppResult<ServerInfo> {
    let s = pool.connect(&store, &req.host_id, req.accept_fingerprint).await?;
    server_info_of(&s).await
}

async fn server_info_of(s: &Session) -> AppResult<ServerInfo> {
    let out = s.exec_read(INFO).await?.stdout;
    let l: Vec<&str> = out.lines().collect();
    let g = |i: usize| l.get(i).map(|s| s.trim().to_string()).unwrap_or_default();
    Ok(ServerInfo {
        hostname: g(0),
        os: g(1),
        kernel: g(2),
        user: g(3),
        is_root: g(4) == "0",
        home: g(5),
    })
}

#[tauri::command]
async fn server_info(store: S<'_, HostStore>, pool: S<'_, Pool>, host_id: String) -> AppResult<ServerInfo> {
    let s = session(&store, &pool, &host_id).await?;
    server_info_of(&s).await
}

#[tauri::command]
async fn disconnect(pool: S<'_, Pool>, npm: S<'_, NpmClients>, host_id: String) -> AppResult<()> {
    npm.forget(&host_id).await;
    pool.disconnect(&host_id).await;
    Ok(())
}

#[tauri::command]
async fn connected(pool: S<'_, Pool>) -> AppResult<Vec<String>> {
    Ok(pool.connected_ids().await)
}

#[tauri::command]
async fn exec(
    store: S<'_, HostStore>,
    pool: S<'_, Pool>,
    host_id: String,
    command: String,
    read: Option<bool>,
) -> AppResult<ExecResult> {
    let s = session(&store, &pool, &host_id).await?;
    if read.unwrap_or(false) {
        s.exec_read(&command).await
    } else {
        s.exec(&command).await
    }
}

// ---------------------------------------------------------------- command log

#[tauri::command]
fn cmdlog_list() -> Vec<CmdLogEntry> {
    cmdlog::list()
}

#[tauri::command]
fn cmdlog_clear() {
    cmdlog::clear()
}

/// The UI pre-typed `command` in a terminal (sudo actions): log it too.
#[tauri::command]
fn cmdlog_terminal(store: S<'_, HostStore>, host_id: String, command: String) {
    let name = store.list().into_iter().find(|h| h.id == host_id).map(|h| h.name).unwrap_or_default();
    cmdlog::record(&host_id, &name, &command, CmdLogKind::Terminal, None, 0);
}

// ---------------------------------------------------------------- terminal

#[tauri::command]
async fn term_open(
    app: AppHandle,
    store: S<'_, HostStore>,
    pool: S<'_, Pool>,
    terms: S<'_, Arc<Terminals>>,
    host_id: String,
    id: String,
    cols: u32,
    rows: u32,
    input: Option<String>,
) -> AppResult<String> {
    let s = session(&store, &pool, &host_id).await?;
    terms.open(app, s, cols, rows, id, input).await
}

/// `data` is base64 so arbitrary bytes survive the IPC.
#[tauri::command]
async fn term_write(terms: S<'_, Arc<Terminals>>, id: String, data: String) -> AppResult<()> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|e| AppError::msg(e.to_string()))?;
    terms.send(&id, TermCmd::Data(bytes)).await
}

#[tauri::command]
async fn term_resize(terms: S<'_, Arc<Terminals>>, id: String, cols: u32, rows: u32) -> AppResult<()> {
    terms.send(&id, TermCmd::Resize(cols, rows)).await
}

#[tauri::command]
async fn term_close(terms: S<'_, Arc<Terminals>>, id: String) -> AppResult<()> {
    terms.send(&id, TermCmd::Close).await.or(Ok(()))
}

// ---------------------------------------------------------------- files

async fn with_sftp<T, F, Fut>(store: &HostStore, pool: &Pool, host_id: &str, f: F) -> AppResult<T>
where
    F: Fn(Arc<russh_sftp::client::SftpSession>) -> Fut,
    Fut: std::future::Future<Output = AppResult<T>>,
{
    let sftp = pool.sftp(store, host_id).await?;
    match f(sftp).await {
        Ok(v) => Ok(v),
        Err(e) => {
            // The SFTP channel may have died with the connection: retry once on a fresh one.
            if pool.get(store, host_id).await.is_ok() {
                let msg = e.0.message();
                if msg.contains("closed") || msg.contains("Timeout") || msg.contains("timeout") {
                    pool.reset_sftp(host_id).await;
                    let sftp = pool.sftp(store, host_id).await?;
                    return f(sftp).await;
                }
            }
            Err(e)
        }
    }
}

#[tauri::command]
async fn sftp_list(store: S<'_, HostStore>, pool: S<'_, Pool>, host_id: String, path: String) -> AppResult<Vec<FileEntry>> {
    with_sftp(&store, &pool, &host_id, |s| {
        let path = path.clone();
        async move { sftp::list(&s, &path).await }
    })
    .await
}

#[tauri::command]
async fn file_read(store: S<'_, HostStore>, pool: S<'_, Pool>, host_id: String, path: String) -> AppResult<FileContent> {
    let r = with_sftp(&store, &pool, &host_id, |s| {
        let path = path.clone();
        async move { sftp::read_text(&s, &path).await }
    })
    .await;
    match r {
        Ok(c) => Ok(c),
        Err(e) if e.0.message().to_lowercase().contains("permission") => Err(AppError::msg(format!(
            "Permission denied reading {path}. Open it from a terminal with: sudo -e {}",
            sh_quote(&path)
        ))),
        Err(e) => Err(e),
    }
}

#[tauri::command]
async fn file_write(
    store: S<'_, HostStore>,
    pool: S<'_, Pool>,
    host_id: String,
    path: String,
    text: String,
) -> AppResult<WriteResult> {
    let session = session(&store, &pool, &host_id).await?;
    let s = pool.sftp(&store, &host_id).await?;
    sftp::write_text(&session, &s, &path, &text).await
}

/// Shell-level file operations; the UI shows the exact command before running it.
fn file_op_command(op: &str, a: &str, b: Option<&str>) -> AppResult<String> {
    Ok(match (op, b) {
        ("mkdir", _) => format!("mkdir -p -- {}", sh_quote(a)),
        ("touch", _) => format!("touch -- {}", sh_quote(a)),
        ("rename", Some(b)) => format!("mv -n -- {} {}", sh_quote(a), sh_quote(b)),
        ("copy", Some(b)) => format!("cp -a -- {} {}", sh_quote(a), sh_quote(b)),
        ("delete", _) => format!("rm -rf -- {}", sh_quote(a)),
        _ => return Err(AppError::msg(format!("Unknown file operation {op}"))),
    })
}

#[tauri::command]
fn file_op_preview(op: String, path: String, target: Option<String>) -> AppResult<String> {
    file_op_command(&op, &path, target.as_deref())
}

#[tauri::command]
async fn file_op(
    store: S<'_, HostStore>,
    pool: S<'_, Pool>,
    host_id: String,
    op: String,
    path: String,
    target: Option<String>,
) -> AppResult<()> {
    let cmd = file_op_command(&op, &path, target.as_deref())?;
    // Refuse "rm -rf /" and top-level directories such as /etc or /usr.
    if op == "delete" && path.trim_end_matches('/').matches('/').count() < 2 {
        return Err(AppError::msg(format!("Refusing to delete {path}")));
    }
    let s = session(&store, &pool, &host_id).await?;
    let r = s.exec(&cmd).await?;
    if r.ok() {
        return Ok(());
    }
    if r.stderr.contains("Permission denied") || r.stderr.contains("Operation not permitted") {
        return Err(AppError::needs_terminal(format!("sudo {cmd}")));
    }
    Err(AppError::msg(r.stderr.trim().to_string()))
}

#[tauri::command]
async fn upload(
    app: AppHandle,
    store: S<'_, HostStore>,
    pool: S<'_, Pool>,
    host_id: String,
    local: String,
    remote_dir: String,
) -> AppResult<String> {
    let s = pool.sftp(&store, &host_id).await?;
    sftp::upload(&app, s, &local, &remote_dir).await
}

#[tauri::command]
async fn download(
    app: AppHandle,
    store: S<'_, HostStore>,
    pool: S<'_, Pool>,
    host_id: String,
    remote: String,
    local: String,
) -> AppResult<()> {
    let s = pool.sftp(&store, &host_id).await?;
    sftp::download(&app, s, &remote, &local).await
}

// ---------------------------------------------------------------- monitor

#[tauri::command]
async fn monitor_snapshot(
    store: S<'_, HostStore>,
    pool: S<'_, Pool>,
    mon: S<'_, Monitor>,
    host_id: String,
) -> AppResult<MonitorSnapshot> {
    let s = session(&store, &pool, &host_id).await?;
    mon.snapshot(&host_id, &s).await
}

// ---------------------------------------------------------------- browser

#[tauri::command]
async fn browser_ports(store: S<'_, HostStore>, pool: S<'_, Pool>, host_id: String) -> AppResult<Vec<sshos_shared::ListeningPort>> {
    let s = session(&store, &pool, &host_id).await?;
    browser::ports(&s).await
}

#[tauri::command]
async fn browser_ping(store: S<'_, HostStore>, pool: S<'_, Pool>, host_id: String, url: String) -> AppResult<sshos_shared::PingResult> {
    let s = session(&store, &pool, &host_id).await?;
    browser::ping(&s, &url).await
}

/// Tunnels the URL's host:port through SSH and returns the equivalent local URL.
#[tauri::command]
async fn browser_open(store: S<'_, HostStore>, pool: S<'_, Pool>, host_id: String, url: String) -> AppResult<String> {
    let u = browser::parse_url(&url)?;
    let port = pool.local_forward(&store, &host_id, &u.remote()).await?;
    Ok(format!("{}://127.0.0.1:{port}{}", if u.https { "https" } else { "http" }, u.path))
}

// ---------------------------------------------------------------- tunnels

#[tauri::command]
async fn tunnel_open(store: S<'_, HostStore>, pool: S<'_, Pool>, host_id: String, remote: String, local_port: u16) -> AppResult<sshos_shared::TunnelInfo> {
    pool.tunnel_open(&store, &host_id, &remote, local_port).await
}

#[tauri::command]
async fn tunnel_list(pool: S<'_, Pool>) -> AppResult<Vec<sshos_shared::TunnelInfo>> {
    Ok(pool.tunnel_list().await)
}

#[tauri::command]
async fn tunnel_close(pool: S<'_, Pool>, id: u32) -> AppResult<()> {
    pool.tunnel_close(id).await;
    Ok(())
}

// ---------------------------------------------------------------- docker

#[tauri::command]
async fn docker_overview(store: S<'_, HostStore>, pool: S<'_, Pool>, host_id: String) -> AppResult<DockerOverview> {
    let s = session(&store, &pool, &host_id).await?;
    docker::overview(&s).await
}

#[tauri::command]
async fn docker_stats(store: S<'_, HostStore>, pool: S<'_, Pool>, host_id: String) -> AppResult<Vec<DockerStat>> {
    let s = session(&store, &pool, &host_id).await?;
    docker::stats(&s).await
}

#[tauri::command]
async fn docker_visitors(store: S<'_, HostStore>, pool: S<'_, Pool>, host_id: String) -> AppResult<Vec<DockerVisitors>> {
    let s = session(&store, &pool, &host_id).await?;
    docker::visitors(&s).await
}

#[tauri::command]
async fn homelab_snapshot(store: S<'_, HostStore>, pool: S<'_, Pool>, host_id: String) -> AppResult<HomelabSnapshot> {
    let s = session(&store, &pool, &host_id).await?;
    homelab::snapshot(&s).await
}

#[tauri::command]
fn docker_preview(req: DockerRequest) -> AppResult<String> {
    docker::command(&req)
}

#[tauri::command]
async fn docker_run(store: S<'_, HostStore>, pool: S<'_, Pool>, host_id: String, req: DockerRequest) -> AppResult<String> {
    let s = session(&store, &pool, &host_id).await?;
    docker::run(&s, &req).await
}

#[tauri::command]
async fn docker_logs(store: S<'_, HostStore>, pool: S<'_, Pool>, host_id: String, id: String, tail: u32) -> AppResult<String> {
    let s = session(&store, &pool, &host_id).await?;
    docker::logs(&s, &id, tail).await
}

#[tauri::command]
async fn docker_inspect(store: S<'_, HostStore>, pool: S<'_, Pool>, host_id: String, id: String) -> AppResult<String> {
    let s = session(&store, &pool, &host_id).await?;
    docker::inspect(&s, &id).await
}

// ---------------------------------------------------------------- security

#[tauri::command]
async fn security_report(store: S<'_, HostStore>, pool: S<'_, Pool>, host_id: String) -> AppResult<SecurityReport> {
    let s = session(&store, &pool, &host_id).await?;
    security::report(&s).await
}

#[tauri::command]
async fn failed_services(store: S<'_, HostStore>, pool: S<'_, Pool>, host_id: String) -> AppResult<Vec<FailedService>> {
    let s = session(&store, &pool, &host_id).await?;
    services::failed(&s).await
}

#[tauri::command]
fn fail2ban_command(action: Fail2banAction, jail: Option<String>, ip: Option<String>) -> AppResult<String> {
    security::fail2ban_command(action, jail.as_deref(), ip.as_deref())
}

#[tauri::command]
fn fail2ban_config_command(config: Fail2banConfig) -> AppResult<String> {
    security::fail2ban_config_command(&config)
}

#[tauri::command]
async fn kill_process(
    store: S<'_, HostStore>,
    pool: S<'_, Pool>,
    host_id: String,
    pid: u32,
    force: bool,
) -> AppResult<()> {
    let sig = if force { "KILL" } else { "TERM" };
    let cmd = format!("kill -s {sig} {pid}");
    let r = session(&store, &pool, &host_id).await?.exec(&cmd).await?;
    if r.ok() {
        Ok(())
    } else if r.stderr.contains("not permitted") {
        Err(AppError::needs_terminal(format!("sudo {cmd}")))
    } else {
        Err(AppError::msg(r.stderr.trim().to_string()))
    }
}

// ---------------------------------------------------------------- nginx (native)

async fn is_root(s: &Session) -> bool {
    s.exec_ok("id -u").await.map(|u| u.trim() == "0").unwrap_or(false)
}

async fn stage(store: &HostStore, pool: &Pool, host_id: &str, text: &str) -> AppResult<String> {
    let path = format!("/tmp/sshos-{}", uuid::Uuid::new_v4().simple());
    let s = pool.sftp(store, host_id).await?;
    use tokio::io::AsyncWriteExt;
    let mut f = s.create(&path).await?;
    f.write_all(text.as_bytes()).await?;
    f.shutdown().await?;
    Ok(path)
}

async fn timestamp(s: &Session) -> String {
    s.exec_ok("date +%Y%m%d-%H%M%S").await.map(|t| t.trim().to_string()).unwrap_or_else(|_| "backup".into())
}

#[tauri::command]
async fn nginx_detect(store: S<'_, HostStore>, pool: S<'_, Pool>, host_id: String) -> AppResult<NginxDetect> {
    let s = session(&store, &pool, &host_id).await?;
    let host = store.get(&host_id)?;
    nginx::detect(&s, &host).await
}

#[tauri::command]
async fn nginx_sites(
    store: S<'_, HostStore>,
    pool: S<'_, Pool>,
    host_id: String,
    native: NginxNative,
) -> AppResult<Vec<NginxSite>> {
    let s = session(&store, &pool, &host_id).await?;
    nginx::list_sites(&s, &native).await
}

#[tauri::command]
async fn nginx_read(store: S<'_, HostStore>, pool: S<'_, Pool>, host_id: String, path: String) -> AppResult<String> {
    let s = session(&store, &pool, &host_id).await?;
    let r = s.exec(&format!("cat -- {}", sh_quote(&path))).await?;
    if r.ok() {
        Ok(r.stdout)
    } else {
        Err(AppError::msg(r.stderr.trim().to_string()))
    }
}

/// Config preview while the user fills the "New site" form.
#[tauri::command]
fn nginx_preview(req: NewSiteRequest) -> AppResult<String> {
    nginx::validate_new_site(&req)?;
    Ok(nginx::files::generate_site(&req))
}

#[tauri::command]
fn nginx_certbot_command(domains: Vec<String>) -> String {
    nginx::files::certbot_command(&domains)
}

#[tauri::command]
async fn nginx_plan_create(
    store: S<'_, HostStore>,
    pool: S<'_, Pool>,
    host_id: String,
    native: NginxNative,
    req: NewSiteRequest,
) -> AppResult<NginxPlan> {
    nginx::validate_new_site(&req)?;
    let config = nginx::files::generate_site(&req);
    let staged = stage(&store, &pool, &host_id, &config).await?;
    Ok(nginx::files::plan_new_site(&native.layout, &native.conf_dir, &req.name, &staged, config))
}

#[tauri::command]
fn nginx_plan_toggle(native: NginxNative, name: String, enable: bool) -> NginxPlan {
    nginx::files::plan_toggle(&native.layout, &native.conf_dir, &name, enable)
}

#[tauri::command]
fn nginx_plan_remove(native: NginxNative, name: String, cert_name: Option<String>) -> NginxPlan {
    nginx::files::plan_remove(&native.layout, &native.conf_dir, &name, cert_name.as_deref())
}

/// Save an edited site config: stage it, then `nginx -t` + reload, restoring the backup on failure.
#[tauri::command]
async fn nginx_plan_save(
    store: S<'_, HostStore>,
    pool: S<'_, Pool>,
    host_id: String,
    path: String,
    text: String,
) -> AppResult<NginxPlan> {
    let s = session(&store, &pool, &host_id).await?;
    let staged = stage(&store, &pool, &host_id, &text).await?;
    let mut plan = nginx::files::plan_replace(&path, &staged, &timestamp(&s).await);
    plan.config = Some(text);
    Ok(plan)
}

#[tauri::command]
async fn nginx_plan_quick(
    store: S<'_, HostStore>,
    pool: S<'_, Pool>,
    host_id: String,
    path: String,
    add_domains: Vec<String>,
    proxy: Option<String>,
    body_size: Option<String>,
) -> AppResult<NginxPlan> {
    let s = session(&store, &pool, &host_id).await?;
    let src = s.exec_ok(&format!("cat -- {}", sh_quote(&path))).await?;
    let text = nginx::files::quick_change(
        &src,
        &add_domains,
        proxy.as_deref().filter(|p| !p.is_empty()),
        body_size.as_deref().filter(|b| !b.is_empty()),
    );
    let staged = stage(&store, &pool, &host_id, &text).await?;
    let mut plan = nginx::files::plan_replace(&path, &staged, &timestamp(&s).await);
    plan.config = Some(text);
    Ok(plan)
}

/// Run a plan as root, or return `needs_terminal` with the sudo command for the user to run.
#[tauri::command]
async fn nginx_apply(store: S<'_, HostStore>, pool: S<'_, Pool>, host_id: String, plan: NginxPlan) -> AppResult<String> {
    let s = session(&store, &pool, &host_id).await?;
    let root = is_root(&s).await;
    let r = nginx::apply(&s, root, &plan).await?;
    Ok(format!("{}{}", r.stdout, r.stderr))
}

// ---------------------------------------------------------------- nginx proxy manager

/// Detect NPM, open (or reuse) the SSH tunnel to its admin API and register it.
#[tauri::command]
async fn npm_connect(
    store: S<'_, HostStore>,
    pool: S<'_, Pool>,
    npm: S<'_, NpmClients>,
    host_id: String,
) -> AppResult<NpmLocation> {
    let s = session(&store, &pool, &host_id).await?;
    let host = store.get(&host_id)?;
    let det = nginx::detect(&s, &host).await?;
    let loc = det
        .npm
        .ok_or_else(|| AppError::msg("Nginx Proxy Manager was not found on this server. Set its address (host:port of the admin API) in the connection settings."))?;
    let port = pool.local_forward(&store, &host_id, &loc.target).await?;
    npm.set_base(&host_id, format!("http://127.0.0.1:{port}")).await;
    Ok(loc)
}

#[tauri::command]
async fn npm_login(npm: S<'_, NpmClients>, req: NpmLogin) -> AppResult<()> {
    npm.login(&req.host_id, &req.identity, &req.secret, req.remember).await
}

#[tauri::command]
async fn npm_logout(npm: S<'_, NpmClients>, host_id: String) -> AppResult<()> {
    npm.logout(&host_id).await
}

#[tauri::command]
async fn npm_overview(npm: S<'_, NpmClients>, host_id: String) -> AppResult<NpmOverview> {
    npm.overview(&host_id).await
}

#[tauri::command]
async fn npm_save_proxy_host(npm: S<'_, NpmClients>, host_id: String, host: NpmProxyHost) -> AppResult<NpmProxyHost> {
    npm.save_proxy_host(&host_id, &host).await
}

#[tauri::command]
async fn npm_save(
    npm: S<'_, NpmClients>,
    host_id: String,
    kind: NpmKind,
    id: Option<i64>,
    body: Value,
) -> AppResult<Value> {
    npm.save(&host_id, kind, id, body).await
}

#[tauri::command]
async fn npm_delete(npm: S<'_, NpmClients>, host_id: String, kind: NpmKind, id: i64) -> AppResult<()> {
    npm.delete(&host_id, kind, id).await
}

#[tauri::command]
async fn npm_toggle(npm: S<'_, NpmClients>, host_id: String, kind: NpmKind, id: i64, enabled: bool) -> AppResult<()> {
    npm.set_enabled(&host_id, kind, id, enabled).await
}

#[tauri::command]
async fn npm_cert_create(
    npm: S<'_, NpmClients>,
    host_id: String,
    domains: Vec<String>,
    email: String,
) -> AppResult<NpmCertificate> {
    npm.create_certificate(&host_id, domains, email).await
}

#[tauri::command]
async fn npm_cert_renew(npm: S<'_, NpmClients>, host_id: String, id: i64) -> AppResult<()> {
    npm.renew_certificate(&host_id, id).await
}

#[tauri::command]
async fn npm_cert_delete(npm: S<'_, NpmClients>, host_id: String, id: i64) -> AppResult<()> {
    npm.delete_certificate(&host_id, id).await
}

/// Read-only view of the nginx config NPM generated for an entry.
#[tauri::command]
async fn npm_conf(
    store: S<'_, HostStore>,
    pool: S<'_, Pool>,
    host_id: String,
    container: String,
    kind: NpmKind,
    id: i64,
) -> AppResult<String> {
    let s = session(&store, &pool, &host_id).await?;
    let cmd = format!("docker exec {} cat {}/{id}.conf", sh_quote(&container), kind.conf_dir());
    let r = s.exec(&cmd).await?;
    if r.ok() {
        Ok(r.stdout)
    } else if r.stderr.contains("permission denied") {
        Err(AppError::needs_terminal(format!("sudo {cmd}")))
    } else {
        Err(AppError::msg(r.stderr.trim().to_string()))
    }
}

// ---------------------------------------------------------------- app

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(HostStore::load())
        .manage(Pool::default())
        .manage(Arc::new(Terminals::default()))
        .manage(Monitor::default())
        .manage(NpmClients::default())
        .invoke_handler(tauri::generate_handler![
            cmdlog_list,
            cmdlog_clear,
            cmdlog_terminal,
            hosts_list,
            hosts_save,
            hosts_delete,
            hosts_import,
            connect,
            server_info,
            disconnect,
            connected,
            exec,
            term_open,
            term_write,
            term_resize,
            term_close,
            sftp_list,
            file_read,
            file_write,
            file_op_preview,
            file_op,
            upload,
            download,
            monitor_snapshot,
            kill_process,
            browser_ports,
            browser_ping,
            browser_open,
            tunnel_open,
            tunnel_list,
            tunnel_close,
            docker_overview,
            docker_stats,
            docker_visitors,
            homelab_snapshot,
            docker_preview,
            docker_run,
            docker_logs,
            docker_inspect,
            security_report,
            failed_services,
            fail2ban_command,
            fail2ban_config_command,
            nginx_detect,
            nginx_sites,
            nginx_read,
            nginx_preview,
            nginx_certbot_command,
            nginx_plan_create,
            nginx_plan_toggle,
            nginx_plan_remove,
            nginx_plan_save,
            nginx_plan_quick,
            nginx_apply,
            npm_connect,
            npm_login,
            npm_logout,
            npm_overview,
            npm_save_proxy_host,
            npm_save,
            npm_delete,
            npm_toggle,
            npm_cert_create,
            npm_cert_renew,
            npm_cert_delete,
            npm_conf,
        ])
        .run(tauri::generate_context!())
        .expect("error while running sshOs");
}
