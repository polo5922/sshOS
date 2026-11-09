//! Scripts: a small library of shell snippets (stored on this computer) run on one or several
//! connected servers at once. The command and the target list are confirmed first; scripts that
//! use `sudo` are typed in one Terminal per server instead (never a silent sudo).

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde::{Deserialize, Serialize};

use crate::icons::Icon;
use crate::state::*;
use crate::tauri::exec;

const KEY: &str = "sshos.scripts";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Script {
    pub name: String,
    pub cmd: String,
}

fn defaults() -> Vec<Script> {
    [
        (tr("Espace disque", "Disk space"), "df -h -x tmpfs -x devtmpfs -x overlay"),
        (tr("Mémoire", "Memory"), "free -h"),
        (tr("Charge et durée de fonctionnement", "Load and uptime"), "uptime"),
        (tr("Conteneurs Docker", "Docker containers"), "docker ps --format 'table {{.Names}}\\t{{.Status}}\\t{{.Ports}}'"),
        (tr("Services en échec", "Failed services"), "systemctl --failed --no-pager"),
        (tr("Dernières connexions", "Last logins"), "last -n 10 -a"),
        (tr("Plus gros dossiers de /var", "Largest folders in /var"), "du -xh /var 2>/dev/null | sort -rh | head -15"),
        (tr("Mettre à jour les paquets (apt)", "Upgrade packages (apt)"), "sudo apt update && sudo apt upgrade"),
    ]
    .into_iter()
    .map(|(n, c)| Script { name: n.into(), cmd: c.into() })
    .collect()
}

fn load_library() -> Vec<Script> {
    storage_get(KEY).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_else(defaults)
}

fn save_library(v: &[Script]) {
    storage_set(KEY, &serde_json::to_string(v).unwrap_or_default());
}

/// Needs root: typed in a Terminal rather than run in the background.
pub fn uses_sudo(cmd: &str) -> bool {
    cmd.split(|c: char| c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | '`' | '$'))
        .any(|w| w == "sudo" || w == "su" || w == "doas")
}

#[derive(Clone, Debug, PartialEq)]
enum Res {
    Running,
    Done { ok: bool, code: Option<u32>, out: String },
    Failed(String),
}

