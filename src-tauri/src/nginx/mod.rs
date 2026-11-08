//! Nginx app backend: native nginx (config files) and Nginx Proxy Manager (API).

pub mod files;
pub mod npm;

use std::collections::HashMap;

use sshos_shared::{
    ExecResult, HostConfig, NewSiteRequest, NginxDetect, NginxLayout, NginxNative, NginxPlan, NginxSite,
    NpmLocation,
};

use crate::error::{AppError, AppResult};
use crate::ssh::{sh_quote, Session};

const DETECT: &str = r#"LC_ALL=C
echo "@@uid $(id -u)"
if command -v nginx >/dev/null 2>&1 || [ -x /usr/sbin/nginx ]; then
  echo "@@version $( (nginx -v || /usr/sbin/nginx -v) 2>&1 | head -1)"
fi
for d in /etc/nginx /usr/local/nginx/conf /usr/local/etc/nginx; do [ -f "$d/nginx.conf" ] && { echo "@@dir $d"; break; }; done
[ -d /etc/nginx/sites-available ] && echo "@@sites"
echo "@@docker"
docker ps --format '{{.Names}}\t{{.Image}}\t{{.Ports}}' 2>/dev/null
echo "@@ss"
(ss -ltnH 2>/dev/null || netstat -ltn 2>/dev/null) | grep -E '[:.]81[[:space:]]' | head -1
true"#;

pub async fn detect(session: &Session, host: &HostConfig) -> AppResult<NginxDetect> {
    let out = session.exec(DETECT).await?.stdout;
    let mut is_root = false;
    let mut version = None;
    let mut dir = None;
    let mut sites = false;
    let mut docker = String::new();
    let mut port81 = false;
    let mut section = "";
    for line in out.lines() {
        if let Some(v) = line.strip_prefix("@@uid ") {
            is_root = v.trim() == "0";
        } else if let Some(v) = line.strip_prefix("@@version ") {
            version = Some(v.trim().trim_start_matches("nginx version: ").to_string());
        } else if let Some(v) = line.strip_prefix("@@dir ") {
            dir = Some(v.trim().to_string());
        } else if line == "@@sites" {
            sites = true;
        } else if line == "@@docker" || line == "@@ss" {
            section = line;
        } else if section == "@@docker" {
            docker += line;
            docker.push('\n');
        } else if section == "@@ss" && !line.trim().is_empty() {
            port81 = true;
        }
    }

    let native = match (version, dir) {
        (Some(version), Some(conf_dir)) => Some(NginxNative {
            version,
            layout: if sites && conf_dir == "/etc/nginx" { NginxLayout::SitesAvailable } else { NginxLayout::ConfD },
            conf_dir,
        }),
        _ => None,
    };

    let npm = if let Some(t) = host.npm_target.clone().filter(|t| !t.trim().is_empty()) {
        let container = npm::parse_docker_ps(&docker).map(|(n, _)| n);
        Some(NpmLocation { container, target: t })
    } else if let Some((name, target)) = npm::parse_docker_ps(&docker) {
        let target = match target {
            Some(t) => t,
            None => {
                // Not published: reach the container IP directly from the server.
                let ip = session
                    .exec_ok(&format!(
                        "docker inspect -f '{{{{range .NetworkSettings.Networks}}}}{{{{.IPAddress}}}} {{{{end}}}}' {}",
                        sh_quote(&name)
                    ))
                    .await
                    .unwrap_or_default();
                let ip = ip.split_whitespace().next().unwrap_or("127.0.0.1").to_string();
                format!("{ip}:81")
            }
        };
        Some(NpmLocation { container: Some(name), target })
    } else if port81 {
        // docker not readable by this user, but something listens on :81.
        Some(NpmLocation { container: None, target: "127.0.0.1:81".into() })
    } else {
        None
    };

    Ok(NginxDetect { native, npm, is_root })
}

