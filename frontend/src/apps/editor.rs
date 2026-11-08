//! Editor: CodeMirror on a remote file. Nginx files are saved through a validated plan
//! (`nginx -t` + reload, backup restored on failure).

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::json;
use sshos_shared::{FileContent, NginxPlan, WriteResult};
use wasm_bindgen::prelude::*;

use crate::apps::nginx::apply_plan;
use crate::icons::Icon;
use crate::state::*;
use crate::tauri::{editor, invoke};

#[component]
pub fn Editor(host: String, path: String, nginx: bool, title: RwSignal<String>) -> impl IntoView {
    let c = ctx();
    let el = NodeRef::<leptos::html::Div>::new();
    let ed = StoredValue::new(None::<u32>);
    let content = RwSignal::new(None::<FileContent>);
    let error = RwSignal::new(None::<String>);
    let dirty = RwSignal::new(false);
    let saving = RwSignal::new(false);
    let host = StoredValue::new(host);
    let path = StoredValue::new(path);
    let base_title = title.get_untracked();

    let load = move || {
        spawn_local(async move {
            match invoke::<FileContent>("file_read", json!({ "hostId": host.get_value(), "path": path.get_value() })).await {
                Ok(f) => {
                    if let Some(id) = ed.get_value() {
                        editor::set_text(id, &f.text);
                    }
                    content.set(Some(f));
                    dirty.set(false);
                    error.set(None);
                }
                Err(e) => error.set(Some(e.message())),
            }
        });
    };
    load();

    let save = move || {
        let Some(id) = ed.get_value() else { return };
        if saving.get_untracked() || content.with_untracked(|f| f.as_ref().map(|f| f.binary).unwrap_or(true)) {
            return;
        }
        let text = editor::get_text(id);
        let (h, p) = (host.get_value(), path.get_value());
        saving.set(true);
        spawn_local(async move {
            if nginx {
                match invoke::<NginxPlan>("nginx_plan_save", json!({ "hostId": h, "path": p, "text": text })).await {
                    Ok(plan) => apply_plan(c, h, plan, Callback::new(move |()| dirty.set(false))),
                    Err(e) => c.handle_err(Some(&h), e),
                }
            } else {
                match invoke::<WriteResult>("file_write", json!({ "hostId": h, "path": p, "text": text })).await {
                    Ok(r) => {
                        dirty.set(false);
                        match r.backup {
                            Some(b) => c.ok(format!("{} ({} {b})", tr("Enregistré", "Saved"), tr("sauvegarde", "backup"))),
                            None => c.ok(tr("Enregistré", "Saved")),
                        }
                    }
                    Err(e) => c.handle_err(Some(&h), e),
                }
            }
            saving.set(false);
        });
    };

    // Create CodeMirror once both the element and the text are there.
    Effect::new(move |_| {
        let (Some(div), Some(f)) = (el.get(), content.get()) else { return };
        if ed.get_value().is_some() {
            return;
        }
        let on_change = Closure::<dyn FnMut()>::new(move || {
            let _ = dirty.try_set(true);
        });
        let on_save = Closure::<dyn FnMut()>::new(move || save());
        let elem: &web_sys::Element = div.as_ref();
        let id = editor::create(
            elem,
            &f.text,
            &path.get_value(),
            c.dark.get_untracked(),
            on_change.into_js_value().unchecked_ref(),
            on_save.into_js_value().unchecked_ref(),
        );
        ed.set_value(Some(id));
        editor::focus(id);
    });
    Effect::new(move |_| {
        let d = c.dark.get();
        if let Some(id) = ed.get_value() {
            editor::set_dark(id, d);
        }
    });
    Effect::new(move |_| {
        title.set(if dirty.get() { format!("● {base_title}") } else { base_title.clone() });
    });
    on_cleanup(move || {
        if let Some(id) = ed.get_value() {
            editor::dispose(id);
        }
    });

    let reload = move |_| {
        if dirty.get_untracked() {
            c.show(Dialog::confirm(
                tr("Recharger", "Reload"),
                tr("Abandonner les modifications non enregistrées ?", "Discard unsaved changes?"),
                tr("Recharger", "Reload"),
                load,
            ));
        } else {
            load();
        }
    };

    app_menus(vec![MenuSection::new(
        tr("Fichier", "File"),
        vec![MenuItem::new(tr("Enregistrer", "Save"), move || save()).key("Ctrl+S").disabled(move || !dirty.get() || saving.get())],
    )]);

    view! {
        <div class="editor">
            <div class="toolbar">
                <span class="mono small ellipsis grow">{path.get_value()}</span>
                <Show when=move || nginx>
                    <span class="badge native" title=tr(
                        "Enregistrement validé par nginx -t puis rechargement",
                        "Saved through nginx -t then reload",
                    )>"nginx"</span>
                </Show>
                <Show when=move || content.with(|f| f.as_ref().map(|f| f.binary).unwrap_or(false))>
                    <span class="badge warn">{tr("binaire — lecture seule", "binary — read-only")}</span>
                </Show>
                <button on:click=reload><Icon name="refresh" size=14 /></button>
                <button class="primary" disabled=move || !dirty.get() || saving.get() on:click=move |_| save()>
                    <Icon name="save" size=14 />
                    {move || if saving.get() { tr("Enregistrement…", "Saving…") } else { tr("Enregistrer", "Save") }}
                </button>
            </div>
            {move || error.get().map(|e| view! { <div class="error-banner">{e}</div> })}
            <div class="editor-host" node_ref=el></div>
        </div>
    }
}
