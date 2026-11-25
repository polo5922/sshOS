//! Image viewer: shows a remote picture (fit, zoom, drag to pan) instead of its raw bytes.

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::json;
use wasm_bindgen::prelude::*;

use crate::icons::Icon;
use crate::state::*;
use crate::tauri::{invoke, invoke_unit, pick_save};

/// MIME type of the pictures the webview can draw, by extension.
pub fn image_mime(path: &str) -> Option<&'static str> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let (_, ext) = name.rsplit_once('.')?;
    Some(match ext.to_ascii_lowercase().as_str() {
        "png" | "apng" => "image/png",
        "jpg" | "jpeg" | "jfif" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "svg" => "image/svg+xml",
        _ => return None,
    })
}

/// Size in bytes of the data behind a base64 string.
fn decoded_len(b64: &str) -> usize {
    b64.len() / 4 * 3 - b64.bytes().rev().take_while(|b| *b == b'=').count()
}

fn num(o: &JsValue, key: &str) -> f64 {
    js_sys::Reflect::get(o, &key.into()).ok().and_then(|v| v.as_f64()).unwrap_or(0.0)
}

#[component]
pub fn ImageView(host: String, path: String, mime: &'static str, on_text: Callback<()>) -> impl IntoView {
    let c = ctx();
    let host = StoredValue::new(host);
    let path = StoredValue::new(path);
    let data = RwSignal::new(None::<String>);
    let bytes = RwSignal::new(0usize);
    let error = RwSignal::new(None::<String>);
    let loading = RwSignal::new(false);
    let natural = RwSignal::new((0.0, 0.0));
    // None = fit to the window.
    let zoom = RwSignal::new(None::<f64>);
    let area = NodeRef::<leptos::html::Div>::new();
    let drag = StoredValue::new(None::<(f64, f64, i32, i32)>);

    // try_*: the window may be closed before the file is there.
    let load = move || {
        loading.set(true);
        let args = json!({ "hostId": host.get_value(), "path": path.get_value() });
        spawn_local(async move {
            let r = invoke::<String>("file_read_base64", args).await;
            let _ = loading.try_set(false);
            match r {
                Ok(b64) => {
                    let _ = bytes.try_set(decoded_len(&b64));
                    let _ = error.try_set(None);
                    let _ = data.try_set(Some(format!("data:{mime};base64,{b64}")));
                }
                Err(e) => {
                    let _ = error.try_set(Some(e.message()));
                }
            }
        });
    };
    load();

    let fit_ratio = move || {
        let (w, h) = natural.get_untracked();
        match area.get_untracked() {
            Some(a) if w > 0.0 && h > 0.0 => (a.client_width() as f64 / w).min(a.client_height() as f64 / h).min(1.0),
            _ => 1.0,
        }
    };
    // Zooming before the size is known would collapse the image.
    let set_zoom = move |z: Option<f64>| {
        if natural.get_untracked().0 > 0.0 {
            zoom.set(z);
        }
    };
    // Zooms around a point of the window (its center by default), which stays in place.
    let scale = move |f: f64, at: Option<(f64, f64)>| {
        let old = zoom.get_untracked().unwrap_or_else(fit_ratio);
        let new = (old * f).clamp(0.05, 16.0);
        set_zoom(Some(new));
        let Some(a) = area.get_untracked() else { return };
        let (w, h) = natural.get_untracked();
        let (cw, ch) = (a.client_width() as f64, a.client_height() as f64);
        let (x, y) = at.unwrap_or((cw / 2.0, ch / 2.0));
        let anchored = |scroll: f64, at: f64, size: f64, view: f64| {
            let margin = |z: f64| ((view - size * z) / 2.0).max(0.0);
            (scroll + at - margin(old)) / old * new + margin(new) - at
        };
        let left = anchored(a.scroll_left() as f64, x, w, cw);
        let top = anchored(a.scroll_top() as f64, y, h, ch);
        // Once the new size is laid out.
        request_animation_frame(move || {
            a.set_scroll_left(left as i32);
            a.set_scroll_top(top as i32);
        });
    };
    let download = move || {
        let (h, p) = (host.get_value(), path.get_value());
        spawn_local(async move {
            let Some(local) = pick_save(p.rsplit('/').next().unwrap_or(&p)).await else { return };
            if let Err(e) = invoke_unit("download", json!({ "hostId": h, "remote": p, "local": local })).await {
                c.handle_err(Some(&h), e);
            }
        });
    };

    let img_style = move || match zoom.get() {
        None => "max-width:100%;max-height:100%".to_string(),
        Some(z) => {
            let (w, h) = natural.get();
            format!("width:{}px;height:{}px", w * z, h * z)
        }
    };

    view! {
        <div class="editor">
            <div class="toolbar">
                <span class="mono small ellipsis grow">{path.get_value()}</span>
                <span class="muted small" style="white-space:nowrap">
                    {move || {
                        let (w, h) = natural.get();
                        (w > 0.0).then(|| format!("{w} × {h} · {}", human_bytes(bytes.get() as f64)))
                    }}
                </span>
                <button class="small" title=tr("Zoom arrière", "Zoom out") on:click=move |_| scale(0.8, None)>"−"</button>
                <button class="small" title=tr("Ajuster à la fenêtre", "Fit to window") on:click=move |_| set_zoom(None)>
                    {move || zoom.get().map(|z| format!("{:.0} %", z * 100.0)).unwrap_or_else(|| tr("Ajusté", "Fitted").to_string())}
                </button>
                <button class="small" title=tr("Zoom avant", "Zoom in") on:click=move |_| scale(1.25, None)>"+"</button>
                <button class="small" title=tr("Taille réelle", "Actual size") on:click=move |_| set_zoom(Some(1.0))>"1:1"</button>
                {(mime == "image/svg+xml").then(|| view! {
                    <button class="small" on:click=move |_| on_text.run(())><Icon name="edit" size=13 />{tr("Modifier le code", "Edit source")}</button>
                })}
                <button class="small" title=tr("Télécharger", "Download") on:click=move |_| download()><Icon name="download" size=13 /></button>
                <button class="small" title=tr("Actualiser", "Refresh") disabled=move || loading.get() on:click=move |_| load()><Icon name="refresh" size=13 /></button>
            </div>
            {move || error.get().map(|e| view! { <div class="error-banner">{e}</div> })}
            <div
                class="img-area"
                class:zoomed=move || zoom.get().is_some()
                node_ref=area
                on:wheel=move |e| {
                    e.prevent_default();
                    let Some(a) = area.get_untracked() else { return };
                    let r = a.get_bounding_client_rect();
                    let at = (e.client_x() as f64 - r.left(), e.client_y() as f64 - r.top());
                    scale(if e.delta_y() < 0.0 { 1.12 } else { 1.0 / 1.12 }, Some(at));
                }
                on:dblclick=move |_| set_zoom(if zoom.get_untracked().is_some() { None } else { Some(1.0) })
                on:pointerdown=move |e| {
                    if let Some(a) = area.get_untracked().filter(|_| e.button() == 0) {
                        let _ = a.set_pointer_capture(e.pointer_id());
                        drag.set_value(Some((e.client_x() as f64, e.client_y() as f64, a.scroll_left(), a.scroll_top())));
                    }
                }
                on:pointermove=move |e| {
                    if let (Some((x, y, l, t)), Some(a)) = (drag.get_value(), area.get_untracked()) {
                        a.set_scroll_left(l - (e.client_x() as f64 - x) as i32);
                        a.set_scroll_top(t - (e.client_y() as f64 - y) as i32);
                    }
                }
                on:pointerup=move |_| drag.set_value(None)
                on:pointercancel=move |_| drag.set_value(None)
            >
                {move || (loading.get() && data.with(|d| d.is_none())).then(|| view! { <div class="empty">{tr("Chargement…", "Loading…")}</div> })}
                {move || data.get().map(|src| view! {
                    <img
                        src=src
                        alt=""
                        draggable="false"
                        style=img_style
                        on:load=move |e| {
                            if let Some(t) = e.target() {
                                natural.set((num(&t, "naturalWidth"), num(&t, "naturalHeight")));
                            }
                        }
                        on:error=move |_| error.set(Some(tr("Image illisible ou format non pris en charge", "Unreadable image or unsupported format").to_string()))
                    />
                })}
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_by_extension() {
        assert_eq!(image_mime("/srv/a/Photo.JPG"), Some("image/jpeg"));
        assert_eq!(image_mime("/srv/logo.svg"), Some("image/svg+xml"));
        assert_eq!(image_mime("/srv/pics.png/readme"), None);
        assert_eq!(image_mime("/etc/hosts"), None);
        assert_eq!(image_mime("/srv/png"), None);
    }

    #[test]
    fn base64_len() {
        assert_eq!(decoded_len(""), 0);
        assert_eq!(decoded_len("YQ=="), 1);
        assert_eq!(decoded_len("YWI="), 2);
        assert_eq!(decoded_len("YWJj"), 3);
    }
}
