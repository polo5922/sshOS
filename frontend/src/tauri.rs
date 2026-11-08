//! Bindings to the Tauri JS API (`withGlobalTauri`) and to the bundled vendor widgets.
//! Values cross the boundary as JSON so serde attributes (tags, flatten…) behave exactly as on
//! the Rust backend.

use serde::de::DeserializeOwned;
use serde_json::Value;
use sshos_shared::ErrorPayload;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "core"], js_name = invoke, catch)]
    async fn tauri_invoke(cmd: &str, args: JsValue) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "event"], js_name = listen, catch)]
    async fn tauri_listen(event: &str, handler: &js_sys::Function) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "dialog"], js_name = open, catch)]
    async fn dialog_open_js(opts: JsValue) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "dialog"], js_name = save, catch)]
    async fn dialog_save_js(opts: JsValue) -> Result<JsValue, JsValue>;
}

pub mod term {
    use wasm_bindgen::prelude::*;
    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_namespace = SshOsTerm)]
        pub fn create(el: &web_sys::Element, dark: bool, on_data: &js_sys::Function, on_resize: &js_sys::Function) -> u32;
        #[wasm_bindgen(js_namespace = SshOsTerm)]
        pub fn size(id: u32) -> Vec<u32>;
        #[wasm_bindgen(js_namespace = SshOsTerm)]
        pub fn write(id: u32, b64: &str);
        #[wasm_bindgen(js_namespace = SshOsTerm, js_name = writeText)]
        pub fn write_text(id: u32, text: &str);
        #[wasm_bindgen(js_namespace = SshOsTerm)]
        pub fn fit(id: u32);
        #[wasm_bindgen(js_namespace = SshOsTerm)]
        pub fn focus(id: u32);
        #[wasm_bindgen(js_namespace = SshOsTerm, js_name = setDark)]
        pub fn set_dark(id: u32, dark: bool);
        #[wasm_bindgen(js_namespace = SshOsTerm)]
        pub fn dispose(id: u32);
    }
}

pub mod editor {
    use wasm_bindgen::prelude::*;
    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_namespace = SshOsEditor)]
        pub fn create(
            el: &web_sys::Element,
            text: &str,
            filename: &str,
            dark: bool,
            on_change: &js_sys::Function,
            on_save: &js_sys::Function,
        ) -> u32;
        #[wasm_bindgen(js_namespace = SshOsEditor, js_name = getText)]
        pub fn get_text(id: u32) -> String;
        #[wasm_bindgen(js_namespace = SshOsEditor, js_name = setText)]
        pub fn set_text(id: u32, text: &str);
        #[wasm_bindgen(js_namespace = SshOsEditor, js_name = setDark)]
        pub fn set_dark(id: u32, dark: bool);
        #[wasm_bindgen(js_namespace = SshOsEditor)]
        pub fn focus(id: u32);
        #[wasm_bindgen(js_namespace = SshOsEditor)]
        pub fn dispose(id: u32);
    }
}

fn to_js(v: &Value) -> JsValue {
    js_sys::JSON::parse(&v.to_string()).unwrap_or(JsValue::NULL)
}

pub fn js_to_value(v: &JsValue) -> Value {
    if v.is_undefined() || v.is_null() {
        return Value::Null;
    }
    js_sys::JSON::stringify(v)
        .ok()
        .and_then(|s| s.as_string())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null)
}

fn js_error(e: JsValue) -> ErrorPayload {
    if let Some(s) = e.as_string() {
        return ErrorPayload::Message { message: s };
    }
    serde_json::from_value(js_to_value(&e)).unwrap_or_else(|_| ErrorPayload::Message {
        message: format!("{e:?}"),
    })
}

/// `invoke(cmd, args)`; argument names are camelCase on this side (Tauri converts them).
pub async fn invoke<T: DeserializeOwned>(cmd: &str, args: Value) -> Result<T, ErrorPayload> {
    let r = tauri_invoke(cmd, to_js(&args)).await.map_err(js_error)?;
    serde_json::from_value(js_to_value(&r)).map_err(|e| ErrorPayload::Message {
        message: format!("{cmd}: unexpected response ({e})"),
    })
}

/// Fire-and-forget variant for commands returning `()`.
pub async fn invoke_unit(cmd: &str, args: Value) -> Result<(), ErrorPayload> {
    tauri_invoke(cmd, to_js(&args)).await.map(|_| ()).map_err(js_error)
}

/// Event subscription; dropping it unsubscribes.
pub struct Listener {
    _closure: Closure<dyn FnMut(JsValue)>,
    unlisten: Option<js_sys::Function>,
}

impl Drop for Listener {
    fn drop(&mut self) {
        if let Some(f) = self.unlisten.take() {
            let _ = f.call0(&JsValue::NULL);
        }
    }
}

pub async fn listen(event: &str, mut f: impl FnMut(Value) + 'static) -> Listener {
    let closure = Closure::<dyn FnMut(JsValue)>::new(move |ev: JsValue| {
        let payload = js_sys::Reflect::get(&ev, &JsValue::from_str("payload")).unwrap_or(JsValue::NULL);
        f(js_to_value(&payload));
    });
    let unlisten = tauri_listen(event, closure.as_ref().unchecked_ref())
        .await
        .ok()
        .and_then(|u| u.dyn_into::<js_sys::Function>().ok());
    Listener { _closure: closure, unlisten }
}

