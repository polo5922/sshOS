//! First-launch tour: what sshOs is, the first server, the desktop, the safety rules.
//! Shown once (`ONBOARDED_KEY`), and again from the sshOs menu.

use leptos::prelude::*;

use crate::icons::Icon;
use crate::state::*;

const STEPS: usize = 4;

#[component]
pub fn Onboarding() -> impl IntoView {
    let c = ctx();
    let step = RwSignal::new(0usize);

    // Done (or skipped): remember it, then make sure there is a server to work with.
    let finish = move |open_connect: bool| {
        storage_set(ONBOARDED_KEY, "1");
        c.onboarding.set(false);
        step.set(0);
        if c.sessions.with_untracked(|s| !s.is_empty()) {
            return;
        }
        let last = storage_get(LAST_HOST_KEY).filter(|id| c.hosts.with_untracked(|h| h.iter().any(|x| &x.id == id)));
        match last {
            Some(id) if !open_connect => c.connect(id, None),
            _ => c.open(AppKind::Connect, None),
        }
    };
    let next = move |_| {
        if step.get_untracked() + 1 >= STEPS {
            finish(false);
            // First run: chain with the quick tour of the desktop.
            if storage_get(crate::tour::TOUR_KEY).is_none() {
                c.tour.set(Some(0));
            }
        } else {
            step.update(|s| *s += 1);
        }
    };

    let body = move || match step.get() {
        0 => welcome().into_any(),
        1 => servers(c, move || finish(true)).into_any(),
        2 => desktop().into_any(),
        _ => safety().into_any(),
    };

    view! {
        <div class="overlay onboarding-overlay">
            <div class="onboarding">
                <div class="ob-body">{body}</div>
                <div class="ob-foot">
                    <div class="ob-dots">
                        {(0..STEPS)
                            .map(|i| view! { <button class="ob-dot" class:on=move || step.get() == i on:click=move |_| step.set(i)></button> })
                            .collect_view()}
                    </div>
                    <span class="spacer"></span>
                    <Show when=move || step.get() + 1 < STEPS>
                        <button class="link" on:click=move |_| finish(false)>{tr("Passer", "Skip")}</button>
                    </Show>
                    <Show when=move || step.get() != 0>
                        <button on:click=move |_| step.update(|s| *s -= 1)>{tr("Précédent", "Back")}</button>
                    </Show>
                    <button class="primary" on:click=next>
                        {move || if step.get() + 1 >= STEPS { tr("Commencer", "Get started") } else { tr("Suivant", "Next") }}
                    </button>
                </div>
            </div>
        </div>
    }
}

fn welcome() -> impl IntoView {
    view! {
        <div class="ob-hero">
            <div class="logo-big">"sshOs"</div>
            <h2>{tr("Votre serveur, comme un bureau", "Your server, as a desktop")}</h2>
            <p>
                {tr(
                    "sshOs gère vos serveurs sans rien y installer : terminal, fichiers, Docker, nginx, sécurité… tout passe par SSH et SFTP.",
                    "sshOs manages your servers without installing anything on them: terminal, files, Docker, nginx, security… everything goes through SSH and SFTP.",
                )}
            </p>
            <p class="muted small">
                {tr("Portage en Rust de Davish, dont il reprend le concept et les règles de sécurité.", "A Rust port of Davish, keeping its concept and safety rules.")}
            </p>
            // Language: defaults to the system one; picking another reloads the UI on this step.
            <div class="seg ob-lang">
                {Lang::ALL
                    .into_iter()
                    .map(|l| {
                        view! {
                            <button class:on=current_lang() == l on:click=move |_| {
                                if current_lang() != l {
                                    l.apply()
                                }
                            }>{l.native_name()}</button>
                        }
                    })
                    .collect_view()}
            </div>
        </div>
    }
}

