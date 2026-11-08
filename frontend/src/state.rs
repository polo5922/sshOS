//! Global UI state shared through Leptos context.

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::json;
use sshos_shared::{ErrorPayload, HostConfig, MonitorSnapshot, ServerInfo, TransferProgress};

use crate::prefs::{Prefs, ThemeMode};
use crate::tauri::invoke;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    Fr,
    En,
    De,
    Es,
}

impl Lang {
    pub const ALL: [Lang; 4] = [Lang::En, Lang::Fr, Lang::De, Lang::Es];

    pub fn code(self) -> &'static str {
        match self {
            Lang::Fr => "fr",
            Lang::En => "en",
            Lang::De => "de",
            Lang::Es => "es",
        }
    }

    /// Accepts "fr", "fr-FR", "de_AT"… Unknown languages fall back to English.
    pub fn from_code(code: &str) -> Lang {
        match code.get(..2).unwrap_or("").to_ascii_lowercase().as_str() {
            "fr" => Lang::Fr,
            "de" => Lang::De,
            "es" => Lang::Es,
            _ => Lang::En,
        }
    }

    pub fn native_name(self) -> &'static str {
        match self {
            Lang::Fr => "Français",
            Lang::En => "English",
            Lang::De => "Deutsch",
            Lang::Es => "Español",
        }
    }

    /// Saved choice, else the system language (navigator.language).
    pub fn detect() -> Lang {
        if let Some(code) = storage_get("sshos.lang") {
            return Lang::from_code(&code);
        }
        let nav = js_sys::eval("navigator.language || ''").ok().and_then(|v| v.as_string()).unwrap_or_default();
        Lang::from_code(&nav)
    }

    /// Saves the choice and reloads the UI (labels are not reactive; SSH sessions survive).
    pub fn apply(self) {
        storage_set("sshos.lang", self.code());
        if let Some(w) = web_sys::window() {
            let _ = w.location().reload();
        }
    }
}

thread_local! {
    static LANG: std::cell::Cell<Lang> = const { std::cell::Cell::new(Lang::Fr) };
}

// Translation picked at call time. Changing the language reloads the UI (sessions survive in
// the backend), so labels do not need to be reactive.

pub fn current_lang() -> Lang {
    LANG.try_with(|l| l.get()).unwrap_or(Lang::En)
}