/// Native "open file(s)" dialog. Returns local paths.
pub async fn pick_files() -> Vec<String> {
    let opts = to_js(&serde_json::json!({ "multiple": true, "directory": false }));
    match dialog_open_js(opts).await {
        Ok(v) => match js_to_value(&v) {
            Value::String(s) => vec![s],
            Value::Array(a) => a.into_iter().filter_map(|x| x.as_str().map(String::from)).collect(),
            _ => vec![],
        },
        Err(_) => vec![],
    }
}

/// Native "save as" dialog.
pub async fn pick_save(default_name: &str) -> Option<String> {
    let opts = to_js(&serde_json::json!({ "defaultPath": default_name }));
    dialog_save_js(opts).await.ok().and_then(|v| v.as_string())
}

/// The app window (`__TAURI__.window.getCurrentWindow()`), absent in a plain browser preview.
fn app_window() -> Option<JsValue> {
    let g = js_sys::Reflect::get(&js_sys::global(), &"__TAURI__".into()).ok()?;
    let w = js_sys::Reflect::get(&g, &"window".into()).ok()?;
    let f = js_sys::Reflect::get(&w, &"getCurrentWindow".into()).ok()?.dyn_into::<js_sys::Function>().ok()?;
    f.call0(&w).ok()
}

/// Calls `method(args…)` on the app window and awaits the returned promise.
async fn window_call(method: &str, args: &[JsValue]) -> Option<JsValue> {
    let w = app_window()?;
    let f = js_sys::Reflect::get(&w, &method.into()).ok()?.dyn_into::<js_sys::Function>().ok()?;
    let arr: js_sys::Array = args.iter().collect();
    let p = f.apply(&w, &arr).ok()?;
    match p.dyn_into::<js_sys::Promise>() {
        Ok(p) => wasm_bindgen_futures::JsFuture::from(p).await.ok(),
        Err(v) => Some(v),
    }
}

pub async fn is_fullscreen() -> bool {
    window_call("isFullscreen", &[]).await.and_then(|v| v.as_bool()).unwrap_or(false)
}

pub async fn set_fullscreen(on: bool) {
    window_call("setFullscreen", &[JsValue::from_bool(on)]).await;
}

/// Flips full screen and returns the new state.
pub async fn toggle_fullscreen() -> bool {
    let on = !is_fullscreen().await;
    set_fullscreen(on).await;
    on
}

pub async fn minimize_app() {
    window_call("minimize", &[]).await;
}

/// Closes the app window for good (`destroy`: `close` would fire close-requested again).
pub async fn quit_app() {
    window_call("destroy", &[]).await;
}

/// Runs `f` instead of closing when the OS asks to close the window (Alt+F4, taskbar…).
pub fn on_close_requested(f: impl Fn() + 'static) {
    let Some(w) = app_window() else { return };
    let Ok(reg) = js_sys::Reflect::get(&w, &"onCloseRequested".into()).and_then(|v| v.dyn_into::<js_sys::Function>()) else {
        return;
    };
    let cb = Closure::<dyn FnMut(JsValue)>::new(move |e: JsValue| {
        if let Ok(pd) = js_sys::Reflect::get(&e, &"preventDefault".into()).and_then(|v| v.dyn_into::<js_sys::Function>()) {
            let _ = pd.call0(&e);
        }
        f();
    });
    let _ = reg.call1(&w, cb.as_ref().unchecked_ref());
    cb.forget(); // lives as long as the window
}

/// Opens `url` in a separate top-level webview window (pages that refuse to be framed work there).
pub fn open_webview_window(label: &str, url: &str, title: &str) -> bool {
    let Some(ctor) = js_sys::Reflect::get(&js_sys::global(), &"__TAURI__".into())
        .ok()
        .and_then(|g| js_sys::Reflect::get(&g, &"webviewWindow".into()).ok())
        .and_then(|m| js_sys::Reflect::get(&m, &"WebviewWindow".into()).ok())
        .and_then(|c| c.dyn_into::<js_sys::Function>().ok())
    else {
        return false;
    };
    let opts = to_js(&serde_json::json!({ "url": url, "title": title, "width": 1200, "height": 800 }));
    let args: js_sys::Array = [JsValue::from_str(label), opts].iter().collect();
    js_sys::Reflect::construct(&ctor, &args).is_ok()
}

/// Copy text to the system clipboard (best effort).
pub fn copy_text(text: &str) {
    let nav = js_sys::Reflect::get(&web_sys::window().unwrap().into(), &"navigator".into()).ok();
    let clip = nav.and_then(|n| js_sys::Reflect::get(&n, &"clipboard".into()).ok());
    if let Some(clip) = clip {
        if let Ok(f) = js_sys::Reflect::get(&clip, &"writeText".into()) {
            if let Some(f) = f.dyn_ref::<js_sys::Function>() {
                let _ = f.call1(&clip, &text.into());
            }
        }
    }
}

/// Run a command on a host. `read` marks it as a read-only query in the command log.
pub async fn exec(host: &str, command: &str, read: bool) -> Result<sshos_shared::ExecResult, ErrorPayload> {
    invoke("exec", serde_json::json!({ "hostId": host, "command": command, "read": read })).await
}
