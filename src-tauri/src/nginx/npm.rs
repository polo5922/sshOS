//! Nginx Proxy Manager client. The admin API (port 81 on the server, usually only bound inside
//! Docker or to localhost) is reached through an SSH `direct-tcpip` tunnel, never exposed.
//! All changes go through the API: NPM regenerates and reloads its nginx config itself, so editing
//! the generated files directly would be overwritten.

use std::collections::HashMap;

use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sshos_shared::{
    npm_proxy_host_payload, NpmAccessList, NpmCertificate, NpmKind, NpmOverview, NpmProxyHost,
    NpmRedirectionHost, NpmStream,
};
use tokio::sync::Mutex;

use crate::error::{AppError, AppResult};
use crate::hosts;

#[derive(Serialize, Deserialize)]
struct Creds {
    identity: String,
    secret: String,
}

#[derive(Clone)]
struct Conn {
    base: String,
    token: Option<String>,
}

pub struct NpmClients {
    http: reqwest::Client,
    conns: Mutex<HashMap<String, Conn>>,
}

impl Default for NpmClients {
    fn default() -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120)) // LE issuance can be slow
                .build()
                .expect("reqwest client"),
            conns: Mutex::new(HashMap::new()),
        }
    }
}

/// Keys NPM's JSON schemas reject on create/update.
const READ_ONLY: &[&str] = &[
    "id", "created_on", "modified_on", "owner_user_id", "owner", "certificate", "access_list",
    "is_deleted", "status",
];

pub fn clean_payload(mut v: Value) -> Value {
    if let Some(o) = v.as_object_mut() {
        for k in READ_ONLY {
            o.remove(*k);
        }
    }
    v
}

impl NpmClients {
    /// Register the tunnel base URL for a host (http://127.0.0.1:<local port>).
    pub async fn set_base(&self, host_id: &str, base: String) {
        let mut conns = self.conns.lock().await;
        match conns.get_mut(host_id) {
            Some(c) if c.base == base => {}
            Some(c) => {
                c.base = base;
            }
            None => {
                conns.insert(host_id.into(), Conn { base, token: None });
            }
        }
    }

    pub async fn forget(&self, host_id: &str) {
        self.conns.lock().await.remove(host_id);
    }

    async fn base(&self, host_id: &str) -> AppResult<String> {
        self.conns
            .lock()
            .await
            .get(host_id)
            .map(|c| c.base.clone())
            .ok_or_else(|| AppError::msg("Nginx Proxy Manager not connected"))
    }

