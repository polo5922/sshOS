//! Databases: PostgreSQL, MySQL/MariaDB, Redis and MongoDB running in Docker containers.
//! Queries go through `docker exec -i` with credentials read from the container's own environment
//! (never shown nor stored by sshOs); every query is shown before it runs.

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::apps::files::shell_quote;
use crate::apps::kit;
use crate::icons::Icon;
use crate::state::*;
use crate::tauri::exec;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Engine {
    Postgres,
    Mysql,
    Redis,
    Mongo,
}

impl Engine {
    pub fn detect(image: &str) -> Option<Engine> {
        let i = image.to_ascii_lowercase();
        let name = i.rsplit('/').next().unwrap_or(&i);
        let name = name.split([':', '@']).next().unwrap_or(name);
        if name.contains("postgres") || name.contains("postgis") || name.contains("timescale") {
            Some(Engine::Postgres)
        } else if name.contains("mysql") || name.contains("mariadb") || name.contains("percona") {
            Some(Engine::Mysql)
        } else if name.contains("redis") || name.contains("valkey") || name.contains("keydb") {
            Some(Engine::Redis)
        } else if name.contains("mongo") {
            Some(Engine::Mongo)
        } else {
            None
        }
    }
    fn label(self) -> &'static str {
        match self {
            Engine::Postgres => "PostgreSQL",
            Engine::Mysql => "MySQL / MariaDB",
            Engine::Redis => "Redis",
            Engine::Mongo => "MongoDB",
        }
    }
    /// Client run inside the container, reading the query on stdin.
    fn client(self) -> &'static str {
        match self {
            Engine::Postgres => r#"exec psql -U "${POSTGRES_USER:-postgres}" -d "${POSTGRES_DB:-${POSTGRES_USER:-postgres}}" -P pager=off"#,
            Engine::Mysql => r#"P="${MYSQL_ROOT_PASSWORD:-$MARIADB_ROOT_PASSWORD}"; B=$(command -v mariadb || command -v mysql); exec $B -uroot ${P:+-p"$P"} -t"#,
            Engine::Redis => r#"exec redis-cli ${REDIS_PASSWORD:+-a "$REDIS_PASSWORD" --no-auth-warning}"#,
            Engine::Mongo => r#"B=$(command -v mongosh || command -v mongo); exec $B --quiet ${MONGO_INITDB_ROOT_USERNAME:+-u "$MONGO_INITDB_ROOT_USERNAME" -p "$MONGO_INITDB_ROOT_PASSWORD" --authenticationDatabase admin}"#,
        }
    }
    /// Interactive client (`docker exec -it`), typed in a Terminal.
    fn shell(self) -> &'static str {
        match self {
            Engine::Postgres => r#"exec psql -U "${POSTGRES_USER:-postgres}" -d "${POSTGRES_DB:-${POSTGRES_USER:-postgres}}""#,
            Engine::Mysql => r#"P="${MYSQL_ROOT_PASSWORD:-$MARIADB_ROOT_PASSWORD}"; B=$(command -v mariadb || command -v mysql); exec $B -uroot ${P:+-p"$P"}"#,
            Engine::Redis => r#"exec redis-cli ${REDIS_PASSWORD:+-a "$REDIS_PASSWORD" --no-auth-warning}"#,
            Engine::Mongo => r#"B=$(command -v mongosh || command -v mongo); exec $B ${MONGO_INITDB_ROOT_USERNAME:+-u "$MONGO_INITDB_ROOT_USERNAME" -p "$MONGO_INITDB_ROOT_PASSWORD" --authenticationDatabase admin}"#,
        }
    }
    /// Read-only quick queries: (label, query).
    fn quick(self) -> Vec<(&'static str, &'static str)> {
        match self {
            Engine::Postgres => vec![
                (tr("Bases et tailles", "Databases and sizes"), "SELECT datname AS database, pg_size_pretty(pg_database_size(datname)) AS size FROM pg_database WHERE NOT datistemplate ORDER BY pg_database_size(datname) DESC;"),
                (tr("Tables", "Tables"), "SELECT schemaname, relname AS table, n_live_tup AS rows, pg_size_pretty(pg_total_relation_size(relid)) AS size FROM pg_stat_user_tables ORDER BY pg_total_relation_size(relid) DESC LIMIT 50;"),
                (tr("Connexions", "Connections"), "SELECT pid, usename, datname, client_addr, state, now() - query_start AS duration FROM pg_stat_activity WHERE pid <> pg_backend_pid();"),
                (tr("Version", "Version"), "SELECT version();"),
            ],
            Engine::Mysql => vec![
                (tr("Bases et tailles", "Databases and sizes"), "SELECT table_schema AS `database`, ROUND(SUM(data_length + index_length) / 1024 / 1024, 1) AS size_mb FROM information_schema.tables GROUP BY table_schema ORDER BY size_mb DESC;"),
                (tr("Tables", "Tables"), "SELECT table_schema, table_name, table_rows, ROUND((data_length + index_length) / 1024 / 1024, 1) AS size_mb FROM information_schema.tables WHERE table_schema NOT IN ('mysql','sys','information_schema','performance_schema') ORDER BY size_mb DESC LIMIT 50;"),
                (tr("Connexions", "Connections"), "SHOW PROCESSLIST;"),
                (tr("Version", "Version"), "SELECT VERSION();"),
            ],
            Engine::Redis => vec![
                (tr("Clés par base", "Keys per database"), "INFO keyspace"),
                (tr("Mémoire", "Memory"), "INFO memory"),
                (tr("Clients", "Clients"), "CLIENT LIST"),
                (tr("Serveur", "Server"), "INFO server"),
            ],
            Engine::Mongo => vec![
                (tr("Bases et tailles", "Databases and sizes"), "db.adminCommand({ listDatabases: 1 }).databases.forEach(d => print(d.name + '  ' + (d.sizeOnDisk / 1048576).toFixed(1) + ' MB'))"),
                (tr("Collections", "Collections"), "db.getMongo().getDBNames().forEach(n => print(n + ': ' + db.getSiblingDB(n).getCollectionNames().join(', ')))"),
                (tr("Version", "Version"), "print(db.version())"),
            ],
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Container {
    pub name: String,
    pub image: String,
    pub status: String,
    pub engine: Engine,
}

/// `docker ps --format '{{.Names}}|{{.Image}}|{{.Status}}'` → database containers.
pub fn parse(out: &str) -> Vec<Container> {
    out.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.splitn(3, '|').collect();
            let engine = Engine::detect(f.get(1)?)?;
            Some(Container { name: f[0].trim().into(), image: f[1].trim().into(), status: f.get(2).unwrap_or(&"").trim().into(), engine })
        })
        .collect()
}

