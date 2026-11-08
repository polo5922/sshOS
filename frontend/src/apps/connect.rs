//! Connections: saved hosts, add/edit form, ~/.ssh/config import.

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::json;
use sshos_shared::{AuthMethod, HostConfig};

use crate::icons::Icon;
use crate::state::*;
use crate::tauri::{invoke, invoke_unit, pick_files};

#[derive(Clone, Copy)]
struct Form {
    id: RwSignal<String>,
    name: RwSignal<String>,
    host: RwSignal<String>,
    port: RwSignal<String>,
    user: RwSignal<String>,
    auth: RwSignal<String>,
    key_path: RwSignal<String>,
    secret: RwSignal<String>,
    jump: RwSignal<String>,
    npm_target: RwSignal<String>,
}

impl Form {
    fn new() -> Self {
        Form {
            id: RwSignal::new(String::new()),
            name: RwSignal::new(String::new()),
            host: RwSignal::new(String::new()),
            port: RwSignal::new("22".into()),
            user: RwSignal::new("root".into()),
            auth: RwSignal::new("agent".into()),
            key_path: RwSignal::new(String::new()),
            secret: RwSignal::new(String::new()),
            jump: RwSignal::new(String::new()),
            npm_target: RwSignal::new(String::new()),
        }
    }
    fn load(&self, h: Option<&HostConfig>) {
        let d = HostConfig {
            id: String::new(),
            name: String::new(),
            host: String::new(),
            port: 22,
            user: "root".into(),
            auth: AuthMethod::Agent,
            jump: None,
            npm_target: None,
        };
        let h = h.unwrap_or(&d);
        self.id.set(h.id.clone());
        self.name.set(h.name.clone());
        self.host.set(h.host.clone());
        self.port.set(h.port.to_string());
        self.user.set(h.user.clone());
        let (kind, path) = match &h.auth {
            AuthMethod::Password => ("password", String::new()),
            AuthMethod::Key { path } => ("key", path.clone()),
            AuthMethod::Agent => ("agent", String::new()),
        };
        self.auth.set(kind.into());
        self.key_path.set(path);
        self.secret.set(String::new());
        self.jump.set(h.jump.clone().unwrap_or_default());
        self.npm_target.set(h.npm_target.clone().unwrap_or_default());
    }
    fn to_host(&self) -> HostConfig {
        let host = self.host.get_untracked().trim().to_string();
        let name = self.name.get_untracked().trim().to_string();
        HostConfig {
            id: self.id.get_untracked(),
            name: if name.is_empty() { host.clone() } else { name },
            host,
            port: self.port.get_untracked().trim().parse().unwrap_or(22),
            user: self.user.get_untracked().trim().to_string(),
            auth: match self.auth.get_untracked().as_str() {
                "password" => AuthMethod::Password,
                "key" => AuthMethod::Key { path: self.key_path.get_untracked().trim().to_string() },
                _ => AuthMethod::Agent,
            },
            jump: Some(self.jump.get_untracked()).filter(|j| !j.is_empty()),
            npm_target: Some(self.npm_target.get_untracked().trim().to_string()).filter(|t| !t.is_empty()),
        }
    }
}

