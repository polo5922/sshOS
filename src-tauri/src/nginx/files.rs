//! Native nginx: lightweight config parsing and generation of change plans.

use sshos_shared::{NewSiteKind, NewSiteRequest, NginxLayout, NginxPlan, NginxServerBlock};

use crate::ssh::sh_quote;

/// Remove comments while keeping line structure.
fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        let mut in_q: Option<char> = None;
        let mut cut = line.len();
        for (i, c) in line.char_indices() {
            match (in_q, c) {
                (None, '"' | '\'') => in_q = Some(c),
                (Some(q), _) if c == q => in_q = None,
                (None, '#') => {
                    cut = i;
                    break;
                }
                _ => {}
            }
        }
        out.push_str(&line[..cut]);
        out.push('\n');
    }
    out
}

#[derive(Debug)]
enum Tok {
    Word(String, usize),
    Open(usize),
    Close,
    Semi,
}

fn tokenize(src: &str) -> Vec<Tok> {
    let mut toks = Vec::new();
    let mut line = 1;
    let mut cur = String::new();
    let mut cur_line = 1;
    let mut chars = src.chars().peekable();
    let flush = |cur: &mut String, toks: &mut Vec<Tok>, l: usize| {
        if !cur.is_empty() {
            toks.push(Tok::Word(std::mem::take(cur), l));
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '"' | '\'' => {
                if cur.is_empty() {
                    cur_line = line;
                }
                for d in chars.by_ref() {
                    if d == c {
                        break;
                    }
                    if d == '\n' {
                        line += 1;
                    }
                    cur.push(d);
                }
            }
            '{' => {
                flush(&mut cur, &mut toks, cur_line);
                toks.push(Tok::Open(line));
            }
            '}' => {
                flush(&mut cur, &mut toks, cur_line);
                toks.push(Tok::Close);
            }
            ';' => {
                flush(&mut cur, &mut toks, cur_line);
                toks.push(Tok::Semi);
            }
            c if c.is_whitespace() => {
                flush(&mut cur, &mut toks, cur_line);
                if c == '\n' {
                    line += 1;
                }
            }
            c => {
                if cur.is_empty() {
                    cur_line = line;
                }
                cur.push(c);
            }
        }
    }
    flush(&mut cur, &mut toks, cur_line);
    toks
}

/// Extract `server {}` blocks and `upstream name {}` names from a config file.
pub fn parse_config(src: &str) -> (Vec<NginxServerBlock>, Vec<String>) {
    let toks = tokenize(&strip_comments(src));
    let mut servers = Vec::new();
    let mut upstreams = Vec::new();
    // Stack of block names; `None` entries are blocks we do not care about.
    let mut stack: Vec<String> = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let mut words_line = 0;
    let mut current: Option<NginxServerBlock> = None;
    let mut server_depth = 0;

    for t in toks {
        match t {
            Tok::Word(w, l) => {
                if words.is_empty() {
                    words_line = l;
                }
                words.push(w);
            }
            Tok::Open(l) => {
                let name = words.first().cloned().unwrap_or_default();
                if name == "upstream" {
                    if let Some(n) = words.get(1) {
                        upstreams.push(n.clone());
                    }
                }
                if name == "server" && current.is_none() && stack.last().map(|s| s != "upstream").unwrap_or(true) {
                    current = Some(NginxServerBlock {
                        server_names: vec![],
                        listen: vec![],
                        proxy_pass: vec![],
                        root: None,
                        ssl_certificate: None,
                        line: if words_line > 0 { words_line } else { l },
                    });
                    server_depth = stack.len() + 1;
                }
                stack.push(name);
                words.clear();
            }
            Tok::Close => {
                words.clear();
                if current.is_some() && stack.len() == server_depth {
                    servers.push(current.take().unwrap());
                }
                stack.pop();
            }
            Tok::Semi => {
                if let (Some(s), Some(d)) = (current.as_mut(), words.first()) {
                    let args = &words[1..];
                    match d.as_str() {
                        "server_name" => s.server_names.extend(args.iter().cloned()),
                        "listen" => s.listen.push(args.join(" ")),
                        "proxy_pass" | "fastcgi_pass" | "uwsgi_pass" | "grpc_pass" => {
                            if let Some(a) = args.first() {
                                if !s.proxy_pass.contains(a) {
                                    s.proxy_pass.push(a.clone());
                                }
                            }
                        }
                        "root" if stack.len() == server_depth => s.root = args.first().cloned(),
                        "ssl_certificate" => s.ssl_certificate = args.first().cloned(),
                        _ => {}
                    }
                }
                words.clear();
            }
        }
    }
    (servers, upstreams)
}

