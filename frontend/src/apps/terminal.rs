//! Terminal: xterm.js on a remote PTY.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::{json, Value};
use wasm_bindgen::prelude::*;

use crate::state::*;
use crate::tauri::{invoke, invoke_unit, listen, term, Listener};

/// Keystrokes must reach the PTY in order: one `term_write` in flight at a time.
#[derive(Clone, Default)]
struct WriteQueue {
    pending: Rc<RefCell<VecDeque<String>>>,
    busy: Rc<Cell<bool>>,
}

impl WriteQueue {
    fn push(&self, id: String, data: String) {
        self.pending.borrow_mut().push_back(data);
        if self.busy.replace(true) {
            return;
        }
        let q = self.clone();
        spawn_local(async move {
            loop {
                let next = q.pending.borrow_mut().pop_front();
                let Some(data) = next else { break };
                let _ = invoke_unit("term_write", json!({ "id": id, "data": data })).await;
            }
            q.busy.set(false);
        });
    }
}

#[component]
pub fn Terminal(host: String, input: Option<String>) -> impl IntoView {
    let c = ctx();
    let el = NodeRef::<leptos::html::Div>::new();
    let tid = format!("t{:x}", (js_sys::Math::random() * 1e15) as u64);
    let xterm = StoredValue::new(None::<u32>);
    let opened = StoredValue::new(false);
    let alive = StoredValue::new(true);
    let listeners = StoredValue::new_local(Vec::<Listener>::new());

    let tid0 = tid.clone();
    Effect::new(move |_| {
        let Some(div) = el.get() else { return };
        if xterm.get_value().is_some() {
            return;
        }
        let tid = tid0.clone();
        let queue = WriteQueue::default();
        let (tid_d, tid_r) = (tid.clone(), tid.clone());
        let on_data = Closure::<dyn FnMut(String)>::new(move |b64: String| queue.push(tid_d.clone(), b64));
        let on_resize = Closure::<dyn FnMut(u32, u32)>::new(move |cols: u32, rows: u32| {
            if opened.try_get_value().unwrap_or(false) {
                let id = tid_r.clone();
                spawn_local(async move {
                    let _ = invoke_unit("term_resize", json!({ "id": id, "cols": cols, "rows": rows })).await;
                });
            }
        });
        let elem: &web_sys::Element = div.as_ref();
        let x = term::create(
            elem,
            c.dark.get_untracked(),
            on_data.into_js_value().unchecked_ref(),
            on_resize.into_js_value().unchecked_ref(),
        );
        xterm.set_value(Some(x));

        let host = host.clone();
        let input = input.clone();
        spawn_local(async move {
            // Subscribe before opening so the first prompt is not lost.
            let t1 = tid.clone();
            let l1 = listen("term-output", move |v: Value| {
                if v["id"].as_str() == Some(t1.as_str()) {
                    if let Some(d) = v["data"].as_str() {
                        term::write(x, d);
                    }
                }
            })
            .await;
            let t2 = tid.clone();
            let l2 = listen("term-exit", move |v: Value| {
                if v["id"].as_str() == Some(t2.as_str()) {
                    let _ = opened.try_set_value(false);
                    term::write_text(x, &format!("\r\n\x1b[2m[{}]\x1b[0m\r\n", tr("session terminée", "session ended")));
                }
            })
            .await;
            if !alive.try_get_value().unwrap_or(false) {
                return; // window closed meanwhile; listeners dropped here
            }
            listeners.update_value(|v| {
                v.push(l1);
                v.push(l2);
            });
            let size = term::size(x);
            let (cols, rows) = (size.first().copied().unwrap_or(80), size.get(1).copied().unwrap_or(24));
            match invoke::<String>(
                "term_open",
                json!({ "hostId": host, "id": tid, "cols": cols, "rows": rows, "input": input }),
            )
            .await
            {
                Ok(_) => {
                    let _ = opened.try_set_value(true);
                    term::focus(x);
                }
                Err(e) => term::write_text(x, &format!("\x1b[31m{}\x1b[0m\r\n", e.message())),
            }
        });
    });

    Effect::new(move |_| {
        let dark = c.dark.get();
        if let Some(x) = xterm.get_value() {
            term::set_dark(x, dark);
        }
    });

    let tid_c = tid.clone();
    on_cleanup(move || {
        alive.set_value(false);
        listeners.update_value(|v| v.clear());
        if let Some(x) = xterm.get_value() {
            term::dispose(x);
        }
        let id = tid_c.clone();
        spawn_local(async move {
            let _ = invoke_unit("term_close", json!({ "id": id })).await;
        });
    });

    view! {
        <div
            class="terminal"
            node_ref=el
            on:click=move |_| {
                if let Some(x) = xterm.get_value() {
                    term::focus(x);
                }
            }
        ></div>
    }
}