/// `tr("français", "english")`: German and Spanish come from `i18n.tsv`, keyed by the English
/// text (missing entries fall back to English).
pub fn tr(fr: &'static str, en: &'static str) -> &'static str {
    match current_lang() {
        Lang::En => en,
        Lang::Fr => fr,
        Lang::De => crate::i18n::lookup(en, 0),
        Lang::Es => crate::i18n::lookup(en, 1),
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum AppKind {
    Connect,
    Terminal { input: Option<String> },
    Files { path: Option<String> },
    Editor { path: String, nginx: bool },
    Monitor,
    Nginx,
    Docker,
    Security,
    Browser { url: Option<String> },
    Disks,
    TextEdit { path: Option<String> },
    Transfers,
    Logs,
    Git,
    CmdLog,
    Settings,
    About,
}

impl AppKind {
    pub fn needs_host(&self) -> bool {
        !matches!(self, AppKind::Connect | AppKind::About | AppKind::Settings | AppKind::CmdLog | AppKind::Transfers)
    }
    pub fn icon(&self) -> &'static str {
        match self {
            AppKind::Connect => "plug",
            AppKind::Terminal { .. } => "terminal",
            AppKind::Files { .. } => "folder",
            AppKind::Editor { .. } => "edit",
            AppKind::Monitor => "activity",
            AppKind::Nginx => "globe",
            AppKind::Docker => "box",
            AppKind::Security => "shield",
            AppKind::Browser { .. } => "compass",
            AppKind::Disks => "hdd",
            AppKind::TextEdit { .. } => "file",
            AppKind::Transfers => "arrows",
            AppKind::Logs => "logs",
            AppKind::Git => "git",
            AppKind::CmdLog => "list",
            AppKind::Settings => "sliders",
            AppKind::About => "info",
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            AppKind::Connect => tr("Connexions", "Connections"),
            AppKind::Terminal { .. } => "Terminal",
            AppKind::Files { .. } => tr("Fichiers", "Files"),
            AppKind::Editor { .. } => tr("Éditeur", "Editor"),
            AppKind::Monitor => tr("Moniteur", "Monitor"),
            AppKind::Nginx => "Nginx",
            AppKind::Docker => "Docker",
            AppKind::Security => tr("Sécurité", "Security"),
            AppKind::Browser { .. } => tr("Navigateur", "Browser"),
            AppKind::Disks => tr("Disques", "Disks"),
            AppKind::TextEdit { .. } => tr("Texte", "TextEdit"),
            AppKind::Transfers => tr("Transferts", "Transfers"),
            AppKind::Logs => tr("Journaux", "Logs"),
            AppKind::Git => "Git",
            AppKind::CmdLog => tr("Historique", "Command log"),
            AppKind::Settings => tr("Réglages", "Settings"),
            AppKind::About => tr("À propos", "About"),
        }
    }
    /// Apps that exist at most once per host.
    fn single(&self) -> bool {
        matches!(
            self,
            AppKind::Connect
                | AppKind::Monitor
                | AppKind::Nginx
                | AppKind::Docker
                | AppKind::Security
                | AppKind::Disks
                | AppKind::Transfers
                | AppKind::Logs
                | AppKind::Git
                | AppKind::CmdLog
                | AppKind::Settings
                | AppKind::About
        )
    }
    /// Apps drawn edge to edge: no title bar, the traffic lights sit on their sidebar.
    pub fn chromeless(&self) -> bool {
        matches!(
            self,
            AppKind::Docker
                | AppKind::Security
                | AppKind::Files { .. }
                | AppKind::Settings
                | AppKind::Monitor
                | AppKind::Disks
                | AppKind::Logs
                | AppKind::Git
                | AppKind::CmdLog
                | AppKind::Transfers
        )
    }
    fn default_size(&self) -> (f64, f64) {
        match self {
            AppKind::Connect => (880.0, 560.0),
            AppKind::Terminal { .. } => (820.0, 500.0),
            AppKind::Files { .. } => (900.0, 560.0),
            AppKind::Editor { .. } => (900.0, 620.0),
            AppKind::Monitor => (1060.0, 700.0),
            AppKind::Nginx => (1080.0, 680.0),
            AppKind::Docker => (1180.0, 720.0),
            AppKind::Security => (1120.0, 720.0),
            AppKind::Browser { .. } => (1100.0, 720.0),
            AppKind::Disks => (980.0, 640.0),
            AppKind::TextEdit { .. } => (900.0, 620.0),
            AppKind::Transfers => (760.0, 520.0),
            AppKind::Logs => (1080.0, 680.0),
            AppKind::Git => (1080.0, 680.0),
            AppKind::CmdLog => (960.0, 600.0),
            AppKind::Settings => (900.0, 620.0),
            AppKind::About => (520.0, 360.0),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Clone)]
pub struct Win {
    pub id: u32,
    pub app: AppKind,
    pub host_id: Option<String>,
    pub title: RwSignal<String>,
    pub rect: RwSignal<Rect>,
    pub z: RwSignal<u32>,
    pub minimized: RwSignal<bool>,
    pub maximized: RwSignal<bool>,
    /// Set while the close animation runs.
    pub closing: RwSignal<bool>,
}

/// Id of the window an app is drawn in (context provided by the window manager).
#[derive(Clone, Copy)]
pub struct WinId(pub u32);

/// One entry of a menu-bar menu. Plain closures (not arena values): the menu bar may still hold
/// them for a moment after the app that registered them is gone.
#[derive(Clone)]
pub struct MenuItem {
    pub label: String,
    pub shortcut: Option<&'static str>,
    pub checked: Option<std::sync::Arc<dyn Fn() -> bool + Send + Sync>>,
    pub disabled: Option<std::sync::Arc<dyn Fn() -> bool + Send + Sync>>,
    pub run: std::sync::Arc<dyn Fn() + Send + Sync>,
}

impl MenuItem {
    pub fn new(label: impl Into<String>, run: impl Fn() + Send + Sync + 'static) -> Option<Self> {
        Some(MenuItem { label: label.into(), shortcut: None, checked: None, disabled: None, run: std::sync::Arc::new(run) })
    }
}

/// Builder helpers on `Option<MenuItem>` so menus read as `MenuItem::new(..).key("Ctrl+S")`.
pub trait MenuItemExt {
    fn key(self, k: &'static str) -> Self;
    fn checked(self, f: impl Fn() -> bool + Send + Sync + 'static) -> Self;
    fn disabled(self, f: impl Fn() -> bool + Send + Sync + 'static) -> Self;
}

impl MenuItemExt for Option<MenuItem> {
    fn key(self, k: &'static str) -> Self {
        self.map(|mut m| {
            m.shortcut = Some(k);
            m
        })
    }
    fn checked(self, f: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        self.map(|mut m| {
            m.checked = Some(std::sync::Arc::new(f));
            m
        })
    }
    fn disabled(self, f: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        self.map(|mut m| {
            m.disabled = Some(std::sync::Arc::new(f));
            m
        })
    }
}

/// A menu of the menu bar; `None` items are separators.
#[derive(Clone)]
pub struct MenuSection {
    pub title: String,
    pub items: Vec<Option<MenuItem>>,
}

impl MenuSection {
    pub fn new(title: impl Into<String>, items: Vec<Option<MenuItem>>) -> Self {
        MenuSection { title: title.into(), items }
    }
}

/// Called by an app component: its menus show in the menu bar while its window is in front.
pub fn app_menus(sections: Vec<MenuSection>) {
    let c = ctx();
    let Some(WinId(id)) = use_context::<WinId>() else { return };
    c.app_menus.update(|m| {
        m.retain(|(w, _)| *w != id);
        m.push((id, sections));
    });
    on_cleanup(move || {
        let _ = c.app_menus.try_update(|m| m.retain(|(w, _)| *w != id));
    });
}

#[derive(Clone, Copy, PartialEq)]
pub enum ToastKind {
    Info,
    Ok,
    Error,
}

#[derive(Clone)]
pub struct Toast {
    pub id: u32,
    pub kind: ToastKind,
    pub text: String,
}

/// Modal dialog. `input` turns it into a prompt; `code` shows the exact command that will run.
#[derive(Clone)]
pub struct Dialog {
    pub title: String,
    pub body: String,
    pub code: Option<String>,
    pub input: Option<RwSignal<String>>,
    pub ok_label: String,
    pub danger: bool,
    pub on_ok: Option<Callback<()>>,
}

impl Dialog {
    pub fn info(title: impl Into<String>, body: impl Into<String>) -> Self {
        Dialog {
            title: title.into(),
            body: body.into(),
            code: None,
            input: None,
            ok_label: "OK".into(),
            danger: false,
            on_ok: None,
        }
    }
    pub fn confirm(title: impl Into<String>, body: impl Into<String>, ok: impl Into<String>, on_ok: impl Fn() + Send + Sync + 'static) -> Self {
        Dialog {
            title: title.into(),
            body: body.into(),
            code: None,
            input: None,
            ok_label: ok.into(),
            danger: false,
            on_ok: Some(Callback::new(move |()| on_ok())),
        }
    }
    pub fn code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }
    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }
    pub fn with_input(mut self, sig: RwSignal<String>) -> Self {
        self.input = Some(sig);
        self
    }
}

#[derive(Clone, Copy)]
pub struct Ctx {
    pub hosts: RwSignal<Vec<HostConfig>>,
    /// Connected hosts and what we know about them.
    pub sessions: RwSignal<Vec<(String, ServerInfo)>>,
    pub connecting: RwSignal<Vec<String>>,
    pub active: RwSignal<Option<String>>,
    pub windows: RwSignal<Vec<Win>>,
    pub next_id: RwSignal<u32>,
    pub top_z: RwSignal<u32>,
    pub dark: RwSignal<bool>,
    pub lang: RwSignal<Lang>,
    pub toasts: RwSignal<Vec<Toast>>,
    pub dialog: RwSignal<Option<Dialog>>,
    pub spotlight: RwSignal<bool>,
    /// App launcher (all apps by category; pick what goes in the Dock).
    pub launcher: RwSignal<bool>,
    pub transfers: RwSignal<Vec<TransferProgress>>,
    /// Bumped after uploads so open Files windows refresh.
    pub fs_tick: RwSignal<u32>,
    /// Latest usage of the active host (menu bar), shared with the apps.
    pub live: RwSignal<Option<(String, MonitorSnapshot)>>,
    /// Owner of the desktop. Signals created from event handlers must live here: the handler's own
    /// owner (a menu, a row…) may be disposed right after, which would panic the whole UI.
    pub root: StoredValue<Owner>,
    /// The app window covers the whole screen (no OS title bar or task bar).
    pub fullscreen: RwSignal<bool>,
    /// Menus of each app window, shown in the menu bar for the window in front.
    pub app_menus: RwSignal<Vec<(u32, Vec<MenuSection>)>>,
    /// First-launch tour (also reachable from the sshOs menu).
    pub onboarding: RwSignal<bool>,
    /// Quick tour: index of the step shown.
    pub tour: RwSignal<Option<usize>>,
    /// Settings app (appearance, security…), persisted in localStorage.
    pub prefs: RwSignal<Prefs>,
    /// The OS asks for a dark theme (used by the "Auto" theme).
    pub os_dark: RwSignal<bool>,
}

pub fn ctx() -> Ctx {
    expect_context::<Ctx>()
}

pub fn storage_get(key: &str) -> Option<String> {
    web_sys::window()?.local_storage().ok()??.get_item(key).ok()?
}

pub fn storage_set(key: &str, val: &str) {
    if let Some(s) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
        let _ = s.set_item(key, val);
    }
}

pub fn os_prefers_dark() -> bool {
    web_sys::window()
        .and_then(|w| w.match_media("(prefers-color-scheme: dark)").ok().flatten())
        .map(|m| m.matches())
        .unwrap_or(true)
}

/// Every localStorage key owned by sshOs.
pub fn storage_keys() -> Vec<String> {
    let Some(s) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) else { return vec![] };
    let n = s.length().unwrap_or(0);
    (0..n).filter_map(|i| s.key(i).ok().flatten()).filter(|k| k.starts_with("sshos.")).collect()
}

