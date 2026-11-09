//! Connection pool: one SSH session (and one SFTP session) per host, shared by every app.

pub mod session;

use std::collections::HashMap;
use std::sync::Arc;

use russh_sftp::client::SftpSession;
use tokio::sync::Mutex;

use crate::error::{AppError, AppResult};
use crate::hosts::HostStore;
pub use session::{sh_quote, Session};

#[derive(Default)]
pub struct Pool {
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    sftp: Mutex<HashMap<String, Arc<SftpSession>>>,
    /// Local port forwards keyed by (host id, "remote_host:port").
    forwards: Mutex<HashMap<(String, String), (u16, tokio::task::AbortHandle)>>,
    /// Tunnels opened by the user in the Tunnels app (fixed local port, listed and closable).
    tunnels: Mutex<Vec<(sshos_shared::TunnelInfo, tokio::task::AbortHandle)>>,
    next_tunnel: std::sync::atomic::AtomicU32,
}

impl Pool {
    pub async fn connect(
        &self,
        store: &HostStore,
        host_id: &str,
        accept_fingerprint: Option<String>,
    ) -> AppResult<Arc<Session>> {
        if let Some(s) = self.sessions.lock().await.get(host_id) {
            if !s.is_closed() {
                return Ok(s.clone());
            }
        }
        let host = store.get(host_id)?;
        let jump = match &host.jump {
            Some(j) if j != host_id => {
                // A jump host is never connected with the target's accepted fingerprint.
                Some(Box::pin(self.connect(store, j, None)).await?)
            }
            _ => None,
        };
        let s = Arc::new(Session::connect(host, jump, accept_fingerprint).await?);
        self.sessions.lock().await.insert(host_id.to_string(), s.clone());
        self.sftp.lock().await.remove(host_id);
        Ok(s)
    }

    /// Already connected session (reconnects silently if the host key is known).
    pub async fn get(&self, store: &HostStore, host_id: &str) -> AppResult<Arc<Session>> {
        self.connect(store, host_id, None).await
    }

    pub async fn disconnect(&self, host_id: &str) {
        self.sftp.lock().await.remove(host_id);
        self.tunnels.lock().await.retain(|(t, abort)| {
            if t.host_id == host_id {
                abort.abort();
                false
            } else {
                true
            }
        });
        let mut fw = self.forwards.lock().await;
        fw.retain(|(h, _), (_, abort)| {
            if h == host_id {
                abort.abort();
                false
            } else {
                true
            }
        });
        drop(fw);
        if let Some(s) = self.sessions.lock().await.remove(host_id) {
            let _ = s
                .handle
                .disconnect(russh::Disconnect::ByApplication, "bye", "en")
                .await;
        }
    }

    pub async fn connected_ids(&self) -> Vec<String> {
        self.sessions
            .lock()
            .await
            .iter()
            .filter(|(_, s)| !s.is_closed())
            .map(|(k, _)| k.clone())
            .collect()
    }

    pub async fn sftp(&self, store: &HostStore, host_id: &str) -> AppResult<Arc<SftpSession>> {
        if let Some(s) = self.sftp.lock().await.get(host_id) {
            return Ok(s.clone());
        }
        let session = self.get(store, host_id).await?;
        let ch = session.handle.channel_open_session().await?;
        ch.request_subsystem(true, "sftp").await?;
        let sftp = Arc::new(SftpSession::new(ch.into_stream()).await?);
        self.sftp.lock().await.insert(host_id.to_string(), sftp.clone());
        Ok(sftp)
    }

    /// Drop a broken SFTP session so the next call reopens it.
    pub async fn reset_sftp(&self, host_id: &str) {
        self.sftp.lock().await.remove(host_id);
    }

    /// Listen on 127.0.0.1:<random> and tunnel each connection to `remote` (host:port) through SSH.
    pub async fn local_forward(&self, store: &HostStore, host_id: &str, remote: &str) -> AppResult<u16> {
        let key = (host_id.to_string(), remote.to_string());
        if let Some((port, abort)) = self.forwards.lock().await.get(&key) {
            if !abort.is_finished() {
                return Ok(*port);
            }
        }
        let (rhost, rport) = split_host_port(remote)?;
        let session = self.get(store, host_id).await?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let task = spawn_forward(session, listener, rhost, rport);
        self.forwards.lock().await.insert(key, (port, task.abort_handle()));
        Ok(port)
    }

    /// Opens a user tunnel on `127.0.0.1:local_port` (0 = any free port).
    pub async fn tunnel_open(&self, store: &HostStore, host_id: &str, remote: &str, local_port: u16) -> AppResult<sshos_shared::TunnelInfo> {
        let (rhost, rport) = split_host_port(remote)?;
        let session = self.get(store, host_id).await?;
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", local_port))
            .await
            .map_err(|e| AppError::msg(format!("127.0.0.1:{local_port}: {e}")))?;
        let port = listener.local_addr()?.port();
        let task = spawn_forward(session, listener, rhost, rport);
        let info = sshos_shared::TunnelInfo {
            id: self.next_tunnel.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1,
            host_id: host_id.to_string(),
            local_port: port,
            remote: remote.trim().to_string(),
            active: true,
        };
        self.tunnels.lock().await.push((info.clone(), task.abort_handle()));
        Ok(info)
    }

    pub async fn tunnel_list(&self) -> Vec<sshos_shared::TunnelInfo> {
        self.tunnels
            .lock()
            .await
            .iter()
            .map(|(t, a)| sshos_shared::TunnelInfo { active: !a.is_finished(), ..t.clone() })
            .collect()
    }

    pub async fn tunnel_close(&self, id: u32) {
        self.tunnels.lock().await.retain(|(t, a)| {
            if t.id == id {
                a.abort();
                false
            } else {
                true
            }
        });
    }
}

/// Accepts local connections and pipes each one through a `direct-tcpip` channel.
fn spawn_forward(session: Arc<Session>, listener: tokio::net::TcpListener, rhost: String, rport: u16) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, peer)) = listener.accept().await else { break };
            if session.is_closed() {
                break;
            }
            let session = session.clone();
            let rhost = rhost.clone();
            tokio::spawn(async move {
                match session
                    .handle
                    .channel_open_direct_tcpip(rhost, rport as u32, peer.ip().to_string(), peer.port() as u32)
                    .await
                {
                    Ok(ch) => {
                        let mut stream = ch.into_stream();
                        let _ = tokio::io::copy_bidirectional(&mut sock, &mut stream).await;
                    }
                    Err(e) => log::warn!("direct-tcpip failed: {e}"),
                }
            });
        }
    })
}

pub fn split_host_port(s: &str) -> AppResult<(String, u16)> {
    let s = s.trim().trim_start_matches("http://").trim_end_matches('/');
    let (h, p) = s
        .rsplit_once(':')
        .ok_or_else(|| AppError::msg(format!("Expected host:port, got `{s}`")))?;
    let port = p.parse().map_err(|_| AppError::msg(format!("Bad port in `{s}`")))?;
    Ok((h.trim_matches(['[', ']']).to_string(), port))
}

#[cfg(test)]
mod tests {
    #[test]
    fn host_port() {
        assert_eq!(super::split_host_port("127.0.0.1:81").unwrap(), ("127.0.0.1".into(), 81));
        assert_eq!(super::split_host_port("http://172.17.0.2:81/").unwrap(), ("172.17.0.2".into(), 81));
        assert!(super::split_host_port("nope").is_err());
    }
}
