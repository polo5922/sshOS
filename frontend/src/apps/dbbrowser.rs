//! SQL explorer for MySQL / MariaDB and PostgreSQL, in the spirit of phpMyAdmin: databases,
//! tables, paginated rows with sorting and search, table structure, and row editing (update,
//! insert, delete).
//!
//! Queries run through the same `docker exec` login as the SQL console (credentials from the
//! container's Docker config, never seen by sshOs), with machine-readable output so results are
//! parsed exactly (NULL vs. text): `mysql -X` (XML), or `psql` unaligned with control-character
//! separators. Every write is confirmed with the exact command first. Values are written as SQL
//! literals; anything with quotes, backslashes or control characters is hex-encoded, so no value
//! can break out of its literal (or out of the heredoc).

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::apps::databases::{explorer_command, Container, Engine};
use crate::icons::Icon;
use crate::state::*;
use crate::tauri::exec;

pub type Row = Vec<Option<String>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    Mysql,
    Postgres,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResultSet {
    pub cols: Vec<String>,
    pub rows: Vec<Row>,
}

fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.into();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let ch = rest.find(';').and_then(|j| {
            let ent = &rest[1..j];
            let c = match ent {
                "lt" => Some('<'),
                "gt" => Some('>'),
                "amp" => Some('&'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ => ent
                    .strip_prefix("#x")
                    .or_else(|| ent.strip_prefix("#X"))
                    .and_then(|h| u32::from_str_radix(h, 16).ok())
                    .or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok()))
                    .and_then(char::from_u32),
            };
            c.map(|c| (c, j))
        });
        match ch {
            Some((c, j)) => {
                out.push(c);
                rest = &rest[j + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn parse_set(body: &str) -> ResultSet {
    let mut set = ResultSet::default();
    for (n, row) in body.split("<row>").skip(1).enumerate() {
        let mut rest = row.split("</row>").next().unwrap_or("");
        let mut vals = Vec::new();
        while let Some(i) = rest.find("<field name=\"") {
            rest = &rest[i + 13..];
            let Some(q) = rest.find('"') else { break };
            let name = unescape(&rest[..q]);
            rest = &rest[q + 1..];
            let Some(gt) = rest.find('>') else { break };
            let attrs = &rest[..gt];
            rest = &rest[gt + 1..];
            let v = if attrs.contains("xsi:nil") {
                None
            } else if attrs.trim_end().ends_with('/') {
                Some(String::new())
            } else {
                let e = rest.find("</field>").unwrap_or(rest.len());
                let v = unescape(&rest[..e]);
                rest = &rest[e..];
                Some(v)
            };
            if n == 0 {
                set.cols.push(name);
            }
            vals.push(v);
        }
        set.rows.push(vals);
    }
    set
}

/// `mysql -X` output → one result set per statement that returns rows.
pub fn parse_xml(out: &str) -> Vec<ResultSet> {
    let mut sets = Vec::new();
    let mut rest = out;
    while let Some(i) = rest.find("<resultset") {
        rest = &rest[i..];
        let Some(gt) = rest.find('>') else { break };
        if rest[..gt].ends_with('/') {
            sets.push(ResultSet::default());
            rest = &rest[gt + 1..];
            continue;
        }
        let end = rest.find("</resultset>").unwrap_or(rest.len());
        sets.push(parse_set(&rest[gt + 1..end]));
        rest = &rest[end..];
    }
    sets
}

/// Printed by `\echo` before each PostgreSQL statement, to split the result sets.
pub const PG_SET: &str = "@@SSHOS_SET@@";
/// psql settings of [`explorer_command`]: field separator, record separator, NULL marker.
pub const PG_FS: char = '\x1f';
pub const PG_RS: char = '\x1e';
pub const PG_NULL: &str = "\x1a";

/// `psql -A` output (header + records, separators above) → one result set per statement.
pub fn parse_psql(out: &str) -> Vec<ResultSet> {
    out.split(&format!("{PG_SET}\n"))
        .skip(1)
        .map(|chunk| {
            let chunk = chunk.strip_suffix('\n').unwrap_or(chunk);
            if chunk.is_empty() {
                return ResultSet::default();
            }
            let mut recs = chunk.split(PG_RS);
            let cols = recs.next().unwrap_or("").split(PG_FS).map(String::from).collect();
            let rows = recs.map(|r| r.split(PG_FS).map(|v| if v == PG_NULL { None } else { Some(v.to_string()) }).collect()).collect();
            ResultSet { cols, rows }
        })
        .collect()
}

/// Statements → the script sent on stdin (PostgreSQL: each one preceded by its set marker).
pub fn script(d: Dialect, stmts: &[String]) -> String {
    match d {
        Dialect::Mysql => stmts.join("\n"),
        Dialect::Postgres => stmts.iter().map(|s| format!("\\echo {PG_SET}\n{s}")).collect::<Vec<_>>().join("\n"),
    }
}

/// Quoted identifier: `` `name` `` (MySQL) or `"name"` (PostgreSQL), quotes doubled.
pub fn ident(d: Dialect, s: &str) -> String {
    match d {
        Dialect::Mysql => format!("`{}`", s.replace('`', "``")),
        Dialect::Postgres => format!("\"{}\"", s.replace('"', "\"\"")),
    }
}

/// `ns` is the database (MySQL) or the schema (PostgreSQL).
pub fn table_ref(d: Dialect, ns: &str, t: &str) -> String {
    format!("{}.{}", ident(d, ns), ident(d, t))
}

/// Text literal. Plain text is quoted as is; text with quotes, backslashes or control characters
/// is hex-encoded (independent of `NO_BACKSLASH_ESCAPES` / `standard_conforming_strings`, and
/// never spans several lines).
pub fn lit(d: Dialect, v: Option<&str>) -> String {
    match v {
        None => "NULL".into(),
        Some(s) if needs_hex(s) => {
            let hex: String = s.bytes().map(|b| format!("{b:02X}")).collect();
            match d {
                Dialect::Mysql => format!("CONVERT(X'{hex}' USING utf8mb4)"),
                Dialect::Postgres => format!("convert_from(decode('{hex}', 'hex'), 'UTF8')"),
            }
        }
        Some(s) => format!("'{s}'"),
    }
}

fn needs_hex(s: &str) -> bool {
    s.chars().any(|c| c == '\'' || c == '\\' || c.is_control())
}

/// Literal for a column: PostgreSQL types hex-encoded text back to the column's type (a quoted
/// literal is untyped and needs no cast).
pub fn lit_col(d: Dialect, c: &Col, v: Option<&str>) -> String {
    match (d, v) {
        (Dialect::Postgres, Some(s)) if needs_hex(s) => format!("{}::{}", lit(d, v), c.ty),
        _ => lit(d, v),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Col {
    pub name: String,
    pub ty: String,
    pub nullable: bool,
    /// `PRI` for primary key columns.
    pub key: String,
    pub default: Option<String>,
    pub extra: String,
}

impl Col {
    /// Filled in by the database when left out (AUTO_INCREMENT, identity, serial).
    pub fn auto(&self) -> bool {
        self.extra.contains("auto_increment") || self.extra == "identity" || self.default.as_deref().is_some_and(|d| d.starts_with("nextval("))
    }
}

pub fn system_dbs(d: Dialect) -> &'static [&'static str] {
    match d {
        Dialect::Mysql => &["information_schema", "mysql", "performance_schema", "sys"],
        Dialect::Postgres => &["postgres"],
    }
}

pub fn dbs_sql(d: Dialect) -> String {
    match d {
        Dialect::Mysql => "SELECT SCHEMA_NAME FROM information_schema.SCHEMATA ORDER BY SCHEMA_NAME;".into(),
        Dialect::Postgres => "SELECT datname FROM pg_database WHERE NOT datistemplate AND datallowconn ORDER BY datname;".into(),
    }
}

/// Columns: namespace, name, rows estimate, size KiB, `BASE TABLE` or `VIEW`.
/// PostgreSQL lists every user schema of the database the client is connected to.
pub fn tables_sql(d: Dialect, db: &str) -> String {
    match d {
        Dialect::Mysql => format!(
            "SELECT TABLE_SCHEMA, TABLE_NAME, TABLE_ROWS, ROUND((DATA_LENGTH + INDEX_LENGTH) / 1024), TABLE_TYPE FROM information_schema.TABLES WHERE TABLE_SCHEMA = {} ORDER BY TABLE_NAME;",
            lit(d, Some(db))
        ),
        Dialect::Postgres => "SELECT n.nspname, c.relname, CASE WHEN c.relkind IN ('r', 'p') AND c.reltuples >= 0 THEN c.reltuples::bigint END, pg_total_relation_size(c.oid) / 1024, CASE WHEN c.relkind IN ('r', 'p') THEN 'BASE TABLE' ELSE 'VIEW' END FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE c.relkind IN ('r', 'p', 'v', 'm', 'f') AND NOT c.relispartition AND n.nspname NOT IN ('pg_catalog', 'information_schema') AND n.nspname NOT LIKE 'pg\\_%' ORDER BY n.nspname <> 'public', n.nspname, c.relname;".into(),
    }
}

fn columns_sql(d: Dialect, ns: &str, t: &str) -> String {
    match d {
        Dialect::Mysql => format!(
            "SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_KEY, COLUMN_DEFAULT, EXTRA FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = {} AND TABLE_NAME = {} ORDER BY ORDINAL_POSITION;",
            lit(d, Some(ns)),
            lit(d, Some(t))
        ),
        Dialect::Postgres => format!(
            "SELECT a.attname, format_type(a.atttypid, a.atttypmod), CASE WHEN a.attnotnull THEN 'NO' ELSE 'YES' END, CASE WHEN EXISTS (SELECT 1 FROM pg_index i WHERE i.indrelid = a.attrelid AND i.indisprimary AND a.attnum = ANY (i.indkey)) THEN 'PRI' ELSE '' END, pg_get_expr(ad.adbin, ad.adrelid), CASE WHEN a.attidentity <> '' THEN 'identity' WHEN a.attgenerated <> '' THEN 'generated' ELSE '' END FROM pg_attribute a LEFT JOIN pg_attrdef ad ON ad.adrelid = a.attrelid AND ad.adnum = a.attnum WHERE a.attrelid = {}::regclass AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attnum;",
            lit(d, Some(&table_ref(d, ns, t)))
        ),
    }
}

pub fn cols_from(set: &ResultSet) -> Vec<Col> {
    set.rows
        .iter()
        .map(|r| {
            let s = |i: usize| r.get(i).cloned().flatten().unwrap_or_default();
            Col { name: s(0), ty: s(1), nullable: s(2) == "YES", key: s(3), default: r.get(4).cloned().flatten(), extra: s(5) }
        })
        .collect()
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageQuery {
    /// (column, descending)
    pub sort: Option<(String, bool)>,
    /// (columns, text): `col1 LIKE '%text%' OR col2 LIKE …`
    pub search: Option<(Vec<String>, String)>,
    pub limit: u32,
    pub offset: u32,
}

/// Three result sets: columns, row count, rows.
pub fn page_sql(d: Dialect, ns: &str, t: &str, q: &PageQuery) -> Vec<String> {
    let tr_ = table_ref(d, ns, t);
    let (cast, like) = match d {
        Dialect::Mysql => ("CHAR", "LIKE"),
        Dialect::Postgres => ("TEXT", "ILIKE"),
    };
    let w = q
        .search
        .as_ref()
        .filter(|(c, v)| !c.is_empty() && !v.is_empty())
        .map(|(c, v)| {
            let pat = lit(d, Some(&format!("%{v}%")));
            let or = c.iter().map(|c| format!("CAST({} AS {cast}) {like} {pat}", ident(d, c))).collect::<Vec<_>>().join(" OR ");
            format!(" WHERE ({or})")
        })
        .unwrap_or_default();
    let o = q.sort.as_ref().map(|(c, desc)| format!(" ORDER BY {}{}", ident(d, c), if *desc { " DESC" } else { "" })).unwrap_or_default();
    vec![
        columns_sql(d, ns, t),
        format!("SELECT COUNT(*) FROM {tr_}{w};"),
        format!("SELECT * FROM {tr_}{w}{o} LIMIT {} OFFSET {};", q.limit, q.offset),
    ]
}

/// Primary key columns, when the row has a value for each of them.
fn pk_of(cols: &[Col], row: &Row) -> Option<Vec<usize>> {
    let pk: Vec<usize> = cols.iter().enumerate().filter(|(_, c)| c.key == "PRI").map(|(i, _)| i).collect();
    (!pk.is_empty() && pk.iter().all(|&i| row.get(i).is_some_and(Option::is_some))).then_some(pk)
}

/// Identifies one row: by primary key when there is one, otherwise by all its values (NULL-safe;
/// PostgreSQL compares their text form, as displayed).
pub fn where_row(d: Dialect, cols: &[Col], row: &Row) -> String {
    let one = |i: usize, f: &dyn Fn(&str, &str) -> String| f(&ident(d, &cols[i].name), &lit(d, row[i].as_deref()));
    match pk_of(cols, row) {
        Some(pk) => pk.iter().map(|&i| format!("{} = {}", ident(d, &cols[i].name), lit_col(d, &cols[i], row[i].as_deref()))).collect::<Vec<_>>().join(" AND "),
        None => (0..cols.len().min(row.len()))
            .map(|i| match d {
                Dialect::Mysql => one(i, &|c, v| format!("{c} <=> {v}")),
                Dialect::Postgres => one(i, &|c, v| format!("CAST({c} AS TEXT) IS NOT DISTINCT FROM {v}")),
            })
            .collect::<Vec<_>>()
            .join(" AND "),
    }
}

pub fn has_pk(cols: &[Col]) -> bool {
    cols.iter().any(|c| c.key == "PRI")
}

/// PostgreSQL has no `LIMIT` on UPDATE / DELETE: without a primary key, one row is picked by `ctid`.
fn pg_one(tref: &str, cols: &[Col], row: &Row, w: String) -> String {
    if pk_of(cols, row).is_some() { w } else { format!("ctid = (SELECT ctid FROM {tref} WHERE {w} LIMIT 1)") }
}

/// The write, then a one-value result set with the number of rows affected.
fn counted(d: Dialect, stmt: String) -> Vec<String> {
    match d {
        Dialect::Mysql => vec![format!("{stmt};"), "SELECT ROW_COUNT();".into()],
        Dialect::Postgres => vec![format!("WITH w AS ({stmt} RETURNING 1) SELECT count(*) FROM w;")],
    }
}

pub fn update_sql(d: Dialect, ns: &str, t: &str, cols: &[Col], row: &Row, changes: &[(usize, Option<String>)]) -> Vec<String> {
    let tref = table_ref(d, ns, t);
    let set = changes.iter().map(|(i, v)| format!("{} = {}", ident(d, &cols[*i].name), lit_col(d, &cols[*i], v.as_deref()))).collect::<Vec<_>>().join(", ");
    let w = where_row(d, cols, row);
    match d {
        Dialect::Mysql => counted(d, format!("UPDATE {tref} SET {set} WHERE {w} LIMIT 1")),
        Dialect::Postgres => counted(d, format!("UPDATE {tref} SET {set} WHERE {}", pg_one(&tref, cols, row, w))),
    }
}

pub fn delete_sql(d: Dialect, ns: &str, t: &str, cols: &[Col], row: &Row) -> Vec<String> {
    let tref = table_ref(d, ns, t);
    let w = where_row(d, cols, row);
    match d {
        Dialect::Mysql => counted(d, format!("DELETE FROM {tref} WHERE {w} LIMIT 1")),
        Dialect::Postgres => counted(d, format!("DELETE FROM {tref} WHERE {}", pg_one(&tref, cols, row, w))),
    }
}

pub fn insert_sql(d: Dialect, ns: &str, t: &str, vals: &[(Col, Option<String>)]) -> Vec<String> {
    let stmt = if vals.is_empty() {
        format!("INSERT INTO {} DEFAULT VALUES", table_ref(d, ns, t))
    } else {
        let names = vals.iter().map(|(c, _)| ident(d, &c.name)).collect::<Vec<_>>().join(", ");
        let values = vals.iter().map(|(c, v)| lit_col(d, c, v.as_deref())).collect::<Vec<_>>().join(", ");
        format!("INSERT INTO {} ({names}) VALUES ({values})", table_ref(d, ns, t))
    };
    counted(d, stmt)
}

pub fn err_text(r: &sshos_shared::ExecResult) -> String {
    let e: Vec<&str> = r.stderr.lines().filter(|l| !l.contains("[Warning]") && !l.trim().is_empty()).collect();
    if e.is_empty() { r.stdout.trim().to_string() } else { e.join("\n") }
}

/// Runs statements in the container (PostgreSQL: connected to `db`) and parses the result sets.
fn run_sql(cx: Ctx, host: String, c: &Container, d: Dialect, db: Option<String>, stmts: &[String], quiet: bool, f: impl FnOnce(Result<Vec<ResultSet>, String>) + 'static) {
    let cmd = explorer_command(c, db.as_deref(), &script(d, stmts));
    spawn_local(async move {
        match exec(&host, &cmd, quiet).await {
            Ok(r) if r.ok() => f(Ok(match d {
                Dialect::Mysql => parse_xml(&r.stdout),
                Dialect::Postgres => parse_psql(&r.stdout),
            })),
            Ok(r) => f(Err(err_text(&r))),
            Err(e) => {
                cx.handle_err(Some(&host), e);
                f(Err(String::new()));
            }
        }
    });
}

pub fn short(s: &str) -> String {
    let one = s.replace(['\n', '\r'], " ");
    if one.chars().count() > 80 { format!("{}…", one.chars().take(80).collect::<String>()) } else { one }
}

#[derive(Clone, Copy, PartialEq)]
enum Pane {
    Data,
    Structure,
}

#[derive(Clone)]
struct EditState {
    /// `None` when inserting.
    orig: Option<Row>,
    vals: Vec<RwSignal<String>>,
    nulls: Vec<RwSignal<bool>>,
}

/// (namespace, name, rows estimate, size KiB, is a view)
type TableInfo = (String, String, Option<String>, Option<String>, bool);

/// Name shown for a table: PostgreSQL prefixes schemas other than `public`.
fn shown(d: Dialect, ns: &str, t: &str) -> String {
    if d == Dialect::Postgres && ns != "public" { format!("{ns}.{t}") } else { t.to_string() }
}

#[component]
pub fn SqlBrowser(host: String, container: Container) -> impl IntoView {
    // Captured once: loads are chained from async callbacks, where `ctx()` is unavailable.
    let cx = ctx();
    let dl = if container.engine == Engine::Postgres { Dialect::Postgres } else { Dialect::Mysql };
    let default_db = container.default_db();
    let host = StoredValue::new(host);
    let cont = StoredValue::new(container);
    let dbs = RwSignal::new(None::<Vec<String>>);
    let db = RwSignal::new(None::<String>);
    let tables = RwSignal::new(None::<Vec<TableInfo>>);
    let table = RwSignal::new(None::<(String, String)>);
    let pane = RwSignal::new(Pane::Data);
    let cols = RwSignal::new(Vec::<Col>::new());
    let rows = RwSignal::new(None::<Vec<Row>>);
    let total = RwSignal::new(0u64);
    let offset = RwSignal::new(0u32);
    let limit = RwSignal::new(50u32);
    let sort = RwSignal::new(None::<(String, bool)>);
    let search_col = RwSignal::new(String::new());
    let search = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let edit = RwSignal::new(None::<EditState>);

    let fail = move |e: String| {
        let _ = busy.try_set(false);
        if !e.is_empty() {
            let _ = error.try_set(Some(e));
        }
    };

    let load_page = move || {
        let (Some(hid), Some(c)) = (host.try_get_value(), cont.try_get_value()) else { return };
        let (Some(d), Some((ns, t))) = (db.try_get_untracked().flatten(), table.try_get_untracked().flatten()) else { return };
        let q = PageQuery {
            sort: sort.try_get_untracked().unwrap_or_default(),
            // No column picked: search them all (columns known from the previous load).
            search: Some((
                match search_col.try_get_untracked().unwrap_or_default() {
                    c if c.is_empty() => cols.try_get_untracked().unwrap_or_default().into_iter().map(|c| c.name).collect(),
                    c => vec![c],
                },
                search.try_get_untracked().unwrap_or_default(),
            )),
            limit: limit.try_get_untracked().unwrap_or_default(),
            offset: offset.try_get_untracked().unwrap_or_default(),
        };
        let _ = busy.try_set(true);
        run_sql(cx, hid, &c, dl, Some(d), &page_sql(dl, &ns, &t, &q), true, move |r| match r {
            Ok(sets) if sets.len() >= 3 => {
                let _ = busy.try_set(false);
                let _ = error.try_set(None);
                let _ = cols.try_set(cols_from(&sets[0]));
                let n = sets[1].rows.first().and_then(|r| r.first().cloned().flatten()).and_then(|n| n.parse().ok()).unwrap_or(0);
                let _ = total.try_set(n);
                let _ = rows.try_set(Some(sets[2].rows.clone()));
            }
            Ok(_) => fail(tr("Réponse inattendue du serveur de base de données.", "Unexpected answer from the database server.").into()),
            Err(e) => fail(e),
        });
    };

    let open_table = move |t: (String, String)| {
        table.set(Some(t));
        cols.set(Vec::new());
        rows.set(None);
        offset.set(0);
        sort.set(None);
        search.set(String::new());
        search_col.set(String::new());
        load_page();
    };

    let load_tables = move || {
        let (Some(hid), Some(c)) = (host.try_get_value(), cont.try_get_value()) else { return };
        let Some(d) = db.try_get_untracked().flatten() else { return };
        let _ = tables.try_set(None);
        let _ = table.try_set(None);
        let _ = rows.try_set(None);
        run_sql(cx, hid, &c, dl, Some(d.clone()), &[tables_sql(dl, &d)], true, move |r| match r {
            Ok(sets) => {
                let _ = error.try_set(None);
                let list = sets
                    .first()
                    .map(|s| {
                        s.rows
                            .iter()
                            .map(|r| {
                                let g = |i: usize| r.get(i).cloned().flatten();
                                (g(0).unwrap_or_default(), g(1).unwrap_or_default(), g(2), g(3), g(4).is_some_and(|t| t.contains("VIEW")))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let _ = tables.try_set(Some(list));
            }
            Err(e) => {
                let _ = tables.try_set(Some(Vec::new()));
                fail(e)
            }
        });
    };

    let load_dbs = move || {
        let (Some(hid), Some(c)) = (host.try_get_value(), cont.try_get_value()) else { return };
        let default_db = default_db.clone();
        run_sql(cx, hid, &c, dl, None, &[dbs_sql(dl)], true, move |r| match r {
            Ok(sets) => {
                let list: Vec<String> = sets.first().map(|s| s.rows.iter().filter_map(|r| r.first().cloned().flatten()).collect()).unwrap_or_default();
                let keep = db.try_get_untracked().flatten().filter(|d| list.contains(d));
                let pick = keep
                    .or_else(|| default_db.filter(|d| list.contains(d)))
                    .or_else(|| list.iter().find(|d| !system_dbs(dl).contains(&d.as_str())).cloned())
                    .or_else(|| list.first().cloned());
                let _ = dbs.try_set(Some(list));
                let _ = db.try_set(pick);
                load_tables();
            }
            Err(e) => {
                let _ = dbs.try_set(Some(Vec::new()));
                fail(e)
            }
        });
    };
    load_dbs();

    // Davish rule: every write shows the exact command and waits for confirmation.
    let write = move |title: &'static str, stmts: Vec<String>, danger: bool| {
        let (Some(hid), Some(c)) = (host.try_get_value(), cont.try_get_value()) else { return };
        let dbn = db.try_get_untracked().flatten();
        let cmd = explorer_command(&c, dbn.as_deref(), &script(dl, &stmts));
        let mut d = Dialog::confirm(title, tr("Cette commande va être exécutée sur le serveur :", "This command will run on the server:"), tr("Exécuter", "Run"), move || {
            busy.set(true);
            run_sql(cx, hid.clone(), &c, dl, dbn.clone(), &stmts, false, move |r| match r {
                Ok(sets) => {
                    let n = sets.last().and_then(|s| s.rows.first()).and_then(|r| r.first().cloned().flatten()).unwrap_or_default();
                    if n == "0" {
                        cx.toast(ToastKind::Error, tr("Aucune ligne modifiée (la ligne a peut-être changé).", "No row changed (the row may have changed)."));
                    } else {
                        cx.ok(format!("{} {n}", tr("Lignes affectées :", "Rows affected:")));
                    }
                    let _ = edit.try_set(None);
                    load_page();
                }
                Err(e) => fail(e),
            });
        })
        .code(cmd);
        if danger {
            d = d.danger();
        }
        cx.show(d);
    };

    let open_edit = move |orig: Option<Row>| {
        let cs = cols.get_untracked();
        let vals = cs
            .iter()
            .enumerate()
            .map(|(i, _)| RwSignal::new(orig.as_ref().and_then(|r| r.get(i).cloned().flatten()).unwrap_or_default()))
            .collect();
        let nulls = cs.iter().enumerate().map(|(i, _)| RwSignal::new(orig.as_ref().is_some_and(|r| r.get(i).is_some_and(Option::is_none)))).collect();
        edit.set(Some(EditState { orig, vals, nulls }));
    };

    let save = move || {
        let Some(e) = edit.get_untracked() else { return };
        let cs = cols.get_untracked();
        let Some((ns, t)) = table.get_untracked() else { return };
        let vals: Vec<Option<String>> = e.vals.iter().zip(&e.nulls).map(|(v, n)| if n.get_untracked() { None } else { Some(v.get_untracked()) }).collect();
        match &e.orig {
            Some(orig) => {
                let changes: Vec<(usize, Option<String>)> = vals.into_iter().enumerate().filter(|(i, v)| orig.get(*i) != Some(v)).collect();
                if changes.is_empty() {
                    edit.set(None);
                } else {
                    write(tr("Modifier la ligne", "Update row"), update_sql(dl, &ns, &t, &cs, orig, &changes), false);
                }
            }
            None => {
                // Empty fields are left out: the column default (or AUTO_INCREMENT) applies.
                let pairs: Vec<(Col, Option<String>)> = cs.iter().zip(vals).filter(|(_, v)| v.as_deref() != Some("")).map(|(c, v)| (c.clone(), v)).collect();
                write(tr("Insérer une ligne", "Insert row"), insert_sql(dl, &ns, &t, &pairs), false);
            }
        }
    };

    let delete = move |row: Row| {
        let cs = cols.get_untracked();
        let Some((ns, t)) = table.get_untracked() else { return };
        write(tr("Supprimer la ligne", "Delete row"), delete_sql(dl, &ns, &t, &cs, &row), true);
    };

    let sort_by = move |c: String| {
        let next = match sort.get_untracked() {
            Some((s, false)) if s == c => Some((c, true)),
            _ => Some((c, false)),
        };
        sort.set(next);
        offset.set(0);
        load_page();
    };

    let is_view = move || {
        let t = table.get()?;
        tables.get()?.into_iter().find(|x| x.0 == t.0 && x.1 == t.1).map(|x| x.4)
    };

    let tables_list = move || match tables.get() {
        None => view! { <div class="empty muted small"><span class="spinner"></span></div> }.into_any(),
        Some(v) if v.is_empty() => view! { <p class="muted small hint">{tr("Aucune table.", "No table.")}</p> }.into_any(),
        Some(v) => v
            .into_iter()
            .map(|(ns, name, n, kb, view_)| {
                let key = (ns.clone(), name.clone());
                let (n1, n2) = (key.clone(), key);
                let name = shown(dl, &ns, &name);
                let meta = if view_ { tr("vue", "view").to_string() } else { n.unwrap_or_default() };
                let title = kb.map(|k| format!("{name} · {k} KiB")).unwrap_or_else(|| name.clone());
                view! {
                    <button class="dbx-table" class:active=move || table.get().as_ref() == Some(&n1) title=title on:click=move |_| open_table(n2.clone())>
                        <Icon name={if view_ { "eye" } else { "list" }} size=13 />
                        <span class="grow ellipsis">{name}</span>
                        <span class="muted small">{meta}</span>
                    </button>
                }
            })
            .collect_view()
            .into_any(),
    };

    let grid = move || {
        let cs = cols.get();
        let Some(rs) = rows.get() else {
            return if busy.get() { view! { <div class="empty muted"><span class="spinner"></span>{tr("Chargement…", "Loading…")}</div> }.into_any() } else { ().into_any() };
        };
        let editable = is_view() == Some(false);
        let head = cs
            .iter()
            .map(|c| {
                let n = c.name.clone();
                let n2 = c.name.clone();
                let pk = c.key == "PRI";
                view! {
                    <th class="sortable" title=c.ty.clone() on:click=move |_| sort_by(n.clone())>
                        {pk.then(|| view! { <Icon name="key" size=11 /> })}
                        {c.name.clone()}
                        {move || match sort.get() {
                            Some((s, d)) if s == n2 => if d { " ▼" } else { " ▲" },
                            _ => "",
                        }}
                    </th>
                }
            })
            .collect_view();
        let body = rs
            .into_iter()
            .map(|r| {
                let (r1, r2, r3) = (r.clone(), r.clone(), r.clone());
                view! {
                    <tr on:dblclick=move |_| if editable { open_edit(Some(r1.clone())) }>
                        <td class="db-actions">
                            <Show when=move || editable>
                                <button class="icon-btn tiny-btn" title=tr("Modifier", "Edit") on:click={
                                    let r = r2.clone();
                                    move |_| open_edit(Some(r.clone()))
                                }><Icon name="edit" size=13 /></button>
                                <button class="icon-btn tiny-btn danger" title=tr("Supprimer", "Delete") on:click={
                                    let r = r3.clone();
                                    move |_| delete(r.clone())
                                }><Icon name="trash" size=13 /></button>
                            </Show>
                        </td>
                        {r.into_iter()
                            .map(|v| match v {
                                None => view! { <td class="null">"NULL"</td> }.into_any(),
                                Some(s) => {
                                    let t = if s.chars().count() > 80 || s.contains('\n') { s.clone() } else { String::new() };
                                    view! { <td title=t>{short(&s)}</td> }.into_any()
                                }
                            })
                            .collect_view()}
                    </tr>
                }
            })
            .collect_view();
        view! {
            <div class="table-wrap db-grid" class:loading=move || busy.get()>
                <table class="table data-table">
                    <thead><tr><th class="db-actions"></th>{head}</tr></thead>
                    <tbody>{body}</tbody>
                </table>
            </div>
        }
        .into_any()
    };

    let structure = move || {
        let cs = cols.get();
        view! {
            <div class="table-wrap db-grid">
                <table class="table data-table">
                    <thead>
                        <tr>
                            <th>{tr("Colonne", "Column")}</th>
                            <th>"Type"</th>
                            <th>"Null"</th>
                            <th>{tr("Clé", "Key")}</th>
                            <th>{tr("Défaut", "Default")}</th>
                            <th>"Extra"</th>
                        </tr>
                    </thead>
                    <tbody>
                        {cs.into_iter()
                            .map(|c| {
                                view! {
                                    <tr>
                                        <td><b>{c.name}</b></td>
                                        <td class="mono">{c.ty}</td>
                                        <td>{if c.nullable { tr("oui", "yes") } else { tr("non", "no") }}</td>
                                        <td>{(!c.key.is_empty()).then(|| view! { <span class="pill">{c.key}</span> })}</td>
                                        <td class:null=c.default.is_none()>{c.default.clone().unwrap_or_else(|| "NULL".into())}</td>
                                        <td class="muted">{c.extra}</td>
                                    </tr>
                                }
                            })
                            .collect_view()}
                    </tbody>
                </table>
            </div>
        }
    };

    let pager = move || {
        let n = rows.get().map(|r| r.len()).unwrap_or(0) as u64;
        let (off, lim, tot) = (offset.get() as u64, limit.get() as u64, total.get());
        let label = if n == 0 { format!("0 / {tot}") } else { format!("{}–{} / {tot}", off + 1, off + n) };
        view! {
            <div class="dbx-pager">
                <button class="icon-btn" disabled=move || offset.get() == 0 title=tr("Page précédente", "Previous page") on:click=move |_| {
                    offset.set(offset.get_untracked().saturating_sub(limit.get_untracked()));
                    load_page();
                }><Icon name="left" size=14 /></button>
                <span class="muted small">{label}</span>
                <button class="icon-btn" disabled={off + lim >= tot} title=tr("Page suivante", "Next page") on:click=move |_| {
                    offset.set(offset.get_untracked() + limit.get_untracked());
                    load_page();
                }><Icon name="right" size=14 /></button>
                <div class="seg">
                    {[25u32, 50, 100, 250]
                        .into_iter()
                        .map(|l| {
                            view! {
                                <button class:on=move || limit.get() == l on:click=move |_| {
                                    limit.set(l);
                                    offset.set(0);
                                    load_page();
                                }>{l.to_string()}</button>
                            }
                        })
                        .collect_view()}
                </div>
            </div>
        }
    };

    let sheet = move || {
        edit.get().map(|e| {
            let cs = cols.get_untracked();
            let inserting = e.orig.is_none();
            let fields = cs
                .into_iter()
                .enumerate()
                .map(|(i, c)| {
                    let (v, n) = (e.vals[i], e.nulls[i]);
                    let long = c.ty.contains("text") || c.ty.contains("json") || v.get_untracked().len() > 60 || v.get_untracked().contains('\n');
                    let ph = if inserting {
                        if c.auto() {
                            if c.extra.contains("auto_increment") { "AUTO_INCREMENT".to_string() } else { "auto".to_string() }
                        } else if c.default.is_none() && !c.nullable {
                            tr("obligatoire", "required").to_string()
                        } else { format!("{} {}", tr("défaut :", "default:"), c.default.clone().unwrap_or_else(|| "NULL".into())) }
                    } else {
                        String::new()
                    };
                    view! {
                        <div class="db-field">
                            <div class="db-field-head">
                                <b>{c.name.clone()}</b>
                                <span class="muted mono small">{c.ty.clone()}</span>
                                {(c.key == "PRI").then(|| view! { <span class="pill">"PRI"</span> })}
                                <span class="grow"></span>
                                {c.nullable.then(|| view! { <label class="check small"><input type="checkbox" bind:checked=n />"NULL"</label> })}
                            </div>
                            <textarea class="mono" rows={if long { "4" } else { "1" }} spellcheck="false" placeholder=ph bind:value=v disabled=move || n.get()></textarea>
                        </div>
                    }
                })
                .collect_view();
            view! {
                <div class="sheet-overlay" on:click=move |_| edit.set(None)>
                    <div class="sheet wide" on:click=|ev| ev.stop_propagation()>
                        <button class="icon-btn sheet-close" on:click=move |_| edit.set(None)><Icon name="x" /></button>
                        <h3>
                            {if inserting { tr("Insérer une ligne", "Insert row") } else { tr("Modifier la ligne", "Update row") }}
                            " · "
                            {table.get_untracked().map(|(ns, t)| shown(dl, &ns, &t)).unwrap_or_default()}
                        </h3>
                        <div class="form db-edit">
                            {fields}
                            <p class="muted small">
                                {if inserting {
                                    tr("Les champs vides prennent la valeur par défaut de la colonne.", "Empty fields take the column default.")
                                } else {
                                    tr("Seules les colonnes modifiées sont envoyées.", "Only changed columns are sent.")
                                }}
                            </p>
                            <div class="form-actions">
                                <button on:click=move |_| edit.set(None)>{tr("Annuler", "Cancel")}</button>
                                <button class="primary" on:click=move |_| save()>{tr("Enregistrer…", "Save…")}</button>
                            </div>
                        </div>
                    </div>
                </div>
            }
        })
    };

    view! {
        <div class="dbx">
            <aside class="dbx-side">
                <select class="no-drag" prop:value=move || db.get().unwrap_or_default() on:change=move |e| {
                    db.set(Some(event_target_value(&e)));
                    load_tables();
                }>
                    {move || {
                        dbs.get()
                            .unwrap_or_default()
                            .into_iter()
                            .map(|d| {
                                let sys = system_dbs(dl).contains(&d.as_str());
                                let sel = db.get_untracked().as_deref() == Some(d.as_str());
                                let label = if sys { format!("{d} ·") } else { d.clone() };
                                view! { <option value=d selected=sel>{label}</option> }
                            })
                            .collect_view()
                    }}
                </select>
                <div class="dbx-tables">{tables_list}</div>
            </aside>
            <div class="dbx-main">
                {move || error.get().map(|e| view! {
                    <div class="banner crit">
                        <pre class="grow db-error">{e}</pre>
                        <button class="icon-btn" on:click=move |_| error.set(None)><Icon name="x" size=13 /></button>
                    </div>
                })}
                {move || match table.get() {
                    None => crate::apps::kit::empty_view(tr("Choisissez une table.", "Pick a table.")).into_any(),
                    Some(t) => view! {
                        <div class="dbx-bar">
                            <h3 class="ellipsis">{shown(dl, &t.0, &t.1)}</h3>
                            <div class="seg">
                                <button class:on=move || pane.get() == Pane::Data on:click=move |_| pane.set(Pane::Data)>{tr("Données", "Data")}</button>
                                <button class:on=move || pane.get() == Pane::Structure on:click=move |_| pane.set(Pane::Structure)>{tr("Structure", "Structure")}</button>
                            </div>
                            <span class="grow"></span>
                            <Show when=move || pane.get() == Pane::Data>
                                <select class="db-search-col" prop:value=move || search_col.get() on:change=move |e| search_col.set(event_target_value(&e))>
                                    <option value="">{tr("Toutes les colonnes", "All columns")}</option>
                                    {move || cols.get().into_iter().map(|c| view! { <option value=c.name.clone()>{c.name.clone()}</option> }).collect_view()}
                                </select>
                                <label class="search-field">
                                    <Icon name="search" size=13 />
                                    <input placeholder=tr("Contient…", "Contains…") bind:value=search on:keydown=move |e| {
                                        if e.key() == "Enter" {
                                            offset.set(0);
                                            load_page();
                                        }
                                    } />
                                </label>
                                <button class="primary small" disabled=move || is_view() != Some(false) || cols.get().is_empty() on:click=move |_| open_edit(None)>
                                    <Icon name="plus" size=13 />
                                    {tr("Insérer", "Insert")}
                                </button>
                            </Show>
                            <button class="icon-btn" class:spin=move || busy.get() title=tr("Actualiser", "Refresh") on:click=move |_| load_page()>
                                <Icon name="refresh" size=14 />
                            </button>
                        </div>
                        {move || (pane.get() == Pane::Data && !cols.get().is_empty() && !has_pk(&cols.get()) && is_view() == Some(false)).then(|| view! {
                            <p class="muted small">{tr("Pas de clé primaire : une ligne est identifiée par toutes ses valeurs.", "No primary key: a row is identified by all its values.")}</p>
                        })}
                        {move || if pane.get() == Pane::Data { view! { {grid} {pager} }.into_any() } else { structure().into_any() }}
                    }
                    .into_any(),
                }}
            </div>
            {sheet}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Dialect::{Mysql, Postgres};

    fn col(name: &str, key: &str) -> Col {
        Col { name: name.into(), ty: "varchar(20)".into(), nullable: true, key: key.into(), default: None, extra: String::new() }
    }

    #[test]
    fn xml() {
        let out = r#"<?xml version="1.0"?>

<resultset statement="SELECT COUNT(*) FROM `shop`.`users`;
" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
  <row>
	<field name="COUNT(*)">2</field>
  </row>
</resultset>
<resultset statement="SELECT * FROM `shop`.`users` WHERE a &lt; 3" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
  <row>
	<field name="id">1</field>
	<field name="name">Tom &amp; &quot;Jerry&quot; &lt;3 &#233;</field>
	<field name="bio" xsi:nil="true" />
	<field name="note"></field>
  </row>
  <row>
	<field name="id">2</field>
	<field name="name">line1
line2</field>
	<field name="bio">x</field>
	<field name="note"/>
  </row>
</resultset>
<resultset statement="SELECT 1 FROM t WHERE 0" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
</resultset>
"#;
        let s = parse_xml(out);
        assert_eq!(s.len(), 3);
        assert_eq!(s[0].rows, vec![vec![Some("2".to_string())]]);
        assert_eq!(s[1].cols, ["id", "name", "bio", "note"]);
        assert_eq!(s[1].rows[0], vec![Some("1".into()), Some("Tom & \"Jerry\" <3 é".into()), None, Some(String::new())]);
        assert_eq!(s[1].rows[1][1].as_deref(), Some("line1\nline2"));
        assert_eq!(s[1].rows[1][3].as_deref(), Some(""));
        assert_eq!(s[2], ResultSet::default());
    }

    #[test]
    fn psql() {
        let out = "@@SSHOS_SET@@\ncount\x1e2\n@@SSHOS_SET@@\nid\x1fname\x1fbio\x1e1\x1fAnn\x1f\x1a\x1e2\x1fline1\nline2\x1f\n@@SSHOS_SET@@\nid\x1fname\n";
        let s = parse_psql(out);
        assert_eq!(s.len(), 3);
        assert_eq!(s[0].rows, vec![vec![Some("2".to_string())]]);
        assert_eq!(s[1].cols, ["id", "name", "bio"]);
        assert_eq!(s[1].rows[0], vec![Some("1".into()), Some("Ann".into()), None]);
        assert_eq!(s[1].rows[1], vec![Some("2".into()), Some("line1\nline2".into()), Some(String::new())]);
        assert_eq!(s[2], ResultSet { cols: vec!["id".into(), "name".into()], rows: vec![] });
        assert_eq!(script(Postgres, &["SELECT 1;".into(), "SELECT 2;".into()]), "\\echo @@SSHOS_SET@@\nSELECT 1;\n\\echo @@SSHOS_SET@@\nSELECT 2;");
    }

    #[test]
    fn mysql_statements() {
        assert_eq!(ident(Mysql, "we`ird"), "`we``ird`");
        assert_eq!(lit(Mysql, None), "NULL");
        assert_eq!(lit(Mysql, Some("héllo 42")), "'héllo 42'");
        assert_eq!(lit(Mysql, Some("it's")), "CONVERT(X'69742773' USING utf8mb4)");
        assert_eq!(lit(Mysql, Some("a\nSSHOS_SQL")), "CONVERT(X'610A5353484F535F53514C' USING utf8mb4)");
        let cols = vec![col("id", "PRI"), col("name", ""), col("bio", "")];
        let row: Row = vec![Some("7".into()), Some("Ann".into()), None];
        assert_eq!(
            update_sql(Mysql, "shop", "users", &cols, &row, &[(1, Some("Bob".into())), (2, None)]),
            ["UPDATE `shop`.`users` SET `name` = 'Bob', `bio` = NULL WHERE `id` = '7' LIMIT 1;", "SELECT ROW_COUNT();"]
        );
        assert_eq!(delete_sql(Mysql, "shop", "users", &cols, &row), ["DELETE FROM `shop`.`users` WHERE `id` = '7' LIMIT 1;", "SELECT ROW_COUNT();"]);
        // Without a primary key: every column, NULL-safe.
        let nopk = vec![col("a", ""), col("b", "")];
        assert_eq!(where_row(Mysql, &nopk, &vec![Some("1".into()), None]), "`a` <=> '1' AND `b` <=> NULL");
        assert_eq!(
            insert_sql(Mysql, "shop", "users", &[(col("name", ""), Some("Zoé".into())), (col("bio", ""), None)]),
            ["INSERT INTO `shop`.`users` (`name`, `bio`) VALUES ('Zoé', NULL);", "SELECT ROW_COUNT();"]
        );
        let q = PageQuery { sort: Some(("name".into(), true)), search: Some((vec!["name".into(), "bio".into()], "o'k".into())), limit: 50, offset: 100 };
        let sql = page_sql(Mysql, "shop", "users", &q);
        assert_eq!(sql.len(), 3);
        assert_eq!(sql[2], "SELECT * FROM `shop`.`users` WHERE (CAST(`name` AS CHAR) LIKE CONVERT(X'256F276B25' USING utf8mb4) OR CAST(`bio` AS CHAR) LIKE CONVERT(X'256F276B25' USING utf8mb4)) ORDER BY `name` DESC LIMIT 50 OFFSET 100;");
    }

    #[test]
    fn postgres_statements() {
        assert_eq!(ident(Postgres, "we\"ird"), "\"we\"\"ird\"");
        assert_eq!(lit(Postgres, Some("it's")), "convert_from(decode('69742773', 'hex'), 'UTF8')");
        let mut id = col("id", "PRI");
        id.ty = "integer".into();
        let cols = vec![id, col("name", ""), col("bio", "")];
        let row: Row = vec![Some("7".into()), Some("Ann".into()), None];
        assert_eq!(
            update_sql(Postgres, "public", "users", &cols, &row, &[(1, Some("O'Neil".into()))]),
            ["WITH w AS (UPDATE \"public\".\"users\" SET \"name\" = convert_from(decode('4F274E65696C', 'hex'), 'UTF8')::varchar(20) WHERE \"id\" = '7' RETURNING 1) SELECT count(*) FROM w;"]
        );
        // Without a primary key: one row, picked by ctid; values compared as displayed (text).
        let nopk = vec![col("a", ""), col("b", "")];
        assert_eq!(
            delete_sql(Postgres, "app", "log", &nopk, &vec![Some("1".into()), None]),
            ["WITH w AS (DELETE FROM \"app\".\"log\" WHERE ctid = (SELECT ctid FROM \"app\".\"log\" WHERE CAST(\"a\" AS TEXT) IS NOT DISTINCT FROM '1' AND CAST(\"b\" AS TEXT) IS NOT DISTINCT FROM NULL LIMIT 1) RETURNING 1) SELECT count(*) FROM w;"]
        );
        assert_eq!(insert_sql(Postgres, "public", "t", &[]), ["WITH w AS (INSERT INTO \"public\".\"t\" DEFAULT VALUES RETURNING 1) SELECT count(*) FROM w;"]);
        let q = PageQuery { sort: None, search: Some((vec!["name".into()], "ann".into())), limit: 25, offset: 0 };
        let sql = page_sql(Postgres, "public", "users", &q);
        assert!(sql[0].contains("WHERE a.attrelid = '\"public\".\"users\"'::regclass"));
        assert_eq!(sql[1], "SELECT COUNT(*) FROM \"public\".\"users\" WHERE (CAST(\"name\" AS TEXT) ILIKE '%ann%');");
        assert_eq!(shown(Postgres, "app", "log"), "app.log");
        assert_eq!(shown(Postgres, "public", "log"), "log");
    }
}