pub fn storage_remove(key: &str) {
    if let Some(s) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
        let _ = s.remove_item(key);
    }
}

/// Server reconnected at startup (the last active one).
pub const LAST_HOST_KEY: &str = "sshos.lastHost";
pub const ONBOARDED_KEY: &str = "sshos.onboarded";

impl Ctx {
    pub fn new() -> Self {
        // Dark by default, like the macOS-style desktop it imitates.
        let prefs = Prefs::load();
        let os_dark = os_prefers_dark();
        let dark = prefs.is_dark(os_dark);
        let lang = Lang::detect();
        LANG.with(|l| l.set(lang));
        Ctx {
            hosts: RwSignal::new(vec![]),
            sessions: RwSignal::new(vec![]),
            connecting: RwSignal::new(vec![]),
            active: RwSignal::new(None),
            windows: RwSignal::new(vec![]),
            next_id: RwSignal::new(1),
            top_z: RwSignal::new(10),
            dark: RwSignal::new(dark),
            lang: RwSignal::new(lang),
            toasts: RwSignal::new(vec![]),
            dialog: RwSignal::new(None),
            spotlight: RwSignal::new(false),
            launcher: RwSignal::new(false),
            transfers: RwSignal::new(vec![]),
            fs_tick: RwSignal::new(0),
            live: RwSignal::new(None),
            root: StoredValue::new(Owner::current().expect("desktop owner")),
            fullscreen: RwSignal::new(false),
            app_menus: RwSignal::new(vec![]),
            onboarding: RwSignal::new(false),
            tour: RwSignal::new(None),
            prefs: RwSignal::new(prefs),
            os_dark: RwSignal::new(os_dark),
        }
    }