pub fn valid_site_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && name.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        && !name.starts_with('.')
}

pub fn generate_site(req: &NewSiteRequest) -> String {
    let names = if req.domains.is_empty() { "_".to_string() } else { req.domains.join(" ") };
    let mut s = format!(
        "# Generated by sshOs\nserver {{\n    listen 80;\n    listen [::]:80;\n    server_name {names};\n\n"
    );
    match req.kind {
        NewSiteKind::Proxy => {
            s += "    client_max_body_size 50m;\n\n    location / {\n";
            s += &format!("        proxy_pass {};\n", req.target.trim());
            s += "        proxy_http_version 1.1;\n";
            s += "        proxy_set_header Host $host;\n";
            s += "        proxy_set_header X-Real-IP $remote_addr;\n";
            s += "        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;\n";
            s += "        proxy_set_header X-Forwarded-Proto $scheme;\n";
            if req.websocket {
                s += "        proxy_set_header Upgrade $http_upgrade;\n";
                s += "        proxy_set_header Connection \"upgrade\";\n";
                s += "        proxy_read_timeout 3600s;\n";
            }
            s += "    }\n";
        }
        NewSiteKind::Static => {
            s += &format!("    root {};\n    index index.html index.htm;\n\n", req.target.trim());
            s += "    location / {\n        try_files $uri $uri/ =404;\n    }\n";
        }
        NewSiteKind::Php => {
            let sock = req
                .php_socket
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "unix:/run/php/php-fpm.sock".into());
            let sock = if sock.starts_with("unix:") || sock.contains(':') { sock } else { format!("unix:{sock}") };
            s += &format!("    root {};\n    index index.php index.html;\n\n", req.target.trim());
            s += "    location / {\n        try_files $uri $uri/ /index.php?$query_string;\n    }\n\n";
            s += "    location ~ \\.php$ {\n        include fastcgi_params;\n";
            s += "        fastcgi_param SCRIPT_FILENAME $document_root$fastcgi_script_name;\n";
            s += &format!("        fastcgi_pass {sock};\n    }}\n\n");
            s += "    location ~ /\\.(?!well-known) {\n        deny all;\n    }\n";
        }
    }
    s += "}\n";
    s
}

/// Where a site lives and how it is enabled.
pub struct SitePaths {
    pub available: String,
    pub enabled: String,
}

pub fn site_paths(layout: &NginxLayout, conf_dir: &str, name: &str) -> SitePaths {
    match layout {
        NginxLayout::SitesAvailable => SitePaths {
            available: format!("{conf_dir}/sites-available/{name}"),
            enabled: format!("{conf_dir}/sites-enabled/{name}"),
        },
        NginxLayout::ConfD => {
            let base = name.trim_end_matches(".disabled").trim_end_matches(".conf");
            SitePaths {
                available: format!("{conf_dir}/conf.d/{base}.conf.disabled"),
                enabled: format!("{conf_dir}/conf.d/{base}.conf"),
            }
        }
    }
}

const TEST_AND_RELOAD: &str = "nginx -t && (systemctl reload nginx 2>/dev/null || nginx -s reload)";

