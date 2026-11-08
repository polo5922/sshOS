//! Security overview: SSH login attempts (auth.log / secure / journal) and fail2ban.
//! Read-only. Every privileged action (ban, unban, restart, install) is a command the
//! user runs in a Terminal (never a silent sudo).

use std::collections::{HashMap, HashSet};

use sshos_shared::{
    AuthEvent, AuthKind, BanEvent, Fail2banAction, Fail2banConfig, Fail2banJail, Fail2banJailConfig, Fail2banStatus, IpStat, SecurityReport,
};

use crate::error::{AppError, AppResult};
use crate::ssh::{sh_quote, Session};

const SCRIPT: &str = r###"echo '@@uid'; id -u
echo '@@auth'
if [ -r /var/log/auth.log ]; then echo '#src /var/log/auth.log'; tail -n 20000 /var/log/auth.log | grep -a 'sshd\['
elif [ -r /var/log/secure ]; then echo '#src /var/log/secure'; tail -n 20000 /var/log/secure | grep -a 'sshd\['
elif [ -e /var/log/auth.log ] || [ -e /var/log/secure ]; then echo '#unreadable'
else out=$(journalctl -q --no-pager -o short -n 20000 _COMM=sshd 2>/dev/null); if [ -n "$out" ]; then echo '#src journal'; printf '%s\n' "$out"; else echo '#unreadable'; fi
fi
echo '@@who'; who 2>/dev/null
echo '@@f2b'
command -v fail2ban-client >/dev/null 2>&1 && echo installed
systemctl is-active fail2ban 2>/dev/null
echo '@@f2bclient'
if [ "$(id -u)" = 0 ] && command -v fail2ban-client >/dev/null 2>&1; then
  for j in $(fail2ban-client status 2>/dev/null | sed -n 's/.*Jail list:[[:space:]]*//p' | tr ',' ' '); do echo "##jail $j"; fail2ban-client status "$j" 2>/dev/null; done
