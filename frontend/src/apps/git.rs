//! Git: repositories found on the server (or added by hand), with their status, history, branches
//! and diffs. Reading is direct; every action (fetch, pull, checkout) shows its exact command first.

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::apps::files::shell_quote;
use crate::icons::Icon;
use crate::state::*;
use crate::tauri::exec;

/// Never wait for a password or a host key prompt: there is no terminal behind `exec`.
const ENV: &str = "GIT_TERMINAL_PROMPT=0 GIT_SSH_COMMAND='ssh -o BatchMode=yes'";

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Status,
    History,
    Branches,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Status {
    pub branch: String,
    pub track: String,
    /// (XY code, path)
    pub files: Vec<(String, String)>,
}

/// `git status --porcelain=v1 -b`.
pub fn parse_status(out: &str) -> Status {
    let mut s = Status::default();
    for l in out.lines() {
        if let Some(b) = l.strip_prefix("## ") {
            let (head, track) = match b.find(" [") {
                Some(i) => (&b[..i], b[i + 2..].trim_end_matches(']')),
                None => (b, ""),
            };
            s.branch = head.split("...").next().unwrap_or(head).to_string();
            s.track = track.to_string();
        } else if l.len() > 3 {
            let path = l[3..].rsplit(" -> ").next().unwrap_or(&l[3..]).trim_matches('"').to_string();
            s.files.push((l[..2].to_string(), path));
        }
    }
    s
}

#[derive(Clone, Debug, PartialEq)]
pub struct Commit {
    pub hash: String,
    pub author: String,
    pub date: String,
    pub subject: String,
}

/// `git log --pretty=format:%h%x09%an%x09%ad%x09%s --date=short`.
pub fn parse_log(out: &str) -> Vec<Commit> {
    out.lines()
        .filter_map(|l| {
            let mut f = l.splitn(4, '\t');
            Some(Commit {
                hash: f.next()?.into(),
                author: f.next()?.into(),
                date: f.next()?.into(),
                subject: f.next().unwrap_or("").into(),
            })
        })
        .collect()
}

/// `git branch -a --format=%(HEAD)%09%(refname:short)%09%(upstream:track)` → (current, name, track).
pub fn parse_branches(out: &str) -> Vec<(bool, String, String)> {
    out.lines()
        .filter_map(|l| {
            let mut f = l.splitn(3, '\t');
            let head = f.next()? == "*";
            let name = f.next()?.to_string();
            (!name.ends_with("/HEAD") && !name.is_empty()).then(|| (head, name, f.next().unwrap_or("").to_string()))
        })
        .collect()
}

fn code_label(xy: &str) -> (&'static str, &'static str) {
    let c = xy.trim();
    match c.chars().find(|c| *c != ' ').unwrap_or(' ') {
        '?' => ("git-new", "?"),
        'A' => ("git-add", "A"),
        'D' => ("git-del", "D"),
        'R' => ("git-mod", "R"),
        'U' => ("git-del", "U"),
        _ => ("git-mod", "M"),
    }
}

fn repos_key(host: &str) -> String {
    format!("sshos.git_repos.{host}")
}