/// Install a staged config file as a new enabled site; roll back if `nginx -t` fails.
pub fn plan_new_site(layout: &NginxLayout, conf_dir: &str, name: &str, staged: &str, config: String) -> NginxPlan {
    let p = site_paths(layout, conf_dir, name);
    let (dst, enable) = match layout {
        NginxLayout::SitesAvailable => (
            p.available.clone(),
            format!("ln -sfn {} {}", sh_quote(&p.available), sh_quote(&p.enabled)),
        ),
        NginxLayout::ConfD => (p.enabled.clone(), "true".to_string()),
    };
    let rollback = match layout {
        NginxLayout::SitesAvailable => format!("rm -f {}", sh_quote(&p.enabled)),
        NginxLayout::ConfD => format!("mv -f {} {}", sh_quote(&p.enabled), sh_quote(&p.available)),
    };
    let script = format!(
        "install -m 644 {staged} {dst} && rm -f {staged} && {enable} && {{ {TEST_AND_RELOAD} || {{ {rollback}; echo 'nginx -t failed: site turned off again' >&2; exit 1; }}; }}",
        staged = sh_quote(staged),
        dst = sh_quote(&dst),
    );
    NginxPlan { config: Some(config), command: script }
}

pub fn plan_toggle(layout: &NginxLayout, conf_dir: &str, name: &str, enable: bool) -> NginxPlan {
    let p = site_paths(layout, conf_dir, name);
    let (on, off) = match layout {
        NginxLayout::SitesAvailable => (
            format!("ln -sfn {} {}", sh_quote(&p.available), sh_quote(&p.enabled)),
            format!("rm -f {}", sh_quote(&p.enabled)),
        ),
        NginxLayout::ConfD => (
            format!("mv -f {} {}", sh_quote(&p.available), sh_quote(&p.enabled)),
            format!("mv -f {} {}", sh_quote(&p.enabled), sh_quote(&p.available)),
        ),
    };
    let (doit, undo) = if enable { (on, off) } else { (off, on) };
    NginxPlan {
        config: None,
        command: format!("{doit} && {{ {TEST_AND_RELOAD} || {{ {undo}; exit 1; }}; }}"),
    }
}

/// Replace a site's config with a staged file, keeping a backup and restoring it if `nginx -t` fails.
pub fn plan_replace(path: &str, staged: &str, ts: &str) -> NginxPlan {
    let q = sh_quote(path);
    let b = sh_quote(&format!("{path}.bak-{ts}"));
    NginxPlan {
        config: None,
        command: format!(
            "cp -p {q} {b} && cat {s} > {q} && rm -f {s} && {{ {TEST_AND_RELOAD} || {{ cat {b} > {q}; echo 'nginx -t failed: previous config restored' >&2; exit 1; }}; }}",
            s = sh_quote(staged)
        ),
    }
}

pub fn plan_remove(layout: &NginxLayout, conf_dir: &str, name: &str, cert_name: Option<&str>) -> NginxPlan {
    let p = site_paths(layout, conf_dir, name);
    let mut cmd = format!("rm -f {} {} && {TEST_AND_RELOAD}", sh_quote(&p.enabled), sh_quote(&p.available));
    if let Some(c) = cert_name {
        cmd += &format!(" && certbot delete --non-interactive --cert-name {}", sh_quote(c));
    }
    NginxPlan { config: None, command: cmd }
}

pub fn certbot_command(domains: &[String]) -> String {
    let ds: Vec<String> = domains.iter().map(|d| format!("-d {}", sh_quote(d))).collect();
    format!("sudo certbot --nginx {}", ds.join(" "))
}

