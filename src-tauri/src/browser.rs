//! Built-in browser: lists the server's listening ports, requests URLs from the server itself
//! and opens them locally through an SSH tunnel (`direct-tcpip`, see `Pool::local_forward`).

use std::time::Instant;

use sshos_shared::{ListeningPort, PingResult};

use crate::error::{AppError, AppResult};
use crate::ssh::{sh_quote, Session};

/// An `http(s)://host[:port][/path]` URL, split for tunneling.
#[derive(Debug, PartialEq)]
pub struct WebUrl {
    pub https: bool,
    pub host: String,
    pub port: u16,
    /// Path, query and fragment, starting with `/`.
    pub path: String,
}

/// Accepts `localhost:3000`, `http://127.0.0.1:8080/x`, `https://[::1]/`… (scheme defaults to http).
pub fn parse_url(input: &str) -> AppResult<WebUrl> {
    let s = input.trim();
    if s.is_empty() || s.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(AppError::msg(format!("URL invalide : `{s}`")));
    }
    let (https, rest) = if let Some(r) = s.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = s.strip_prefix("http://") {
        (false, r)
    } else if s.contains("://") {
        return Err(AppError::msg("Seules les URL http(s) sont prises en charge"));
    } else {
        (false, s)
    };
    let cut = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (auth, path) = rest.split_at(cut);
    let path = if path.is_empty() {
        "/".to_string()
    } else if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    if auth.contains('@') {
        return Err(AppError::msg("Les identifiants dans l'URL ne sont pas pris en charge"));
    }
    let default = if https { 443 } else { 80 };
    let bad_port = |p: &str| AppError::msg(format!("Port invalide : `{p}`"));
    let (host, port) = if let Some(r) = auth.strip_prefix('[') {
        let (h, after) = r.split_once(']').ok_or_else(|| AppError::msg("Adresse IPv6 mal formée"))?;
        let port = match after.strip_prefix(':') {
            Some(p) => p.parse().map_err(|_| bad_port(p))?,
            None => default,
        };
        (h.to_string(), port)
    } else {
        match auth.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), p.parse().map_err(|_| bad_port(p))?),
            None => (auth.to_string(), default),
        }
    };
    if host.is_empty() || !host.chars().all(|c| c.is_ascii_alphanumeric() || ".-_:".contains(c)) {
        return Err(AppError::msg(format!("Hôte invalide : `{host}`")));
    }
    Ok(WebUrl { https, host, port, path })
}

impl WebUrl {
    pub fn full(&self) -> String {
        let host = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        format!("{}://{}:{}{}", if self.https { "https" } else { "http" }, host, self.port, self.path)
    }

    /// Target of the SSH tunnel; `localhost` means the server itself.
    pub fn remote(&self) -> String {
        let host = if self.host == "localhost" { "127.0.0.1" } else { &self.host };
        if host.contains(':') {
            format!("[{host}]:{}", self.port)
        } else {
            format!("{host}:{}", self.port)
        }
    }
}

/// Parses `ss -ltnpH` (or `netstat -ltn`) output. In both, the 4th column is the local address.
pub fn parse_listening(out: &str) -> Vec<ListeningPort> {
    let mut v: Vec<ListeningPort> = Vec::new();
    for line in out.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 4 || cols[0].eq_ignore_ascii_case("proto") || cols[0].eq_ignore_ascii_case("state") {
            continue;
        }
        if cols[0].starts_with("tcp") && !line.contains("LISTEN") {
            continue;
        }
        let Some((addr, port)) = cols[3].rsplit_once(':') else { continue };
        let Ok(port) = port.parse::<u16>() else { continue };
        let addr = addr.trim_matches(['[', ']']);
        let addr = match addr.split('%').next().unwrap_or(addr) {
            "*" | "::" | "" => "0.0.0.0".to_string(),
            a => a.to_string(),
        };
        let process = line
            .split("users:((\"")
            .nth(1)
            .and_then(|r| r.split('"').next())
            .map(str::to_string);
        match v.iter_mut().find(|p| p.port == port) {
            Some(p) => {
                if p.process.is_none() {
                    p.process = process;
                }
                // Prefer the address reachable from outside when a port is bound twice (v4 + v6).
                if addr == "0.0.0.0" {
                    p.addr = addr;
                }
            }
            None => v.push(ListeningPort { addr, port, process }),
        }
    }
    v.sort_by_key(|p| p.port);
    v
}