    /// Changes the preferences, saves them and applies the theme.
    pub fn set_prefs(&self, f: impl FnOnce(&mut Prefs)) {
        self.prefs.update(f);
        let p = self.prefs.get_untracked();
        p.save();
        self.dark.set(p.is_dark(self.os_dark.get_untracked()));
    }

    pub fn set_theme(&self, mode: ThemeMode) {
        self.set_prefs(|p| p.theme = mode);
    }

    /// Menu bar / Spotlight toggle: always lands on an explicit theme.
    pub fn toggle_theme(&self) {
        let mode = if self.dark.get_untracked() { ThemeMode::Light } else { ThemeMode::Dark };
        self.set_theme(mode);
    }

    /// A signal that outlives the caller's reactive scope (see `root`).
    pub fn root_signal<T: Send + Sync + 'static>(&self, v: T) -> RwSignal<T> {
        self.in_root(|| RwSignal::new(v))
    }

    pub fn in_root<R>(&self, f: impl FnOnce() -> R) -> R {
        self.root.with_value(|o| o.clone()).with(f)
    }

    pub fn host(&self, id: &str) -> Option<HostConfig> {
        self.hosts.with_untracked(|h| h.iter().find(|h| h.id == id).cloned())
    }

    pub fn host_name(&self, id: &str) -> String {
        self.host(id).map(|h| h.name).unwrap_or_else(|| id.to_string())
    }

    pub fn info(&self, id: &str) -> Option<ServerInfo> {
        self.sessions.with_untracked(|s| s.iter().find(|(h, _)| h == id).map(|(_, i)| i.clone()))
    }

    pub fn is_connected(&self, id: &str) -> bool {
        self.sessions.with(|s| s.iter().any(|(h, _)| h == id))
    }

    // ------------------------------------------------------------ toasts

    pub fn toast(&self, kind: ToastKind, text: impl Into<String>) {
        let id = self.next_id.get_untracked();
        self.next_id.set(id + 1);
        self.toasts.update(|t| t.push(Toast { id, kind, text: text.into() }));
        let toasts = self.toasts;
        let ms = if kind == ToastKind::Error { 9000 } else { 4500 };
        set_timeout(
            move || toasts.try_update(|t| t.retain(|x| x.id != id)).unwrap_or(()),
            std::time::Duration::from_millis(ms),
        );
    }
    pub fn ok(&self, text: impl Into<String>) {
        self.toast(ToastKind::Ok, text)
    }
    pub fn info_toast(&self, text: impl Into<String>) {
        self.toast(ToastKind::Info, text)
    }
    pub fn error(&self, text: impl Into<String>) {
        self.toast(ToastKind::Error, text)
    }

    pub fn show(&self, d: Dialog) {
        self.dialog.set(Some(d));
    }

    /// Central error handling: root-only actions open a terminal with the command typed in,
    /// everything else becomes a toast.
    pub fn handle_err(&self, host_id: Option<&str>, e: ErrorPayload) {
        match e {
            ErrorPayload::NeedsTerminal { command } => match host_id {
                Some(h) => self.run_in_terminal(h, &command),
                None => self.error(format!("{}: {command}", tr("Nécessite root", "Needs root"))),
            },
            other => self.error(other.message()),
        }
    }

    /// Davish rule: privileged commands are never run silently. A terminal opens with the command
    /// typed in; the user reads it, presses Enter and types their sudo password there.
    pub fn run_in_terminal(&self, host_id: &str, command: &str) {
        self.open(AppKind::Terminal { input: Some(command.to_string()) }, Some(host_id.to_string()));
        let (h, cmd) = (host_id.to_string(), command.to_string());
        spawn_local(async move {
            let _ = crate::tauri::invoke_unit("cmdlog_terminal", json!({ "hostId": h, "command": cmd })).await;
        });
        self.info_toast(tr(
            "Commande privilégiée prête dans le terminal : vérifiez-la puis appuyez sur Entrée.",
            "Privileged command typed in the terminal: review it, then press Enter.",
        ));
    }

    // ------------------------------------------------------------ windows

    pub fn open(&self, app: AppKind, host_id: Option<String>) {
        let host_id = if app.needs_host() { host_id.or_else(|| self.active.get_untracked()) } else { None };
        if app.needs_host() {
            match &host_id {
                Some(h) if self.sessions.with_untracked(|s| s.iter().any(|(x, _)| x == h)) => {}
                _ => {
                    self.info_toast(tr("Connectez-vous d'abord à un serveur.", "Connect to a server first."));
                    self.open(AppKind::Connect, None);
                    return;
                }
            }
        }
        if app.single() {
            let existing = self.windows.with_untracked(|w| {
                w.iter()
                    .find(|w| std::mem::discriminant(&w.app) == std::mem::discriminant(&app) && w.host_id == host_id)
                    .map(|w| w.id)
            });
            if let Some(id) = existing {
                self.focus(id);
                return;
            }
        }
        if let AppKind::Editor { path, .. } = &app {
            let existing = self.windows.with_untracked(|w| {
                w.iter()
                    .find(|w| matches!(&w.app, AppKind::Editor { path: p, .. } if p == path) && w.host_id == host_id)
                    .map(|w| w.id)
            });
            if let Some(id) = existing {
                self.focus(id);
                return;
            }
        }

        let id = self.next_id.get_untracked();
        self.next_id.set(id + 1);
        let count = self.windows.with_untracked(|w| w.len()) as f64;
        let (vw, vh) = viewport();
        let (w, h) = app.default_size();
        let (w, h) = (w.min(vw - 40.0), h.min(vh - 120.0));
        let x = ((vw - w) / 2.0 + (count % 6.0) * 28.0 - 70.0).max(8.0);
        let y = ((vh - h) / 2.0 + (count % 6.0) * 28.0 - 70.0).max(36.0);
        let title = match (&app, &host_id) {
            (AppKind::Editor { path, .. }, _) => path.rsplit('/').next().unwrap_or(path).to_string(),
            (_, Some(h)) => format!("{} — {}", app.label(), self.host_name(h)),
            _ => app.label().to_string(),
        };
        let z = self.top_z.get_untracked() + 1;
        self.top_z.set(z);
        let win = self.in_root(|| Win {
            id,
            app,
            host_id,
            title: RwSignal::new(title),
            rect: RwSignal::new(Rect { x, y, w, h }),
            z: RwSignal::new(z),
            minimized: RwSignal::new(false),
            maximized: RwSignal::new(false),
            closing: RwSignal::new(false),
        });
        self.windows.update(|ws| ws.push(win));
    }

    pub fn focus(&self, id: u32) {
        let z = self.top_z.get_untracked() + 1;
        self.top_z.set(z);
        self.windows.with_untracked(|ws| {
            if let Some(w) = ws.iter().find(|w| w.id == id) {
                w.z.set(z);
                w.minimized.set(false);
            }
        });
    }

    pub fn close(&self, id: u32) {
        self.windows.update(|ws| ws.retain(|w| w.id != id));
        self.app_menus.update(|m| m.retain(|(w, _)| *w != id));
    }

    /// Close with the shrink animation.
    pub fn close_animated(&self, id: u32) {
        let Some(closing) = self.windows.with_untracked(|ws| ws.iter().find(|w| w.id == id).map(|w| w.closing)) else {
            return;
        };
        if closing.get_untracked() {
            return;
        }
        closing.set(true);
        let c = *self;
        set_timeout(move || c.close(id), std::time::Duration::from_millis(170));
    }

    pub fn toggle_fullscreen(&self) {
        let fs = self.fullscreen;
        spawn_local(async move {
            let on = crate::tauri::toggle_fullscreen().await;
            fs.set(on);
        });
    }

    pub fn focused(&self) -> Option<u32> {
        self.windows.with(|ws| {
            ws.iter()
                .filter(|w| !w.minimized.get())
                .max_by_key(|w| w.z.get())
                .map(|w| w.id)
        })
    }

    pub fn focused_untracked(&self) -> Option<u32> {
        self.windows.with_untracked(|ws| {
            ws.iter()
                .filter(|w| !w.minimized.get_untracked())
                .max_by_key(|w| w.z.get_untracked())
                .map(|w| w.id)
        })
    }

    // ------------------------------------------------------------ hosts & connections

    pub fn reload_hosts(&self) {
        let hosts = self.hosts;
        spawn_local(async move {
            if let Ok(h) = invoke::<Vec<HostConfig>>("hosts_list", json!({})).await {
                hosts.set(h);
            }
        });
    }

    /// Connect, handling trust-on-first-use of the host key.
    pub fn connect(&self, host_id: String, accept: Option<String>) {
        let c = *self;
        spawn_local(async move {
            c.connect_async(host_id, accept).await;
        });
    }

    /// Same as `connect`, but says whether the session is now open.
    pub async fn connect_async(self, host_id: String, accept: Option<String>) -> bool {
        let c = self;
        if c.connecting.with_untracked(|v| v.contains(&host_id)) {
            return false;
        }
        c.connecting.update(|v| v.push(host_id.clone()));
        {
            let r = invoke::<ServerInfo>(
                "connect",
                json!({ "req": { "host_id": host_id, "accept_fingerprint": accept } }),
            )
            .await;
            c.connecting.update(|v| v.retain(|x| x != &host_id));
            match r {
                Ok(info) => {
                    c.sessions.update(|s| {
                        s.retain(|(h, _)| h != &host_id);
                        s.push((host_id.clone(), info.clone()));
                    });
                    c.active.set(Some(host_id.clone()));
                    c.ok(format!("{} {} ({})", tr("Connecté à", "Connected to"), c.host_name(&host_id), info.os));
                    true
                }
                Err(ErrorPayload::UnknownHostKey { host, fingerprint, algorithm }) => {
                    let hid = host_id.clone();
                    let fp = fingerprint.clone();
                    c.show(
                        Dialog::confirm(
                            tr("Hôte inconnu", "Unknown host"),
                            format!(
                                "{} {host}. {}",
                                tr("Première connexion à", "First connection to"),
                                tr(
                                    "Vérifiez l'empreinte de la clé du serveur avant de lui faire confiance :",
                                    "Check the server key fingerprint before trusting it:"
                                )
                            ),
                            tr("Faire confiance et se connecter", "Trust and connect"),
                            move || c.connect(hid.clone(), Some(fp.clone())),
                        )
                        .code(format!("{algorithm} {fingerprint}")),
                    );
                    false
                }
                Err(e @ ErrorPayload::HostKeyChanged { .. }) => {
                    c.show(Dialog::info(tr("⚠ Clé d'hôte modifiée", "⚠ Host key changed"), e.message()).danger());
                    false
                }
                Err(e) => {
                    c.error(e.message());
                    false
                }
            }
        }
    }

    pub fn disconnect(&self, host_id: String) {
        let c = *self;
        spawn_local(async move {
            let _ = invoke::<()>("disconnect", json!({ "hostId": host_id })).await;
            // A deliberate disconnect: do not reconnect it at the next launch.
            if storage_get(LAST_HOST_KEY).as_deref() == Some(host_id.as_str()) {
                storage_remove(LAST_HOST_KEY);
            }
            c.sessions.update(|s| s.retain(|(h, _)| h != &host_id));
            c.windows.update(|ws| ws.retain(|w| w.host_id.as_deref() != Some(host_id.as_str())));
            if c.active.get_untracked().as_deref() == Some(host_id.as_str()) {
                c.active.set(c.sessions.with_untracked(|s| s.first().map(|(h, _)| h.clone())));
            }
        });
    }
}

