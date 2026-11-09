//! MySQL / MariaDB explorer, in the spirit of phpMyAdmin: databases, tables, paginated rows with
//! sorting and search, table structure, and row editing (update, insert, delete).
//!
//! Queries run through the same `docker exec` login as the SQL console (credentials from the
//! container's Docker config, never seen by sshOs), with `mysql -X` so results are parsed exactly
//! (NULL vs. text). Every write is confirmed with the exact command first. Values are written as
//! SQL literals; anything with quotes, backslashes or control characters is hex-encoded, so no
//! value can break out of its literal (or out of the heredoc).

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::apps::databases::{xml_command, Container};
use crate::icons::Icon;
use crate::state::*;
use crate::tauri::exec;

pub type Row = Vec<Option<String>>;

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

/// `identifier` with backticks doubled.
pub fn ident(s: &str) -> String {
    format!("`{}`", s.replace('`', "``"))
}

pub fn table_ref(db: &str, t: &str) -> String {
    format!("{}.{}", ident(db), ident(t))
}

/// SQL literal. Plain text is quoted as is; text with quotes, backslashes or control characters
/// is hex-encoded (independent of `NO_BACKSLASH_ESCAPES`, and never spans several lines).
pub fn lit(v: Option<&str>) -> String {
    match v {
        None => "NULL".into(),
        Some(s) if s.chars().any(|c| c == '\'' || c == '\\' || c.is_control()) => {
            let hex: String = s.bytes().map(|b| format!("{b:02X}")).collect();
            format!("CONVERT(X'{hex}' USING utf8mb4)")
        }
        Some(s) => format!("'{s}'"),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Col {
    pub name: String,
    pub ty: String,
    pub nullable: bool,
    pub key: String,
    pub default: Option<String>,
    pub extra: String,
}

pub const SYSTEM_DBS: [&str; 4] = ["information_schema", "mysql", "performance_schema", "sys"];

pub fn dbs_sql() -> String {
    "SELECT SCHEMA_NAME FROM information_schema.SCHEMATA ORDER BY SCHEMA_NAME;".into()
}

pub fn tables_sql(db: &str) -> String {
    format!(
        "SELECT TABLE_NAME, TABLE_ROWS, ROUND((DATA_LENGTH + INDEX_LENGTH) / 1024), TABLE_TYPE FROM information_schema.TABLES WHERE TABLE_SCHEMA = {} ORDER BY TABLE_NAME;",
        lit(Some(db))
    )
}

fn columns_sql(db: &str, t: &str) -> String {
    format!(
        "SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_KEY, COLUMN_DEFAULT, EXTRA FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = {} AND TABLE_NAME = {} ORDER BY ORDINAL_POSITION;",
        lit(Some(db)),
        lit(Some(t))
    )
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
pub fn page_sql(db: &str, t: &str, q: &PageQuery) -> String {
    let tr_ = table_ref(db, t);
    let w = q
        .search
        .as_ref()
        .filter(|(c, v)| !c.is_empty() && !v.is_empty())
        .map(|(c, v)| {
            let pat = lit(Some(&format!("%{v}%")));
            let or = c.iter().map(|c| format!("CAST({} AS CHAR) LIKE {pat}", ident(c))).collect::<Vec<_>>().join(" OR ");
            format!(" WHERE ({or})")
        })
        .unwrap_or_default();
    let o = q.sort.as_ref().map(|(c, desc)| format!(" ORDER BY {}{}", ident(c), if *desc { " DESC" } else { "" })).unwrap_or_default();
    format!(
        "{}\nSELECT COUNT(*) FROM {tr_}{w};\nSELECT * FROM {tr_}{w}{o} LIMIT {} OFFSET {};",
        columns_sql(db, t),
        q.limit,
        q.offset
    )
}

/// Identifies one row: by primary key when there is one, otherwise by all its values (`<=>`
/// matches NULLs), always with `LIMIT 1` on the statement.
pub fn where_row(cols: &[Col], row: &Row) -> String {
    let pk: Vec<usize> = cols.iter().enumerate().filter(|(_, c)| c.key == "PRI").map(|(i, _)| i).collect();
    let (idx, op) = if !pk.is_empty() && pk.iter().all(|&i| row.get(i).is_some_and(Option::is_some)) {
        (pk, "=")
    } else {
        ((0..cols.len().min(row.len())).collect(), "<=>")
    };
    idx.iter().map(|&i| format!("{} {op} {}", ident(&cols[i].name), lit(row[i].as_deref()))).collect::<Vec<_>>().join(" AND ")
}

pub fn has_pk(cols: &[Col]) -> bool {
    cols.iter().any(|c| c.key == "PRI")
}

pub fn update_sql(db: &str, t: &str, cols: &[Col], row: &Row, changes: &[(usize, Option<String>)]) -> String {
    let set = changes.iter().map(|(i, v)| format!("{} = {}", ident(&cols[*i].name), lit(v.as_deref()))).collect::<Vec<_>>().join(", ");
    format!("UPDATE {} SET {set} WHERE {} LIMIT 1;\nSELECT ROW_COUNT();", table_ref(db, t), where_row(cols, row))
}

pub fn delete_sql(db: &str, t: &str, cols: &[Col], row: &Row) -> String {
    format!("DELETE FROM {} WHERE {} LIMIT 1;\nSELECT ROW_COUNT();", table_ref(db, t), where_row(cols, row))
}

pub fn insert_sql(db: &str, t: &str, vals: &[(String, Option<String>)]) -> String {
    let names = vals.iter().map(|(c, _)| ident(c)).collect::<Vec<_>>().join(", ");
    let values = vals.iter().map(|(_, v)| lit(v.as_deref())).collect::<Vec<_>>().join(", ");
    format!("INSERT INTO {} ({names}) VALUES ({values});\nSELECT ROW_COUNT();", table_ref(db, t))
}

fn err_text(r: &sshos_shared::ExecResult) -> String {
    let e: Vec<&str> = r.stderr.lines().filter(|l| !l.contains("[Warning]") && !l.trim().is_empty()).collect();
    if e.is_empty() { r.stdout.trim().to_string() } else { e.join("\n") }
}

/// Runs SQL in the container and parses the XML result sets.
fn run_sql(cx: Ctx, host: String, c: &Container, sql: &str, quiet: bool, f: impl FnOnce(Result<Vec<ResultSet>, String>) + 'static) {
    let cmd = xml_command(c, sql);
    spawn_local(async move {
        match exec(&host, &cmd, quiet).await {
            Ok(r) if r.ok() => f(Ok(parse_xml(&r.stdout))),
            Ok(r) => f(Err(err_text(&r))),
            Err(e) => {
                cx.handle_err(Some(&host), e);
                f(Err(String::new()));
            }
        }
    });
}

fn short(s: &str) -> String {
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

/// (name, rows estimate, size KiB, is a view)
type TableInfo = (String, Option<String>, Option<String>, bool);

#[component]
pub fn MysqlBrowser(host: String, container: Container) -> impl IntoView {
    // Captured once: loads are chained from async callbacks, where `ctx()` is unavailable.
    let cx = ctx();
    let default_db = container.default_db();
    let host = StoredValue::new(host);
    let cont = StoredValue::new(container);
    let dbs = RwSignal::new(None::<Vec<String>>);
    let db = RwSignal::new(None::<String>);
    let tables = RwSignal::new(None::<Vec<TableInfo>>);
    let table = RwSignal::new(None::<String>);
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
        let (Some(d), Some(t)) = (db.try_get_untracked().flatten(), table.try_get_untracked().flatten()) else { return };
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
        run_sql(cx, hid, &c, &page_sql(&d, &t, &q), true, move |r| match r {
            Ok(sets) if sets.len() >= 3 => {
                let _ = busy.try_set(false);
                let _ = error.try_set(None);
                let _ = cols.try_set(cols_from(&sets[0]));
                let n = sets[1].rows.first().and_then(|r| r.first().cloned().flatten()).and_then(|n| n.parse().ok()).unwrap_or(0);
                let _ = total.try_set(n);
                let _ = rows.try_set(Some(sets[2].rows.clone()));
            }
            Ok(_) => fail(tr("Réponse inattendue du serveur MySQL.", "Unexpected answer from the MySQL server.").into()),
            Err(e) => fail(e),
        });
    };

    let open_table = move |t: String| {
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
        run_sql(cx, hid, &c, &tables_sql(&d), true, move |r| match r {
            Ok(sets) => {
                let _ = error.try_set(None);
                let list = sets
                    .first()
                    .map(|s| {
                        s.rows
                            .iter()
                            .map(|r| {
                                let g = |i: usize| r.get(i).cloned().flatten();
                                (g(0).unwrap_or_default(), g(1), g(2), g(3).is_some_and(|t| t.contains("VIEW")))
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
        run_sql(cx, hid, &c, &dbs_sql(), true, move |r| match r {
            Ok(sets) => {
                let list: Vec<String> = sets.first().map(|s| s.rows.iter().filter_map(|r| r.first().cloned().flatten()).collect()).unwrap_or_default();
                let keep = db.try_get_untracked().flatten().filter(|d| list.contains(d));
                let pick = keep
                    .or_else(|| default_db.filter(|d| list.contains(d)))
                    .or_else(|| list.iter().find(|d| !SYSTEM_DBS.contains(&d.as_str())).cloned())
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
    let write = move |title: &'static str, sql: String, danger: bool| {
        let (Some(hid), Some(c)) = (host.try_get_value(), cont.try_get_value()) else { return };
        let cmd = xml_command(&c, &sql);
        let mut d = Dialog::confirm(title, tr("Cette commande va être exécutée sur le serveur :", "This command will run on the server:"), tr("Exécuter", "Run"), move || {
            busy.set(true);
            run_sql(cx, hid.clone(), &c, &sql, false, move |r| match r {
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
        let (Some(d), Some(t)) = (db.get_untracked(), table.get_untracked()) else { return };
        let vals: Vec<Option<String>> = e.vals.iter().zip(&e.nulls).map(|(v, n)| if n.get_untracked() { None } else { Some(v.get_untracked()) }).collect();
        match &e.orig {
            Some(orig) => {
                let changes: Vec<(usize, Option<String>)> = vals.into_iter().enumerate().filter(|(i, v)| orig.get(*i) != Some(v)).collect();
                if changes.is_empty() {
                    edit.set(None);
                } else {
                    write(tr("Modifier la ligne", "Update row"), update_sql(&d, &t, &cs, orig, &changes), false);
                }
            }
            None => {
                // Empty fields are left out: the column default (or AUTO_INCREMENT) applies.
                let pairs: Vec<(String, Option<String>)> = cs.iter().zip(vals).filter(|(_, v)| v.as_deref() != Some("")).map(|(c, v)| (c.name.clone(), v)).collect();
                write(tr("Insérer une ligne", "Insert row"), insert_sql(&d, &t, &pairs), false);
            }
        }
    };

    let delete = move |row: Row| {
        let cs = cols.get_untracked();
        let (Some(d), Some(t)) = (db.get_untracked(), table.get_untracked()) else { return };
        write(tr("Supprimer la ligne", "Delete row"), delete_sql(&d, &t, &cs, &row), true);
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
        tables.get()?.into_iter().find(|x| x.0 == t).map(|x| x.3)
    };

    let tables_list = move || match tables.get() {
        None => view! { <div class="empty muted small"><span class="spinner"></span></div> }.into_any(),
        Some(v) if v.is_empty() => view! { <p class="muted small hint">{tr("Aucune table.", "No table.")}</p> }.into_any(),
        Some(v) => v
            .into_iter()
            .map(|(name, n, kb, view_)| {
                let (n1, n2) = (name.clone(), name.clone());
                let meta = if view_ { tr("vue", "view").to_string() } else { n.unwrap_or_default() };
                let title = kb.map(|k| format!("{name} · {k} KiB")).unwrap_or_else(|| name.clone());
                view! {
                    <button class="dbx-table" class:active=move || table.get().as_deref() == Some(n1.as_str()) title=title on:click=move |_| open_table(n2.clone())>
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
                        if c.extra.contains("auto_increment") {
                            "AUTO_INCREMENT".to_string()
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
                            {table.get_untracked().unwrap_or_default()}
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
                                let sys = SYSTEM_DBS.contains(&d.as_str());
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
                            <h3 class="ellipsis">{t}</h3>
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
    fn literals_and_statements() {
        assert_eq!(ident("we`ird"), "`we``ird`");
        assert_eq!(lit(None), "NULL");
        assert_eq!(lit(Some("héllo 42")), "'héllo 42'");
        assert_eq!(lit(Some("it's")), "CONVERT(X'69742773' USING utf8mb4)");
        assert_eq!(lit(Some("a\nSSHOS_SQL")), "CONVERT(X'610A5353484F535F53514C' USING utf8mb4)");
        let cols = vec![col("id", "PRI"), col("name", ""), col("bio", "")];
        let row: Row = vec![Some("7".into()), Some("Ann".into()), None];
        assert_eq!(
            update_sql("shop", "users", &cols, &row, &[(1, Some("Bob".into())), (2, None)]),
            "UPDATE `shop`.`users` SET `name` = 'Bob', `bio` = NULL WHERE `id` = '7' LIMIT 1;\nSELECT ROW_COUNT();"
        );
        assert_eq!(delete_sql("shop", "users", &cols, &row), "DELETE FROM `shop`.`users` WHERE `id` = '7' LIMIT 1;\nSELECT ROW_COUNT();");
        // Without a primary key: every column, NULL-safe.
        let nopk = vec![col("a", ""), col("b", "")];
        assert_eq!(where_row(&nopk, &vec![Some("1".into()), None]), "`a` <=> '1' AND `b` <=> NULL");
        assert_eq!(
            insert_sql("shop", "users", &[("name".into(), Some("Zoé".into())), ("bio".into(), None)]),
            "INSERT INTO `shop`.`users` (`name`, `bio`) VALUES ('Zoé', NULL);\nSELECT ROW_COUNT();"
        );
        let q = PageQuery { sort: Some(("name".into(), true)), search: Some((vec!["name".into(), "bio".into()], "o'k".into())), limit: 50, offset: 100 };
        let sql = page_sql("shop", "users", &q);
        assert!(sql.ends_with("SELECT * FROM `shop`.`users` WHERE (CAST(`name` AS CHAR) LIKE CONVERT(X'256F276B25' USING utf8mb4) OR CAST(`bio` AS CHAR) LIKE CONVERT(X'256F276B25' USING utf8mb4)) ORDER BY `name` DESC LIMIT 50 OFFSET 100;"));
        assert_eq!(sql.matches(';').count(), 3);
    }
}
