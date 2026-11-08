//! Docker over SSH: containers (grouped by compose project), images, volumes, live usage.
//! Everything goes through the `docker` CLI on the server; no API socket is exposed.

use std::collections::HashSet;

use serde_json::Value;
use sshos_shared::{DockerAction, DockerContainer, DockerImage, DockerOverview, DockerRequest, DockerStat, DockerVolume};

use crate::error::{AppError, AppResult};
use crate::ssh::{sh_quote, Session};

const OVERVIEW: &str = r#"command -v docker >/dev/null 2>&1 || { echo '@@missing'; exit 0; }
echo '@@version'; docker version --format '{{.Server.Version}}' 2>&1
echo '@@ps'; docker ps -a --no-trunc --format '{{json .}}' 2>/dev/null
echo '@@images'; docker images --no-trunc --format '{{json .}}' 2>/dev/null
echo '@@used'; ids=$(docker ps -aq 2>/dev/null); [ -n "$ids" ] && docker inspect --format '{{.Image}}' $ids 2>/dev/null
echo '@@volumes'; docker volume ls --format '{{json .}}' 2>/dev/null
echo '@@dangling'; docker volume ls -qf dangling=true 2>/dev/null
true"#;

fn sections(out: &str) -> std::collections::HashMap<&str, Vec<&str>> {
    let mut map = std::collections::HashMap::new();
    let mut cur = "";
    for l in out.lines() {
        if let Some(name) = l.strip_prefix("@@") {
            cur = name.trim();
            map.entry(cur).or_insert_with(Vec::new);
        } else if !l.trim().is_empty() {
            map.entry(cur).or_insert_with(Vec::new).push(l);
        }
    }
    map
}

fn s(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or_default().to_string()
}

/// `Labels` from `docker ps` is "k=v,k2=v2" (values may contain commas, keys never contain '=').
fn label<'a>(labels: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("{key}=");
    let start = if labels.starts_with(&needle) {
        0
    } else {
        labels.find(&format!(",{needle}"))? + 1
    };
    let rest = &labels[start + needle.len()..];
    // Value ends at the next ",something=" .
    let mut end = rest.len();
    let mut i = 0;
    while let Some(p) = rest[i..].find(',') {
        let cand = &rest[i + p + 1..];
        let key_end = cand.find('=').unwrap_or(usize::MAX);
        let comma = cand.find(',').unwrap_or(usize::MAX);
        if key_end < comma {
            end = i + p;
            break;
        }
        i += p + 1;
    }
    Some(&rest[..end]).filter(|v| !v.is_empty())
}

pub fn parse_overview(out: &str) -> AppResult<DockerOverview> {
    let sec = sections(out);
    if sec.contains_key("missing") {
        return Err(AppError::msg("Docker is not installed on this server"));
    }
    let version = sec.get("version").and_then(|v| v.first()).map(|s| s.trim()).unwrap_or("");
    if version.contains("permission denied") {
        return Err(AppError::needs_terminal("sudo usermod -aG docker \"$USER\"  # then reconnect"));
    }
    if version.contains("Cannot connect") {
        return Err(AppError::msg(format!("Docker daemon not reachable: {version}")));
    }
    let json = |name: &str| -> Vec<Value> {
        sec.get(name)
            .map(|v| v.iter().filter_map(|l| serde_json::from_str(l).ok()).collect())
            .unwrap_or_default()
    };

    let mut containers: Vec<DockerContainer> = json("ps")
        .iter()
        .map(|v| {
            let labels = s(v, "Labels");
            let status = s(v, "Status");
            let mut state = s(v, "State");
            if state.is_empty() {
                state = if status.starts_with("Up") { "running".into() } else { "exited".into() };
            }
            DockerContainer {
                id: s(v, "ID"),
                name: s(v, "Names").split(',').next().unwrap_or_default().to_string(),
                image: s(v, "Image"),
                state,
                status,
                ports: s(v, "Ports"),
                created: s(v, "CreatedAt"),
                project: label(&labels, "com.docker.compose.project").map(String::from),
                service: label(&labels, "com.docker.compose.service").map(String::from),
                cpu: None,
                mem: None,
            }
        })
        .collect();
    containers.sort_by(|a, b| (&a.project, &a.name).cmp(&(&b.project, &b.name)));

    let used: HashSet<&str> = sec.get("used").map(|v| v.iter().map(|l| l.trim()).collect()).unwrap_or_default();
    let names_used: HashSet<&str> = containers.iter().map(|c| c.image.as_str()).collect();
    let images = json("images")
        .iter()
        .map(|v| {
            let (id, repo, tag) = (s(v, "ID"), s(v, "Repository"), s(v, "Tag"));
            let in_use = used.contains(id.as_str())
                || names_used.contains(format!("{repo}:{tag}").as_str())
                || (tag == "latest" && names_used.contains(repo.as_str()));
            DockerImage { id, repository: repo, tag, size: s(v, "Size"), created: s(v, "CreatedSince"), in_use }
        })
        .collect();

    let dangling: HashSet<&str> = sec.get("dangling").map(|v| v.iter().map(|l| l.trim()).collect()).unwrap_or_default();
    let volumes = json("volumes")
        .iter()
        .map(|v| {
            let name = s(v, "Name");
            DockerVolume { in_use: !dangling.contains(name.as_str()), driver: s(v, "Driver"), name }
        })
        .collect();

    Ok(DockerOverview { containers, images, volumes, version: version.to_string() })
}