pub fn viewport() -> (f64, f64) {
    let w = web_sys::window().unwrap();
    (
        w.inner_width().ok().and_then(|v| v.as_f64()).unwrap_or(1280.0),
        w.inner_height().ok().and_then(|v| v.as_f64()).unwrap_or(800.0),
    )
}

// ---------------------------------------------------------------- formatting helpers

pub fn human_bytes(b: f64) -> String {
    const U: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut v = b;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{v:.0} {}", U[i])
    } else {
        format!("{v:.1} {}", U[i])
    }
}

pub fn human_duration(secs: u64) -> String {
    let d = secs / 86400;
    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    if d > 0 {
        format!("{d}j {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

pub fn mode_string(mode: u32, is_dir: bool, is_link: bool) -> String {
    let mut s = String::with_capacity(10);
    s.push(if is_link { 'l' } else if is_dir { 'd' } else { '-' });
    for shift in [6, 3, 0] {
        let b = (mode >> shift) & 7;
        s.push(if b & 4 != 0 { 'r' } else { '-' });
        s.push(if b & 2 != 0 { 'w' } else { '-' });
        s.push(if b & 1 != 0 { 'x' } else { '-' });
    }
    s
}


/// Days until a "YYYY-MM-DD HH:MM:SS" (NPM) or ISO date.
pub fn days_until(date: &str) -> Option<i64> {
    if date.is_empty() {
        return None;
    }
    let iso = date.replacen(' ', "T", 1);
    let t = js_sys::Date::parse(&iso);
    if t.is_nan() {
        return None;
    }
    Some(((t - js_sys::Date::now()) / 86_400_000.0).floor() as i64)
}

pub fn split_list(s: &str) -> Vec<String> {
    s.split([',', ' ', '\n'])
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(String::from)
        .collect()
}
