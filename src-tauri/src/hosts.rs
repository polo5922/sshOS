//! Saved connections (hosts.json in the app config dir) and secrets (OS keychain).

use std::path::PathBuf;
use std::sync::Mutex;

use sshos_shared::{AuthMethod, HostConfig};

use crate::error::{AppError, AppResult};

const KEYRING_SERVICE: &str = "sshOs";

pub struct HostStore {
    path: PathBuf,
    hosts: Mutex<Vec<HostConfig>>,
}

impl HostStore {
    pub fn load() -> Self {
        let dir = dirs::config_dir().unwrap_or_else(std::env::temp_dir).join("sshOs");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("hosts.json");
        let hosts = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self { path, hosts: Mutex::new(hosts) }
    }

    /// Store that is never written to disk where the user's hosts live (tests only).
    #[cfg(test)]
    pub fn in_memory(hosts: Vec<HostConfig>) -> Self {
        let path = std::env::temp_dir().join(format!("sshos-test-{}.json", std::process::id()));
        Self { path, hosts: Mutex::new(hosts) }
    }

    pub fn list(&self) -> Vec<HostConfig> {
        self.hosts.lock().unwrap().clone()
    }

    pub fn get(&self, id: &str) -> AppResult<HostConfig> {
        self.hosts
            .lock()
            .unwrap()
            .iter()
            .find(|h| h.id == id)
            .cloned()
            .ok_or_else(|| AppError::msg(format!("Unknown host {id}")))
    }

    pub fn upsert(&self, mut host: HostConfig) -> AppResult<HostConfig> {
        if host.id.is_empty() {
            host.id = uuid::Uuid::new_v4().to_string();
        }
        if host.port == 0 {
            host.port = 22;
        }
        {
            let mut hosts = self.hosts.lock().unwrap();
            match hosts.iter_mut().find(|h| h.id == host.id) {
                Some(h) => *h = host.clone(),
                None => hosts.push(host.clone()),
            }
        }
        self.save()?;
        Ok(host)
    }

    pub fn remove(&self, id: &str) -> AppResult<()> {
        self.hosts.lock().unwrap().retain(|h| h.id != id);
        let _ = delete_secret(&host_secret_key(id));
        let _ = delete_secret(&npm_secret_key(id));
        self.save()
    }

    fn save(&self) -> AppResult<()> {
        let json = serde_json::to_string_pretty(&*self.hosts.lock().unwrap())?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(tmp, &self.path)?;
        Ok(())
    }
}

// ---------------------------------------------------------------- keychain

pub fn host_secret_key(id: &str) -> String {
    format!("host/{id}")
}
pub fn npm_secret_key(id: &str) -> String {
    format!("npm/{id}")
}

pub fn set_secret(key: &str, value: &str) -> AppResult<()> {
    keyring::Entry::new(KEYRING_SERVICE, key)?.set_password(value)?;
    Ok(())
}

pub fn get_secret(key: &str) -> Option<String> {
    keyring::Entry::new(KEYRING_SERVICE, key).ok()?.get_password().ok()
}

pub fn delete_secret(key: &str) -> AppResult<()> {
    match keyring::Entry::new(KEYRING_SERVICE, key)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

// ---------------------------------------------------------------- ~/.ssh/config import

pub fn import_ssh_config(existing: &[HostConfig]) -> AppResult<Vec<HostConfig>> {
    let path = dirs::home_dir()
        .ok_or_else(|| AppError::msg("No home directory"))?
        .join(".ssh")
        .join("config");
    let file = std::fs::File::open(&path)
        .map_err(|e| AppError::msg(format!("{}: {e}", path.display())))?;
    let mut reader = std::io::BufReader::new(file);
    let cfg = ssh2_config::SshConfig::default()
        .parse(&mut reader, ssh2_config::ParseRule::ALLOW_UNKNOWN_FIELDS)
        .map_err(|e| AppError::msg(format!("~/.ssh/config: {e}")))?;

    let mut out: Vec<HostConfig> = Vec::new();
    for h in cfg.get_hosts() {
        for clause in &h.pattern {
            let alias = &clause.pattern;
            if clause.negated || alias.contains('*') || alias.contains('?') {
                continue;
            }
            if existing.iter().chain(out.iter()).any(|e| &e.name == alias) {
                continue;
            }
            let p = cfg.query(alias);
            let auth = match p.identity_file.as_ref().and_then(|v| v.first()) {
                Some(path) => AuthMethod::Key { path: path.display().to_string() },
                None => AuthMethod::Agent,
            };
            out.push(HostConfig {
                id: uuid::Uuid::new_v4().to_string(),
                name: alias.clone(),
                host: p.host_name.clone().unwrap_or_else(|| alias.clone()),
                port: p.port.unwrap_or(22),
                user: p.user.clone().unwrap_or_else(whoami),
                auth,
                // ProxyJump is resolved after all aliases exist.
                jump: p.proxy_jump.as_ref().and_then(|v| v.first().cloned()),
                npm_target: None,
            });
        }
    }
    // Map ProxyJump alias -> host id (only single-hop aliases we know about).
    let names: Vec<(String, String)> = existing
        .iter()
        .chain(out.iter())
        .map(|h| (h.name.clone(), h.id.clone()))
        .collect();
    for h in &mut out {
        if let Some(j) = h.jump.take() {
            let alias = j.split('@').next_back().unwrap_or(&j).to_string();
            h.jump = names.iter().find(|(n, _)| *n == alias).map(|(_, id)| id.clone());
        }
    }
    Ok(out)
}

fn whoami() -> String {
    std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_else(|_| "root".into())
}