/// Simple edits applied to an existing config ("quick change").
pub fn quick_change(src: &str, add_domains: &[String], new_proxy: Option<&str>, body_size: Option<&str>) -> String {
    let mut out = String::new();
    let mut body_done = body_size.is_none();
    for line in src.lines() {
        let t = line.trim_start();
        let indent = &line[..line.len() - t.len()];
        if t.starts_with("server_name ") && !add_domains.is_empty() {
            let cur = t.trim_start_matches("server_name").trim().trim_end_matches(';');
            let mut names: Vec<&str> = cur.split_whitespace().filter(|n| *n != "_").collect();
            for d in add_domains {
                if !names.contains(&d.as_str()) {
                    names.push(d);
                }
            }
            out += &format!("{indent}server_name {};\n", names.join(" "));
            if !body_done && !src.contains("client_max_body_size ") {
                out += &format!("{indent}client_max_body_size {};\n", body_size.unwrap());
                body_done = true;
            }
            continue;
        }
        if let (true, Some(p)) = (t.starts_with("proxy_pass "), new_proxy) {
            out += &format!("{indent}proxy_pass {p};\n");
            continue;
        }
        if let (true, Some(b)) = (t.starts_with("client_max_body_size "), body_size) {
            out += &format!("{indent}client_max_body_size {b};\n");
            body_done = true;
            continue;
        }
        out += line;
        out.push('\n');
        if !body_done && t.starts_with("server_name ") && !src.contains("client_max_body_size ") {
            out += &format!("{indent}client_max_body_size {};\n", body_size.unwrap());
            body_done = true;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONF: &str = r#"
upstream app_backend {
    server 127.0.0.1:3000;
}
# server { not real }
server {
    listen 80;
    listen [::]:80;
    server_name example.com www.example.com;
    root /var/www/html;
    location / {
        proxy_pass http://app_backend;
    }
    location /api { proxy_pass "http://127.0.0.1:4000"; }
}
server {
    listen 443 ssl http2;
    server_name example.com;
    ssl_certificate /etc/letsencrypt/live/example.com/fullchain.pem; # cert
}
"#;

    #[test]
    fn parses_servers_and_upstreams() {
        let (servers, ups) = parse_config(CONF);
        assert_eq!(ups, vec!["app_backend"]);
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[0].server_names, vec!["example.com", "www.example.com"]);
        assert_eq!(servers[0].listen, vec!["80", "[::]:80"]);
        assert_eq!(servers[0].proxy_pass, vec!["http://app_backend", "http://127.0.0.1:4000"]);
        assert_eq!(servers[0].root.as_deref(), Some("/var/www/html"));
        assert_eq!(servers[0].line, 6);
        assert_eq!(
            servers[1].ssl_certificate.as_deref(),
            Some("/etc/letsencrypt/live/example.com/fullchain.pem")
        );
    }

    #[test]
    fn generated_proxy_site_parses() {
        let cfg = generate_site(&NewSiteRequest {
            name: "app".into(),
            domains: vec!["app.example.com".into()],
            kind: NewSiteKind::Proxy,
            target: "http://127.0.0.1:3000".into(),
            websocket: true,
            php_socket: None,
        });
        let (s, _) = parse_config(&cfg);
        assert_eq!(s[0].server_names, vec!["app.example.com"]);
        assert_eq!(s[0].proxy_pass, vec!["http://127.0.0.1:3000"]);
        assert!(cfg.contains("Upgrade $http_upgrade"));
    }

    #[test]
    fn quick_change_edits() {
        let cfg = "server {\n    server_name a.com;\n    location / {\n        proxy_pass http://x:1;\n    }\n}\n";
        let out = quick_change(cfg, &["b.com".into()], Some("http://y:2"), Some("100m"));
        assert!(out.contains("server_name a.com b.com;"));
        assert!(out.contains("proxy_pass http://y:2;"));
        assert!(out.contains("client_max_body_size 100m;"));
    }

    #[test]
    fn site_names() {
        assert!(valid_site_name("my-site.conf"));
        assert!(!valid_site_name("../etc"));
        assert!(!valid_site_name("a b"));
    }

    #[test]
    fn conf_d_paths() {
        let p = site_paths(&NginxLayout::ConfD, "/etc/nginx", "app.conf");
        assert_eq!(p.enabled, "/etc/nginx/conf.d/app.conf");
        assert_eq!(p.available, "/etc/nginx/conf.d/app.conf.disabled");
    }
}
