//! Shared helpers for the admin apps (services, updates, firewall, cron…): run a read-only query,
//! run an action after showing its exact command, and route privileged actions through a terminal.

use leptos::prelude::*;
use leptos::task::spawn_local;
use sshos_shared::ExecResult;

use crate::state::*;
use crate::tauri::exec;

/// Admin tools live in /usr/sbin, which is not in a non-root user's PATH over SSH on Debian.
pub const SBIN: &str = "PATH=$PATH:/usr/sbin:/sbin; ";

/// Read-only query; errors other than "needs a terminal" are toasted.
pub fn query(host: String, cmd: impl Into<String>, f: impl FnOnce(ExecResult) + 'static) {
    let cmd = cmd.into();
    // Captured now: the callback may run after the window (and its reactive owner) is gone.
    let c = ctx();
    spawn_local(async move {
        match exec(&host, &cmd, true).await {
            Ok(r) => f(r),
            Err(e) => c.handle_err(Some(&host), e),
        }
    });
}

/// Davish rule: the exact command is shown in a confirmation before it runs. On failure, the
/// output is shown; `after` runs either way (usually a reload).
pub fn confirm_run(host: String, title: impl Into<String>, cmd: impl Into<String>, after: impl Fn() + Send + Sync + Clone + 'static) {
    let c = ctx();
    let (title, cmd) = (title.into(), cmd.into());
    let display = cmd.clone();
    let t2 = title.clone();
    c.show(
        Dialog::confirm(title.clone(), tr("Cette commande va être exécutée sur le serveur :", "This command will run on the server:"), title, move || {
            let (h, cmd, t, after) = (host.clone(), cmd.clone(), t2.clone(), after.clone());
            spawn_local(async move {
                match exec(&h, &cmd, false).await {
                    Ok(r) if r.ok() => c.ok(tr("Terminé.", "Done.")),
                    Ok(r) => c.show(Dialog::info(t, format!("{}{}", r.stdout, r.stderr))),
                    Err(e) => c.handle_err(Some(&h), e),
                }
                after();
            });
        })
        .code(display),
    );
}

/// Root-only action. As root: confirmed with the command shown, then run. Otherwise the command is
/// typed (with `sudo`) in a terminal and the user presses Enter — never a silent sudo.
pub fn privileged(host: String, title: impl Into<String>, cmd: impl Into<String>, after: impl Fn() + Send + Sync + Clone + 'static) {
    let c = ctx();
    let cmd = cmd.into();
    if c.info(&host).map(|i| i.is_root).unwrap_or(false) {
        confirm_run(host, title, cmd, after);
    } else {
        c.run_in_terminal(&host, &sudo(&cmd));
    }
}

/// `sudo cmd`, or `sudo sh -c '…'` when the line chains several commands (`&&`, `;`, `|`, `>`).
pub fn sudo(cmd: &str) -> String {
    if cmd.contains("&&") || cmd.contains(';') || cmd.contains('|') || cmd.contains('>') {
        format!("sudo sh -c {}", crate::apps::files::shell_quote(cmd))
    } else {
        format!("sudo {cmd}")
    }
}

/// Long or interactive root command (upgrades, reboot…): always typed in a terminal, with `sudo`
/// unless the SSH user is root. The user reads it and presses Enter.
pub fn in_terminal(host: &str, cmd: &str) {
    let c = ctx();
    let root = c.info(host).map(|i| i.is_root).unwrap_or(false);
    c.run_in_terminal(host, &if root { cmd.to_string() } else { sudo(cmd) });
}

/// Side sheet showing a title and a block of text (status, output, journal…).
pub fn text_sheet(sig: RwSignal<Option<(String, String)>>) -> impl IntoView {
    move || {
        sig.get().map(|(title, text)| {
            view! {
                <div class="sheet-overlay" on:click=move |_| sig.set(None)>
                    <div class="sheet wide" on:click=|e| e.stop_propagation()>
                        <button class="icon-btn sheet-close" on:click=move |_| sig.set(None)><crate::icons::Icon name="x" /></button>
                        <h3>{title}</h3>
                        <pre class="logview sheet-log">{text}</pre>
                    </div>
                </div>
            }
        })
    }
}

/// Runs a read-only command and shows its output in a text sheet.
pub fn show_output(host: String, title: impl Into<String>, cmd: impl Into<String>, sig: RwSignal<Option<(String, String)>>) {
    let title = title.into();
    query(host, cmd, move |r| {
        let _ = sig.try_set(Some((title, format!("{}{}", r.stdout, r.stderr))));
    });
}

pub fn loading_view() -> impl IntoView {
    view! { <div class="empty muted"><span class="spinner"></span>{tr("Chargement…", "Loading…")}</div> }
}

pub fn empty_view(text: &'static str) -> impl IntoView {
    view! { <div class="empty muted">{text}</div> }
}

/// Splits on whitespace into at most `n` fields, the last one keeping the rest of the line.
pub fn fields(line: &str, n: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = line.trim_start();
    while out.len() + 1 < n {
        match rest.find(char::is_whitespace) {
            Some(i) => {
                out.push(rest[..i].to_string());
                rest = rest[i..].trim_start();
            }
            None => break,
        }
    }
    if !rest.is_empty() {
        out.push(rest.trim_end().to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_and_sudo() {
        assert_eq!(fields("a  b c d", 3), vec!["a", "b", "c d"]);
        assert_eq!(fields("a", 3), vec!["a"]);
        assert_eq!(sudo("ufw allow 80/tcp"), "sudo ufw allow 80/tcp");
        assert_eq!(sudo("apt-get update && apt-get upgrade"), "sudo sh -c 'apt-get update && apt-get upgrade'");
    }
}
