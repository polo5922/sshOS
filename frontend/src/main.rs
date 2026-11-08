mod apps;
mod i18n;
mod tour;
mod icons;
mod launcher;
mod onboarding;
mod prefs;
mod state;
mod tauri;
mod wm;

fn main() {
    std::panic::set_hook(Box::new(|info| {
        console_error_panic_hook::hook(info);
        show_crash_banner();
    }));
    leptos::mount::mount_to_body(wm::App);
}

/// After a panic the WASM side is unusable (every later task fails). Say so and offer a reload:
/// SSH sessions live in the Rust backend and survive it.
fn show_crash_banner() {
    let Some(doc) = web_sys::window().and_then(|w| w.document()) else { return };
    if doc.get_element_by_id("crash-banner").is_some() {
        return;
    }
    let Ok(el) = doc.create_element("div") else { return };
    el.set_id("crash-banner");
    el.set_inner_html(&format!(
        "<span>{}</span><button onclick=\"location.reload()\">{}</button>",
        state::tr(
            "sshOs a rencontré une erreur interne. Les connexions SSH sont conservées.",
            "sshOs hit an internal error. SSH connections are kept."
        ),
        state::tr("Recharger l'interface", "Reload the interface"),
    ));
    if let Some(body) = doc.body() {
        let _ = body.append_child(&el);
    }
}