fn servers(c: Ctx, add: impl Fn() + Copy + Send + Sync + 'static) -> impl IntoView {
    let rows = move || {
        c.hosts
            .get()
            .into_iter()
            .map(|h| {
                let id = h.id.clone();
                let (id2, id3) = (id.clone(), id.clone());
                let on = move || c.is_connected(&id2);
                let wait = move || c.connecting.with(|v| v.contains(&id3));
                view! {
                    <div class="ob-host">
                        <span class="status-dot" class:on=on.clone() class:busy=wait.clone()></span>
                        <div class="grow">
                            <b>{h.name.clone()}</b>
                            <div class="muted small addr">{format!("{}@{}:{}", h.user, h.host, h.port)}</div>
                        </div>
                        {move || {
                            if on() {
                                view! { <span class="pill ok">{tr("Connecté", "Connected")}</span> }.into_any()
                            } else {
                                let (id, wait) = (id.clone(), wait.clone());
                                view! {
                                    <button disabled=wait on:click=move |_| c.connect(id.clone(), None)>
                                        {tr("Se connecter", "Connect")}
                                    </button>
                                }
                                .into_any()
                            }
                        }}
                    </div>
                }
            })
            .collect_view()
    };
    view! {
        <div class="ob-step">
            <div class="ob-icon t-plug"><Icon name="plug" size=26 /></div>
            <h2>{tr("Votre premier serveur", "Your first server")}</h2>
            <p>
                {tr(
                    "Choisissez le serveur actif dans le sélecteur en haut à gauche, à côté de l'icône sshOs. Le dernier serveur utilisé est reconnecté automatiquement au démarrage.",
                    "Pick the active server in the selector at the top left, next to the sshOs icon. The last server used is reconnected automatically at startup.",
                )}
            </p>
            <div class="ob-hosts">{rows}</div>
            <Show when=move || c.hosts.with(|h| h.is_empty())>
                <p class="muted small">{tr("Aucun serveur enregistré pour l'instant.", "No saved server yet.")}</p>
            </Show>
            <button class="primary" on:click=move |_| add()>
                <Icon name="plus" size=14 /> {tr("Ajouter un serveur", "Add a server")}
            </button>
            <p class="muted small">
                {tr(
                    "Mot de passe, clé, agent SSH ou import de ~/.ssh/config. Les secrets restent dans le trousseau du système.",
                    "Password, key, SSH agent or ~/.ssh/config import. Secrets stay in the OS keychain.",
                )}
            </p>
        </div>
    }
}

fn desktop() -> impl IntoView {
    let tips: [(&str, &str, &str); 6] = [
        ("server", tr("Barre de menus", "Menu bar"), tr("Les menus de l'app au premier plan, comme sur macOS.", "The menus of the app in front, like on macOS.")),
        ("box", tr("Dock", "Dock"), tr("Clic droit sur une icône : ses fenêtres, Nouvelle fenêtre, Quitter.", "Right-click an icon: its windows, New window, Quit.")),
        ("search", "Spotlight", tr("Ctrl+K pour lancer une app ou changer de serveur.", "Ctrl+K to launch an app or switch server.")),
        ("compass", tr("Navigateur", "Browser"), tr("Ouvre les pages localhost du serveur à travers un tunnel SSH.", "Opens the server's localhost pages through an SSH tunnel.")),
        ("terminal", tr("Shell Docker", "Docker shell"), tr("Le shell d'un conteneur s'ouvre dans l'app Docker.", "A container shell opens inside the Docker app.")),
        ("external", tr("Plein écran", "Full screen"), tr("F11 bascule le plein écran ; Quitter est dans le menu sshOs.", "F11 toggles full screen; Quit is in the sshOs menu.")),
    ];
    view! {
        <div class="ob-step">
            <h2>{tr("Le bureau", "The desktop")}</h2>
            <div class="ob-tips">
                {tips
                    .into_iter()
                    .map(|(icon, title, text)| {
                        view! {
                            <div class="ob-tip">
                                <Icon name=icon size=18 />
                                <div>
                                    <b>{title}</b>
                                    <div class="muted small">{text}</div>
                                </div>
                            </div>
                        }
                    })
                    .collect_view()}
            </div>
        </div>
    }
}

fn safety() -> impl IntoView {
    let rules = [
        tr("Jamais de sudo silencieux : la commande s'ouvre pré-tapée dans un Terminal, c'est vous qui appuyez sur Entrée.", "Never a silent sudo: the command opens pre-typed in a Terminal, you press Enter."),
        tr("Chaque opération affiche la commande exacte avant d'être exécutée.", "Every operation shows the exact command before running it."),
        tr("nginx -t et fail2ban-client -t sont toujours lancés avant un rechargement.", "nginx -t and fail2ban-client -t always run before a reload."),
        tr("Nginx Proxy Manager n'est modifié que par son API.", "Nginx Proxy Manager is only changed through its API."),
    ];
    view! {
        <div class="ob-step">
            <div class="ob-icon t-shield"><Icon name="shield" size=26 /></div>
            <h2>{tr("Vos règles de sécurité", "Safety rules")}</h2>
            <ul class="ob-rules">
                {rules
                    .into_iter()
                    .map(|r| view! { <li><Icon name="check" size=14 /><span>{r}</span></li> })
                    .collect_view()}
            </ul>
        </div>
    }
}
