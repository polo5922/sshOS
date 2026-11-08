//! Command log: every command sshOs runs on a server (except the periodic pollers, see
//! `Session::exec_quiet`), plus the commands it pre-types in a terminal. Kept in memory only.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use sshos_shared::{CmdLogEntry, CmdLogKind};

const CAPACITY: usize = 1000;

static LOG: Mutex<VecDeque<CmdLogEntry>> = Mutex::new(VecDeque::new());
static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub fn record(host_id: &str, host_name: &str, command: &str, kind: CmdLogKind, code: Option<u32>, ms: u64) {
    let entry = CmdLogEntry {
        id: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        at_ms: now_ms(),
        host_id: host_id.into(),
        host_name: host_name.into(),
        command: command.into(),
        kind,
        code,
        duration_ms: ms,
    };
    if let Ok(mut l) = LOG.lock() {
        l.push_back(entry);
        while l.len() > CAPACITY {
            l.pop_front();
        }
    }
}

/// Newest first.
pub fn list() -> Vec<CmdLogEntry> {
    LOG.lock().map(|l| l.iter().rev().cloned().collect()).unwrap_or_default()
}

pub fn clear() {
    if let Ok(mut l) = LOG.lock() {
        l.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_newest_first() {
        clear();
        record("h", "web", "uptime", CmdLogKind::Read, Some(0), 3);
        record("h", "web", "systemctl restart nginx", CmdLogKind::Terminal, None, 0);
        let l = list();
        assert!(l.len() >= 2);
        let i = l.iter().position(|e| e.command == "systemctl restart nginx").unwrap();
        let j = l.iter().position(|e| e.command == "uptime").unwrap();
        assert!(i < j);
    }
}