pub async fn overview(session: &Session) -> AppResult<DockerOverview> {
    parse_overview(&session.exec_read(OVERVIEW).await?.stdout)
}

/// "264.1MiB / 22.4GiB" -> bytes of the first part.
pub fn parse_size(s: &str) -> u64 {
    let s = s.split('/').next().unwrap_or("").trim();
    let idx = s.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(s.len());
    let (num, unit) = s.split_at(idx);
    let n: f64 = num.trim().parse().unwrap_or(0.0);
    let mul = match unit.trim() {
        "KiB" => 1024f64,
        "MiB" => 1024f64.powi(2),
        "GiB" => 1024f64.powi(3),
        "TiB" => 1024f64.powi(4),
        "kB" | "KB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        _ => 1.0,
    };
    (n * mul) as u64
}

pub fn parse_stats(out: &str) -> Vec<DockerStat> {
    out.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .map(|v| DockerStat {
            id: s(&v, "ID"),
            cpu: s(&v, "CPUPerc").trim_end_matches('%').parse().unwrap_or(0.0),
            mem: parse_size(&s(&v, "MemUsage")),
        })
        .collect()
}

/// `docker stats --no-stream` samples for ~2 s, so it is a separate call.
pub async fn stats(session: &Session) -> AppResult<Vec<DockerStat>> {
    let r = session.exec_quiet("docker stats --no-stream --no-trunc --format '{{json .}}'").await?;
    Ok(parse_stats(&r.stdout))
}

fn valid_target(t: &str) -> bool {
    !t.is_empty()
        && t.len() < 256
        && !t.starts_with('-')
        && t.chars().all(|c| c.is_ascii_alphanumeric() || "_.-:/@".contains(c))
}

/// The exact command an action runs (shown to the user before it runs).
pub fn command(req: &DockerRequest) -> AppResult<String> {
    if req.targets.is_empty() {
        return Err(AppError::msg("Nothing selected"));
    }
    if let Some(bad) = req.targets.iter().find(|t| !valid_target(t)) {
        return Err(AppError::msg(format!("Invalid docker name: {bad}")));
    }
    let verb = match req.action {
        DockerAction::Start => "start",
        DockerAction::Stop => "stop",
        DockerAction::Restart => "restart",
        DockerAction::Pause => "pause",
        DockerAction::Unpause => "unpause",
        DockerAction::Remove => "rm -f",
        DockerAction::RemoveImage => "rmi",
        DockerAction::RemoveVolume => "volume rm",
    };
    let targets: Vec<String> = req.targets.iter().map(|t| sh_quote(t)).collect();
    Ok(format!("docker {verb} {}", targets.join(" ")))
}