    pub async fn login(&self, host_id: &str, identity: &str, secret: &str, remember: bool) -> AppResult<()> {
        let base = self.base(host_id).await?;
        let res = self
            .http
            .post(format!("{base}/api/tokens"))
            .json(&json!({ "identity": identity, "secret": secret }))
            .send()
            .await
            .map_err(|e| AppError::msg(format!("NPM API unreachable through the SSH tunnel: {e}")))?;
        let status = res.status();
        let body: Value = res.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            return Err(AppError::npm_auth(api_error(&body, status)));
        }
        let token = body["token"]
            .as_str()
            .ok_or_else(|| AppError::msg("NPM: no token in response"))?
            .to_string();
        if let Some(c) = self.conns.lock().await.get_mut(host_id) {
            c.token = Some(token);
        }
        if remember {
            hosts::set_secret(
                &hosts::npm_secret_key(host_id),
                &serde_json::to_string(&Creds { identity: identity.into(), secret: secret.into() })?,
            )?;
        }
        Ok(())
    }

    pub async fn logout(&self, host_id: &str) -> AppResult<()> {
        if let Some(c) = self.conns.lock().await.get_mut(host_id) {
            c.token = None;
        }
        hosts::delete_secret(&hosts::npm_secret_key(host_id))
    }

    async fn token(&self, host_id: &str, force_login: bool) -> AppResult<String> {
        if !force_login {
            if let Some(t) = self.conns.lock().await.get(host_id).and_then(|c| c.token.clone()) {
                return Ok(t);
            }
        }
        let creds: Creds = hosts::get_secret(&hosts::npm_secret_key(host_id))
            .and_then(|s| serde_json::from_str(&s).ok())
            .ok_or_else(|| AppError::npm_auth("Sign in to Nginx Proxy Manager"))?;
        self.login(host_id, &creds.identity, &creds.secret, false).await?;
        self.conns
            .lock()
            .await
            .get(host_id)
            .and_then(|c| c.token.clone())
            .ok_or_else(|| AppError::npm_auth("Sign in to Nginx Proxy Manager"))
    }

    /// Authenticated request; re-logs in once with the saved credentials on 401.
    pub async fn request(&self, host_id: &str, method: Method, path: &str, body: Option<Value>) -> AppResult<Value> {
        let base = self.base(host_id).await?;
        let mut retried = false;
        loop {
            let token = self.token(host_id, retried).await?;
            let mut req = self.http.request(method.clone(), format!("{base}/api/{path}")).bearer_auth(token);
            if let Some(b) = &body {
                req = req.json(b);
            }
            let res = req.send().await.map_err(|e| AppError::msg(format!("NPM API: {e}")))?;
            let status = res.status();
            let text = res.text().await.unwrap_or_default();
            let val: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
            if status == StatusCode::UNAUTHORIZED && !retried {
                if let Some(c) = self.conns.lock().await.get_mut(host_id) {
                    c.token = None;
                }
                retried = true;
                continue;
            }
            if status == StatusCode::UNAUTHORIZED {
                return Err(AppError::npm_auth(api_error(&val, status)));
            }
            if !status.is_success() {
                return Err(AppError::msg(format!("NPM: {}", api_error(&val, status))));
            }
            return Ok(val);
        }
    }

    pub async fn overview(&self, host_id: &str) -> AppResult<NpmOverview> {
        let g = |p: &'static str| self.request(host_id, Method::GET, p, None);
        let (ph, rh, st, ce, al) = tokio::join!(
            g("nginx/proxy-hosts?expand=certificate,access_list"),
            g("nginx/redirection-hosts?expand=certificate"),
            g("nginx/streams"),
            g("nginx/certificates"),
            g("nginx/access-lists"),
        );
        Ok(NpmOverview {
            proxy_hosts: from::<Vec<NpmProxyHost>>(ph?)?,
            redirection_hosts: from::<Vec<NpmRedirectionHost>>(rh?)?,
            streams: from::<Vec<NpmStream>>(st?).unwrap_or_default(),
            certificates: from::<Vec<NpmCertificate>>(ce?)?,
            access_lists: from::<Vec<NpmAccessList>>(al?).unwrap_or_default(),
        })
    }

    pub async fn save_proxy_host(&self, host_id: &str, h: &NpmProxyHost) -> AppResult<NpmProxyHost> {
        let body = npm_proxy_host_payload(h);
        let v = if h.id > 0 {
            self.request(host_id, Method::PUT, &format!("nginx/proxy-hosts/{}", h.id), Some(body)).await?
        } else {
            self.request(host_id, Method::POST, "nginx/proxy-hosts", Some(body)).await?
        };
        from(v)
    }

    /// Generic create/update for redirection hosts, streams and 404 hosts.
    pub async fn save(&self, host_id: &str, kind: NpmKind, id: Option<i64>, body: Value) -> AppResult<Value> {
        let body = clean_payload(body);
        match id.filter(|i| *i > 0) {
            Some(id) => self.request(host_id, Method::PUT, &format!("{}/{id}", kind.path()), Some(body)).await,
            None => self.request(host_id, Method::POST, kind.path(), Some(body)).await,
        }
    }

    pub async fn delete(&self, host_id: &str, kind: NpmKind, id: i64) -> AppResult<()> {
        self.request(host_id, Method::DELETE, &format!("{}/{id}", kind.path()), None).await?;
        Ok(())
    }

    pub async fn set_enabled(&self, host_id: &str, kind: NpmKind, id: i64, enabled: bool) -> AppResult<()> {
        let action = if enabled { "enable" } else { "disable" };
        self.request(host_id, Method::POST, &format!("{}/{id}/{action}", kind.path()), None).await?;
        Ok(())
    }

    pub async fn create_certificate(&self, host_id: &str, domains: Vec<String>, email: String) -> AppResult<NpmCertificate> {
        let body = json!({
            "provider": "letsencrypt",
            "nice_name": domains.first().cloned().unwrap_or_default(),
            "domain_names": domains,
            "meta": { "letsencrypt_email": email, "letsencrypt_agree": true, "dns_challenge": false }
        });
        from(self.request(host_id, Method::POST, "nginx/certificates", Some(body)).await?)
    }

    pub async fn renew_certificate(&self, host_id: &str, id: i64) -> AppResult<()> {
        self.request(host_id, Method::POST, &format!("nginx/certificates/{id}/renew"), None).await?;
        Ok(())
    }

    pub async fn delete_certificate(&self, host_id: &str, id: i64) -> AppResult<()> {
        self.request(host_id, Method::DELETE, &format!("nginx/certificates/{id}"), None).await?;
        Ok(())
    }
}

