//! Backups: a quick `tar.gz` of a folder (made in /tmp on the server, then downloaded over SFTP),
//! and restic / borg repositories driven from a Terminal (their passphrase is typed by the user).

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::json;

use crate::apps::files::shell_quote;
use crate::apps::kit;
use crate::icons::Icon;
use crate::state::*;
use crate::tauri::{invoke_unit, pick_save};

const SCRIPT: &str = r#"echo "@@restic $(restic version 2>/dev/null | head -1)"; echo "@@borg $(borg --version 2>/dev/null)"
find /tmp -maxdepth 1 -name 'sshos-backup-*.tar.gz' -printf '@@arc %T@|%s|%p\n' 2>/dev/null"#;

#[derive(Clone, Debug, PartialEq)]
pub struct Archive {
    pub path: String,
    pub size: u64,
    pub mtime: i64,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Data {
    pub restic: String,
    pub borg: String,
    pub archives: Vec<Archive>,
}

pub fn parse(out: &str) -> Data {
    let mut d = Data::default();
    for l in out.lines() {
        if let Some(v) = l.strip_prefix("@@restic") {
            d.restic = v.trim().to_string();
        } else if let Some(v) = l.strip_prefix("@@borg") {
            d.borg = v.trim().to_string();
        } else if let Some(v) = l.strip_prefix("@@arc ") {
            let f: Vec<&str> = v.splitn(3, '|').collect();
            if f.len() == 3 {
                d.archives.push(Archive {
                    mtime: f[0].split('.').next().and_then(|s| s.parse().ok()).unwrap_or(0),
                    size: f[1].parse().unwrap_or(0),
                    path: f[2].to_string(),
                });
            }
        }
    }
    d.archives.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    d
}

/// Absolute path without control characters.
pub fn valid_path(p: &str) -> bool {
    p.starts_with('/') && !p.chars().any(|c| c.is_control())
}

/// `tar` command archiving `dir` into /tmp; the timestamp is computed on the server.
pub fn tar_command(dir: &str) -> Option<String> {
    let dir = dir.trim().trim_end_matches('/');
    let dir = if dir.is_empty() { "/" } else { dir };
    if !valid_path(dir) {
        return None;
    }
    let (parent, base) = match dir.rsplit_once('/') {
        Some(("", "")) | None => ("/", "."),
        Some(("", b)) => ("/", b),
        Some((p, b)) => (p, b),
    };
    let tag: String = base.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    let tag = if tag.is_empty() || tag == "_" { "root".to_string() } else { tag };
    Some(format!(
        "f=/tmp/sshos-backup-{tag}-$(date +%Y%m%d-%H%M%S).tar.gz; umask 077; tar -czf \"$f\" -C {} {} && ls -lh \"$f\"",
        shell_quote(parent),
        shell_quote(base)
    ))
}

fn fmt_time(ts: i64) -> String {
    let d = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(ts as f64 * 1000.0));
    format!("{:04}-{:02}-{:02} {:02}:{:02}", d.get_full_year(), d.get_month() + 1, d.get_date(), d.get_hours(), d.get_minutes())
}

#[derive(Clone, Copy, PartialEq)]
enum Tool {
    Restic,
    Borg,
}

impl Tool {
    fn name(self) -> &'static str {
        match self {
            Tool::Restic => "restic",
            Tool::Borg => "borg",
        }
    }
    /// (label, command) for this tool and repository.
    fn actions(self, repo: &str, path: &str) -> Vec<(&'static str, String)> {
        let r = shell_quote(repo);
        let p = shell_quote(path);
        match self {
            Tool::Restic => vec![
                (tr("Initialiser le dépôt", "Initialize repository"), format!("restic -r {r} init")),
                (tr("Sauvegarder maintenant", "Back up now"), format!("restic -r {r} backup {p}")),
                (tr("Lister les instantanés", "List snapshots"), format!("restic -r {r} snapshots")),
                (tr("Vérifier le dépôt", "Check repository"), format!("restic -r {r} check")),
            ],
            Tool::Borg => vec![
                (tr("Initialiser le dépôt", "Initialize repository"), format!("borg init --encryption=repokey {r}")),
                (tr("Sauvegarder maintenant", "Back up now"), format!("borg create --stats {r}::{{hostname}}-{{now}} {p}")),
                (tr("Lister les archives", "List archives"), format!("borg list {r}")),
                (tr("Vérifier le dépôt", "Check repository"), format!("borg check {r}")),
            ],
        }
    }
}