#[component]
pub fn Scripts() -> impl IntoView {
    let c = ctx();
    let library = RwSignal::new(load_library());
    let sel = RwSignal::new(Some(0usize));
    let name = RwSignal::new(String::new());
    let cmd = RwSignal::new(String::new());
    let targets = RwSignal::new(Vec::<String>::new());
    let results = RwSignal::new(Vec::<(String, Res)>::new());

    let select = move |i: Option<usize>| {
        sel.set(i);
        let s = i.and_then(|i| library.with_untracked(|l| l.get(i).cloned()));
        name.set(s.as_ref().map(|s| s.name.clone()).unwrap_or_default());
        cmd.set(s.map(|s| s.cmd).unwrap_or_default());
        results.set(Vec::new());
    };
    select(Some(0));
    // Default targets: every connected server; disconnected ones drop out.
    Effect::new(move |_| {
        let ids: Vec<String> = c.sessions.get().into_iter().map(|(h, _)| h).collect();
        targets.update(|t| {
            if t.is_empty() {
                *t = ids.clone();
            } else {
                t.retain(|h| ids.contains(h));
            }
        });
    });

    let dirty = move || {
        let (n, k) = (name.get(), cmd.get());
        match sel.get().and_then(|i| library.with(|l| l.get(i).cloned())) {
            Some(s) => s.name != n || s.cmd != k,
            None => !k.trim().is_empty(),
        }
    };
    let save = move || {
        let (n, k) = (name.get_untracked().trim().to_string(), cmd.get_untracked());
        if k.trim().is_empty() {
            return;
        }
        let n = if n.is_empty() { k.lines().next().unwrap_or("script").chars().take(40).collect() } else { n };
        let s = Script { name: n.clone(), cmd: k };
        let mut idx = 0;
        library.update(|l| {
            match sel.get_untracked().filter(|i| *i < l.len()) {
                Some(i) => {
                    l[i] = s;
                    idx = i;
                }
                None => {
                    l.push(s);
                    idx = l.len() - 1;
                }
            }
            save_library(l);
        });
        sel.set(Some(idx));
        name.set(n);
        c.ok(tr("Script enregistré.", "Script saved."));
    };
    let delete = move || {
        let Some(i) = sel.get_untracked() else { return };
        library.update(|l| {
            if i < l.len() {
                l.remove(i);
            }
            save_library(l);
        });
        select(library.with_untracked(|l| (!l.is_empty()).then_some(0)));
    };

    let run = move || {
        let k = cmd.get_untracked();
        let hosts = targets.get_untracked();
        if k.trim().is_empty() || hosts.is_empty() {
            return;
        }
        let names = hosts.iter().map(|h| c.host_name(h)).collect::<Vec<_>>().join(", ");
        if uses_sudo(&k) {
            // Davish rule: one Terminal per server with the command typed; the user presses Enter.
            for h in &hosts {
                c.run_in_terminal(h, k.trim());
            }
            return;
        }
        let shown = k.trim().to_string();
        let body = format!("{} {names}", tr("Cette commande va être exécutée sur :", "This command will run on:"));
        c.show(
            Dialog::confirm(tr("Exécuter le script", "Run script"), body, tr("Exécuter", "Run"), move || {
                results.set(hosts.iter().map(|h| (h.clone(), Res::Running)).collect());
                for h in hosts.clone() {
                    let k = k.clone();
                    spawn_local(async move {
                        let r = match exec(&h, &k, false).await {
                            Ok(r) => Res::Done { ok: r.ok(), code: r.code, out: format!("{}{}", r.stdout, r.stderr) },
                            Err(e) => Res::Failed(e.message()),
                        };
                        let _ = results.try_update(|v| {
                            if let Some(x) = v.iter_mut().find(|(x, _)| *x == h) {
                                x.1 = r;
                            }
                        });
                    });
                }
            })
            .code(shown),
        );
    };

    app_menus(vec![MenuSection::new(tr("Script", "Script"), vec![
        MenuItem::new(tr("Nouveau script", "New script"), move || select(None)),
        MenuItem::new(tr("Enregistrer", "Save"), save).key("Ctrl+S"),
        MenuItem::new(tr("Exécuter…", "Run…"), run),
        None,
        MenuItem::new(tr("Supprimer", "Delete"), delete),
        MenuItem::new(tr("Restaurer les exemples", "Restore examples"), move || {
            library.update(|l| {
                for d in defaults() {
                    if !l.iter().any(|s| s.cmd == d.cmd) {
                        l.push(d);
                    }
                }
                save_library(l);
            });
        }),
    ])]);

    view! {
        <div class="split-app scripts-app">
            <aside class="side drag-region">
                <div class="side-top"></div>
                <div class="side-title cell-flex">
                    <span class="grow">{tr("Bibliothèque", "Library")}</span>
                    <button class="icon-btn no-drag" title=tr("Nouveau script", "New script") on:click=move |_| select(None)><Icon name="plus" size=14 /></button>
                </div>
                {move || {
                    library
                        .get()
                        .into_iter()
                        .enumerate()
                        .map(|(i, s)| {
                            let root = uses_sudo(&s.cmd);
                            view! {
                                <button class="side-item no-drag" class:active=move || sel.get() == Some(i) on:click=move |_| select(Some(i)) title=s.cmd.clone()>
                                    <Icon name=if root { "lock" } else { "code" } size=15 />
                                    <span class="grow ellipsis">{s.name.clone()}</span>
                                </button>
                            }
                        })
                        .collect_view()
                }}
            </aside>
            <section class="main">
                <header class="main-head drag-region">
                    <input class="grow script-name no-drag" placeholder=tr("Nom du script", "Script name") bind:value=name />
                    <button class="icon-btn" disabled=move || !dirty() title=tr("Enregistrer", "Save") on:click=move |_| save()><Icon name="save" size=15 /></button>
                    <button class="icon-btn danger" disabled=move || sel.get().is_none() title=tr("Supprimer", "Delete") on:click=move |_| delete()><Icon name="trash" size=15 /></button>
                </header>
                <div class="main-body">
                    <textarea class="mono db-query" rows="6" spellcheck="false" placeholder="df -h" bind:value=cmd on:keydown=move |e| {
                        if e.key() == "s" && (e.ctrl_key() || e.meta_key()) {
                            e.prevent_default();
                            save();
                        }
                    }></textarea>
                    <Show when=move || uses_sudo(&cmd.get())>
                        <div class="banner warn">
                            {tr(
                                "Ce script utilise sudo : il sera tapé dans un Terminal par serveur, à valider vous-même avec Entrée.",
                                "This script uses sudo: it is typed in one Terminal per server, for you to confirm with Enter.",
                            )}
                        </div>
                    </Show>
                    <div class="section-title">{tr("Serveurs cibles", "Target servers")}</div>
                    {move || {
                        let sessions = c.sessions.get();
                        if sessions.is_empty() {
                            return view! {
                                <p class="muted small">
                                    {tr("Aucun serveur connecté.", "No connected server.")}" "
                                    <button class="link" on:click=move |_| c.open(AppKind::Connect, None)>{tr("Se connecter…", "Connect…")}</button>
                                </p>
                            }
                            .into_any();
                        }
                        view! {
                            <div class="chips">
                                {sessions
                                    .into_iter()
                                    .map(|(h, info)| {
                                        let (h1, h2) = (h.clone(), h.clone());
                                        view! {
                                            <button class="chip" class:on=move || targets.with(|t| t.contains(&h1)) title=info.hostname.clone() on:click=move |_| {
                                                let h = h2.clone();
                                                targets.update(|t| if let Some(p) = t.iter().position(|x| *x == h) { t.remove(p); } else { t.push(h); });
                                            }>{c.host_name(&h)}</button>
                                        }
                                    })
                                    .collect_view()}
                            </div>
                        }
                        .into_any()
                    }}
                    <div class="form-actions">
                        <span class="muted small grow">{move || format!("{} {}", targets.with(|t| t.len()), tr("serveur(s) sélectionné(s)", "server(s) selected"))}</span>
                        <button class="primary" disabled=move || cmd.get().trim().is_empty() || targets.with(|t| t.is_empty()) on:click=move |_| run()>
                            <Icon name="play" size=14 />" "{tr("Exécuter…", "Run…")}
                        </button>
                    </div>
                    {move || {
                        results
                            .get()
                            .into_iter()
                            .map(|(h, r)| {
                                let (pill, text) = match &r {
                                    Res::Running => (view! { <span class="pill info">{tr("en cours…", "running…")}</span> }.into_any(), String::new()),
                                    Res::Done { ok: true, out, .. } => (view! { <span class="pill ok">"OK"</span> }.into_any(), out.clone()),
                                    Res::Done { code, out, .. } => (view! { <span class="pill crit">{code.map(|c| format!("{} {c}", tr("code", "exit"))).unwrap_or_else(|| tr("échec", "failed").to_string())}</span> }.into_any(), out.clone()),
                                    Res::Failed(e) => (view! { <span class="pill crit">{tr("erreur", "error")}</span> }.into_any(), e.clone()),
                                };
                                view! {
                                    <div class="card script-result">
                                        <div class="cell-flex"><Icon name="server" size=14 /><b class="grow">{c.host_name(&h)}</b>{pill}</div>
                                        {(!text.is_empty()).then(|| view! { <pre class="logview">{text}</pre> })}
                                    </div>
                                }
                            })
                            .collect_view()
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
    fn sudo_detection() {
        assert!(uses_sudo("sudo apt update"));
        assert!(uses_sudo("apt update && sudo apt upgrade"));
        assert!(uses_sudo("x=$(sudo cat /etc/shadow)"));
        assert!(!uses_sudo("echo pseudo"));
        assert!(!uses_sudo("df -h"));
        assert!(defaults().iter().filter(|s| uses_sudo(&s.cmd)).count() == 1);
    }
}