#[component]
pub fn Git(host: String) -> impl IntoView {
    let c = ctx();
    let host = StoredValue::new(host);
    let repos = RwSignal::new(None::<Vec<String>>);
    let added = RwSignal::new(
        storage_get(&repos_key(&host.get_value())).map(|s| s.lines().map(String::from).collect::<Vec<_>>()).unwrap_or_default(),
    );
    let repo = RwSignal::new(None::<String>);
    let tab = RwSignal::new(Tab::Status);
    let status = RwSignal::new(None::<Result<Status, String>>);
    let log = RwSignal::new(None::<Vec<Commit>>);
    let branches = RwSignal::new(None::<Vec<(bool, String, String)>>);
    let detail = RwSignal::new(None::<(String, String)>); // (title, diff text)
    let busy = RwSignal::new(false);

    let git = move |args: String| format!("{ENV} git -C {} {args}", shell_quote(&repo.get_untracked().unwrap_or_default()));

    let scan = move || {
        repos.set(None);
        spawn_local(async move {
            let h = host.get_value();
            let cmd = "find ~ /var/www /srv /opt -maxdepth 4 -type d -name .git -prune 2>/dev/null | sed 's#/.git$##' | sort -u | head -100";
            match exec(&h, cmd, true).await {
                Ok(r) => {
                    let found: Vec<String> = r.stdout.lines().map(String::from).collect();
                    if repo.get_untracked().is_none() {
                        if let Some(first) = found.first().cloned().or_else(|| added.get_untracked().first().cloned()) {
                            repo.set(Some(first));
                        }
                    }
                    let _ = repos.try_set(Some(found));
                }
                Err(e) => c.handle_err(Some(&h), e),
            }
        });
    };

    let load = move || {
        if repo.get_untracked().is_none() {
            return;
        }
        detail.set(None);
        let t = tab.get_untracked();
        let cmd = match t {
            Tab::Status => git("status --porcelain=v1 -b 2>&1".into()),
            Tab::History => git("log -n 150 --date=short --pretty=format:%h%x09%an%x09%ad%x09%s 2>&1".into()),
            Tab::Branches => git("branch -a '--format=%(HEAD)%09%(refname:short)%09%(upstream:track)' 2>&1".into()),
        };
        spawn_local(async move {
            let h = host.get_value();
            match exec(&h, &cmd, true).await {
                Ok(r) => {
                    let failed = r.code.unwrap_or(0) != 0;
                    match t {
                        Tab::Status => {
                            let _ = status.try_set(Some(if failed { Err(r.stdout) } else { Ok(parse_status(&r.stdout)) }));
                        }
                        Tab::History => {
                            let _ = log.try_set(Some(if failed { vec![] } else { parse_log(&r.stdout) }));
                        }
                        Tab::Branches => {
                            let _ = branches.try_set(Some(if failed { vec![] } else { parse_branches(&r.stdout) }));
                        }
                    }
                }
                Err(e) => c.handle_err(Some(&h), e),
            }
        });
    };
    let show = move |title: String, args: String| {
        let cmd = git(args);
        spawn_local(async move {
            let h = host.get_value();
            if let Ok(r) = exec(&h, &cmd, true).await {
                let text = format!("{}{}", r.stdout, r.stderr);
                let _ = detail.try_set(Some((title, if text.trim().is_empty() { tr("(aucune différence)", "(no difference)").into() } else { text })));
            }
        });
    };
    // Actions: always confirmed with the exact command, then run; the output is shown.
    let action = move |title: &'static str, args: String| {
        let cmd = git(args);
        let display = cmd.clone();
        c.show(
            Dialog::confirm(title, tr("Cette commande va être exécutée sur le serveur :", "This command will run on the server:"), title, move || {
                let cmd = cmd.clone();
                busy.set(true);
                spawn_local(async move {
                    let h = host.get_value();
                    let r = exec(&h, &cmd, false).await;
                    let _ = busy.try_set(false);
                    match r {
                        Ok(r) if r.code.unwrap_or(0) == 0 => {
                            c.ok(tr("Terminé.", "Done."));
                            let out = format!("{}{}", r.stdout, r.stderr);
                            if !out.trim().is_empty() {
                                let _ = detail.try_set(Some((title.to_string(), out)));
                            }
                        }
                        Ok(r) => {
                            c.show(Dialog::info(tr("Échec de la commande git", "git command failed"), format!("{}{}", r.stdout, r.stderr)));
                        }
                        Err(e) => c.handle_err(Some(&h), e),
                    }
                    load();
                });
            })
            .code(display),
        );
    };

    let add_repo = move || {
        let input = RwSignal::new(String::new());
        c.show(
            Dialog::confirm(
                tr("Ajouter un dépôt", "Add a repository"),
                tr("Chemin du dossier contenant le dépôt git :", "Path of the folder holding the git repository:"),
                tr("Ajouter", "Add"),
                move || {
                    let p = input.get_untracked().trim().trim_end_matches('/').to_string();
                    if p.is_empty() {
                        return;
                    }
                    added.update(|a| {
                        if !a.contains(&p) {
                            a.push(p.clone())
                        }
                    });
                    storage_set(&repos_key(&host.get_value()), &added.get_untracked().join("\n"));
                    repo.set(Some(p));
                    load();
                },
            )
            .with_input(input),
        );
    };

    scan();
    Effect::new(move |_| {
        repo.track();
        tab.track();
        status.set(None);
        log.set(None);
        branches.set(None);
        load();
    });

    let repo_list = move || {
        let mut all = repos.get().unwrap_or_default();
        for a in added.get() {
            if !all.contains(&a) {
                all.push(a);
            }
        }
        if repos.with(|r| r.is_none()) && all.is_empty() {
            return view! { <div class="muted small side-note"><span class="spinner"></span>{tr("Recherche…", "Searching…")}</div> }.into_any();
        }
        if all.is_empty() {
            return view! { <div class="muted small side-note">{tr("Aucun dépôt trouvé dans ~, /var/www, /srv, /opt.", "No repository found in ~, /var/www, /srv, /opt.")}</div> }
                .into_any();
        }
        all.into_iter()
            .map(|p| {
                let (p1, p2) = (p.clone(), p.clone());
                let name = p.rsplit('/').next().unwrap_or(&p).to_string();
                view! {
                    <button class="side-item" class:active=move || repo.get().as_deref() == Some(&p1) on:click=move |_| repo.set(Some(p2.clone())) title=p.clone()>
                        <Icon name="git" size=15 />
                        <span class="grow ellipsis">{name}</span>
                    </button>
                }
            })
            .collect_view()
            .into_any()
    };

    let status_view = move || match status.get() {
        None => view! { <div class="empty muted"><span class="spinner"></span></div> }.into_any(),
        Some(Err(e)) => view! { <pre class="logview">{e}</pre> }.into_any(),
        Some(Ok(s)) => {
            let clean = s.files.is_empty();
            view! {
                <div class="git-branch">
                    <Icon name="git" size=14 />
                    <b>{s.branch.clone()}</b>
                    <span class="muted">{if s.track.is_empty() { tr("à jour", "up to date").to_string() } else { s.track.clone() }}</span>
                </div>
                {clean.then(|| view! { <div class="empty muted">{tr("Rien à valider, arbre de travail propre.", "Nothing to commit, working tree clean.")}</div> })}
                <div class="git-files">
                    {s.files
                        .into_iter()
                        .map(|(xy, p)| {
                            let (cls, lab) = code_label(&xy);
                            let staged = !xy.starts_with(' ') && !xy.starts_with('?');
                            let p2 = p.clone();
                            view! {
                                <button
                                    class="git-file"
                                    on:click=move |_| {
                                        let args = if staged {
                                            format!("diff --cached -- {} 2>&1", shell_quote(&p2))
                                        } else {
                                            format!("diff -- {} 2>&1", shell_quote(&p2))
                                        };
                                        show(p2.clone(), args)
                                    }
                                >
                                    <span class=format!("git-code {cls}")>{lab}</span>
                                    <span class="mono ellipsis grow">{p}</span>
                                    {staged.then(|| view! { <span class="muted small">{tr("indexé", "staged")}</span> })}
                                </button>
                            }
                        })
                        .collect_view()}
                </div>
            }
            .into_any()
        }
    };

    let history_view = move || match log.get() {
        None => view! { <div class="empty muted"><span class="spinner"></span></div> }.into_any(),
        Some(v) if v.is_empty() => view! { <div class="empty muted">{tr("Aucun commit.", "No commit.")}</div> }.into_any(),
        Some(v) => view! {
            <table class="table git-log">
                <tbody>
                    {v.into_iter()
                        .map(|cm| {
                            let h = cm.hash.clone();
                            let t = format!("{} {}", cm.hash, cm.subject);
                            view! {
                                <tr on:click=move |_| show(t.clone(), format!("show --stat --patch {} 2>&1 | head -600", shell_quote(&h)))>
                                    <td class="mono accent">{cm.hash}</td>
                                    <td class="ellipsis">{cm.subject}</td>
                                    <td class="muted">{cm.author}</td>
                                    <td class="muted num">{cm.date}</td>
                                </tr>
                            }
                        })
                        .collect_view()}
                </tbody>
            </table>
        }
        .into_any(),
    };

    let branches_view = move || match branches.get() {
        None => view! { <div class="empty muted"><span class="spinner"></span></div> }.into_any(),
        Some(v) => v
            .into_iter()
            .map(|(cur, name, track)| {
                let remote = name.contains('/') && !cur;
                let n2 = name.clone();
                view! {
                    <div class="git-file" class:current=cur>
                        <Icon name=if cur { "check" } else { "git" } size=14 />
                        <span class="mono grow ellipsis">{name.clone()}</span>
                        <span class="muted small">{track}</span>
                        {(!cur && !remote).then(|| view! {
                            <button class="small" on:click=move |_| action(tr("Changer de branche", "Switch branch"), format!("checkout {}", shell_quote(&n2)))>
                                {tr("Basculer", "Checkout")}
                            </button>
                        })}
                    </div>
                }
            })
            .collect_view()
            .into_any(),
    };

    app_menus(vec![
        MenuSection::new(
            tr("Dépôt", "Repository"),
            vec![
                MenuItem::new(tr("Ajouter un dépôt…", "Add a repository…"), add_repo),
                MenuItem::new(tr("Rechercher les dépôts", "Scan for repositories"), scan),
                None,
                MenuItem::new("Fetch", move || action("Fetch", "fetch --all --prune".into())).disabled(move || repo.get().is_none()),
                MenuItem::new("Pull (fast-forward)", move || action("Pull", "pull --ff-only".into())).disabled(move || repo.get().is_none()),
                None,
                MenuItem::new(tr("Terminal ici", "Terminal here"), move || {
                    if let Some(p) = repo.get_untracked() {
                        c.open(AppKind::Terminal { input: Some(format!("cd {}", shell_quote(&p))) }, Some(host.get_value()))
                    }
                })
                .disabled(move || repo.get().is_none()),
            ],
        ),
        MenuSection::new(
            tr("Présentation", "View"),
            vec![
                MenuItem::new(tr("Statut", "Status"), move || tab.set(Tab::Status)).checked(move || tab.get() == Tab::Status),
                MenuItem::new(tr("Historique", "History"), move || tab.set(Tab::History)).checked(move || tab.get() == Tab::History),
                MenuItem::new(tr("Branches", "Branches"), move || tab.set(Tab::Branches)).checked(move || tab.get() == Tab::Branches),
                None,
                MenuItem::new(tr("Actualiser", "Refresh"), load),
            ],
        ),
    ]);

    view! {
        <div class="split-app git-app">
            <aside class="side drag-region">
                <div class="side-top"></div>
                <div class="side-title row">
                    <span class="grow">{tr("Dépôts", "Repositories")}</span>
                    <button class="icon-btn no-drag" title=tr("Ajouter un dépôt", "Add a repository") on:click=move |_| add_repo()>
                        <Icon name="plus" size=13 />
                    </button>
                </div>
                <div class="side-scroll">{repo_list}</div>
            </aside>
            <section class="main">
                <header class="main-head drag-region">
                    <h2 class="grow ellipsis" title=move || repo.get().unwrap_or_default()>
                        {move || repo.get().map(|p| p.rsplit('/').next().unwrap_or("").to_string()).unwrap_or_else(|| "Git".into())}
                    </h2>
                    <div class="seg no-drag">
                        {[(Tab::Status, tr("Statut", "Status")), (Tab::History, tr("Historique", "History")), (Tab::Branches, tr("Branches", "Branches"))]
                            .into_iter()
                            .map(|(t, l)| view! { <button class:on=move || tab.get() == t on:click=move |_| tab.set(t)>{l}</button> })
                            .collect_view()}
                    </div>
                    <button class="icon-btn" disabled=move || repo.get().is_none() || busy.get() title="Fetch" on:click=move |_| action("Fetch", "fetch --all --prune".into())>
                        <Icon name="download" size=15 />
                    </button>
                    <button class="icon-btn" disabled=move || repo.get().is_none() || busy.get() title="Pull --ff-only" on:click=move |_| action("Pull", "pull --ff-only".into())>
                        <Icon name="arrows" size=15 />
                    </button>
                    <button class="icon-btn" title=tr("Actualiser", "Refresh") on:click=move |_| load()>
                        <Icon name="refresh" size=15 />
                    </button>
                </header>
                <div class="main-body git-body">
                    {move || match repo.get() {
                        None => view! { <div class="empty muted">{tr("Choisissez un dépôt.", "Pick a repository.")}</div> }.into_any(),
                        Some(_) => match tab.get() {
                            Tab::Status => status_view().into_any(),
                            Tab::History => history_view().into_any(),
                            Tab::Branches => branches_view().into_any(),
                        },
                    }}
                    {move || detail.get().map(|(t, d)| view! {
                        <div class="git-detail">
                            <div class="git-detail-head">
                                <span class="mono ellipsis grow">{t}</span>
                                <button class="icon-btn" on:click=move |_| detail.set(None)><Icon name="x" size=13 /></button>
                            </div>
                            <pre class="logview diff">
                                {d.lines()
                                    .map(|l| {
                                        let cls = if l.starts_with("+++") || l.starts_with("---") {
                                            "d-file"
                                        } else if l.starts_with('+') {
                                            "d-add"
                                        } else if l.starts_with('-') {
                                            "d-del"
                                        } else if l.starts_with("@@") {
                                            "d-hunk"
                                        } else {
                                            ""
                                        };
                                        view! { <div class=format!("ll {cls}")>{l.to_string()}</div> }
                                    })
                                    .collect_view()}
                            </pre>
                        </div>
                    })}
                </div>
            </section>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_git_outputs() {
        let s = parse_status("## main...origin/main [ahead 1, behind 2]\n M src/a.rs\n?? new.txt\nR  old -> new\n");
        assert_eq!(s.branch, "main");
        assert_eq!(s.track, "ahead 1, behind 2");
        assert_eq!(s.files.len(), 3);
        assert_eq!(s.files[2].1, "new");
        assert_eq!(code_label("??"), ("git-new", "?"));
        let l = parse_log("abc1234\tAna\t2026-01-02\tFix: a\tb\n");
        assert_eq!(l[0].subject, "Fix: a\tb");
        let b = parse_branches("*\tmain\t[ahead 1]\n \tdev\t\n \torigin/HEAD\t\n");
        assert_eq!(b.len(), 2);
        assert!(b[0].0);
    }
}
