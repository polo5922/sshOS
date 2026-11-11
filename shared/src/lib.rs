//! Types shared between the Tauri backend and the Leptos frontend.
//! Everything here crosses the IPC boundary as JSON.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

// ---------------------------------------------------------------- hosts

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthMethod {
    /// Password stored in the OS keychain.
    Password,
    /// Private key file; passphrase (if any) stored in the OS keychain.
    Key { path: String },
    /// Running SSH agent (OpenSSH agent pipe / Pageant on Windows, SSH_AUTH_SOCK elsewhere).
    Agent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HostConfig {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: AuthMethod,
    /// Id of another saved host used as ProxyJump.
    #[serde(default)]
    pub jump: Option<String>,
    /// Where the Nginx Proxy Manager admin API listens, seen from the server (e.g. "127.0.0.1:81").
    #[serde(default)]
    pub npm_target: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveHostRequest {
    pub host: HostConfig,
    /// New secret (password or key passphrase). `None` keeps the stored one.
    pub secret: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectRequest {
    pub host_id: String,
    /// Fingerprint the user accepted for an unknown host key (trust on first use).
    #[serde(default)]
    pub accept_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    pub hostname: String,
    pub os: String,
    pub kernel: String,
    pub user: String,
    pub is_root: bool,
    pub home: String,
}

// ---------------------------------------------------------------- errors

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ErrorPayload {
    /// Generic error with a human readable message.
    Message { message: String },
    /// Host key unknown: the UI asks the user to confirm the fingerprint.
    UnknownHostKey { host: String, fingerprint: String, algorithm: String },
    /// Host key differs from known_hosts. Never auto-accepted.
    HostKeyChanged { host: String, line: usize },
    /// The action needs root: the UI opens a terminal with this command typed in.
    NeedsTerminal { command: String },
    /// Nginx Proxy Manager credentials missing or rejected.
    NpmAuthRequired { message: String },
}

impl ErrorPayload {
    pub fn message(&self) -> String {
        match self {
            ErrorPayload::Message { message } => message.clone(),
            ErrorPayload::UnknownHostKey { host, fingerprint, .. } => {
                format!("Unknown host key for {host}: {fingerprint}")
            }
            ErrorPayload::HostKeyChanged { host, line } => format!(
                "Host key for {host} has CHANGED (known_hosts line {line}). Possible man-in-the-middle attack."
            ),
            ErrorPayload::NeedsTerminal { command } => format!("Needs root: {command}"),
            ErrorPayload::NpmAuthRequired { message } => message.clone(),
        }
    }
}

// ---------------------------------------------------------------- exec / files

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    pub code: Option<u32>,
}

impl ExecResult {
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub is_link: bool,
    pub size: u64,
    pub mtime: u32,
    pub mode: u32,
    pub user: String,
    pub group: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileContent {
    pub path: String,
    pub text: String,
    /// True when the file is not valid UTF-8 (shown read-only).
    pub binary: bool,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteResult {
    pub path: String,
    /// Backup created before overwriting (under /etc), if any.
    pub backup: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferProgress {
    pub id: String,
    pub name: String,
    pub done: u64,
    pub total: u64,
    pub finished: bool,
    pub error: Option<String>,
}

// ---------------------------------------------------------------- terminal

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TermOutput {
    pub id: String,
    /// Base64 encoded bytes.
    pub data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TermExit {
    pub id: String,
    pub code: Option<u32>,
}

// ---------------------------------------------------------------- monitor

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DiskUsage {
    pub mount: String,
    pub fs: String,
    pub total: u64,
    pub used: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NetRate {
    pub iface: String,
    pub rx_bytes_per_s: f64,
    pub tx_bytes_per_s: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProcessInfo {
    pub pid: u32,
    pub user: String,
    pub cpu: f32,
    pub mem: f32,
    pub command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GpuInfo {
    pub name: String,
    pub util: f32,
    pub mem_used_mb: u64,
    pub mem_total_mb: u64,
    pub temp_c: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MonitorSnapshot {
    pub cpu_percent: f32,
    pub cores: usize,
    pub load: [f32; 3],
    pub mem_total: u64,
    pub mem_used: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    pub uptime_s: u64,
    pub disks: Vec<DiskUsage>,
    pub net: Vec<NetRate>,
    pub processes: Vec<ProcessInfo>,
    pub gpus: Vec<GpuInfo>,
    /// CPU time split (percent of all cores): user+nice, system+irq+softirq, iowait, steal.
    #[serde(default)]
    pub cpu_user: f32,
    #[serde(default)]
    pub cpu_system: f32,
    #[serde(default)]
    pub cpu_iowait: f32,
    #[serde(default)]
    pub cpu_steal: f32,
    /// Busy percent of each logical core (cpu0, cpu1…).
    #[serde(default)]
    pub per_core: Vec<f32>,
    /// Bytes: page cache (+ reclaimable slab), buffers, MemAvailable.
    #[serde(default)]
    pub mem_cached: u64,
    #[serde(default)]
    pub mem_buffers: u64,
    #[serde(default)]
    pub mem_available: u64,
    /// Whole-disk I/O summed over physical disks, bytes per second.
    #[serde(default)]
    pub disk_read_bps: f64,
    #[serde(default)]
    pub disk_write_bps: f64,
    #[serde(default)]
    pub info: SystemInfo,
}

// ---------------------------------------------------------------- command log

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CmdLogKind {
    /// Run by an app (change or one-off query).
    Exec,
    /// Read-only query issued by an app view (listing, status…).
    Read,
    /// Pre-typed in a terminal: the user pressed Enter (or not).
    Terminal,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CmdLogEntry {
    pub id: u64,
    pub at_ms: u64,
    pub host_id: String,
    pub host_name: String,
    pub command: String,
    pub kind: CmdLogKind,
    /// Exit code; None for terminal commands or a dropped channel.
    pub code: Option<u32>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct SystemInfo {
    pub hostname: String,
    pub os: String,
    pub kernel: String,
    pub arch: String,
    pub cpu_model: String,
}

// ---------------------------------------------------------------- nginx (native)

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NginxServerBlock {
    pub server_names: Vec<String>,
    pub listen: Vec<String>,
    pub proxy_pass: Vec<String>,
    pub root: Option<String>,
    pub ssl_certificate: Option<String>,
    /// 1-based line where the `server {` block starts.
    pub line: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NginxSite {
    pub name: String,
    /// Absolute path of the config file (in sites-available or conf.d).
    pub path: String,
    pub enabled: bool,
    pub servers: Vec<NginxServerBlock>,
    pub upstreams: Vec<String>,
    /// Days until the first TLS certificate expires, if one is configured and readable.
    pub cert_days_left: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NginxLayout {
    /// Debian style: sites-available + sites-enabled symlinks.
    SitesAvailable,
    /// conf.d/*.conf ; disabling renames to *.conf.disabled.
    ConfD,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NginxNative {
    pub version: String,
    pub layout: NginxLayout,
    pub conf_dir: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NpmLocation {
    /// Docker container name running NPM, when detected.
    pub container: Option<String>,
    /// host:port of the admin API as seen from the server.
    pub target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NginxDetect {
    pub native: Option<NginxNative>,
    pub npm: Option<NpmLocation>,
    pub is_root: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NewSiteKind {
    Proxy,
    Static,
    Php,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewSiteRequest {
    pub name: String,
    pub domains: Vec<String>,
    pub kind: NewSiteKind,
    /// Proxy: upstream URL (http://127.0.0.1:3000). Static/PHP: document root.
    pub target: String,
    pub websocket: bool,
    /// Optional php-fpm socket for PHP sites.
    pub php_socket: Option<String>,
}

/// A planned change: the generated config and the exact shell command that applies it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NginxPlan {
    pub config: Option<String>,
    pub command: String,
}

// ---------------------------------------------------------------- Nginx Proxy Manager

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NpmLogin {
    pub host_id: String,
    pub identity: String,
    pub secret: String,
    pub remember: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct NpmProxyHost {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub domain_names: Vec<String>,
    #[serde(default = "default_scheme")]
    pub forward_scheme: String,
    #[serde(default)]
    pub forward_host: String,
    #[serde(default)]
    pub forward_port: u16,
    /// 0 = none, "new" when requesting a new Let's Encrypt cert.
    #[serde(default)]
    pub certificate_id: Value,
    #[serde(default, deserialize_with = "de_bool")]
    pub ssl_forced: bool,
    #[serde(default, deserialize_with = "de_bool")]
    pub hsts_enabled: bool,
    #[serde(default, deserialize_with = "de_bool")]
    pub http2_support: bool,
    #[serde(default, deserialize_with = "de_bool")]
    pub block_exploits: bool,
    #[serde(default, deserialize_with = "de_bool")]
    pub caching_enabled: bool,
    #[serde(default, deserialize_with = "de_bool")]
    pub allow_websocket_upgrade: bool,
    #[serde(default)]
    pub access_list_id: i64,
    #[serde(default)]
    pub advanced_config: String,
    #[serde(default = "default_true", deserialize_with = "de_bool")]
    pub enabled: bool,
    #[serde(default)]
    pub meta: Value,
    #[serde(default)]
    pub locations: Value,
    /// Expanded certificate (when listed with `?expand=certificate`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate: Option<NpmCertificate>,
    /// Fields we do not model are kept so a PUT round-trips them.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct NpmRedirectionHost {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub domain_names: Vec<String>,
    #[serde(default)]
    pub forward_scheme: String,
    #[serde(default)]
    pub forward_domain_name: String,
    #[serde(default)]
    pub forward_http_code: Value,
    #[serde(default, deserialize_with = "de_bool")]
    pub preserve_path: bool,
    #[serde(default)]
    pub certificate_id: Value,
    #[serde(default, deserialize_with = "de_bool")]
    pub ssl_forced: bool,
    #[serde(default = "default_true", deserialize_with = "de_bool")]
    pub enabled: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct NpmStream {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub incoming_port: u16,
    #[serde(default)]
    pub forwarding_host: String,
    #[serde(default)]
    pub forwarding_port: u16,
    #[serde(default, deserialize_with = "de_bool")]
    pub tcp_forwarding: bool,
    #[serde(default, deserialize_with = "de_bool")]
    pub udp_forwarding: bool,
    #[serde(default = "default_true", deserialize_with = "de_bool")]
    pub enabled: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct NpmCertificate {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub nice_name: String,
    #[serde(default)]
    pub domain_names: Vec<String>,
    /// "YYYY-MM-DD HH:MM:SS"
    #[serde(default)]
    pub expires_on: String,
    #[serde(default)]
    pub meta: Value,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct NpmAccessList {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub name: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NpmOverview {
    pub proxy_hosts: Vec<NpmProxyHost>,
    pub redirection_hosts: Vec<NpmRedirectionHost>,
    pub streams: Vec<NpmStream>,
    pub certificates: Vec<NpmCertificate>,
    pub access_lists: Vec<NpmAccessList>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum NpmKind {
    ProxyHosts,
    RedirectionHosts,
    Streams,
    DeadHosts,
}

impl NpmKind {
    pub fn path(self) -> &'static str {
        match self {
            NpmKind::ProxyHosts => "nginx/proxy-hosts",
            NpmKind::RedirectionHosts => "nginx/redirection-hosts",
            NpmKind::Streams => "nginx/streams",
            NpmKind::DeadHosts => "nginx/dead-hosts",
        }
    }
    /// Directory of the generated nginx config inside the NPM container.
    pub fn conf_dir(self) -> &'static str {
        match self {
            NpmKind::ProxyHosts => "/data/nginx/proxy_host",
            NpmKind::RedirectionHosts => "/data/nginx/redirection_host",
            NpmKind::Streams => "/data/nginx/stream",
            NpmKind::DeadHosts => "/data/nginx/dead_host",
        }
    }
}

fn default_scheme() -> String {
    "http".into()
}
fn default_true() -> bool {
    true
}

/// NPM returns booleans as `true`/`false` or as `0`/`1` depending on the endpoint/version.
fn de_bool<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Bool(b) => b,
        Value::Number(n) => n.as_i64().unwrap_or(0) != 0,
        Value::String(s) => matches!(s.as_str(), "1" | "true"),
        _ => false,
    })
}

/// Fields NPM accepts on create/update of a proxy host (the API rejects unknown/read-only ones).
pub fn npm_proxy_host_payload(h: &NpmProxyHost) -> Value {
    serde_json::json!({
        "domain_names": h.domain_names,
        "forward_scheme": h.forward_scheme,
        "forward_host": h.forward_host,
        "forward_port": h.forward_port,
        "certificate_id": if h.certificate_id.is_null() { Value::from(0) } else { h.certificate_id.clone() },
        "ssl_forced": h.ssl_forced,
        "hsts_enabled": h.hsts_enabled,
        "hsts_subdomains": h.extra.get("hsts_subdomains").and_then(Value::as_bool).unwrap_or(false),
        "http2_support": h.http2_support,
        "block_exploits": h.block_exploits,
        "caching_enabled": h.caching_enabled,
        "allow_websocket_upgrade": h.allow_websocket_upgrade,
        "access_list_id": h.access_list_id,
        "advanced_config": h.advanced_config,
        "meta": if h.meta.is_null() { serde_json::json!({}) } else { h.meta.clone() },
        "locations": if h.locations.is_null() { serde_json::json!([]) } else { h.locations.clone() },
    })
}

// ---------------------------------------------------------------- docker

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct DockerContainer {
    pub id: String,
    pub name: String,
    pub image: String,
    /// running, exited, restarting, paused, created, dead
    pub state: String,
    /// Human status from docker ("Up 3 days (healthy)").
    pub status: String,
    pub ports: String,
    pub created: String,
    /// Compose project / service (labels), if any.
    pub project: Option<String>,
    pub service: Option<String>,
    /// Filled by `docker_stats`.
    pub cpu: Option<f32>,
    pub mem: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct DockerImage {
    pub id: String,
    pub repository: String,
    pub tag: String,
    pub size: String,
    pub created: String,
    pub in_use: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct DockerVolume {
    pub name: String,
    pub driver: String,
    pub in_use: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DockerOverview {
    pub containers: Vec<DockerContainer>,
    pub images: Vec<DockerImage>,
    pub volumes: Vec<DockerVolume>,
    pub version: String,
}

/// Per-container live usage, keyed by full container id.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DockerStat {
    pub id: String,
    pub cpu: f32,
    pub mem: u64,
}

/// Current visitors of a container that serves the web: distinct client IPs with an open
/// connection to one of its web ports, or seen by Nginx Proxy Manager in the last minutes.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct DockerVisitors {
    /// Full container id.
    pub id: String,
    /// Distinct client IPs, both sources together.
    pub visitors: u32,
    /// Distinct IPs connected right now to a web port of the container.
    pub direct: u32,
    /// Distinct IPs in the proxy's access logs for the sites pointing to it (last 5 minutes).
    pub proxied: u32,
    /// Web ports the container listens on (inside the container).
    pub ports: Vec<u16>,
    /// Proxy host domains that point to the container.
    pub sites: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DockerAction {
    Start,
    Stop,
    Restart,
    Pause,
    Unpause,
    Remove,
    RemoveImage,
    RemoveVolume,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DockerRequest {
    pub action: DockerAction,
    /// Container ids/names, image ids or volume names.
    pub targets: Vec<String>,
}

// ---------------------------------------------------------------- security

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthKind {
    /// Login refused for an existing user.
    Failed,
    /// Login attempt with a user that does not exist.
    InvalidUser,
    /// Successful login.
    Accepted,
    /// Scanner: no SSH handshake, bad banner, no common algorithms…
    Probe,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuthEvent {
    pub time: String,
    pub kind: AuthKind,
    pub user: String,
    pub ip: String,
    /// publickey / password for accepted logins.
    pub method: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct IpStat {
    pub ip: String,
    pub attempts: u32,
    pub last: String,
    pub users: Vec<String>,
    /// Currently banned by fail2ban (any jail).
    pub banned: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SecurityReport {
    /// Log file or "journal" the events come from; empty when nothing was readable.
    pub source: String,
    /// When the log cannot be read: why, and a command that fixes it.
    pub hint: Option<String>,
    pub fix_command: Option<String>,
    pub first: String,
    pub last: String,
    pub failed: u32,
    pub invalid_user: u32,
    pub accepted: u32,
    pub probes: u32,
    pub top_ips: Vec<IpStat>,
    pub top_users: Vec<(String, u32)>,
    /// Attempts (failed + invalid user) per hour, oldest first, at most 48 buckets.
    pub hourly: Vec<(String, u32)>,
    /// Newest first, at most 500.
    pub events: Vec<AuthEvent>,
    /// `who` output: user, tty, from, since.
    pub sessions: Vec<String>,
    pub fail2ban: Fail2banStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Fail2banJail {
    pub name: String,
    pub enabled: bool,
    pub banned: Vec<String>,
    /// Bans seen in the log window (or "Total banned" from fail2ban-client).
    pub total_bans: u32,
    pub failures: u32,
    pub maxretry: String,
    pub findtime: String,
    pub bantime: String,
    pub port: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BanEvent {
    pub time: String,
    pub jail: String,
    pub ip: String,
    /// ban, unban, found
    pub action: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Fail2banStatus {
    pub installed: bool,
    pub running: bool,
    /// "client" (exact, as root) or "log" (estimated from fail2ban.log).
    pub source: String,
    pub jails: Vec<Fail2banJail>,
    /// Newest first, bans/unbans only, at most 200.
    pub recent: Vec<BanEvent>,
    pub config_files: Vec<String>,
    pub hint: Option<String>,
    /// [DEFAULT] values read from jail.local / jail.d (empty = fail2ban's built-in default).
    #[serde(default)]
    pub defaults: Fail2banJailConfig,
}

/// One section of the fail2ban config edited from the Security app. Empty values are inherited.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Fail2banJailConfig {
    pub name: String,
    pub enabled: bool,
    pub maxretry: String,
    pub findtime: String,
    pub bantime: String,
    pub port: String,
    /// Only used for [DEFAULT].
    #[serde(default)]
    pub ignoreip: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Fail2banConfig {
    pub defaults: Fail2banJailConfig,
    pub jails: Vec<Fail2banJailConfig>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Fail2banAction {
    Ban,
    Unban,
    Restart,
    Install,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn npm_proxy_host_roundtrip_keeps_unknown_fields() {
        let raw = r#"{"id":3,"created_on":"2024-01-01 10:00:00","owner_user_id":1,
            "domain_names":["app.example.com"],"forward_scheme":"http","forward_host":"10.0.0.5",
            "forward_port":3000,"certificate_id":7,"ssl_forced":1,"http2_support":0,
            "block_exploits":true,"allow_websocket_upgrade":1,"access_list_id":0,
            "advanced_config":"","enabled":1,"meta":{"letsencrypt_agree":true},"locations":[]}"#;
        let h: NpmProxyHost = serde_json::from_str(raw).unwrap();
        assert_eq!(h.id, 3);
        assert!(h.ssl_forced && h.block_exploits && h.allow_websocket_upgrade && h.enabled);
        assert!(!h.http2_support);
        assert_eq!(h.extra.get("owner_user_id"), Some(&Value::from(1)));
        let p = npm_proxy_host_payload(&h);
        assert_eq!(p["forward_port"], 3000);
        assert_eq!(p["certificate_id"], 7);
        assert!(p.get("id").is_none());
    }

    #[test]
    fn error_payload_tagging() {
        let e = ErrorPayload::NeedsTerminal { command: "sudo nginx -t".into() };
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains("\"kind\":\"needs_terminal\""));
    }
}

/// A systemd service in the `failed` state (notification centre).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FailedService {
    pub unit: String,
    pub description: String,
}

// ---------------------------------------------------------------- browser

/// A TCP port listening on the server (`ss -ltn`), offered as a shortcut in the browser.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListeningPort {
    /// Bound address: `0.0.0.0`, `127.0.0.1`…
    pub addr: String,
    pub port: u16,
    /// Owning process when visible to the SSH user.
    pub process: Option<String>,
}

/// Result of an HTTP request made from the server itself.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PingResult {
    /// HTTP status; 0 when nothing answered.
    pub code: u16,
    pub ms: f64,
    pub error: Option<String>,
}

/// A user-defined SSH local port forward (Tunnels app): `127.0.0.1:local` → `remote` seen from the server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TunnelInfo {
    pub id: u32,
    pub host_id: String,
    pub local_port: u16,
    /// `host:port` as seen from the server (`localhost:5432`, `10.0.0.5:80`…).
    pub remote: String,
    pub active: bool,
}

// ---------- Homelab diagram ----------

/// Everything the homelab diagram shows, collected read-only in one pass.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct HomelabSnapshot {
    pub hostname: String,
    pub os: String,
    pub kernel: String,
    pub cpu: String,
    pub cores: u32,
    /// Bytes.
    pub mem_total: u64,
    pub ips: Vec<String>,
    /// Server date, YYYY-MM-DD.
    pub date: String,
    /// Empty when docker is missing or not readable by this user.
    pub docker_version: String,
    pub docker_root: String,
    pub disks: Vec<HomelabDisk>,
    pub networks: Vec<HomelabNetwork>,
    pub containers: Vec<HomelabContainer>,
    pub proxies: Vec<HomelabProxy>,
    pub open_ports: Vec<HomelabPort>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct HomelabDisk {
    /// Device name (sda, nvme0n1, vda), or the filesystem source when it has no block device (ZFS, NFS…).
    pub name: String,
    pub model: String,
    /// "NVMe", "SSD", "HDD" or "" when unknown.
    pub kind: String,
    pub size: u64,
    pub filesystems: Vec<HomelabFs>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct HomelabFs {
    pub source: String,
    pub mount: String,
    pub fstype: String,
    pub size: u64,
    pub used: u64,
    /// Containers whose volumes or bind mounts live here.
    pub containers: Vec<String>,
    /// Docker's own data (images, layers) lives here.
    pub docker_root: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct HomelabNetwork {
    pub name: String,
    pub driver: String,
    pub subnet: String,
    pub containers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct HomelabContainer {
    pub name: String,
    pub image: String,
    pub state: String,
    /// Compose project, empty for standalone containers.
    pub project: String,
    /// (network, ip)
    pub networks: Vec<(String, String)>,
    /// "8080→80/tcp", or "80/tcp" when not published.
    pub ports: Vec<String>,
    pub mounts: Vec<HomelabMount>,
    /// Domains routed here by nginx or Nginx Proxy Manager.
    pub domains: Vec<String>,
    /// Reachable from outside the host (published on a non-loopback address, or behind the proxy).
    pub public: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct HomelabMount {
    /// "volume" or "bind".
    pub kind: String,
    /// Volume name, or host path for a bind mount.
    pub source: String,
    pub destination: String,
    /// Mount point of the host filesystem holding it.
    pub fs: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct HomelabProxy {
    pub domains: Vec<String>,
    /// Upstream as written in the config (http://127.0.0.1:8080, app:3000…).
    pub target: String,
    pub container: Option<String>,
    pub ssl: bool,
    /// "npm" or "nginx".
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct HomelabPort {
    pub port: u16,
    /// "tcp" or "udp".
    pub proto: String,
    pub service: String,
}