#[component]
pub fn Backups(host: String) -> impl IntoView {
    let host = StoredValue::new(host);
    let data = RwSignal::new(None::<Data>);
    let loading = RwSignal::new(false);
    let home = ctx().info(&host.get_value()).map(|i| i.home).filter(|h| !h.is_empty()).unwrap_or_else(|| "/var/www".into());
    let dir = RwSignal::new(home.clone());
    let key = format!("sshos.backups.{}", host.get_value());
    let saved: (String, String) = storage_get(&key).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let repo = RwSignal::new(saved.0);
    let src = RwSignal::new(if saved.1.is_empty() { home } else { saved.1 });
    let key = StoredValue::new(key);
    let h = move || host.get_value();

    let load = move || {
        loading.set(true);
        kit::query(host.get_value(), SCRIPT, move |r| {
            let _ = loading.try_set(false);
            let _ = data.try_set(Some(parse(&r.stdout)));
        });
    };
    load();

    let make_tar = move |sudo: bool| {
        let Some(cmd) = tar_command(&dir.get_untracked()) else {
            ctx().error(tr("Chemin absolu attendu (ex. /var/www).", "Absolute path expected (e.g. /var/www)."));
            return;
        };
        if sudo {
            // Archive as root, then hand the file back to the SSH user so SFTP can read it.
            let user = ctx().info(&h()).map(|i| i.user).unwrap_or_default();
            kit::in_terminal(&h(), &format!("{cmd} && chown {} \"$f\"", shell_quote(&user)));
        } else {
            kit::confirm_run(h(), tr("Créer une archive", "Create an archive"), cmd, load);
        }
    };
    let download = move |path: String| {
        let (c, hid) = (ctx(), host.get_value());
        spawn_local(async move {
            let name = path.rsplit('/').next().unwrap_or("backup.tar.gz").to_string();
            let Some(local) = pick_save(&name).await else { return };
            c.info_toast(tr("Téléchargement…", "Downloading…"));
            match invoke_unit("download", json!({ "hostId": hid, "remote": path, "local": local })).await {
                Ok(()) => c.ok(tr("Archive téléchargée.", "Archive downloaded.")),
                Err(e) => c.handle_err(Some(&hid), e),
            }
        });
    };
    let delete = move |path: String| kit::confirm_run(h(), tr("Supprimer l'archive", "Delete archive"), format!("rm -f {}", shell_quote(&path)), load);
    let save_repo = move || storage_set(&key.get_value(), &serde_json::to_string(&(repo.get_untracked(), src.get_untracked())).unwrap_or_default());

    let tool_card = move |tool: Tool, version: String| {
        let installed = !version.is_empty();
        view! {
            <div class="card backup-card">
                <div class="cell-flex">
                    <Icon name="archive" size=16 />
                    <b class="grow">{tool.name()}</b>
                    {if installed {
                        let short = version.split_whitespace().take(2).collect::<Vec<_>>().join(" ");
                        view! { <span class="pill ok" title=version.clone()>{short}</span> }.into_any()
                    } else {
                        view! { <span class="pill off">{tr("non installé", "not installed")}</span> }.into_any()
                    }}
                </div>
                {if installed {
                    view! {
                        <div class="chips">
                            {move || {
                                let (r, p) = (repo.get(), src.get());
                                let ok = !r.trim().is_empty() && valid_path(p.trim());
                                tool.actions(r.trim(), p.trim())
                                    .into_iter()
                                    .map(|(label, cmd)| {
                                        view! {
                                            <button class="chip" disabled=!ok title=cmd.clone() on:click=move |_| {
                                                save_repo();
                                                // Not root: the repository passphrase is typed by the user in the terminal.
                                                ctx().run_in_terminal(&h(), &cmd);
                                            }>{label}</button>
                                        }
                                    })
                                    .collect_view()
                            }}
                        </div>
                    }
                    .into_any()
                } else {
                    let name = tool.name();
                    view! {
                        <button class="small" on:click=move |_| kit::in_terminal(&h(), &format!("apt-get install -y {name} || dnf install -y {name}"))>
                            {tr("Installer…", "Install…")}
                        </button>
                    }
                    .into_any()
                }}
            </div>
        }
    };

    app_menus(vec![MenuSection::new(tr("Sauvegardes", "Backups"), vec![
        MenuItem::new(tr("Actualiser", "Refresh"), load),
        MenuItem::new(tr("Créer une archive…", "Create an archive…"), move || make_tar(false)),
    ])]);

    view! {
        <div class="split-app backups-app">
            <section class="main">
                <header class="main-head drag-region">
                    <h2 class="grow ellipsis">{tr("Sauvegardes", "Backups")}</h2>
                    <button class="icon-btn" class:spin=move || loading.get() title=tr("Actualiser", "Refresh") on:click=move |_| load()>
                        <Icon name="refresh" size=15 />
                    </button>
                </header>
                <div class="main-body">
                    <div class="section-title">{tr("Archive rapide (tar.gz)", "Quick archive (tar.gz)")}</div>
                    <div class="fw-row">
                        <input class="grow mono" placeholder="/var/www" bind:value=dir />
                        <button class="primary" disabled=move || tar_command(&dir.get()).is_none() on:click=move |_| make_tar(false)>{tr("Archiver…", "Archive…")}</button>
                        <button disabled=move || tar_command(&dir.get()).is_none() on:click=move |_| make_tar(true) title=tr("Pour les dossiers lisibles seulement par root", "For folders only root can read")>
                            {tr("Avec sudo…", "With sudo…")}
                        </button>
                    </div>
                    <pre class="code-preview">{move || tar_command(&dir.get()).unwrap_or_else(|| tr("Chemin absolu attendu.", "Absolute path expected.").to_string())}</pre>
                    {move || {
                        let Some(d) = data.get() else { return kit::loading_view().into_any() };
                        if d.archives.is_empty() {
                            return kit::empty_view(tr("Aucune archive dans /tmp.", "No archives in /tmp.")).into_any();
                        }
                        view! {
                            <div class="table-card">
                                <table class="table data-table">
                                    <thead><tr><th>{tr("Archive", "Archive")}</th><th>{tr("Taille", "Size")}</th><th>{tr("Date", "Date")}</th><th></th></tr></thead>
                                    <tbody>
                                        {d.archives
                                            .into_iter()
                                            .map(|a| {
                                                let (p1, p2) = (a.path.clone(), a.path.clone());
                                                view! {
                                                    <tr>
                                                        <td class="name-cell"><div class="cell-flex"><Icon name="archive" size=14 /><span class="mono ellipsis">{a.path.clone()}</span></div></td>
                                                        <td>{human_bytes(a.size as f64)}</td>
                                                        <td class="muted">{fmt_time(a.mtime)}</td>
                                                        <td class="row-actions">
                                                            <button class="icon-btn" title=tr("Télécharger…", "Download…") on:click=move |_| download(p1.clone())><Icon name="download" size=14 /></button>
                                                            <button class="icon-btn danger" title=tr("Supprimer…", "Delete…") on:click=move |_| delete(p2.clone())><Icon name="trash" size=14 /></button>
                                                        </td>
                                                    </tr>
                                                }
                                            })
                                            .collect_view()}
                                    </tbody>
                                </table>
                            </div>
                        }
                        .into_any()
                    }}
                    <div class="section-title">{tr("Dépôts restic / borg", "restic / borg repositories")}</div>
                    <p class="muted small hint">
                        {tr(
                            "Les commandes s'ouvrent dans un Terminal : la phrase de passe du dépôt est saisie par vous, jamais par sshOs.",
                            "Commands open in a Terminal: you type the repository passphrase, never sshOs.",
                        )}
                    </p>
                    <div class="fw-row">
                        <label class="grow">
                            <span class="muted small">{tr("Dépôt", "Repository")}</span>
                            <input class="mono" placeholder="/srv/backup  ·  sftp:user@nas:/backup  ·  s3:…" bind:value=repo />
                        </label>
                        <label class="grow">
                            <span class="muted small">{tr("Dossier à sauvegarder", "Folder to back up")}</span>
                            <input class="mono" bind:value=src />
                        </label>
                    </div>
                    {move || {
                        let d = data.get().unwrap_or_default();
                        view! { <div class="cards">{tool_card(Tool::Restic, d.restic)}{tool_card(Tool::Borg, d.borg)}</div> }
                    }}
                </div>
            </section>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_commands() {
        let d = parse("@@restic restic 0.16.4 compiled with go1.21\n@@borg \n@@arc 1700000000.5|2048|/tmp/sshos-backup-www-1.tar.gz\n@@arc 1800000000.0|10|/tmp/sshos-backup-etc-2.tar.gz\n");
        assert_eq!(d.restic, "restic 0.16.4 compiled with go1.21");
        assert!(d.borg.is_empty());
        assert_eq!(d.archives[0].path, "/tmp/sshos-backup-etc-2.tar.gz");
        assert_eq!(d.archives[1].size, 2048);
        let c = tar_command("/var/www/").unwrap();
        assert!(c.starts_with("f=/tmp/sshos-backup-www-$(date"));
        assert!(c.contains("-C '/var' 'www'"));
        assert!(tar_command("/etc").unwrap().contains("-C '/' 'etc'"));
        assert!(tar_command("relative").is_none());
        assert!(Tool::Borg.actions("/r", "/d")[1].1.contains("'/r'::{hostname}-{now} '/d'"));
    }
}