fi
echo '@@f2blog'
if [ -r /var/log/fail2ban.log ]; then tail -n 20000 /var/log/fail2ban.log | grep -a -E '\] (Restore )?Ban |\] Unban |\] Found '
elif [ -e /var/log/fail2ban.log ]; then echo '#unreadable'; fi
echo '@@f2bconf'
for f in /etc/fail2ban/jail.d/*.conf /etc/fail2ban/jail.local /etc/fail2ban/jail.d/*.local; do [ -r "$f" ] && { echo "##file $f"; cat "$f"; }; done
true"###;

fn sections(out: &str) -> HashMap<&str, Vec<&str>> {
    let mut map: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut cur = "";
    for l in out.lines() {
        if let Some(name) = l.strip_prefix("@@") {
            cur = name.trim();
            map.entry(cur).or_default();
        } else if !l.trim().is_empty() {
            map.entry(cur).or_default().push(l);
        }
    }
    map
}

// ------------------------------------------------------------------ sshd log

struct Line<'a> {
    time: String,
    /// "2026-11-08 18" or "Nov 8 18": the hour bucket.
    hour: String,
    pid: &'a str,
    msg: &'a str,
}

/// Syslog ("Nov  8 18:09:49 host sshd[1]: msg") or ISO ("2026-11-08T18:09:49.1+01:00 host sshd[1]: msg").
fn split_line(l: &str) -> Option<Line<'_>> {
    let s = l.find("sshd[")?;
    let close = l[s..].find("]: ")? + s;
    let pid = &l[s + 5..close];
    let msg = &l[close + 3..];
    let head: Vec<&str> = l[..s].split_whitespace().collect();
    let first = *head.first()?;
    let (time, hour) = if first.len() >= 19 && first.as_bytes()[4] == b'-' && first.as_bytes()[10] == b'T' {
        (format!("{} {}", &first[..10], &first[11..19]), format!("{} {}h", &first[..10], &first[11..13]))
    } else if head.len() >= 3 {
        let t = head[2];
        (format!("{} {} {}", head[0], head[1], t), format!("{} {} {}h", head[0], head[1], t.get(..2).unwrap_or(t)))
    } else {
        return None;
    };
    Some(Line { time, hour, pid, msg })
}

fn token_after<'a>(s: &'a str, pat: &str) -> Option<&'a str> {
    let i = s.rfind(pat)? + pat.len();
    s[i..].split_whitespace().next()
}

/// "U IP port P [preauth]" -> (U, IP); U may be empty.
fn user_ip_port(rest: &str) -> Option<(&str, &str)> {
    let before = &rest[..rest.rfind(" port ")?];
    match before.rsplit_once(' ') {
        Some((u, ip)) => Some((u.trim(), ip)),
        None => Some(("", before)),
    }
}

fn parse_event(msg: &str) -> Option<(AuthKind, String, String, String)> {
    use AuthKind::*;
    let ev = |k, u: &str, ip: &str, m: &str| Some((k, u.to_string(), ip.to_string(), m.to_string()));
    if let Some(rest) = msg.strip_prefix("Accepted ") {
        let (method, rest) = rest.split_once(" for ")?;
        let user = &rest[..rest.rfind(" from ")?];
        return ev(Accepted, user, token_after(rest, " from ")?, method);
    }
    if let Some(rest) = msg.strip_prefix("Failed ") {
        let (method, rest) = rest.split_once(" for ")?;
        if rest.starts_with("invalid user ") {
            return None; // already counted by the "Invalid user" line
        }
        let user = &rest[..rest.rfind(" from ")?];
        return ev(Failed, user, token_after(rest, " from ")?, method);
    }
    if let Some(rest) = msg.strip_prefix("Invalid user ") {
        let user = rest.rfind(" from ").map(|i| &rest[..i]).unwrap_or("");
        return ev(InvalidUser, user.trim(), token_after(rest, " from ")?, "");
    }
    for p in ["Connection closed by authenticating user ", "Connection reset by authenticating user "] {
        if let Some(rest) = msg.strip_prefix(p) {
            let (u, ip) = user_ip_port(rest)?;
            return ev(Failed, u, ip, "publickey");
        }
    }
    if msg.starts_with("Connection closed by invalid user ") || msg.starts_with("Disconnected from ") {
        return None;
    }
    if let Some(rest) = msg.strip_prefix("Connection closed by ").or_else(|| msg.strip_prefix("Connection reset by ")) {
        let ip = rest.split_whitespace().next()?;
        if ip.contains('.') || ip.contains(':') {
            return ev(Probe, "", ip, "");
        }
        return None;
    }
    if let Some(ip) = msg
        .strip_prefix("Did not receive identification string from ")
        .or_else(|| msg.strip_prefix("Unable to negotiate with "))
        .or_else(|| msg.strip_prefix("banner exchange: Connection from "))
    {
        return ev(Probe, "", ip.split_whitespace().next()?, "");
    }
    None
}

pub fn parse_auth(lines: &[&str], report: &mut SecurityReport) {
    let mut ips: HashMap<String, IpStat> = HashMap::new();
    let mut users: HashMap<String, u32> = HashMap::new();
    let mut hourly: Vec<(String, u32)> = Vec::new();
    let mut counted_pids: HashSet<String> = HashSet::new();
    let mut events = Vec::new();

    for l in lines {
        let Some(line) = split_line(l) else { continue };
        if report.first.is_empty() {
            report.first = line.time.clone();
        }
        report.last = line.time.clone();
        let Some((kind, user, ip, method)) = parse_event(line.msg) else { continue };

        let attempt = matches!(kind, AuthKind::Failed | AuthKind::InvalidUser);
        if attempt {
            // One failed connection logs several lines; count it once.
            if !counted_pids.insert(line.pid.to_string()) && kind == AuthKind::Failed && method == "publickey" {
                continue;
            }
            let st = ips.entry(ip.clone()).or_insert_with(|| IpStat { ip: ip.clone(), ..Default::default() });
            st.attempts += 1;
            st.last = line.time.clone();
            if !user.is_empty() && st.users.len() < 5 && !st.users.contains(&user) {
                st.users.push(user.clone());
            }
            if !user.is_empty() {
                *users.entry(user.clone()).or_default() += 1;
            }
            match hourly.last_mut() {
                Some((h, n)) if *h == line.hour => *n += 1,
                _ => hourly.push((line.hour.clone(), 1)),
            }
        }
        match kind {
            AuthKind::Failed => report.failed += 1,
            AuthKind::InvalidUser => report.invalid_user += 1,
            AuthKind::Accepted => report.accepted += 1,
            AuthKind::Probe => report.probes += 1,
        }
        events.push(AuthEvent { time: line.time, kind, user, ip, method });
    }

    let mut top: Vec<IpStat> = ips.into_values().collect();
    top.sort_by(|a, b| b.attempts.cmp(&a.attempts).then(a.ip.cmp(&b.ip)));
    top.truncate(50);
    report.top_ips = top;
    let mut tu: Vec<(String, u32)> = users.into_iter().collect();
    tu.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    tu.truncate(20);
    report.top_users = tu;
    let skip = hourly.len().saturating_sub(48);
    report.hourly = hourly.into_iter().skip(skip).collect();
    events.reverse();
    events.truncate(500);
    report.events = events;
}

// ------------------------------------------------------------------ fail2ban

/// `fail2ban-client status <jail>` blocks, each preceded by "##jail name".
pub fn parse_client(lines: &[&str]) -> Vec<Fail2banJail> {
    let mut jails: Vec<Fail2banJail> = Vec::new();
    for l in lines {
        if let Some(name) = l.strip_prefix("##jail ") {
            jails.push(Fail2banJail { name: name.trim().into(), enabled: true, ..Default::default() });
            continue;
        }
        let Some(j) = jails.last_mut() else { continue };
        let Some((k, v)) = l.split_once(':') else { continue };
        let k = k.trim_start_matches(|c: char| c == '|' || c == '`' || c == '-' || c.is_whitespace()).trim();
        let v = v.trim();
        match k {
            "Total failed" => j.failures = v.parse().unwrap_or(0),
            "Total banned" => j.total_bans = v.parse().unwrap_or(0),
            "Banned IP list" => j.banned = v.split_whitespace().map(String::from).collect(),
            _ => {}
        }
    }
    jails
}

/// "2026-11-08 12:50:00,803 fail2ban.actions [1]: NOTICE  [sshd] Ban 1.2.3.4"
fn parse_f2b_line(l: &str) -> Option<BanEvent> {
    let time = l.get(..19)?.to_string();
    // Skip "fail2ban.x   [pid]: LEVEL", the jail is the next "[...]".
    let after_pid = l.find("]: ")? + 3;
    let open = l[after_pid..].find('[')? + after_pid + 1;
    let close = l[open..].find(']')? + open;
    let jail = l[open..close].to_string();
    let rest = l[close + 1..].trim();
    let (action, ip) = if let Some(ip) = rest.strip_prefix("Restore Ban ") {
        ("ban", ip)
    } else if let Some(ip) = rest.strip_prefix("Ban ") {
        ("ban", ip)
    } else if let Some(ip) = rest.strip_prefix("Unban ") {
        ("unban", ip)
    } else if let Some(ip) = rest.strip_prefix("Found ") {
        ("found", ip)
    } else {
        return None;
    };
    Some(BanEvent { time, jail, ip: ip.split_whitespace().next()?.to_string(), action: action.into() })
}

/// Minimal ini reader for jail.local / jail.d: section -> key -> value (later files win).
pub fn parse_jail_conf(lines: &[&str]) -> (Vec<String>, HashMap<String, HashMap<String, String>>) {
    let mut files = Vec::new();
    let mut map: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut section = String::new();
    for l in lines {
        if let Some(f) = l.strip_prefix("##file ") {
            files.push(f.trim().to_string());
            section.clear();
            continue;
        }
        let t = l.trim();
        if t.starts_with('#') || t.starts_with(';') || t.is_empty() {
            continue;
        }
        if t.starts_with('[') && t.ends_with(']') {
            section = t[1..t.len() - 1].trim().to_string();
            continue;
        }
        if let Some((k, v)) = t.split_once('=') {
            if !section.is_empty() {
                map.entry(section.clone()).or_default().insert(k.trim().to_lowercase(), v.trim().to_string());
            }
        }
    }
    (files, map)
}

pub fn build_fail2ban(sec: &HashMap<&str, Vec<&str>>) -> Fail2banStatus {
    let empty = Vec::new();
    let f2b = sec.get("f2b").unwrap_or(&empty);
    let mut st = Fail2banStatus {
        installed: f2b.iter().any(|l| l.trim() == "installed"),
        running: f2b.iter().any(|l| l.trim() == "active"),
        ..Default::default()
    };
    let (mut files, conf) = parse_jail_conf(sec.get("f2bconf").unwrap_or(&empty));
    files.insert(0, "/etc/fail2ban/jail.conf".into());
    st.config_files = files;
    if let Some(d) = conf.get("DEFAULT") {
        let g = |k: &str| d.get(k).cloned().unwrap_or_default();
        st.defaults = Fail2banJailConfig {
            name: "DEFAULT".into(),
            enabled: true,
            maxretry: g("maxretry"),
            findtime: g("findtime"),
            bantime: g("bantime"),
            port: String::new(),
            ignoreip: g("ignoreip"),
        };
    } else {
        st.defaults.name = "DEFAULT".into();
    }
    if !st.installed {
        return st;
    }

    let log = sec.get("f2blog").unwrap_or(&empty);
    let events: Vec<BanEvent> = log.iter().filter_map(|l| parse_f2b_line(l)).collect();

    let client = parse_client(sec.get("f2bclient").unwrap_or(&empty));
    let mut jails = if !client.is_empty() {
        st.source = "client".into();
        client
    } else {
        st.source = "log".into();
        if log.iter().any(|l| l.starts_with("#unreadable")) {
            st.hint = Some("fail2ban.log is not readable by this user (add it to the adm group).".into());
        }
        // Replay bans/unbans to estimate who is banned right now.
        let mut by_jail: Vec<Fail2banJail> = Vec::new();
        for e in &events {
            let j = match by_jail.iter().position(|j| j.name == e.jail) {
                Some(i) => &mut by_jail[i],
                None => {
                    by_jail.push(Fail2banJail { name: e.jail.clone(), enabled: true, ..Default::default() });
                    by_jail.last_mut().unwrap()
                }
            };
            match e.action.as_str() {
                "ban" => {
                    j.total_bans += 1;
                    if !j.banned.contains(&e.ip) {
                        j.banned.push(e.ip.clone());
                    }
                }
                "unban" => j.banned.retain(|ip| ip != &e.ip),
                _ => j.failures += 1,
            }
        }
        by_jail
    };

    // Enabled jails from the config that had no activity, and settings for all.
    let default = conf.get("DEFAULT");
    for (name, kv) in &conf {
        if name == "DEFAULT" || name == "INCLUDES" {
            continue;
        }
        let enabled = kv.get("enabled").map(|v| v == "true" || v == "1").unwrap_or(false);
        if enabled && !jails.iter().any(|j| &j.name == name) {
            jails.push(Fail2banJail { name: name.clone(), enabled: true, ..Default::default() });
        }
    }
    for j in &mut jails {
        let get = |k: &str, dflt: &str| {
            conf.get(&j.name)
                .and_then(|m| m.get(k))
                .or_else(|| default.and_then(|m| m.get(k)))
                .cloned()
                .unwrap_or_else(|| dflt.to_string())
        };
        j.maxretry = get("maxretry", "5");
        j.findtime = get("findtime", "10m");
        j.bantime = get("bantime", "10m");
        j.port = get("port", if j.name == "sshd" { "ssh" } else { "" });
    }
    jails.sort_by(|a, b| a.name.cmp(&b.name));
    st.jails = jails;

    let mut recent: Vec<BanEvent> = events.into_iter().filter(|e| e.action != "found").collect();
    recent.reverse();
    recent.truncate(200);
    st.recent = recent;
    st
}

pub fn parse_report(out: &str) -> SecurityReport {
    let sec = sections(out);
    let mut r = SecurityReport::default();
    let auth = sec.get("auth").cloned().unwrap_or_default();
    match auth.first() {
        Some(l) if l.starts_with("#src ") => {
            r.source = l[5..].trim().to_string();
            parse_auth(&auth[1..], &mut r);
        }
        _ => {
            r.hint = Some(
                "The SSH log (/var/log/auth.log) is not readable by this user. Members of the adm group can read it."
                    .into(),
            );
            r.fix_command = Some("sudo usermod -aG adm \"$USER\"   # then reconnect".into());
        }
    }
    r.sessions = sec.get("who").map(|v| v.iter().map(|s| s.to_string()).collect()).unwrap_or_default();
    r.fail2ban = build_fail2ban(&sec);
    let banned: HashSet<&String> = r.fail2ban.jails.iter().flat_map(|j| j.banned.iter()).collect();
    for ip in &mut r.top_ips {
        ip.banned = banned.contains(&ip.ip);
    }
    r
}

pub async fn report(session: &Session) -> AppResult<SecurityReport> {
    Ok(parse_report(&session.exec_read(SCRIPT).await?.stdout))
}

fn valid_ip(ip: &str) -> bool {
    !ip.is_empty() && ip.len() <= 45 && ip.chars().all(|c| c.is_ascii_hexdigit() || c == '.' || c == ':' || c == '/')
}

fn valid_jail(j: &str) -> bool {
    !j.is_empty() && j.len() < 64 && j.chars().all(|c| c.is_ascii_alphanumeric() || "_-.".contains(c))
}

/// The command for a fail2ban action; it is always run by the user in a Terminal.
pub fn fail2ban_command(action: Fail2banAction, jail: Option<&str>, ip: Option<&str>) -> AppResult<String> {
    let target = || -> AppResult<(String, String)> {
        let j = jail.filter(|j| valid_jail(j)).ok_or_else(|| AppError::msg("Invalid jail"))?;
        let ip = ip.filter(|i| valid_ip(i)).ok_or_else(|| AppError::msg("Invalid IP address"))?;
        Ok((sh_quote(j), sh_quote(ip)))
    };
    Ok(match action {
        Fail2banAction::Ban => {
            let (j, ip) = target()?;
            format!("sudo fail2ban-client set {j} banip {ip}")
        }
        Fail2banAction::Unban => {
            let (j, ip) = target()?;
            format!("sudo fail2ban-client set {j} unbanip {ip}")
        }
        Fail2banAction::Restart => "sudo systemctl restart fail2ban && sudo fail2ban-client status".into(),
        Fail2banAction::Install => {
            "sudo apt-get install -y fail2ban && sudo systemctl enable --now fail2ban".into()
        }
    })
}

/// File written by the visual editor; `zz-` so it is read after every other jail.d file.
pub const F2B_FILE: &str = "/etc/fail2ban/jail.d/zz-sshos.local";

fn valid_value(v: &str) -> bool {
    v.len() <= 300 && v.chars().all(|c| c.is_ascii_alphanumeric() || " .:/,_-".contains(c))
}

/// The ini lines of the managed file.
pub fn fail2ban_config_lines(cfg: &Fail2banConfig) -> AppResult<Vec<String>> {
    let check = |what: &str, v: &str| -> AppResult<()> {
        if valid_value(v) {
            Ok(())
        } else {
            Err(AppError::msg(format!("Invalid value for {what}: {v}")))
        }
    };
    let mut out = vec!["# Managed by sshOs (Security > Fail2ban > Configure).".to_string()];
    let d = &cfg.defaults;
    let mut def = Vec::new();
    for (k, v) in [("bantime", &d.bantime), ("findtime", &d.findtime), ("maxretry", &d.maxretry), ("ignoreip", &d.ignoreip)] {
        let v = v.trim();
        check(k, v)?;
        if !v.is_empty() {
            def.push(format!("{k} = {v}"));
        }
    }
    if !def.is_empty() {
        out.push(String::new());
        out.push("[DEFAULT]".into());
        out.extend(def);
    }
    for j in &cfg.jails {
        if !valid_jail(&j.name) || j.name == "DEFAULT" || j.name == "INCLUDES" {
            return Err(AppError::msg(format!("Invalid jail name: {}", j.name)));
        }
        out.push(String::new());
        out.push(format!("[{}]", j.name));
        out.push(format!("enabled = {}", j.enabled));
        for (k, v) in [("port", &j.port), ("maxretry", &j.maxretry), ("findtime", &j.findtime), ("bantime", &j.bantime)] {
            let v = v.trim();
            check(k, v)?;
            if !v.is_empty() {
                out.push(format!("{k} = {v}"));
            }
        }
    }
    Ok(out)
}

/// Writes the managed file, checks the whole config with `fail2ban-client -t`, then reloads;
/// on a failed check the previous file is put back. Run by the user in a Terminal (sudo).
pub fn fail2ban_config_command(cfg: &Fail2banConfig) -> AppResult<String> {
    let lines = fail2ban_config_lines(cfg)?;
    let args: Vec<String> = lines.iter().map(|l| sh_quote(l)).collect();
    let f = F2B_FILE;
    Ok(format!(
        "sudo rm -f {f}.bak; sudo cp -a {f} {f}.bak 2>/dev/null; \
printf '%s\\n' {} | sudo tee {f} >/dev/null && \
if sudo fail2ban-client -t; then sudo fail2ban-client reload && echo 'fail2ban: OK'; \
else echo 'fail2ban: invalid config, restoring the previous file' >&2; \
if [ -f {f}.bak ]; then sudo mv {f}.bak {f}; else sudo rm -f {f}; fi; fi",
        args.join(" ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const AUTH: &str = "@@uid
1000
@@auth
#src /var/log/auth.log
Nov  8 18:00:01 srv sshd[10]: Invalid user admin from 1.1.1.1 port 4000
Nov  8 18:00:02 srv sshd[10]: Connection closed by invalid user admin 1.1.1.1 port 4000 [preauth]
Nov  8 18:00:03 srv sshd[11]: Connection closed by authenticating user root 2.2.2.2 port 5000 [preauth]
Nov  8 18:00:04 srv sshd[12]: Failed password for root from 2.2.2.2 port 5001 ssh2
Nov  8 18:00:05 srv sshd[12]: Connection closed by authenticating user root 2.2.2.2 port 5001 [preauth]
Nov  8 19:00:06 srv sshd[13]: Invalid user  from 1.1.1.1 port 4001
Nov  8 19:00:07 srv sshd[14]: error: kex_exchange_identification: Connection closed by remote host
Nov  8 19:00:07 srv sshd[14]: Connection closed by 3.3.3.3 port 6000
Nov  8 19:00:08 srv sshd[15]: banner exchange: Connection from 4.4.4.4 port 7000: invalid format
Nov  8 19:00:09 srv sshd[16]: Accepted publickey for polo from 5.5.5.5 port 8000 ssh2: RSA SHA256:xx
@@who
polo     pts/0        2026-11-08 18:09 (5.5.5.5)
@@f2b
installed
active
@@f2bclient
@@f2blog
2026-11-08 12:00:00,000 fail2ban.filter         [166656]: INFO    [sshd] Found 2.2.2.2 - 2026-11-08 12:00:00
2026-11-08 12:50:00,803 fail2ban.actions [1]: NOTICE  [sshd] Ban 2.2.2.2
2026-11-08 12:51:00,803 fail2ban.actions [1]: NOTICE  [sshd] Ban 9.9.9.9
2026-11-08 13:00:00,803 fail2ban.actions [1]: NOTICE  [sshd] Unban 9.9.9.9
@@f2bconf
##file /etc/fail2ban/jail.d/defaults-debian.conf
[sshd]
enabled = true
backend = systemd
[recidive]
enabled = true
";

    #[test]
    fn report() {
        let r = parse_report(AUTH);
        assert_eq!(r.source, "/var/log/auth.log");
        assert_eq!(r.invalid_user, 2);
        assert_eq!(r.failed, 2, "pid 12 counted once, pid 11 once");
        assert_eq!(r.accepted, 1);
        assert_eq!(r.probes, 2);
        assert_eq!(r.top_ips[0].ip, "1.1.1.1");
        assert_eq!(r.top_ips[0].attempts, 2);
        let two = r.top_ips.iter().find(|i| i.ip == "2.2.2.2").unwrap();
        assert!(two.banned);
        assert_eq!(r.hourly, vec![("Nov 8 18h".to_string(), 3), ("Nov 8 19h".to_string(), 1)]);
        assert_eq!(r.events[0].kind, AuthKind::Accepted);
        assert_eq!(r.events[0].method, "publickey");
        assert_eq!(r.sessions.len(), 1);

        let f = &r.fail2ban;
        assert!(f.installed && f.running);
        assert_eq!(f.source, "log");
        let sshd = f.jails.iter().find(|j| j.name == "sshd").unwrap();
        assert_eq!(sshd.banned, vec!["2.2.2.2"]);
        assert_eq!(sshd.total_bans, 2);
        assert_eq!(sshd.maxretry, "5");
        assert!(f.jails.iter().any(|j| j.name == "recidive"));
        assert_eq!(f.recent.len(), 3);
        assert_eq!(f.recent[0].action, "unban");
    }

    #[test]
    fn iso_lines_and_unreadable() {
        let l = split_line("2026-11-08T18:09:49.123456+01:00 srv sshd[42]: Accepted password for a from ::1 port 2 ssh2")
            .unwrap();
        assert_eq!(l.time, "2026-11-08 18:09:49");
        assert_eq!(l.hour, "2026-11-08 18h");
        assert_eq!(l.pid, "42");
        let r = parse_report("@@auth\n#unreadable\n@@f2b\n");
        assert!(r.hint.is_some() && r.fix_command.is_some());
        assert!(!r.fail2ban.installed);
    }

    #[test]
    fn client_status() {
        let lines = [
            "##jail sshd",
            "Status for the jail: sshd",
            "|- Filter",
            "|  |- Currently failed:\t1",
            "|  |- Total failed:\t12",
            "`- Actions",
            "   |- Currently banned:\t2",
            "   |- Total banned:\t7",
            "   `- Banned IP list:\t1.2.3.4 5.6.7.8",
        ];
        let j = parse_client(&lines);
        assert_eq!(j[0].failures, 12);
        assert_eq!(j[0].total_bans, 7);
        assert_eq!(j[0].banned, vec!["1.2.3.4", "5.6.7.8"]);
    }

    #[test]
    fn config_command() {
        let cfg = Fail2banConfig {
            defaults: Fail2banJailConfig { bantime: "1h".into(), ignoreip: "127.0.0.1/8 ::1".into(), ..Default::default() },
            jails: vec![Fail2banJailConfig {
                name: "sshd".into(),
                enabled: true,
                maxretry: "3".into(),
                port: "ssh,2222".into(),
                ..Default::default()
            }],
        };
        let lines = fail2ban_config_lines(&cfg).unwrap();
        assert_eq!(
            lines[1..],
            ["", "[DEFAULT]", "bantime = 1h", "ignoreip = 127.0.0.1/8 ::1", "", "[sshd]", "enabled = true", "port = ssh,2222", "maxretry = 3"]
        );
        let cmd = fail2ban_config_command(&cfg).unwrap();
        assert!(cmd.contains("'[sshd]'") && cmd.contains("sudo fail2ban-client -t") && cmd.contains(F2B_FILE));
        let mut bad = cfg.clone();
        bad.jails[0].bantime = "1h; reboot".into();
        assert!(fail2ban_config_command(&bad).is_err());
        bad.jails[0].bantime = "1h'".into();
        assert!(fail2ban_config_command(&bad).is_err());
        bad.jails[0].bantime = String::new();
        bad.jails[0].name = "a]b".into();
        assert!(fail2ban_config_command(&bad).is_err());
    }

    #[test]
    fn commands() {
        assert_eq!(
            fail2ban_command(Fail2banAction::Unban, Some("sshd"), Some("1.2.3.4")).unwrap(),
            "sudo fail2ban-client set sshd unbanip 1.2.3.4"
        );
        assert!(fail2ban_command(Fail2banAction::Ban, Some("sshd"), Some("1.2.3.4; reboot")).is_err());
        assert!(fail2ban_command(Fail2banAction::Ban, Some("a b"), Some("1.2.3.4")).is_err());
    }
}
