//! Logs: the systemd journal (whole system, kernel, errors, one service) and the files of
//! /var/log, with a filter and a "follow" mode (polled). Read-only; when a log needs root, the
//! sudo command is typed in a terminal for the user.

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::apps::files::shell_quote;
use crate::icons::Icon;
use crate::state::*;
use crate::tauri::exec;

#[derive(Clone, PartialEq, Debug)]
enum Source {
    Journal,
    Kernel,
    Errors,
    Unit(String),
    File(String),
}

impl Source {
    fn title(&self) -> String {
        match self {
            Source::Journal => tr("Journal système", "System journal").into(),
            Source::Kernel => tr("Noyau", "Kernel").into(),
            Source::Errors => tr("Erreurs", "Errors").into(),
            Source::Unit(u) => u.clone(),
            Source::File(f) => f.clone(),
        }
    }
    fn command(&self, n: u32) -> String {
        match self {
            Source::Journal => format!("journalctl -n {n} --no-pager -o short-iso 2>&1"),
            Source::Kernel => format!("journalctl -k -n {n} --no-pager -o short-iso 2>&1"),
            Source::Errors => format!("journalctl -p err -n {n} --no-pager -o short-iso 2>&1"),
            Source::Unit(u) => format!("journalctl -u {} -n {n} --no-pager -o short-iso 2>&1", shell_quote(u)),
            Source::File(f) => format!("tail -n {n} {} 2>&1", shell_quote(f)),
        }
    }
}

/// Output that means "you need root to read this".
fn needs_root(out: &str) -> bool {
    let l = out.to_lowercase();
    l.contains("permission denied")
        || l.contains("insufficient permissions")
        || l.contains("not seeing messages from other users")
        || l.contains("no journal files were opened")
}

fn level(line: &str) -> &'static str {
    let l = line.to_lowercase();
    if l.contains("error") || l.contains("fail") || l.contains("crit") || l.contains("fatal") || l.contains("panic") {
        "lv-err"
    } else if l.contains("warn") || l.contains("denied") || l.contains("refused") {
        "lv-warn"
    } else {
        ""
    }
}

/// `systemctl list-units --type=service --no-legend --plain` → (unit, active state).
pub fn parse_units(out: &str) -> Vec<(String, String)> {
    out.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            (f.len() >= 4 && f[0].ends_with(".service")).then(|| (f[0].to_string(), f[2].to_string()))
        })
        .collect()
}