pub async fn list_sites(session: &Session, native: &NginxNative) -> AppResult<Vec<NginxSite>> {
    let d = sh_quote(&native.conf_dir);
    let script = match native.layout {
        NginxLayout::SitesAvailable => format!(
            r#"cd {d}/sites-available 2>/dev/null && for f in *; do [ -f "$f" ] || continue; echo "@@file $PWD/$f"; [ -e "../sites-enabled/$f" ] && echo "@@on"; cat "$f"; echo; done
cd {d}/sites-enabled 2>/dev/null && for f in *; do [ -e "../sites-available/$f" ] && continue; [ -f "$f" ] || continue; echo "@@file $PWD/$f"; echo "@@on"; cat "$f"; echo; done; true"#
        ),
        NginxLayout::ConfD => format!(
            r#"cd {d}/conf.d 2>/dev/null && for f in *.conf *.conf.disabled; do [ -f "$f" ] || continue; echo "@@file $PWD/$f"; case "$f" in *.conf) echo "@@on";; esac; cat "$f"; echo; done; true"#
        ),
    };
    let out = session.exec(&script).await?.stdout;

    let mut sites: Vec<NginxSite> = Vec::new();
    let mut cur: Option<(String, bool, String)> = None;
    let finish = |c: Option<(String, bool, String)>, sites: &mut Vec<NginxSite>| {
        if let Some((path, enabled, body)) = c {
            let (servers, upstreams) = files::parse_config(&body);
            let name = path.rsplit('/').next().unwrap_or(&path).to_string();
            sites.push(NginxSite { name, path, enabled, servers, upstreams, cert_days_left: None });
        }
    };
    for line in out.lines() {
        if let Some(p) = line.strip_prefix("@@file ") {
            finish(cur.take(), &mut sites);
            cur = Some((p.to_string(), false, String::new()));
        } else if line == "@@on" {
            if let Some(c) = cur.as_mut() {
                c.1 = true;
            }
        } else if let Some(c) = cur.as_mut() {
            c.2 += line;
            c.2.push('\n');
        }
    }
    finish(cur, &mut sites);

    // TLS expiry for every referenced certificate.
    let certs: Vec<String> = {
        let mut v: Vec<String> = sites
            .iter()
            .flat_map(|s| s.servers.iter().filter_map(|b| b.ssl_certificate.clone()))
            .collect();
        v.sort();
        v.dedup();
        v
    };
    if !certs.is_empty() {
        let mut script = String::from("now=$(date +%s)\n");
        for c in &certs {
            script += &format!(
                "e=$(openssl x509 -enddate -noout -in {q} 2>/dev/null | cut -d= -f2) && [ -n \"$e\" ] && echo \"{c}\t$(( ($(date -d \"$e\" +%s) - now) / 86400 ))\"\n",
                q = sh_quote(c),
            );
        }
        script += "true";
        let out = session.exec(&script).await.map(|r| r.stdout).unwrap_or_default();
        let days: HashMap<&str, i64> = out
            .lines()
            .filter_map(|l| l.split_once('\t'))
            .filter_map(|(p, d)| d.trim().parse().ok().map(|d| (p, d)))
            .collect();
        for s in &mut sites {
            s.cert_days_left = s
                .servers
                .iter()
                .filter_map(|b| b.ssl_certificate.as_deref())
                .filter_map(|c| days.get(c).copied())
                .min();
        }
    }
    Ok(sites)
}

/// Run a root script: directly when logged in as root, otherwise hand the exact `sudo` command
/// to the UI so the user types their password in a terminal (never a silent sudo).
pub async fn apply(session: &Session, is_root: bool, plan: &NginxPlan) -> AppResult<ExecResult> {
    if !is_root {
        return Err(AppError::needs_terminal(format!("sudo sh -c {}", sh_quote(&plan.command))));
    }
    let r = session.exec(&format!("sh -c {}", sh_quote(&plan.command))).await?;
    if r.ok() {
        Ok(r)
    } else {
        let msg = if r.stderr.trim().is_empty() { &r.stdout } else { &r.stderr };
        Err(AppError::msg(msg.trim().to_string()))
    }
}

pub fn validate_new_site(req: &NewSiteRequest) -> AppResult<()> {
    if !files::valid_site_name(&req.name) {
        return Err(AppError::msg("Site name may only contain letters, digits, '-', '_' and '.'"));
    }
    for d in &req.domains {
        if d.is_empty() || !d.chars().all(|c| c.is_ascii_alphanumeric() || "-.*_".contains(c)) {
            return Err(AppError::msg(format!("Invalid domain `{d}`")));
        }
    }
    if req.target.trim().is_empty() || req.target.contains([';', '{', '}', '\n']) {
        return Err(AppError::msg("Invalid target"));
    }
    Ok(())
}
