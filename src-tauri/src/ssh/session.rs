//! One authenticated SSH connection per host (optionally through a ProxyJump host).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::client::{self, Handle};
use russh::keys::{self, HashAlg, PrivateKeyWithHashAlg, PublicKey, PublicKeyOrCertificate};
use russh::ChannelMsg;
use sshos_shared::{AuthMethod, ErrorPayload, ExecResult, HostConfig};

use crate::error::{AppError, AppResult};
use crate::hosts;

/// russh handler: verifies the server key against ~/.ssh/known_hosts.
pub struct Client {
    host: String,
    port: u16,
    accept_fingerprint: Option<String>,
    /// Set when the key check fails so we can report something better than "disconnected".
    rejection: Arc<Mutex<Option<ErrorPayload>>>,
}

impl client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let key: &PublicKey = match key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key,
            PublicKeyOrCertificate::Certificate(c) => {
                // Treat certificates by their underlying key.
                let k = PublicKey::from(c.public_key().clone());
                return self.check_key(&k);
            }
        };
        self.check_key(key)
    }
}

impl Client {
    fn check_key(&mut self, key: &PublicKey) -> Result<bool, russh::Error> {
        let fp = key.fingerprint(HashAlg::Sha256).to_string();
        match keys::check_known_hosts(&self.host, self.port, key) {
            Ok(true) => Ok(true),
            Ok(false) => {
                if self.accept_fingerprint.as_deref() == Some(fp.as_str()) {
                    if let Err(e) = keys::known_hosts::learn_known_hosts(&self.host, self.port, key) {
                        log::warn!("could not write known_hosts: {e}");
                    }
                    Ok(true)
                } else {
                    *self.rejection.lock().unwrap() = Some(ErrorPayload::UnknownHostKey {
                        host: format!("{}:{}", self.host, self.port),
                        fingerprint: fp,
                        algorithm: key.algorithm().to_string(),
                    });
                    Ok(false)
                }
            }
            Err(keys::Error::KeyChanged { line }) => {
                *self.rejection.lock().unwrap() = Some(ErrorPayload::HostKeyChanged {
                    host: format!("{}:{}", self.host, self.port),
                    line,
                });
                Ok(false)
            }
            Err(e) => {
                // No known_hosts file yet: same as unknown.
                log::debug!("known_hosts check: {e}");
                if self.accept_fingerprint.as_deref() == Some(fp.as_str()) {
                    let _ = keys::known_hosts::learn_known_hosts(&self.host, self.port, key);
                    Ok(true)
                } else {
                    *self.rejection.lock().unwrap() = Some(ErrorPayload::UnknownHostKey {
                        host: format!("{}:{}", self.host, self.port),
                        fingerprint: fp,
                        algorithm: key.algorithm().to_string(),
                    });
                    Ok(false)
                }
            }
        }
    }
}

pub struct Session {
    pub host: HostConfig,
    pub handle: Handle<Client>,
    /// Keeps the jump host connection alive.
    _jump: Option<Arc<Session>>,
}

fn config() -> Arc<client::Config> {
    Arc::new(client::Config {
        inactivity_timeout: None,
        keepalive_interval: Some(Duration::from_secs(20)),
        keepalive_max: 3,
        nodelay: true,
        ..Default::default()
    })
}

impl Session {
    /// Connect and authenticate. `jump` is an already connected session used as ProxyJump.
    pub async fn connect(
        host: HostConfig,
        jump: Option<Arc<Session>>,
        accept_fingerprint: Option<String>,
    ) -> AppResult<Session> {
        let rejection = Arc::new(Mutex::new(None));
        let handler = Client {
            host: host.host.clone(),
            port: host.port,
            accept_fingerprint,
            rejection: rejection.clone(),
        };

        let connect = async {
            match &jump {
                Some(j) => {
                    let ch = j
                        .handle
                        .channel_open_direct_tcpip(host.host.clone(), host.port as u32, "127.0.0.1", 0)
                        .await?;
                    client::connect_stream(config(), ch.into_stream(), handler).await
                }
                None => client::connect(config(), (host.host.as_str(), host.port), handler).await,
            }
        };
        let mut handle = match tokio::time::timeout(Duration::from_secs(20), connect).await {
            Err(_) => return Err(AppError::msg(format!("Timed out connecting to {}", host.host))),
            Ok(Err(e)) => {
                if let Some(p) = rejection.lock().unwrap().take() {
                    return Err(AppError(p));
                }
                return Err(AppError::msg(format!("{}: {e}", host.host)));
            }
            Ok(Ok(h)) => h,
        };

        authenticate(&mut handle, &host).await?;
        Ok(Session { host, handle, _jump: jump })
    }

    pub fn is_closed(&self) -> bool {
        self.handle.is_closed()
    }

    /// Run a command and collect its output. Recorded in the command log.
    pub async fn exec(&self, command: &str) -> AppResult<ExecResult> {
        self.exec_logged(command, Some(sshos_shared::CmdLogKind::Exec)).await
    }

    /// Read-only query from an app view: logged as a read.
    pub async fn exec_read(&self, command: &str) -> AppResult<ExecResult> {
        self.exec_logged(command, Some(sshos_shared::CmdLogKind::Read)).await
    }

    /// Periodic pollers (monitor, notifications, live stats): not logged, they would drown it.
    pub async fn exec_quiet(&self, command: &str) -> AppResult<ExecResult> {
        self.exec_logged(command, None).await
    }