#[component]
pub fn Logs(host: String) -> impl IntoView {
    let c = ctx();
    let host = StoredValue::new(host);
    let source = RwSignal::new(Source::Journal);
    let lines = RwSignal::new(None::<String>);
    let count = RwSignal::new(300u32);
    let filter = RwSignal::new(String::new());
    let side_filter = RwSignal::new(String::new());
    let follow = RwSignal::new(false);
    let loading = RwSignal::new(false);
    let units = RwSignal::new(Vec::<(String, String)>::new());
    let files = RwSignal::new(Vec::<String>::new());
    let body = NodeRef::<leptos::html::Pre>::new();

    let load = move || {
        let src = source.get_untracked();
        let cmd = src.command(count.get_untracked());
        loading.set(true);
        spawn_local(async move {
            let h = host.get_value();
            let r = exec(&h, &cmd, true).await;
            let _ = loading.try_set(false);
            if source.try_get_untracked().as_ref() != Some(&src) {
                return;
            }
            match r {
                Ok(r) => {
                    let _ = lines.try_set(Some(format!("{}{}", r.stdout, r.stderr)));
                    // Stick to the bottom like `tail -f`.
                    if let Some(el) = body.get_untracked() {
                        set_timeout(move || el.set_scroll_top(el.scroll_height()), std::time::Duration::from_millis(0));
                    }
                }
                Err(e) => c.handle_err(Some(&h), e),
            }
        });
    };
    let select = move |s: Source| {
        source.set(s);
        lines.set(None);
        load();
    };

    // Sidebar lists: running/failed services and the readable files of /var/log.
    spawn_local(async move {
        let h = host.get_value();
        if let Ok(r) = exec(&h, "systemctl list-units --type=service --all --no-legend --plain 2>/dev/null", true).await {
            let mut u = parse_units(&r.stdout);
            u.retain(|(_, a)| a != "inactive");
            let _ = units.try_set(u);
        }
        if let Ok(r) = exec(&h, "find /var/log -maxdepth 2 -type f ! -name '*.gz' ! -name '*.[0-9]' ! -name '*.journal' 2>/dev/null | sort | head -200", true).await {
            let _ = files.try_set(r.stdout.lines().map(String::from).collect());
        }
    });
    load();

    let handle = set_interval_with_handle(
        move || {
            if follow.get_untracked() && !loading.get_untracked() {
                load();
            }
        },
        std::time::Duration::from_secs(3),
    )
    .ok();
    on_cleanup(move || {
        if let Some(h) = handle {
            h.clear();
        }
    });

    let sudo = move || {
        let cmd = source.get_untracked().command(count.get_untracked()).replace(" 2>&1", "");
        c.run_in_terminal(&host.get_value(), &format!("sudo {cmd}"));
    };

    let item = move |s: Source, icon: &'static str, label: String, badge: Option<String>| {
        let s2 = s.clone();
        let failed = badge.as_deref() == Some("failed");
        view! {
            <button class="side-item" class:active=move || source.get() == s on:click=move |_| select(s2.clone()) title=label.clone()>
                <Icon name=icon size=15 />
                <span class="grow ellipsis">{label.clone()}</span>
                {failed.then(|| view! { <span class="side-count crit">"!"</span> })}
            </button>
        }
    };
    let matches = move |s: &str| {
        let f = side_filter.get().to_lowercase();
        f.is_empty() || s.to_lowercase().contains(&f)
    };

    let text_view = move || {
        let Some(all) = lines.get() else {
            return view! { <div class="empty muted"><span class="spinner"></span>{tr("Chargement…", "Loading…")}</div> }.into_any();
        };
        let root = needs_root(&all);
        let f = filter.get().to_lowercase();
        let shown: Vec<String> = all.lines().filter(|l| f.is_empty() || l.to_lowercase().contains(&f)).map(String::from).collect();
        let n = shown.len();
        view! {
            {root.then(|| view! {
                <div class="banner warn">
                    <Icon name="lock" size=14 />
                    <span class="grow">{tr("Une partie de ce journal demande les droits root.", "Part of this log needs root.")}</span>
                    <button on:click=move |_| sudo()>{tr("Lire avec sudo dans un terminal", "Read with sudo in a terminal")}</button>
                </div>
            })}
            <pre class="logview" node_ref=body>
                {shown
                    .into_iter()
                    .map(|l| {
                        let cls = level(&l);
                        view! { <div class=format!("ll {cls}")>{l}</div> }
                    })
                    .collect_view()}
            </pre>
            <div class="logfoot muted small">{format!("{n} {}", tr("lignes", "lines"))}</div>
        }
        .into_any()
    };

    app_menus(vec![MenuSection::new(
        tr("Présentation", "View"),
        vec![
            MenuItem::new(tr("Suivre (actualisation auto)", "Follow (auto refresh)"), move || follow.update(|f| *f = !*f))
                .checked(move || follow.get()),
            None,
            MenuItem::new(tr("100 lignes", "100 lines"), move || {
                count.set(100);
                load()
            })
            .checked(move || count.get() == 100),
            MenuItem::new(tr("300 lignes", "300 lines"), move || {
                count.set(300);
                load()
            })
            .checked(move || count.get() == 300),
            MenuItem::new(tr("1000 lignes", "1000 lines"), move || {
                count.set(1000);
                load()
            })
            .checked(move || count.get() == 1000),
            None,
            MenuItem::new(tr("Actualiser", "Refresh"), load),
            MenuItem::new(tr("Lire avec sudo…", "Read with sudo…"), sudo),
        ],
    )]);

    view! {
        <div class="split-app logs-app">
            <aside class="side drag-region">
                <div class="side-top"></div>
                <label class="search-field side-search no-drag">
                    <Icon name="search" size=13 />
                    <input placeholder=tr("Filtrer", "Filter") bind:value=side_filter />
                </label>
                <div class="side-scroll">
                    <div class="side-title">{tr("Journal", "Journal")}</div>
                    {item(Source::Journal, "logs", Source::Journal.title(), None)}
                    {item(Source::Errors, "alert", Source::Errors.title(), None)}
                    {item(Source::Kernel, "cpu", Source::Kernel.title(), None)}
                    <div class="side-title">{tr("Services", "Services")}</div>
                    {move || {
                        units
                            .get()
                            .into_iter()
                            .filter(|(u, _)| matches(u))
                            .map(|(u, a)| item(Source::Unit(u.clone()), "server", u.trim_end_matches(".service").to_string(), Some(a)))
                            .collect_view()
                    }}
                    <div class="side-title">"/var/log"</div>
                    {move || {
                        files
                            .get()
                            .into_iter()
                            .filter(|f| matches(f))
                            .map(|f| item(Source::File(f.clone()), "file", f.trim_start_matches("/var/log/").to_string(), None))
                            .collect_view()
                    }}
                </div>
            </aside>
            <section class="main">
                <header class="main-head drag-region">
                    <h2 class="grow ellipsis">{move || source.get().title()}</h2>
                    <label class="search-field no-drag">
                        <Icon name="search" size=13 />
                        <input placeholder=tr("Rechercher", "Search") bind:value=filter />
                    </label>
                    <div class="seg no-drag">
                        {[100u32, 300, 1000]
                            .into_iter()
                            .map(|n| {
                                view! {
                                    <button class:on=move || count.get() == n on:click=move |_| {
                                        count.set(n);
                                        load()
                                    }>{n.to_string()}</button>
                                }
                            })
                            .collect_view()}
                    </div>
                    <button
                        class="icon-btn"
                        class:on=move || follow.get()
                        title=tr("Suivre (toutes les 3 s)", "Follow (every 3 s)")
                        on:click=move |_| follow.update(|f| *f = !*f)
                    >
                        {move || view! { <Icon name=if follow.get() { "pause" } else { "play" } size=15 /> }}
                    </button>
                    <button class="icon-btn" class:spin=move || loading.get() title=tr("Actualiser", "Refresh") on:click=move |_| load()>
                        <Icon name="refresh" size=15 />
                    </button>
                </header>
                <div class="main-body logs-body">{text_view}</div>
            </section>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_and_levels() {
        let out = "nginx.service loaded active running A high performance web server\nfoo.service loaded failed failed Foo\n";
        let u = parse_units(out);
        assert_eq!(u, vec![("nginx.service".into(), "active".into()), ("foo.service".into(), "failed".into())]);
        assert_eq!(level("nginx: [error] boom"), "lv-err");
        assert_eq!(level("all good"), "");
        assert!(needs_root("tail: cannot open '/var/log/auth.log' for reading: Permission denied"));
        assert_eq!(Source::File("/var/log/a b".into()).command(10), "tail -n 10 '/var/log/a b' 2>&1");
    }
}
