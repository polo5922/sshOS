//! MongoDB explorer: databases, collections, paginated documents (sort, text search or query
//! filter), indexes, and document editing as Extended JSON (replace, insert, delete).
//!
//! Scripts run with `mongosh --eval` through `docker exec`, logged in with the container's own
//! Docker config (never seen by sshOs). Every value sent (names, documents, filters) is embedded
//! as a JSON string literal, so it can't break out of the script nor out of the heredoc. Results
//! come back as one line of JSON after a marker. Every write is confirmed with the exact command.

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::Value;

use crate::apps::databases::{explorer_command, Container};
use crate::apps::dbbrowser::{err_text, short};
use crate::icons::Icon;
use crate::state::*;
use crate::tauri::exec;

pub const MARK: &str = "@@SSHOS@@";
pub const SYSTEM_DBS: [&str; 3] = ["admin", "config", "local"];

/// JavaScript string literal (JSON-escaped: never spans several lines).
pub fn js(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

const OUT: &str = "const out = v => print('@@SSHOS@@' + EJSON.stringify(v, { relaxed: true }));";

fn coll(db: &str, c: &str) -> String {
    format!("const c = db.getSiblingDB({}).getCollection({});", js(db), js(c))
}

pub fn dbs_js() -> String {
    format!("{OUT}\nout(db.adminCommand({{ listDatabases: 1, nameOnly: true }}).databases.map(d => d.name).sort());")
}

/// `[[name, type, estimated count | null], …]`
pub fn colls_js(db: &str) -> String {
    format!(
        "{OUT}\nconst d = db.getSiblingDB({});\nout(d.getCollectionInfos().filter(i => !i.name.startsWith('system.')).map(i => [i.name, i.type || 'collection', i.type === 'view' ? null : d.getCollection(i.name).estimatedDocumentCount()]).sort((a, b) => a[0] < b[0] ? -1 : 1));",
        js(db)
    )
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DocQuery {
    /// Plain text (case-insensitive match on the given fields) or a filter `{ … }`.
    pub search: String,
    pub fields: Vec<String>,
    pub sort: Option<(String, bool)>,
    pub limit: u32,
    pub offset: u32,
}

/// `{ total, cols, rows, ids, docs }`: top-level fields of the page, cell texts (`null` when the
/// field is missing), each `_id` as canonical Extended JSON, each document as indented JSON.
pub fn page_js(db: &str, c: &str, q: &DocQuery) -> String {
    let fields = serde_json::to_string(&q.fields).unwrap_or_else(|_| "[]".into());
    let sort = q.sort.as_ref().map(|(f, desc)| format!("cur = cur.sort({{ [{}]: {} }});", js(f), if *desc { -1 } else { 1 })).unwrap_or_default();
    format!(
        r#"{OUT}
{}
const t = {}.trim(), fields = {fields};
let f = {{}};
if (t.startsWith('{{')) f = eval('(' + t + ')');
else if (t && fields.length) {{ const re = {{ $regex: t.replace(/[.*+?^${{}}()|[\]\\]/g, '\\$&'), $options: 'i' }}; f = {{ $or: fields.map(k => ({{ [k]: re }})) }}; }}
const total = c.countDocuments(f);
let cur = c.find(f);
{sort}
const docs = cur.skip({}).limit({}).toArray();
const cols = [];
for (const d of docs) for (const k of Object.keys(d)) if (!cols.includes(k)) cols.push(k);
const cell = v => typeof v === 'string' ? v : EJSON.stringify(v, {{ relaxed: true }});
out({{ total, cols, rows: docs.map(d => cols.map(k => k in d ? cell(d[k]) : null)), ids: docs.map(d => EJSON.stringify(d._id, {{ relaxed: false }})), docs: docs.map(d => EJSON.stringify(d, null, 2, {{ relaxed: true }})) }});"#,
        coll(db, c),
        js(&q.search),
        q.offset,
        q.limit
    )
}

/// `[[name, key, unique], …]`
pub fn indexes_js(db: &str, c: &str) -> String {
    format!("{OUT}\n{}\nout(c.getIndexes().map(i => [i.name, EJSON.stringify(i.key), !!i.unique]));", coll(db, c))
}

pub fn replace_js(db: &str, c: &str, id: &str, doc: &str) -> String {
    format!("{OUT}\n{}\nconst r = c.replaceOne({{ _id: EJSON.parse({}) }}, EJSON.parse({}));\nout({{ n: r.modifiedCount, matched: r.matchedCount }});", coll(db, c), js(id), js(doc))
}

pub fn insert_js(db: &str, c: &str, doc: &str) -> String {
    format!("{OUT}\n{}\nconst r = c.insertOne(EJSON.parse({}));\nout({{ n: r.acknowledged ? 1 : 0 }});", coll(db, c), js(doc))
}

pub fn delete_js(db: &str, c: &str, id: &str) -> String {
    format!("{OUT}\n{}\nconst r = c.deleteOne({{ _id: EJSON.parse({}) }});\nout({{ n: r.deletedCount }});", coll(db, c), js(id))
}

/// The JSON after the marker line.
pub fn parse_out(stdout: &str) -> Option<Value> {
    stdout.lines().find_map(|l| l.strip_prefix(MARK)).and_then(|j| serde_json::from_str(j).ok())
}

fn run_js(cx: Ctx, host: String, c: &Container, script: &str, quiet: bool, f: impl FnOnce(Result<Value, String>) + 'static) {
    let cmd = explorer_command(c, None, script);
    spawn_local(async move {
        match exec(&host, &cmd, quiet).await {
            Ok(r) if r.ok() => match parse_out(&r.stdout) {
                Some(v) => f(Ok(v)),
                None => f(Err(err_text(&r))),
            },
            Ok(r) => f(Err(err_text(&r))),
            Err(e) => {
                cx.handle_err(Some(&host), e);
                f(Err(String::new()));
            }
        }
    });
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Page {
    total: u64,
    cols: Vec<String>,
    rows: Vec<Vec<Option<String>>>,
    ids: Vec<String>,
    docs: Vec<String>,
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().map(|x| x.as_str().unwrap_or_default().to_string()).collect()).unwrap_or_default()
}

fn page_from(v: &Value) -> Page {
    Page {
        total: v["total"].as_u64().unwrap_or(0),
        cols: strings(&v["cols"]),
        rows: v["rows"].as_array().map(|a| a.iter().map(|r| r.as_array().map(|r| r.iter().map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()).collect()).unwrap_or_default(),
        ids: strings(&v["ids"]),
        docs: strings(&v["docs"]),
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Pane {
    Docs,
    Indexes,
}

/// (id of the document edited — `None` when inserting —, text)
type Edit = (Option<String>, RwSignal<String>);

#[component]
pub fn MongoBrowser(host: String, container: Container) -> impl IntoView {
    // Captured once: loads are chained from async callbacks, where `ctx()` is unavailable.
    let cx = ctx();
    let default_db = container.default_db();
    let host = StoredValue::new(host);
    let cont = StoredValue::new(container);
    let dbs = RwSignal::new(None::<Vec<String>>);
    let db = RwSignal::new(None::<String>);
    // (name, is a view, estimated count)
    let colls = RwSignal::new(None::<Vec<(String, bool, Option<u64>)>>);
    let coll = RwSignal::new(None::<String>);
    let pane = RwSignal::new(Pane::Docs);
    let page = RwSignal::new(None::<Page>);
    let indexes = RwSignal::new(None::<Vec<(String, String, bool)>>);
    let offset = RwSignal::new(0u32);
    let limit = RwSignal::new(50u32);
    let sort = RwSignal::new(None::<(String, bool)>);
    let search = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let edit = RwSignal::new(None::<Edit>);

    let fail = move |e: String| {
        let _ = busy.try_set(false);
        if !e.is_empty() {
            let _ = error.try_set(Some(e));
        }
    };

    let load_page = move || {
        let (Some(hid), Some(c)) = (host.try_get_value(), cont.try_get_value()) else { return };
        let (Some(d), Some(cl)) = (db.try_get_untracked().flatten(), coll.try_get_untracked().flatten()) else { return };
        let q = DocQuery {
            search: search.try_get_untracked().unwrap_or_default(),
            // Text search: on the fields seen on the previous page.
            fields: page.try_get_untracked().flatten().map(|p| p.cols).unwrap_or_default(),
            sort: sort.try_get_untracked().flatten(),
            limit: limit.try_get_untracked().unwrap_or(50),
            offset: offset.try_get_untracked().unwrap_or(0),
        };
        let _ = busy.try_set(true);
        run_js(cx, hid, &c, &page_js(&d, &cl, &q), true, move |r| match r {
            Ok(v) => {
                let _ = busy.try_set(false);
                let _ = error.try_set(None);
                let _ = page.try_set(Some(page_from(&v)));
            }
            Err(e) => fail(e),
        });
    };

    let load_indexes = move || {
        let (Some(hid), Some(c)) = (host.try_get_value(), cont.try_get_value()) else { return };
        let (Some(d), Some(cl)) = (db.try_get_untracked().flatten(), coll.try_get_untracked().flatten()) else { return };
        let _ = indexes.try_set(None);
        run_js(cx, hid, &c, &indexes_js(&d, &cl), true, move |r| match r {
            Ok(v) => {
                let list = v
                    .as_array()
                    .map(|a| a.iter().map(|i| (i[0].as_str().unwrap_or_default().to_string(), i[1].as_str().unwrap_or_default().to_string(), i[2].as_bool().unwrap_or(false))).collect())
                    .unwrap_or_default();
                let _ = indexes.try_set(Some(list));
            }
            Err(e) => {
                let _ = indexes.try_set(Some(Vec::new()));
                fail(e)
            }
        });
    };

    let open_coll = move |name: String| {
        coll.set(Some(name));
        page.set(None);
        indexes.set(None);
        offset.set(0);
        sort.set(None);
        search.set(String::new());
        if pane.get_untracked() == Pane::Indexes {
            load_indexes();
        }
        load_page();
    };

    let load_colls = move || {
        let (Some(hid), Some(c)) = (host.try_get_value(), cont.try_get_value()) else { return };
        let Some(d) = db.try_get_untracked().flatten() else { return };
        let _ = colls.try_set(None);
        let _ = coll.try_set(None);
        let _ = page.try_set(None);
        run_js(cx, hid, &c, &colls_js(&d), true, move |r| match r {
            Ok(v) => {
                let _ = error.try_set(None);
                let list = v
                    .as_array()
                    .map(|a| a.iter().map(|i| (i[0].as_str().unwrap_or_default().to_string(), i[1].as_str() == Some("view"), i[2].as_u64())).collect())
                    .unwrap_or_default();
                let _ = colls.try_set(Some(list));
            }
            Err(e) => {
                let _ = colls.try_set(Some(Vec::new()));
                fail(e)
            }
        });
    };

    let load_dbs = move || {
        let (Some(hid), Some(c)) = (host.try_get_value(), cont.try_get_value()) else { return };
        let default_db = default_db.clone();
        run_js(cx, hid, &c, &dbs_js(), true, move |r| match r {
            Ok(v) => {
                let list = strings(&v);
                let keep = db.try_get_untracked().flatten().filter(|d| list.contains(d));
                let pick = keep
                    .or_else(|| default_db.filter(|d| list.contains(d)))
                    .or_else(|| list.iter().find(|d| !SYSTEM_DBS.contains(&d.as_str())).cloned())
                    .or_else(|| list.first().cloned());
                let _ = dbs.try_set(Some(list));
                let _ = db.try_set(pick);
                load_colls();
            }
            Err(e) => {
                let _ = dbs.try_set(Some(Vec::new()));
                fail(e)
            }
        });
    };
    load_dbs();

    // Davish rule: every write shows the exact command and waits for confirmation.
    let write = move |title: &'static str, script: String, danger: bool| {
        let (Some(hid), Some(c)) = (host.try_get_value(), cont.try_get_value()) else { return };
        let cmd = explorer_command(&c, None, &script);
        let mut d = Dialog::confirm(title, tr("Cette commande va être exécutée sur le serveur :", "This command will run on the server:"), tr("Exécuter", "Run"), move || {
            busy.set(true);
            run_js(cx, hid.clone(), &c, &script, false, move |r| match r {
                Ok(v) => {
                    let n = v["n"].as_u64().unwrap_or(0);
                    if n == 0 && v["matched"].as_u64() != Some(1) {
                        cx.toast(ToastKind::Error, tr("Aucun document modifié (il a peut-être changé).", "No document changed (it may have changed)."));
                    } else {
                        cx.ok(format!("{} {n}", tr("Documents affectés :", "Documents affected:")));
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

    let save = move || {
        let Some((id, text)) = edit.get_untracked() else { return };
        let (Some(d), Some(cl)) = (db.get_untracked(), coll.get_untracked()) else { return };
        let doc = text.get_untracked();
        if let Err(e) = serde_json::from_str::<Value>(&doc) {
            cx.toast(ToastKind::Error, format!("{} {e}", tr("JSON invalide :", "Invalid JSON:")));
            return;
        }
        match id {
            Some(id) => write(tr("Remplacer le document", "Replace document"), replace_js(&d, &cl, &id, &doc), false),
            None => write(tr("Insérer un document", "Insert document"), insert_js(&d, &cl, &doc), false),
        }
    };

    let delete = move |id: String| {
        let (Some(d), Some(cl)) = (db.get_untracked(), coll.get_untracked()) else { return };
        write(tr("Supprimer le document", "Delete document"), delete_js(&d, &cl, &id), true);
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
        let c = coll.get()?;
        colls.get()?.into_iter().find(|x| x.0 == c).map(|x| x.1)
    };

    let colls_list = move || match colls.get() {
        None => view! { <div class="empty muted small"><span class="spinner"></span></div> }.into_any(),
        Some(v) if v.is_empty() => view! { <p class="muted small hint">{tr("Aucune collection.", "No collection.")}</p> }.into_any(),
        Some(v) => v
            .into_iter()
            .map(|(name, view_, n)| {
                let (n1, n2) = (name.clone(), name.clone());
                let meta = if view_ { tr("vue", "view").to_string() } else { n.map(|n| n.to_string()).unwrap_or_default() };
                view! {
                    <button class="dbx-table" class:active=move || coll.get().as_deref() == Some(n1.as_str()) on:click=move |_| open_coll(n2.clone())>
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
        let Some(p) = page.get() else {
            return if busy.get() { view! { <div class="empty muted"><span class="spinner"></span>{tr("Chargement…", "Loading…")}</div> }.into_any() } else { ().into_any() };
        };
        let editable = is_view() == Some(false);
        let head = p
            .cols
            .iter()
            .map(|c| {
                let (n, n2) = (c.clone(), c.clone());
                view! {
                    <th class="sortable" on:click=move |_| sort_by(n.clone())>
                        {(c == "_id").then(|| view! { <Icon name="key" size=11 /> })}
                        {c.clone()}
                        {move || match sort.get() {
                            Some((s, d)) if s == n2 => if d { " ▼" } else { " ▲" },
                            _ => "",
                        }}
                    </th>
                }
            })
            .collect_view();
        let body = p
            .rows
            .into_iter()
            .enumerate()
            .map(|(i, r)| {
                let id = p.ids.get(i).cloned().unwrap_or_default();
                let doc = p.docs.get(i).cloned().unwrap_or_default();
                let (id1, id2, id3) = (id.clone(), id.clone(), id);
                let (d1, d2) = (doc.clone(), doc);
                view! {
                    <tr on:dblclick=move |_| if editable { edit.set(Some((Some(id1.clone()), RwSignal::new(d1.clone())))) }>
                        <td class="db-actions">
                            <Show when=move || editable>
                                <button class="icon-btn tiny-btn" title=tr("Modifier", "Edit") on:click={
                                    let (id, d) = (id2.clone(), d2.clone());
                                    move |_| edit.set(Some((Some(id.clone()), RwSignal::new(d.clone()))))
                                }><Icon name="edit" size=13 /></button>
                                <button class="icon-btn tiny-btn danger" title=tr("Supprimer", "Delete") on:click={
                                    let id = id3.clone();
                                    move |_| delete(id.clone())
                                }><Icon name="trash" size=13 /></button>
                            </Show>
                        </td>
                        {r.into_iter()
                            .map(|v| match v {
                                None => view! { <td class="null">"—"</td> }.into_any(),
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

    let index_view = move || match indexes.get() {
        None => view! { <div class="empty muted"><span class="spinner"></span></div> }.into_any(),
        Some(list) => view! {
            <div class="table-wrap db-grid">
                <table class="table data-table">
                    <thead>
                        <tr>
                            <th>{tr("Nom", "Name")}</th>
                            <th>{tr("Clé", "Key")}</th>
                            <th>{tr("Unique", "Unique")}</th>
                        </tr>
                    </thead>
                    <tbody>
                        {list.into_iter()
                            .map(|(name, key, unique)| view! {
                                <tr>
                                    <td><b>{name}</b></td>
                                    <td class="mono">{key}</td>
                                    <td>{if unique { tr("oui", "yes") } else { tr("non", "no") }}</td>
                                </tr>
                            })
                            .collect_view()}
                    </tbody>
                </table>
            </div>
        }
        .into_any(),
    };

    let pager = move || {
        let p = page.get().unwrap_or_default();
        let n = p.rows.len() as u64;
        let (off, lim, tot) = (offset.get() as u64, limit.get() as u64, p.total);
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
        edit.get().map(|(id, text)| {
            let inserting = id.is_none();
            view! {
                <div class="sheet-overlay" on:click=move |_| edit.set(None)>
                    <div class="sheet wide" on:click=|ev| ev.stop_propagation()>
                        <button class="icon-btn sheet-close" on:click=move |_| edit.set(None)><Icon name="x" /></button>
                        <h3>
                            {if inserting { tr("Insérer un document", "Insert document") } else { tr("Modifier le document", "Edit document") }}
                            " · "
                            {coll.get_untracked().unwrap_or_default()}
                        </h3>
                        <div class="form db-edit">
                            <textarea class="mono db-doc" rows="18" spellcheck="false" bind:value=text></textarea>
                            <p class="muted small">
                                {tr(
                                    "JSON étendu (EJSON) : {\"$oid\": …}, {\"$date\": …}, {\"$numberLong\": …}. Le document entier est remplacé.",
                                    "Extended JSON (EJSON): {\"$oid\": …}, {\"$date\": …}, {\"$numberLong\": …}. The whole document is replaced.",
                                )}
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
                    load_colls();
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
                <div class="dbx-tables">{colls_list}</div>
            </aside>
            <div class="dbx-main">
                {move || error.get().map(|e| view! {
                    <div class="banner crit">
                        <pre class="grow db-error">{e}</pre>
                        <button class="icon-btn" on:click=move |_| error.set(None)><Icon name="x" size=13 /></button>
                    </div>
                })}
                {move || match coll.get() {
                    None => crate::apps::kit::empty_view(tr("Choisissez une collection.", "Pick a collection.")).into_any(),
                    Some(cl) => view! {
                        <div class="dbx-bar">
                            <h3 class="ellipsis">{cl}</h3>
                            <div class="seg">
                                <button class:on=move || pane.get() == Pane::Docs on:click=move |_| pane.set(Pane::Docs)>{tr("Documents", "Documents")}</button>
                                <button class:on=move || pane.get() == Pane::Indexes on:click=move |_| {
                                    pane.set(Pane::Indexes);
                                    if indexes.get_untracked().is_none() {
                                        load_indexes();
                                    }
                                }>{tr("Index", "Indexes")}</button>
                            </div>
                            <span class="grow"></span>
                            <Show when=move || pane.get() == Pane::Docs>
                                <label class="search-field db-filter">
                                    <Icon name="search" size=13 />
                                    <input class="mono" placeholder=tr("Texte, ou filtre { champ: valeur }", "Text, or { field: value } filter") bind:value=search on:keydown=move |e| {
                                        if e.key() == "Enter" {
                                            offset.set(0);
                                            load_page();
                                        }
                                    } />
                                </label>
                                <button class="primary small" disabled=move || is_view() != Some(false) on:click=move |_| edit.set(Some((None, RwSignal::new("{\n  \n}".to_string()))))>
                                    <Icon name="plus" size=13 />
                                    {tr("Insérer", "Insert")}
                                </button>
                            </Show>
                            <button class="icon-btn" class:spin=move || busy.get() title=tr("Actualiser", "Refresh") on:click=move |_| {
                                if pane.get_untracked() == Pane::Indexes { load_indexes() } else { load_page() }
                            }>
                                <Icon name="refresh" size=14 />
                            </button>
                        </div>
                        {move || if pane.get() == Pane::Docs { view! { {grid} {pager} }.into_any() } else { index_view().into_any() }}
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

    #[test]
    fn scripts_embed_values_as_literals() {
        assert_eq!(js("a\"b\nSSHOS_SQL"), "\"a\\\"b\\nSSHOS_SQL\"");
        let s = replace_js("app", "users", "{\"$oid\":\"65a\"}", "{\n  \"name\": \"O'Neil\"\n}");
        assert!(s.contains("db.getSiblingDB(\"app\").getCollection(\"users\")"));
        assert!(s.contains("c.replaceOne({ _id: EJSON.parse(\"{\\\"$oid\\\":\\\"65a\\\"}\") }, EJSON.parse(\"{\\n  \\\"name\\\": \\\"O'Neil\\\"\\n}\"))"));
        // No value can end the heredoc: everything sent stays on the lines of the script.
        assert!(!s.lines().any(|l| l == "SSHOS_SQL"));
        let q = DocQuery { search: "ann".into(), fields: vec!["_id".into(), "name".into()], sort: Some(("name".into(), true)), limit: 25, offset: 50 };
        let p = page_js("app", "users", &q);
        assert!(p.contains("const t = \"ann\".trim(), fields = [\"_id\",\"name\"];"));
        assert!(p.contains("cur = cur.sort({ [\"name\"]: -1 });"));
        assert!(p.contains("cur.skip(50).limit(25)"));
    }

    #[test]
    fn output() {
        let v = parse_out("Warning: something\n@@SSHOS@@{\"total\":2,\"cols\":[\"_id\",\"name\"],\"rows\":[[\"1\",\"Ann\"],[\"2\",null]],\"ids\":[\"1\",\"2\"],\"docs\":[\"{}\",\"{}\"]}\n").unwrap();
        let p = page_from(&v);
        assert_eq!(p.total, 2);
        assert_eq!(p.cols, ["_id", "name"]);
        assert_eq!(p.rows[1], vec![Some("2".into()), None]);
        assert_eq!(parse_out("nothing"), None);
    }
}
