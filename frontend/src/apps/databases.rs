//! Databases: PostgreSQL, MySQL/MariaDB, Redis and MongoDB running in Docker containers.
//! Queries go through `docker exec -i` with credentials read from the container's own environment
//! (never shown nor stored by sshOs); every query is shown before it runs.

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::apps::files::shell_quote;
use crate::apps::dbbrowser::MysqlBrowser;
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
    /// Fallback for custom images: the container's Docker config (env variable names, exposed ports).
    fn detect_config(env: &[(String, String)], ports: &[String]) -> Option<Engine> {
        let has = |p: &str| env.iter().any(|(k, _)| k.starts_with(p));
        let port = |p: &str| ports.iter().any(|x| x.split('/').next() == Some(p));
        if has("MYSQL_") || has("MARIADB_") || port("3306") {
            Some(Engine::Mysql)
        } else if has("POSTGRES_") || port("5432") {
            Some(Engine::Postgres)
        } else if has("MONGO_INITDB_") || port("27017") {
            Some(Engine::Mongo)
        } else if port("6379") {
            Some(Engine::Redis)
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
    fn client(self) -> String {
        match self {
            Engine::Postgres => r#"exec psql -U "${POSTGRES_USER:-postgres}" -d "${POSTGRES_DB:-${POSTGRES_USER:-postgres}}" -P pager=off"#.into(),
            Engine::Mysql => format!("{MYSQL_LOGIN} -t"),
            Engine::Redis => r#"exec redis-cli ${REDIS_PASSWORD:+-a "$REDIS_PASSWORD" --no-auth-warning}"#.into(),
            Engine::Mongo => r#"B=$(command -v mongosh || command -v mongo); exec $B --quiet ${MONGO_INITDB_ROOT_USERNAME:+-u "$MONGO_INITDB_ROOT_USERNAME" -p "$MONGO_INITDB_ROOT_PASSWORD" --authenticationDatabase admin}"#.into(),
        }
    }
    /// Interactive client (`docker exec -it`), typed in a Terminal.
    fn shell(self) -> String {
        match self {
            Engine::Postgres => r#"exec psql -U "${POSTGRES_USER:-postgres}" -d "${POSTGRES_DB:-${POSTGRES_USER:-postgres}}""#.into(),
            Engine::Mysql => MYSQL_LOGIN.into(),
            Engine::Redis => r#"exec redis-cli ${REDIS_PASSWORD:+-a "$REDIS_PASSWORD" --no-auth-warning}"#.into(),
            Engine::Mongo => r#"B=$(command -v mongosh || command -v mongo); exec $B ${MONGO_INITDB_ROOT_USERNAME:+-u "$MONGO_INITDB_ROOT_USERNAME" -p "$MONGO_INITDB_ROOT_PASSWORD" --authenticationDatabase admin}"#.into(),
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

/// MySQL / MariaDB login, run inside the container with the environment Docker gave it
/// (`environment:` / `env_file:` of the compose file, including `*_PASSWORD_FILE` secrets).
/// Root is used when its password is known (or allowed empty), otherwise the application user
/// (`MYSQL_USER`, e.g. with `MYSQL_RANDOM_ROOT_PASSWORD`). The password goes through `MYSQL_PWD`:
/// it never appears in the command shown, in the process list nor in sshOs.
const MYSQL_LOGIN: &str = r#"f() { if [ -n "$2" ] && [ -r "$2" ]; then cat "$2"; else printf %s "$1"; fi; }; RP=$(f "${MYSQL_ROOT_PASSWORD:-$MARIADB_ROOT_PASSWORD}" "${MYSQL_ROOT_PASSWORD_FILE:-$MARIADB_ROOT_PASSWORD_FILE}"); U="${MYSQL_USER:-$MARIADB_USER}"; D="${MYSQL_DATABASE:-$MARIADB_DATABASE}"; if [ -n "$RP" ] || [ -n "$MYSQL_ALLOW_EMPTY_PASSWORD$MARIADB_ALLOW_EMPTY_ROOT_PASSWORD" ] || [ -z "$U" ]; then U="${MYSQL_ROOT_USER:-${MARIADB_ROOT_USER:-root}}"; P="$RP"; else P=$(f "${MYSQL_PASSWORD:-$MARIADB_PASSWORD}" "${MYSQL_PASSWORD_FILE:-$MARIADB_PASSWORD_FILE}"); fi; export MYSQL_PWD="$P"; B=$(command -v mariadb || command -v mysql) || { echo "mysql client not found in the container" >&2; exit 127; }; exec "$B" -u"$U" ${D:+"$D"}"#;

/// Listing: running containers, then their Docker config (exposed ports and database-related
/// environment variables). Secret values are masked on the server: only `***` (set) leaves it.
pub const LIST_CMD: &str = r#"docker ps --format '{{.Names}}|{{.Image}}|{{.Status}}' 2>&1; echo @@inspect; docker ps -q 2>/dev/null | xargs -r docker inspect --format '{{range $p, $v := .Config.ExposedPorts}}{{$.Name}} @port {{$p}}{{println}}{{end}}{{range .Config.Env}}{{$.Name}} {{.}}{{println}}{{end}}' 2>/dev/null | awk '{n=$1; sub(/^\//,"",n); r=substr($0,length($1)+2); if (r ~ /^@port /) {print n" @port "substr(r,7); next} i=index(r,"="); k=substr(r,1,i-1); v=substr(r,i+1); if (k !~ /^(MYSQL|MARIADB|POSTGRES|REDIS|MONGO)/) next; if (k ~ /PASS|SECRET|TOKEN|KEY/ && k !~ /_FILE$/) v=(v=="" ? "" : "***"); print n" "k"="v}'"#;

#[derive(Clone, Debug, PartialEq)]
pub struct Container {
    pub name: String,
    pub image: String,
    pub status: String,
    pub engine: Engine,
    /// Database-related env from the Docker config; secret values are `***`.
    pub env: Vec<(String, String)>,
}

impl Container {
    fn env(&self, k: &str) -> Option<&str> {
        self.env.iter().find(|(x, _)| x == k).map(|(_, v)| v.as_str()).filter(|v| !v.is_empty())
    }
    fn any(&self, keys: &[&str]) -> Option<&str> {
        keys.iter().find_map(|k| self.env(k))
    }
    pub fn default_db(&self) -> Option<String> {
        self.any(&["MYSQL_DATABASE", "MARIADB_DATABASE"]).map(String::from)
    }
    /// Who sshOs logs in as, mirroring `MYSQL_LOGIN`: (user, database, password source).
    pub fn mysql_login(&self) -> (String, Option<String>, &'static str) {
        let root_pw = self.any(&["MYSQL_ROOT_PASSWORD", "MARIADB_ROOT_PASSWORD", "MYSQL_ROOT_PASSWORD_FILE", "MARIADB_ROOT_PASSWORD_FILE"]).is_some();
        let empty_ok = self.any(&["MYSQL_ALLOW_EMPTY_PASSWORD", "MARIADB_ALLOW_EMPTY_ROOT_PASSWORD"]).is_some();
        let db = self.any(&["MYSQL_DATABASE", "MARIADB_DATABASE"]).map(String::from);
        let (user, pw) = match self.any(&["MYSQL_USER", "MARIADB_USER"]) {
            Some(u) if !root_pw && !empty_ok => (u, self.any(&["MYSQL_PASSWORD", "MARIADB_PASSWORD", "MYSQL_PASSWORD_FILE", "MARIADB_PASSWORD_FILE"]).is_some()),
            _ => (self.any(&["MYSQL_ROOT_USER", "MARIADB_ROOT_USER"]).unwrap_or("root"), root_pw),
        };
        let pw = if pw { tr("mot de passe lu dans la config Docker", "password read from the Docker config") } else { tr("sans mot de passe", "no password") };
        (user.into(), db, pw)
    }
}

/// Output of [`LIST_CMD`] → database containers, detected by image name, then by Docker config.
pub fn parse(out: &str) -> Vec<Container> {
    let (ps, inspect) = out.split_once("@@inspect").unwrap_or((out, ""));
    let mut env: std::collections::HashMap<&str, Vec<(String, String)>> = Default::default();
    let mut ports: std::collections::HashMap<&str, Vec<String>> = Default::default();
    for l in inspect.lines() {
        let Some((name, rest)) = l.trim().split_once(' ') else { continue };
        if let Some(p) = rest.strip_prefix("@port ") {
            ports.entry(name).or_default().push(p.trim().into());
        } else if let Some((k, v)) = rest.split_once('=') {
            env.entry(name).or_default().push((k.into(), v.into()));
        }
    }
    ps.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.splitn(3, '|').collect();
            let (name, image) = (f[0].trim(), f.get(1)?.trim());
            let e = env.remove(name).unwrap_or_default();
            let engine = Engine::detect(image).or_else(|| Engine::detect_config(&e, ports.get(name).map(Vec::as_slice).unwrap_or(&[])))?;
            Some(Container { name: name.into(), image: image.into(), status: f.get(2).unwrap_or(&"").trim().into(), engine, env: e })
        })
        .collect()
}

/// Full command: the query is passed on stdin through a quoted heredoc (no shell expansion).
pub fn query_command(c: &Container, query: &str) -> String {
    let q = query.trim_end();
    format!("docker exec -i {} sh -c {} <<'SSHOS_SQL'\n{q}\nSSHOS_SQL", shell_quote(&c.name), shell_quote(&c.engine.client()))
}

/// MySQL / MariaDB with XML output (`mysql -X`), for the explorer: exact values, NULL included.
pub fn xml_command(c: &Container, sql: &str) -> String {
    let client = format!("{MYSQL_LOGIN} -X --default-character-set=utf8mb4");
    format!("docker exec -i {} sh -c {} <<'SSHOS_SQL'\n{}\nSSHOS_SQL", shell_quote(&c.name), shell_quote(&client), sql.trim_end())
}

pub fn shell_command(c: &Container) -> String {
    format!("docker exec -it {} sh -c {}", shell_quote(&c.name), shell_quote(&c.engine.shell()))
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
    // MySQL: explorer (phpMyAdmin-like) or SQL console.
    let explorer = RwSignal::new(true);
    let h = move || host.get_value();

    let load = move || {
        loading.set(true);
        kit::query(h(), LIST_CMD, move |r| {
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

    // Memo: a refresh that changes nothing keeps the explorer (and its state) alive.
    let current = Memo::new(move |_| {
        let s = selected.get()?;
        containers.get()?.into_iter().find(|c| c.name == s)
    });
    let current = move || current.get();
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
                    <Show when=move || current().is_some_and(|c| c.engine == Engine::Mysql)>
                        <div class="seg no-drag">
                            <button class:on=move || explorer.get() on:click=move |_| explorer.set(true)>{tr("Explorateur", "Explorer")}</button>
                            <button class:on=move || !explorer.get() on:click=move |_| explorer.set(false)>"SQL"</button>
                        </div>
                    </Show>
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
                            let login = match c.engine {
                                Engine::Mysql => {
                                    let (user, db, pw) = c.mysql_login();
                                    let db = db.map(|d| format!(" · {} {d}", tr("base", "database"))).unwrap_or_default();
                                    Some(format!("{} {user}{db} · {pw}", tr("Connexion :", "Login:")))
                                }
                                _ => None,
                            };
                            let is_mysql = c.engine == Engine::Mysql;
                            let browser = is_mysql.then(|| {
                                view! {
                                    <div class="dbx-wrap" style:display=move || if explorer.get() { "" } else { "none" }>
                                        <MysqlBrowser host=h() container=c.clone() />
                                    </div>
                                }
                            });
                            view! {
                                {login.map(|l| view! { <p class="muted small db-login">{l}</p> })}
                                {browser}
                                <div class="db-sql" style:display=move || if is_mysql && explorer.get() { "none" } else { "" }>
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
                                </div>
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

    #[test]
    fn docker_config() {
        let out = "shop-db|ghcr.io/acme/shop-db:2|Up 1 day\nwp-db|mysql:8.0|Up 2 days\napp|ghcr.io/acme/app|Up\nraw|acme/store|Up\n@@inspect\n\
                   shop-db MARIADB_USER=shop\nshop-db MARIADB_PASSWORD=***\nshop-db MARIADB_RANDOM_ROOT_PASSWORD=***\nshop-db MARIADB_DATABASE=shop\n\
                   wp-db MYSQL_ROOT_PASSWORD=***\nwp-db MYSQL_DATABASE=wordpress\nwp-db MYSQL_USER=wp\nraw @port 3306/tcp\n";
        let v = parse(out);
        assert_eq!(v.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["shop-db", "wp-db", "raw"]);
        assert!(v.iter().all(|c| c.engine == Engine::Mysql));
        // Random root password: the application user is used.
        let (u, db, _) = v[0].mysql_login();
        assert_eq!((u.as_str(), db.as_deref()), ("shop", Some("shop")));
        // Root password known: root.
        let (u, db, _) = v[1].mysql_login();
        assert_eq!((u.as_str(), db.as_deref()), ("root", Some("wordpress")));
        assert_eq!(v[2].mysql_login().0, "root");
        // The password never reaches the command line.
        let cmd = query_command(&v[1], "SELECT 1;");
        assert!(cmd.contains("MYSQL_PWD") && !cmd.contains("-p\""));
        assert!(LIST_CMD.contains("@@inspect") && LIST_CMD.contains("\"***\""));
    }
}