pub async fn ports(s: &Session) -> AppResult<Vec<ListeningPort>> {
    let r = s.exec("ss -ltnpH 2>/dev/null || ss -ltnH 2>/dev/null || netstat -ltn 2>/dev/null").await?;
    Ok(parse_listening(&r.stdout))
}

/// Requests `url` from the server (curl, else wget) and reports the status code and timing.
pub async fn ping(s: &Session, url: &str) -> AppResult<PingResult> {
    let u = parse_url(url)?;
    let q = sh_quote(&u.full());
    let cmd = format!(
        "if command -v curl >/dev/null 2>&1; then curl -sk -o /dev/null -m 5 -w '%{{http_code}} %{{time_total}}' {q}; \
         elif command -v wget >/dev/null 2>&1; then wget -q -S --spider --no-check-certificate -T 5 -t 1 {q} 2>&1 | awk '/^ *HTTP\\//{{c=$2}} END{{print (c?c:0)\" -\"}}'; \
         else echo '@@none'; fi"
    );
    let t = Instant::now();
    let r = s.exec(&cmd).await?;
    let elapsed = t.elapsed().as_secs_f64() * 1000.0;
    let out = r.stdout.trim();
    if out == "@@none" {
        return Err(AppError::msg("Ni curl ni wget n'est installé sur le serveur"));
    }
    let mut it = out.split_whitespace();
    let code = it.next().and_then(|c| c.parse::<u16>().ok()).unwrap_or(0);
    let ms = it.next().and_then(|t| t.parse::<f64>().ok()).map(|s| s * 1000.0).unwrap_or(elapsed);
    let error = (code == 0).then(|| format!("Aucune réponse de {}", u.full()));
    Ok(PingResult { code, ms, error })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        let u = parse_url("localhost:3000").unwrap();
        assert_eq!(u, WebUrl { https: false, host: "localhost".into(), port: 3000, path: "/".into() });
        assert_eq!(u.remote(), "127.0.0.1:3000");
        let u = parse_url("https://172.17.0.2/admin?x=1").unwrap();
        assert_eq!((u.https, u.port, u.path.as_str()), (true, 443, "/admin?x=1"));
        let u = parse_url("http://[::1]:8080/").unwrap();
        assert_eq!((u.host.as_str(), u.port), ("::1", 8080));
        assert_eq!(u.remote(), "[::1]:8080");
        assert_eq!(u.full(), "http://[::1]:8080/");
        assert!(parse_url("file:///etc/passwd").is_err());
        assert!(parse_url("http://a b").is_err());
        assert!(parse_url("http://x;rm -rf/").is_err());
        assert!(parse_url("http://user:pw@host/").is_err());
        assert!(parse_url("http://host:99999/").is_err());
    }

    #[test]
    fn listening() {
        let ss = "LISTEN 0      4096       127.0.0.1:5432      0.0.0.0:*    users:((\"postgres\",pid=812,fd=5))\n\
                  LISTEN 0      511          0.0.0.0:80        0.0.0.0:*\n\
                  LISTEN 0      4096            [::]:80           [::]:*\n\
                  LISTEN 0      4096   127.0.0.53%lo:53        0.0.0.0:*\n\
                  LISTEN 0      4096               *:9443            *:*\n";
        let p = parse_listening(ss);
        assert_eq!(p.iter().map(|p| p.port).collect::<Vec<_>>(), vec![53, 80, 5432, 9443]);
        assert_eq!(p[0].addr, "127.0.0.53");
        assert_eq!(p[1].addr, "0.0.0.0");
        assert_eq!(p[2].process.as_deref(), Some("postgres"));
        assert_eq!(p[3].addr, "0.0.0.0");
        let ns = "Active Internet connections (only servers)\n\
                  Proto Recv-Q Send-Q Local Address           Foreign Address         State\n\
                  tcp        0      0 0.0.0.0:22              0.0.0.0:*               LISTEN\n\
                  tcp6       0      0 :::3000                 :::*                    LISTEN\n";
        let p = parse_listening(ns);
        assert_eq!(p.iter().map(|p| p.port).collect::<Vec<_>>(), vec![22, 3000]);
        assert_eq!(p[1].addr, "0.0.0.0");
    }
}
