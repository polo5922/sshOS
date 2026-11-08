//! Failed systemd units, polled by the menu bar for the notification centre.

use sshos_shared::FailedService;

use crate::error::AppResult;
use crate::ssh::Session;

/// No sudo needed; prints nothing on hosts without systemd.
const CMD: &str = "systemctl list-units --type=service --state=failed --no-legend --plain --no-pager 2>/dev/null; true";

/// Parses `UNIT LOAD ACTIVE SUB DESCRIPTION…` lines.
pub fn parse_failed(out: &str) -> Vec<FailedService> {
    out.lines()
        .filter_map(|l| {
            // Some systemd versions prefix the line with a "●" marker.
            let l = l.trim().trim_start_matches('●').trim();
            let mut it = l.split_whitespace();
            let unit = it.next()?.to_string();
            let (_load, active, _sub) = (it.next()?, it.next()?, it.next()?);
            if active != "failed" {
                return None;
            }
            let description = it.collect::<Vec<_>>().join(" ");
            Some(FailedService { unit, description })
        })
        .collect()
}

pub async fn failed(s: &Session) -> AppResult<Vec<FailedService>> {
    Ok(parse_failed(&s.exec_quiet(CMD).await?.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse() {
        let out = "teamspeak.service loaded failed failed TeamSpeak 3 Server\n\
                   ● nginx.service    loaded failed failed A high performance web server\n\
                   garbage\n";
        let v = parse_failed(out);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].unit, "teamspeak.service");
        assert_eq!(v[0].description, "TeamSpeak 3 Server");
        assert_eq!(v[1].unit, "nginx.service");
        assert!(parse_failed("").is_empty());
    }
}