pub async fn run(session: &Session, req: &DockerRequest) -> AppResult<String> {
    let cmd = command(req)?;
    let r = session.exec(&cmd).await?;
    if r.ok() {
        Ok(r.stdout)
    } else if r.stderr.contains("permission denied") {
        Err(AppError::needs_terminal(format!("sudo {cmd}")))
    } else {
        Err(AppError::msg(r.stderr.trim().to_string()))
    }
}

pub async fn logs(session: &Session, id: &str, tail: u32) -> AppResult<String> {
    if !valid_target(id) {
        return Err(AppError::msg("Invalid container"));
    }
    let r = session
        .exec(&format!("docker logs --timestamps --tail {} {} 2>&1", tail.clamp(10, 5000), sh_quote(id)))
        .await?;
    Ok(r.stdout)
}

pub async fn inspect(session: &Session, id: &str) -> AppResult<String> {
    if !valid_target(id) {
        return Err(AppError::msg("Invalid target"));
    }
    let r = session.exec(&format!("docker inspect {}", sh_quote(id))).await?;
    if r.ok() {
        Ok(r.stdout)
    } else {
        Err(AppError::msg(r.stderr.trim().to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        let l = "com.docker.compose.project=web,desc=a,b,com.docker.compose.service=app";
        assert_eq!(label(l, "com.docker.compose.project"), Some("web"));
        assert_eq!(label(l, "desc"), Some("a,b"));
        assert_eq!(label(l, "com.docker.compose.service"), Some("app"));
        assert_eq!(label(l, "nope"), None);
    }

    #[test]
    fn overview_parse() {
        let out = r#"@@version
29.8.0
@@ps
{"ID":"abc","Names":"web-app-1","Image":"nginx:latest","State":"running","Status":"Up 2 days","Ports":"0.0.0.0:80->80/tcp","Labels":"com.docker.compose.project=web,com.docker.compose.service=app","CreatedAt":"2026-01-01"}
{"ID":"def","Names":"lonely","Image":"redis","State":"exited","Status":"Exited (0) 3 hours ago","Ports":"","Labels":"","CreatedAt":"2026-01-02"}
@@images
{"ID":"sha256:111","Repository":"nginx","Tag":"latest","Size":"190MB","CreatedSince":"2 weeks ago"}
{"ID":"sha256:222","Repository":"redis","Tag":"latest","Size":"120MB","CreatedSince":"1 month ago"}
{"ID":"sha256:333","Repository":"old","Tag":"1.0","Size":"50MB","CreatedSince":"1 year ago"}
@@used
sha256:111
@@volumes
{"Name":"data","Driver":"local"}
{"Name":"junk","Driver":"local"}
@@dangling
junk
"#;
        let o = parse_overview(out).unwrap();
        assert_eq!(o.version, "29.8.0");
        assert_eq!(o.containers.len(), 2);
        let web = o.containers.iter().find(|c| c.id == "abc").unwrap();
        assert_eq!(web.project.as_deref(), Some("web"));
        assert_eq!(web.service.as_deref(), Some("app"));
        assert!(o.images[0].in_use && o.images[1].in_use && !o.images[2].in_use);
        assert!(o.volumes[0].in_use && !o.volumes[1].in_use);
    }

    #[test]
    fn sizes_and_stats() {
        assert_eq!(parse_size("264.1MiB / 22.4GiB"), (264.1 * 1048576.0) as u64);
        assert_eq!(parse_size("1.5GB"), 1_500_000_000);
        let st = parse_stats(r#"{"ID":"abc","CPUPerc":"1.25%","MemUsage":"10MiB / 1GiB"}"#);
        assert_eq!(st[0].cpu, 1.25);
        assert_eq!(st[0].mem, 10 * 1048576);
    }

    #[test]
    fn commands_are_quoted_and_validated() {
        let r = DockerRequest { action: DockerAction::Restart, targets: vec!["web-app-1".into()] };
        assert_eq!(command(&r).unwrap(), "docker restart web-app-1");
        let bad = DockerRequest { action: DockerAction::Remove, targets: vec!["x; rm -rf /".into()] };
        assert!(command(&bad).is_err());
        let opt = DockerRequest { action: DockerAction::Remove, targets: vec!["--all".into()] };
        assert!(command(&opt).is_err());
    }
}