fn from<T: serde::de::DeserializeOwned>(v: Value) -> AppResult<T> {
    serde_json::from_value(v).map_err(|e| AppError::msg(format!("Unexpected NPM response: {e}")))
}

fn api_error(body: &Value, status: StatusCode) -> String {
    body["error"]["message"]
        .as_str()
        .map(str::to_string)
        .or_else(|| body.as_str().map(str::to_string).filter(|s| !s.is_empty()))
        .unwrap_or_else(|| status.to_string())
}

/// `docker ps` line: name \t image \t ports  ->  NPM admin target as seen from the server.
pub fn parse_docker_ps(out: &str) -> Option<(String, Option<String>)> {
    for line in out.lines() {
        let mut parts = line.split('\t');
        let (Some(name), Some(image)) = (parts.next(), parts.next()) else { continue };
        if !image.contains("nginx-proxy-manager") {
            continue;
        }
        let ports = parts.next().unwrap_or("");
        // e.g. "0.0.0.0:80-81->80-81/tcp, 0.0.0.0:443->443/tcp" or "127.0.0.1:8181->81/tcp"
        let mut target = None;
        for p in ports.split(", ") {
            let Some((host, cont)) = p.split_once("->") else { continue };
            let cont = cont.trim_end_matches("/tcp");
            let (hip, hport) = host.rsplit_once(':').unwrap_or(("0.0.0.0", host));
            let hip = match hip.trim_matches(['[', ']']) {
                "0.0.0.0" | "::" | "" => "127.0.0.1",
                ip => ip,
            };
            if cont == "81" {
                target = Some(format!("{hip}:{hport}"));
            } else if let (Some((c0, c1)), Some((h0, _))) = (cont.split_once('-'), hport.split_once('-')) {
                // Port ranges.
                if let (Ok(c0), Ok(c1), Ok(h0)) = (c0.parse::<u16>(), c1.parse::<u16>(), h0.parse::<u16>()) {
                    if (c0..=c1).contains(&81) {
                        target = Some(format!("{hip}:{}", h0 + (81 - c0)));
                    }
                }
            }
            if target.is_some() {
                break;
            }
        }
        return Some((name.to_string(), target));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_ps_published() {
        let out = "web\tnginx:latest\t0.0.0.0:8080->80/tcp\nnpm\tjc21/nginx-proxy-manager:latest\t0.0.0.0:80->80/tcp, :::80->80/tcp, 127.0.0.1:8181->81/tcp, 0.0.0.0:443->443/tcp\n";
        assert_eq!(parse_docker_ps(out), Some(("npm".into(), Some("127.0.0.1:8181".into()))));
    }

    #[test]
    fn docker_ps_range_and_unpublished() {
        let out = "npm\tjc21/nginx-proxy-manager\t0.0.0.0:80-81->80-81/tcp\n";
        assert_eq!(parse_docker_ps(out), Some(("npm".into(), Some("127.0.0.1:81".into()))));
        let out = "app\tjc21/nginx-proxy-manager:2\t80/tcp, 81/tcp\n";
        assert_eq!(parse_docker_ps(out), Some(("app".into(), None)));
        assert_eq!(parse_docker_ps("x\tredis\t\n"), None);
    }

    #[test]
    fn payload_cleaning() {
        let v = clean_payload(json!({"id": 1, "created_on": "x", "forward_domain_name": "a.com"}));
        assert_eq!(v, json!({"forward_domain_name": "a.com"}));
    }
}