    async fn exec_logged(&self, command: &str, kind: Option<sshos_shared::CmdLogKind>) -> AppResult<ExecResult> {
        let started = std::time::Instant::now();
        let r = self.exec_raw(command).await;
        if let Some(kind) = kind {
            let code = r.as_ref().ok().and_then(|r| r.code);
            crate::cmdlog::record(&self.host.id, &self.host.name, command, kind, code, started.elapsed().as_millis() as u64);
        }
        r
    }

    async fn exec_raw(&self, command: &str) -> AppResult<ExecResult> {
        let mut ch = self.handle.channel_open_session().await?;
        ch.exec(true, command).await?;
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut code = None;
        while let Some(msg) = ch.wait().await {
            match msg {
                ChannelMsg::Data { data } => out.extend_from_slice(&data),
                ChannelMsg::ExtendedData { data, .. } => err.extend_from_slice(&data),
                ChannelMsg::ExitStatus { exit_status } => code = Some(exit_status),
                _ => {}
            }
        }
        Ok(ExecResult {
            stdout: String::from_utf8_lossy(&out).into_owned(),
            stderr: String::from_utf8_lossy(&err).into_owned(),
            code,
        })
    }

    /// Like `exec` but fails on a non-zero exit code.
    pub async fn exec_ok(&self, command: &str) -> AppResult<String> {
        let r = self.exec(command).await?;
        if r.ok() {
            Ok(r.stdout)
        } else {
            let msg = if r.stderr.trim().is_empty() { r.stdout } else { r.stderr };
            Err(AppError::msg(format!("`{command}` failed: {}", msg.trim())))
        }
    }
}

async fn authenticate(handle: &mut Handle<Client>, host: &HostConfig) -> AppResult<()> {
    let user = host.user.clone();
    let secret = hosts::get_secret(&hosts::host_secret_key(&host.id));
    let ok = match &host.auth {
        AuthMethod::Password => {
            let pw = secret.ok_or_else(|| AppError::msg("No password saved for this host"))?;
            handle.authenticate_password(user, pw).await?.success()
        }
        AuthMethod::Key { path } => {
            let path = expand_tilde(path);
            let key = keys::load_secret_key(&path, secret.as_deref())
                .map_err(|e| AppError::msg(format!("{path}: {e}")))?;
            let hash = handle.best_supported_rsa_hash().await?.flatten();
            handle
                .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                .await?
                .success()
        }
        AuthMethod::Agent => match auth_agent(handle, &user).await {
            Ok(true) => true,
            // Like OpenSSH: no agent (or it had no accepted key) -> try the default key files.
            agent => match auth_default_keys(handle, &user).await? {
                true => true,
                false => {
                    agent?;
                    false
                }
            },
        },
    };
    if ok {
        Ok(())
    } else {
        Err(AppError::msg(format!("Authentication failed for {}@{}", host.user, host.host)))
    }
}

/// Tries ~/.ssh/id_ed25519, id_ecdsa, id_rsa in turn (unencrypted keys only; key contents stay in memory).
async fn auth_default_keys(handle: &mut Handle<Client>, user: &str) -> AppResult<bool> {
    for name in ["id_ed25519", "id_ecdsa", "id_rsa"] {
        let path = expand_tilde(&format!("~/.ssh/{name}"));
        let Ok(key) = keys::load_secret_key(&path, None) else { continue };
        let hash = handle.best_supported_rsa_hash().await?.flatten();
        if handle
            .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
            .await?
            .success()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(windows)]
async fn auth_agent(handle: &mut Handle<Client>, user: &str) -> AppResult<bool> {
    use russh::keys::agent::client::AgentClient;
    // Windows OpenSSH agent first, then Pageant.
    if let Ok(mut agent) = AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent").await {
        if try_agent(handle, user, &mut agent).await? {
            return Ok(true);
        }
    }
    if let Ok(mut agent) = AgentClient::connect_pageant().await {
        return try_agent(handle, user, &mut agent).await;
    }
    Err(AppError::msg("No SSH agent running (OpenSSH agent service or Pageant)"))
}

#[cfg(not(windows))]
async fn auth_agent(handle: &mut Handle<Client>, user: &str) -> AppResult<bool> {
    use russh::keys::agent::client::AgentClient;
    let mut agent = AgentClient::connect_env()
        .await
        .map_err(|e| AppError::msg(format!("SSH agent: {e}")))?;
    try_agent(handle, user, &mut agent).await
}

async fn try_agent<S>(
    handle: &mut Handle<Client>,
    user: &str,
    agent: &mut russh::keys::agent::client::AgentClient<S>,
) -> AppResult<bool>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let ids = agent.request_identities().await?;
    let hash = handle.best_supported_rsa_hash().await?.flatten();
    for id in ids {
        let key = match &id {
            russh::keys::agent::AgentIdentity::PublicKey { key, .. } => key.clone(),
            _ => continue,
        };
        match handle.authenticate_publickey_with(user, key, hash, agent).await {
            Ok(r) if r.success() => return Ok(true),
            Ok(_) => continue,
            Err(e) => log::debug!("agent key rejected: {e}"),
        }
    }
    Ok(false)
}

pub fn expand_tilde(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest).display().to_string();
        }
    }
    p.to_string()
}

/// Quote a string for a POSIX shell.
pub fn sh_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::sh_quote;

    #[test]
    fn quoting() {
        assert_eq!(sh_quote("/etc/nginx"), "/etc/nginx");
        assert_eq!(sh_quote("a b"), "'a b'");
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        assert_eq!(sh_quote(""), "''");
    }
}