#[component]
pub fn Connect() -> impl IntoView {
    let c = ctx();
    let f = Form::new();
    let editing = RwSignal::new(false);

    let save = move || {
        let host = f.to_host();
        if host.host.is_empty() || host.user.is_empty() {
            c.error(tr("Hôte et utilisateur requis.", "Host and user are required."));
            return;
        }
        let secret = Some(f.secret.get_untracked()).filter(|s| !s.is_empty());
        spawn_local(async move {
            match invoke::<HostConfig>("hosts_save", json!({ "req": { "host": host, "secret": secret } })).await {
                Ok(h) => {
                    c.ok(format!("{} {}", tr("Enregistré :", "Saved:"), h.name));
                    f.load(Some(&h));
                    c.reload_hosts();
                }
                Err(e) => c.handle_err(None, e),
            }
        });
    };
    let import = move |_| {
        spawn_local(async move {
            match invoke::<Vec<HostConfig>>("hosts_import", json!({})).await {
                Ok(v) => {
                    c.ok(format!("{} {}", v.len(), tr("hôte(s) importé(s) depuis ~/.ssh/config", "host(s) imported from ~/.ssh/config")));
                    c.reload_hosts();
                }
                Err(e) => c.handle_err(None, e),
            }
        });
    };
    let delete = move |h: HostConfig| {
        c.show(
            Dialog::confirm(
                tr("Supprimer la connexion", "Delete connection"),
                format!("{} « {} » ({}@{}) ?", tr("Supprimer", "Delete"), h.name, h.user, h.host),
                tr("Supprimer", "Delete"),
                move || {
                    let id = h.id.clone();
                    spawn_local(async move {
                        match invoke_unit("hosts_delete", json!({ "id": id })).await {
                            Ok(()) => {
                                c.sessions.update(|s| s.retain(|(x, _)| *x != id));
                                c.reload_hosts();
                                if f.id.get_untracked() == id {
                                    f.load(None);
                                    editing.set(false);
                                }
                            }
                            Err(e) => c.handle_err(None, e),
                        }
                    });
                },
            )
            .danger(),
        );
    };
    let browse_key = move |_| {
        spawn_local(async move {
            if let Some(p) = pick_files().await.into_iter().next() {
                f.key_path.set(p);
            }
        });
    };

    let list = move || {
        let hosts = c.hosts.get();
        if hosts.is_empty() {
            return view! {
                <div class="empty">{tr("Aucune connexion enregistrée.", "No saved connection.")}</div>
            }
            .into_any();
        }
        hosts
            .into_iter()
            .map(|h| {
                let id = h.id.clone();
                let id2 = id.clone();
                let id3 = id.clone();
                let id4 = id.clone();
                let h_edit = h.clone();
                let h_del = h.clone();
                let connected = move || c.is_connected(&id);
                let connecting = move || c.connecting.with(|v| v.contains(&id2));
                view! {
                    <div class="host-row" class:active=move || f.id.get() == id4 && editing.get()>
                        <span class="status-dot" class:on=connected.clone() class:busy=connecting.clone()></span>
                        <div class="host-main" on:click=move |_| {
                            f.load(Some(&h_edit));
                            editing.set(true);
                        }>
                            <div class="host-name">{h.name.clone()}</div>
                            <div class="muted small addr">{format!("{}@{}:{}", h.user, h.host, h.port)}</div>
                        </div>
                        {move || {
                            let id = id3.clone();
                            if connected() {
                                let id2 = id.clone();
                                view! {
                                    <button on:click=move |_| c.active.set(Some(id.clone()))>{tr("Activer", "Use")}</button>
                                    <button on:click=move |_| c.disconnect(id2.clone())>{tr("Déconnecter", "Disconnect")}</button>
                                }
                                    .into_any()
                            } else {
                                view! {
                                    <button class="primary" disabled=connecting.clone() on:click=move |_| c.connect(id.clone(), None)>
                                        {tr("Connecter", "Connect")}
                                    </button>
                                }
                                    .into_any()
                            }
                        }}
                        <button class="icon-btn" title=tr("Supprimer", "Delete") on:click=move |_| delete(h_del.clone())>
                            <Icon name="trash" size=14 />
                        </button>
                    </div>
                }
            })
            .collect_view()
            .into_any()
    };

    view! {
        <div class="connect split">
            <div class="pane-left">
                <div class="toolbar">
                    <button class="primary" on:click=move |_| {
                        f.load(None);
                        editing.set(true);
                    }>
                        <Icon name="plus" size=14 /> {tr("Nouvelle", "New")}
                    </button>
                    <button on:click=import>{tr("Importer ~/.ssh/config", "Import ~/.ssh/config")}</button>
                </div>
                <div class="host-list">{list}</div>
            </div>
            <div class="pane-right">
                <Show
                    when=move || editing.get()
                    fallback=|| view! {
                        <div class="empty">
                            {tr(
                                "Sélectionnez une connexion ou créez-en une.",
                                "Select a connection or create one.",
                            )}
                        </div>
                    }
                >
                    <form class="form" on:submit=move |e| {
                        e.prevent_default();
                        save();
                    }>
                        <h3>{move || if f.id.get().is_empty() { tr("Nouvelle connexion", "New connection") } else { tr("Modifier la connexion", "Edit connection") }}</h3>
                        <label>{tr("Nom", "Name")}<input bind:value=f.name placeholder="prod-web-1" /></label>
                        <div class="row">
                            <label class="grow">{tr("Hôte", "Host")}<input bind:value=f.host placeholder="203.0.113.10" /></label>
                            <label class="w80">"Port"<input bind:value=f.port /></label>
                        </div>
                        <label>{tr("Utilisateur", "User")}<input bind:value=f.user /></label>
                        <label>
                            {tr("Authentification", "Authentication")}
                            <select prop:value=move || f.auth.get() on:change=move |e| f.auth.set(event_target_value(&e))>
                                <option value="agent">{tr("Agent SSH / clés par défaut", "SSH agent / default keys")}</option>
                                <option value="key">{tr("Clé privée", "Private key")}</option>
                                <option value="password">{tr("Mot de passe", "Password")}</option>
                            </select>
                        </label>
                        <Show when=move || f.auth.get() == "key">
                            <label>
                                {tr("Fichier de clé", "Key file")}
                                <div class="row">
                                    <input class="grow" bind:value=f.key_path placeholder="~/.ssh/id_ed25519" />
                                    <button type="button" on:click=browse_key>"…"</button>
                                </div>
                            </label>
                        </Show>
                        <Show when=move || f.auth.get() != "agent">
                            <label>
                                {move || if f.auth.get() == "key" { tr("Phrase de passe (trousseau)", "Passphrase (keychain)") } else { tr("Mot de passe (trousseau)", "Password (keychain)") }}
                                <input
                                    type="password"
                                    bind:value=f.secret
                                    placeholder=move || if f.id.get().is_empty() { "" } else { tr("inchangé si vide", "unchanged if empty") }
                                />
                            </label>
                        </Show>
                        <label>
                            {tr("Rebond (ProxyJump)", "Jump host (ProxyJump)")}
                            <select prop:value=move || f.jump.get() on:change=move |e| f.jump.set(event_target_value(&e))>
                                <option value="">{tr("Aucun", "None")}</option>
                                {move || {
                                    let me = f.id.get();
                                    c.hosts
                                        .get()
                                        .into_iter()
                                        .filter(|h| h.id != me)
                                        .map(|h| {
                                            let sel = f.jump.get_untracked() == h.id;
                                            view! { <option value=h.id selected=sel>{h.name}</option> }
                                        })
                                        .collect_view()
                                }}
                            </select>
                        </label>
                        <label>
                            {tr("API Nginx Proxy Manager (optionnel)", "Nginx Proxy Manager API (optional)")}
                            <input bind:value=f.npm_target placeholder=tr("auto-détectée, ex. 127.0.0.1:81", "auto-detected, e.g. 127.0.0.1:81") />
                        </label>
                        <div class="form-actions">
                            <button type="submit" class="primary"><Icon name="save" size=14 /> {tr("Enregistrer", "Save")}</button>
                            <Show when=move || !f.id.get().is_empty()>
                                <button type="button" on:click=move |_| c.connect(f.id.get_untracked(), None)>
                                    {tr("Connecter", "Connect")}
                                </button>
                            </Show>
                        </div>
                        <p class="muted small">
                            {tr(
                                "Les secrets sont stockés dans le trousseau du système, jamais en clair.",
                                "Secrets are stored in the OS keychain, never in plain text.",
                            )}
                        </p>
                    </form>
                </Show>
            </div>
        </div>
    }
}