/// Full command: the query is passed on stdin through a quoted heredoc (no shell expansion).
pub fn query_command(c: &Container, query: &str) -> String {
    let q = query.trim_end();
    format!("docker exec -i {} sh -c {} <<'SSHOS_SQL'\n{q}\nSSHOS_SQL", shell_quote(&c.name), shell_quote(c.engine.client()))
}

pub fn shell_command(c: &Container) -> String {
    format!("docker exec -it {} sh -c {}", shell_quote(&c.name), shell_quote(c.engine.shell()))
}

#[component]
pub fn Databases(host: String) -> impl IntoView {
    let host = StoredValue::new(host);
    let containers = RwSignal::new(None::<Vec<Container>>);
    let denied = RwSignal::new(false);
    let selected = RwSignal::new(None::<String>);
    let query = RwSignal::new(String::new());
    let output = RwSignal::new(None::<(String, bool)>);
    let running = RwSignal::new(false);
    let loading = RwSignal::new(false);
    let h = move || host.get_value();

    let load = move || {
        loading.set(true);
        kit::query(h(), "docker ps --format '{{.Names}}|{{.Image}}|{{.Status}}' 2>&1", move |r| {
            let _ = loading.try_set(false);
            let out = format!("{}{}", r.stdout, r.stderr);
            let no_access = out.to_ascii_lowercase().contains("permission denied");
            let _ = denied.try_set(no_access);
            let list = if no_access { Vec::new() } else { parse(&out) };
            if selected.get_untracked().is_none_or(|s| !list.iter().any(|c| c.name == s)) {
                let _ = selected.try_set(list.first().map(|c| c.name.clone()));
            }
            let _ = containers.try_set(Some(list));
        });
    };
    load();

    let current = move || {
        let s = selected.get()?;
        containers.get()?.into_iter().find(|c| c.name == s)
    };
    let run = move |q: String| {
        let Some(c) = current() else { return };
        let cmd = query_command(&c, &q);
        running.set(true);
        let (cx, hid) = (ctx(), host.get_value());
        spawn_local(async move {
            match exec(&hid, &cmd, false).await {
                Ok(r) => {
                    let ok = r.ok();
                    let text = format!("{}{}", r.stdout, r.stderr);
                    let text = if text.trim().is_empty() { tr("(aucune sortie)", "(no output)").to_string() } else { text };
                    let _ = output.try_set(Some((text, ok)));
                }
                Err(e) => cx.handle_err(Some(&hid), e),
            }
            let _ = running.try_set(false);
        });
    };
    // Davish rule: a free-form query may write, so it is confirmed with the exact command first.
    let run_custom = move || {
        let q = query.get_untracked();
        let Some(c) = current() else { return };
        if q.trim().is_empty() {
            return;
        }
        ctx().show(
            Dialog::confirm(
                tr("Exécuter la requête", "Run query"),
                tr("Cette commande va être exécutée sur le serveur :", "This command will run on the server:"),
                tr("Exécuter", "Run"),
                move || run(q.clone()),
            )
            .code(query_command(&c, &query.get_untracked())),
        );
    };
    let open_shell = move || {
        if let Some(c) = current() {
            ctx().run_in_terminal(&h(), &shell_command(&c));
        }
    };

    app_menus(vec![MenuSection::new(tr("Bases de données", "Databases"), vec![
        MenuItem::new(tr("Actualiser", "Refresh"), load),
        MenuItem::new(tr("Ouvrir la console interactive", "Open interactive console"), open_shell),
        None,
        MenuItem::new(tr("Exécuter la requête", "Run query"), run_custom),
    ])]);

    view! {
        <div class="split-app databases-app">
            <aside class="side drag-region">
                <div class="side-top"></div>
                <div class="side-title">{tr("Conteneurs", "Containers")}</div>
                {move || match containers.get() {
                    None => kit::loading_view().into_any(),
                    Some(v) if v.is_empty() => view! { <p class="muted small hint">{tr("Aucune base détectée.", "No database found.")}</p> }.into_any(),
                    Some(v) => v
                        .into_iter()
                        .map(|c| {
                            let name = c.name.clone();
                            let n2 = name.clone();
                            let up = c.status.starts_with("Up");
                            view! {
                                <button class="side-item no-drag" class:active=move || selected.get().as_deref() == Some(n2.as_str()) on:click=move |_| {
                                    if selected.get_untracked().as_deref() != Some(name.as_str()) {
                                        selected.set(Some(name.clone()));
                                        output.set(None);
                                        query.set(String::new());
                                    }
                                } title=c.image.clone()>
                                    <Icon name="database" size=15 />
                                    <span class="grow ellipsis">{c.name.clone()}</span>
                                    <span class="side-count" class:ok=up>{c.engine.label().split(' ').next().unwrap_or("")}</span>
                                </button>
                            }
                        })
                        .collect_view()
                        .into_any(),
                }}
            </aside>
            <section class="main">
                <header class="main-head drag-region">
                    <h2 class="grow ellipsis">{move || current().map(|c| format!("{} · {}", c.name, c.engine.label())).unwrap_or_else(|| tr("Bases de données", "Databases").to_string())}</h2>
                    <button class="icon-btn" disabled=move || current().is_none() title=tr("Console interactive", "Interactive console") on:click=move |_| open_shell()>
                        <Icon name="terminal" size=15 />
                    </button>
                    <button class="icon-btn" class:spin=move || loading.get() title=tr("Actualiser", "Refresh") on:click=move |_| load()>
                        <Icon name="refresh" size=15 />
                    </button>
                </header>
                <div class="main-body">
                    <Show when=move || denied.get()>
                        <div class="banner warn">
                            <span class="grow">
                                {tr(
                                    "Votre utilisateur n'a pas accès à Docker. Ajoutez-le au groupe docker puis reconnectez-vous.",
                                    "Your user cannot access Docker. Add it to the docker group, then reconnect.",
                                )}
                            </span>
                            <button class="small" on:click=move |_| {
                                let user = ctx().info(&h()).map(|i| i.user).unwrap_or_default();
                                kit::in_terminal(&h(), &format!("usermod -aG docker {}", shell_quote(&user)));
                            }>{tr("Corriger…", "Fix…")}</button>
                        </div>
                    </Show>
                    {move || match current() {
                        None => kit::empty_view(tr(
                            "Aucun conteneur PostgreSQL, MySQL/MariaDB, Redis ou MongoDB en cours d'exécution.",
                            "No PostgreSQL, MySQL/MariaDB, Redis or MongoDB container running.",
                        ))
                        .into_any(),
                        Some(c) => {
                            let placeholder = match c.engine {
                                Engine::Postgres | Engine::Mysql => "SELECT * FROM … LIMIT 20;",
                                Engine::Redis => "KEYS user:*",
                                Engine::Mongo => "db.getSiblingDB('app').users.find().limit(5)",
                            };
                            view! {
                                <div class="chips">
                                    {c.engine
                                        .quick()
                                        .into_iter()
                                        .map(|(label, q)| {
                                            view! {
                                                <button class="chip" title=q disabled=move || running.get() on:click=move |_| {
                                                    query.set(q.to_string());
                                                    run(q.to_string());
                                                }>{label}</button>
                                            }
                                        })
                                        .collect_view()}
                                </div>
                                <textarea class="mono db-query" rows="5" placeholder=placeholder spellcheck="false" bind:value=query on:keydown=move |e| {
                                    if e.key() == "Enter" && (e.ctrl_key() || e.meta_key()) {
                                        e.prevent_default();
                                        run_custom();
                                    }
                                }></textarea>
                                <div class="form-actions">
                                    <span class="muted small grow">{tr("Ctrl+Entrée pour exécuter · identifiants lus dans l'environnement du conteneur", "Ctrl+Enter to run · credentials read from the container environment")}</span>
                                    <button class="primary" disabled=move || running.get() || query.get().trim().is_empty() on:click=move |_| run_custom()>
                                        {move || if running.get() { tr("Exécution…", "Running…") } else { tr("Exécuter…", "Run…") }}
                                    </button>
                                </div>
                                {move || output.get().map(|(text, ok)| view! { <pre class="logview" class:err=!ok>{text}</pre> })}
                            }
                            .into_any()
                        }
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
    fn detect_and_commands() {
        let v = parse("db|postgres:16-alpine|Up 2 days\nweb|nginx:latest|Up 1 hour\ncache|redis:7|Up 3 hours\nmaria|mariadb:11|Exited (0)\nm|bitnami/mongodb:7|Up\n");
        assert_eq!(v.len(), 4);
        assert_eq!(v[0].engine, Engine::Postgres);
        assert_eq!(v[1].engine, Engine::Redis);
        assert_eq!(v[2].engine, Engine::Mysql);
        assert_eq!(v[3].engine, Engine::Mongo);
        let cmd = query_command(&v[0], "SELECT 1;\n");
        assert!(cmd.starts_with("docker exec -i 'db' sh -c 'exec psql"));
        assert!(cmd.ends_with("<<'SSHOS_SQL'\nSELECT 1;\nSSHOS_SQL"));
        assert!(shell_command(&v[1]).starts_with("docker exec -it 'cache' sh -c "));
        assert_eq!(Engine::detect("ghcr.io/x/my-app:postgres"), None);
    }
}
