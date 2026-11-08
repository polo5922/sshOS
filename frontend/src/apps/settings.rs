//! Settings: general options, appearance, desktop, security and local data.
//! Everything here is stored locally (see `prefs.rs`); secrets stay in the OS keychain.

use leptos::prelude::*;

use crate::icons::Icon;
use crate::prefs::*;
use crate::state::*;

#[derive(Clone, Copy, PartialEq)]
enum Pane {
    General,
    Appearance,
    Desktop,
    Security,
    Data,
}

impl Pane {
    const ALL: [Pane; 5] = [Pane::General, Pane::Appearance, Pane::Desktop, Pane::Security, Pane::Data];
    fn title(self) -> &'static str {
        match self {
            Pane::General => tr("Général", "General"),
            Pane::Appearance => tr("Apparence", "Appearance"),
            Pane::Desktop => tr("Bureau et Dock", "Desktop & Dock"),
            Pane::Security => tr("Sécurité", "Security"),
            Pane::Data => tr("Données locales", "Local data"),
        }
    }
    fn icon(self) -> &'static str {
        match self {
            Pane::General => "sliders",
            Pane::Appearance => "sun",
            Pane::Desktop => "grid",
            Pane::Security => "shield",
            Pane::Data => "database",
        }
    }
}

#[component]
pub fn Settings() -> impl IntoView {
    let c = ctx();
    let pane = RwSignal::new(Pane::General);

    app_menus(vec![MenuSection::new(
        tr("Présentation", "View"),
        Pane::ALL
            .into_iter()
            .map(|p| MenuItem::new(p.title(), move || pane.set(p)).checked(move || pane.get() == p))
            .collect(),
    )]);

    let item = move |p: Pane| {
        view! {
            <button class="side-item" class:active=move || pane.get() == p on:click=move |_| pane.set(p)>
                <Icon name=p.icon() size=15 />
                <span class="grow">{p.title()}</span>
            </button>
        }
    };

    view! {
        <div class="split-app settings">
            <aside class="side drag-region">
                <div class="side-top"></div>
                {Pane::ALL.into_iter().map(item).collect_view()}
                <div class="grow"></div>
                <div class="side-foot muted small">{tr("Enregistré automatiquement", "Saved automatically")}</div>
            </aside>
            <section class="main">
                <header class="main-head drag-region">
                    <h2>{move || pane.get().title()}</h2>
                </header>
                <div class="main-body settings-body">
                    {move || match pane.get() {
                        Pane::General => general(c).into_any(),
                        Pane::Appearance => appearance(c).into_any(),
                        Pane::Desktop => desktop(c).into_any(),
                        Pane::Security => security(c).into_any(),
                        Pane::Data => data(c).into_any(),
                    }}
                </div>
            </section>
        </div>
    }
}

// ---------------------------------------------------------------- building blocks

fn group(title: &'static str, body: impl IntoView) -> impl IntoView {
    view! {
        <div class="set-group">
            <div class="set-group-title">{title}</div>
            <div class="set-card">{body}</div>
        </div>
    }
}

fn row(label: &'static str, desc: &'static str, control: impl IntoView) -> impl IntoView {
    view! {
        <div class="set-row">
            <div class="grow">
                <div class="set-label">{label}</div>
                {(!desc.is_empty()).then(|| view! { <div class="muted small">{desc}</div> })}
            </div>
            <div class="set-ctl">{control}</div>
        </div>
    }
}

/// On/off switch bound to one boolean preference.
fn pswitch(c: Ctx, get: fn(&Prefs) -> bool, set: fn(&mut Prefs, bool)) -> impl IntoView {
    let on = move || c.prefs.with(get);
    view! {
        <button
            class="switch"
            class:on=on
            on:click=move |_| {
                let v = !c.prefs.with_untracked(get);
                c.set_prefs(|p| set(p, v));
            }
        >
            <span></span>
        </button>
    }
}

/// Segmented control.
fn seg<T: Copy + PartialEq + Send + Sync + 'static>(
    opts: Vec<(T, &'static str)>,
    cur: impl Fn() -> T + Copy + Send + Sync + 'static,
    pick: impl Fn(T) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    view! {
        <div class="seg">
            {opts
                .into_iter()
                .map(|(v, label)| view! { <button class:on=move || cur() == v on:click=move |_| pick(v)>{label}</button> })
                .collect_view()}
        </div>
    }
}

// ---------------------------------------------------------------- panes

fn general(c: Ctx) -> impl IntoView {
    let lang = move || c.lang.get();
    let set_lang = move |l: Lang| {
        if l != c.lang.get_untracked() {
            l.apply();
        }
    };
    let shortcuts: [(&str, &str); 5] = [
        ("Ctrl+K", tr("Spotlight : lancer une app, changer de serveur", "Spotlight: launch an app, switch server")),
        ("Ctrl+,", tr("Ouvrir les Réglages", "Open Settings")),
        ("F11", tr("Plein écran", "Full screen")),
        ("Alt+F4", tr("Quitter sshOs", "Quit sshOs")),
        (tr("Clic droit", "Right-click"), tr("Menu d'une icône du Dock", "Menu of a Dock icon")),
    ];
    view! {
        {group(
            tr("Langue et fenêtre", "Language & window"),
            view! {
                {row(
                    tr("Langue", "Language"),
                    tr("Recharge l'interface ; les connexions restent ouvertes.", "Reloads the interface; connections stay open."),
                    seg(Lang::ALL.iter().map(|l| (*l, l.native_name())).collect(), lang, set_lang),
                )}
                {row(
                    tr("Plein écran", "Full screen"),
                    tr("Désactivé : fenêtre classique, avec la barre des tâches et le dock du système. Activé : plein écran (F11).", "Off: a regular window, with the system task bar or dock. On: full screen (F11)."),
                    view! {
                        <button class="switch" class:on=move || c.fullscreen.get() on:click=move |_| c.toggle_fullscreen()>
                            <span></span>
                        </button>
                    },
                )}
            },
        )}
        {group(
            tr("Découvrir", "Discover"),
            view! {
                {row(
                    tr("Présentation de sshOs", "sshOs tour"),
                    tr("Revoir la visite guidée du premier lancement.", "Replay the first-launch tour."),
                    view! { <button on:click=move |_| c.onboarding.set(true)>{tr("Revoir", "Replay")}</button> },
                )}
            },
        )}
        {group(
            tr("Raccourcis clavier", "Keyboard shortcuts"),
            shortcuts
                .into_iter()
                .map(|(k, what)| view! { <div class="set-row"><span class="grow">{what}</span><kbd>{k}</kbd></div> })
                .collect_view(),
        )}
    }
}

fn appearance(c: Ctx) -> impl IntoView {
    let theme_card = move |m: ThemeMode, cls: &'static str, label: &'static str| {
        view! {
            <button class="theme-card" class:on=move || c.prefs.with(|p| p.theme == m) on:click=move |_| c.set_theme(m)>
                <span class=format!("theme-prev {cls}")>
                    <span class="tp-bar"></span>
                    <span class="tp-win"></span>
                </span>
                <span>{label}</span>
            </button>
        }
    };
    view! {
        {group(
            tr("Thème", "Theme"),
            view! {
                <div class="theme-cards">
                    {theme_card(ThemeMode::Light, "light", tr("Clair", "Light"))}
                    {theme_card(ThemeMode::Dark, "dark", tr("Sombre", "Dark"))}
                    {theme_card(ThemeMode::Auto, "auto", tr("Automatique", "Auto"))}
                </div>
                <div class="set-note muted small">{tr("Automatique suit le thème de Windows.", "Auto follows the Windows theme.")}</div>
            },
        )}
        {group(
            tr("Couleur d'accentuation", "Accent color"),
            view! {
                <div class="swatches">
                    {Accent::ALL
                        .into_iter()
                        .map(|a| {
                            view! {
                                <button
                                    class=format!("swatch sw-{}", a.id())
                                    class:on=move || c.prefs.with(|p| p.accent == a)
                                    title=a.label()
                                    on:click=move |_| c.set_prefs(|p| p.accent = a)
                                ></button>
                            }
                        })
                        .collect_view()}
                    <span class="muted small">{move || c.prefs.with(|p| p.accent.label())}</span>
                </div>
            },
        )}
        {group(
            tr("Fond d'écran", "Wallpaper"),
            view! {
                <div class="walls">
                    {Wallpaper::ALL
                        .into_iter()
                        .map(|w| {
                            view! {
                                <button class="wall-pick" class:on=move || c.prefs.with(|p| p.wallpaper == w) on:click=move |_| c.set_prefs(|p| p.wallpaper = w)>
                                    <span class="wall-thumb" data-wall=w.id()></span>
                                    <span class="small">{w.label()}</span>
                                </button>
                            }
                        })
                        .collect_view()}
                </div>
            },
        )}
        {group(
            tr("Effets", "Effects"),
            view! {
                {row(
                    tr("Transparence", "Transparency"),
                    tr("Effet verre dépoli des fenêtres, du Dock et des menus.", "Frosted glass on windows, Dock and menus."),
                    seg(
                        vec![(Glass::Clear, tr("Forte", "High")), (Glass::Normal, tr("Normale", "Normal")), (Glass::Opaque, tr("Aucune", "None"))],
                        move || c.prefs.with(|p| p.glass),
                        move |g| c.set_prefs(|p| p.glass = g),
                    ),
                )}
                {row(
                    tr("Réduire les animations", "Reduce motion"),
                    tr("Ouverture et fermeture des fenêtres instantanées.", "Windows open and close instantly."),
                    pswitch(c, |p| p.reduce_motion, |p, v| p.reduce_motion = v),
                )}
            },
        )}
    }
}

fn desktop(c: Ctx) -> impl IntoView {
    view! {
        {group(
            "Dock",
            view! {
                {row(
                    tr("Taille", "Size"),
                    "",
                    seg(
                        vec![(DockSize::Small, tr("Petite", "Small")), (DockSize::Medium, tr("Moyenne", "Medium")), (DockSize::Large, tr("Grande", "Large"))],
                        move || c.prefs.with(|p| p.dock_size),
                        move |d| c.set_prefs(|p| p.dock_size = d),
                    ),
                )}
                {row(
                    tr("Agrandissement au survol", "Magnification"),
                    tr("Les icônes grossissent sous le pointeur.", "Icons grow under the pointer."),
                    pswitch(c, |p| p.dock_magnify, |p, v| p.dock_magnify = v),
                )}
            },
        )}
        {group(
            tr("Barre de menus", "Menu bar"),
            view! {
                {row(
                    tr("Utilisation du serveur", "Server usage"),
                    tr("CPU, mémoire, disque et réseau du serveur actif.", "CPU, memory, disk and network of the active server."),
                    pswitch(c, |p| p.menu_stats, |p, v| p.menu_stats = v),
                )}
                {row(
                    tr("Afficher les secondes", "Show seconds"),
                    tr("Dans l'horloge.", "In the clock."),
                    pswitch(c, |p| p.clock_seconds, |p, v| p.clock_seconds = v),
                )}
            },
        )}
    }
}

fn security(c: Ctx) -> impl IntoView {
    let idle = move || c.prefs.with(|p| p.idle_disconnect_min);
    let rules: [(&str, &str); 6] = [
        (
            tr("Jamais de sudo silencieux", "Never a silent sudo"),
            tr("La commande s'ouvre pré-tapée dans un Terminal ; c'est vous qui appuyez sur Entrée.", "The command opens pre-typed in a Terminal; you press Enter."),
        ),
        (
            tr("Commande affichée avant exécution", "Command shown before running"),
            tr("Chaque opération montre la commande exacte qui sera lancée.", "Every operation shows the exact command it runs."),
        ),
        (
            tr("Configuration testée avant rechargement", "Config tested before reload"),
            tr("nginx -t et fail2ban-client -t passent avant tout rechargement.", "nginx -t and fail2ban-client -t run before any reload."),
        ),
        (
            tr("Nginx Proxy Manager via son API", "Nginx Proxy Manager through its API"),
            tr("Sa base et ses fichiers ne sont jamais modifiés directement.", "Its database and files are never edited directly."),
        ),
        (
            tr("Empreintes des serveurs vérifiées", "Server fingerprints checked"),
            tr("Une clé d'hôte inconnue est montrée avant d'être acceptée ; une clé modifiée bloque la connexion.", "An unknown host key is shown before being trusted; a changed key blocks the connection."),
        ),
        (
            tr("Secrets dans le trousseau du système", "Secrets in the OS keychain"),
            tr("Mots de passe et phrases de passe ne sont jamais écrits sur le disque par sshOs.", "Passwords and passphrases are never written to disk by sshOs."),
        ),
    ];
    view! {
        {group(
            tr("Connexions", "Connections"),
            view! {
                {row(
                    tr("Reconnecter le dernier serveur", "Reconnect the last server"),
                    tr("Au démarrage, rouvre la connexion qui était active.", "At launch, reopens the connection that was active."),
                    pswitch(c, |p| p.auto_reconnect, |p, v| p.auto_reconnect = v),
                )}
                {row(
                    tr("Déconnexion après inactivité", "Disconnect when idle"),
                    tr("Ferme toutes les connexions SSH sans activité du clavier ou de la souris.", "Closes every SSH connection after no keyboard or mouse activity."),
                    view! {
                        <select
                            prop:value=move || idle().to_string()
                            on:change=move |e| {
                                let v = event_target_value(&e).parse().unwrap_or(0);
                                c.set_prefs(|p| p.idle_disconnect_min = v);
                            }
                        >
                            {[0u32, 5, 15, 30, 60]
                                .into_iter()
                                .map(|m| {
                                    let label = if m == 0 { tr("Jamais", "Never").to_string() } else { format!("{m} min") };
                                    view! { <option value=m.to_string() selected=move || idle() == m>{label}</option> }
                                })
                                .collect_view()}
                        </select>
                    },
                )}
                {row(
                    tr("Confirmer avant de quitter", "Confirm before quitting"),
                    tr("Évite de fermer toutes les connexions par erreur.", "Avoids closing every connection by mistake."),
                    pswitch(c, |p| p.confirm_quit, |p, v| p.confirm_quit = v),
                )}
            },
        )}
        {group(
            tr("Confidentialité", "Privacy"),
            view! {
                {row(
                    tr("Masquer les adresses", "Hide addresses"),
                    tr("Floute utilisateur@hôte (partage d'écran, démo). Survolez pour lire.", "Blurs user@host (screen sharing, demos). Hover to read."),
                    pswitch(c, |p| p.hide_addresses, |p, v| p.hide_addresses = v),
                )}
            },
        )}
        {group(
            tr("Toujours actif", "Always on"),
            view! {
                <div class="set-note muted small">
                    {tr(
                        "Ces règles protègent vos serveurs et ne peuvent pas être désactivées.",
                        "These rules protect your servers and cannot be turned off.",
                    )}
                </div>
                {rules
                    .into_iter()
                    .map(|(t, d)| {
                        view! {
                            <div class="set-row locked">
                                <span class="lock-ico"><Icon name="lock" size=13 /></span>
                                <div class="grow">
                                    <div class="set-label">{t}</div>
                                    <div class="muted small">{d}</div>
                                </div>
                                <span class="pill ok">{tr("Actif", "On")}</span>
                            </div>
                        }
                    })
                    .collect_view()}
            },
        )}
    }
}

fn data(c: Ctx) -> impl IntoView {
    // Bumped after each action so the counts refresh.
    let tick = RwSignal::new(0u32);
    let keys = move || {
        tick.get();
        storage_keys()
    };
    let nginx_keys = move || keys().into_iter().filter(|k| k.starts_with("sshos.nginx.")).collect::<Vec<_>>();
    let last = move || {
        tick.get();
        storage_get(LAST_HOST_KEY).map(|h| c.host_name(&h))
    };
    let forget_last = move |_| {
        storage_remove(LAST_HOST_KEY);
        tick.update(|t| *t += 1);
        c.ok(tr("Le dernier serveur ne sera pas reconnecté.", "The last server will not be reconnected."));
    };
    let reset_nginx = move |_| {
        for k in nginx_keys() {
            storage_remove(&k);
        }
        tick.update(|t| *t += 1);
        c.ok(tr("Choix Nginx oubliés : la question reviendra à la prochaine ouverture.", "Nginx choices cleared: you will be asked again."));
    };
    let reset_prefs = move |_| {
        c.dialog.set(Some(Dialog::confirm(
            tr("Réinitialiser les réglages ?", "Reset settings?"),
            tr("Apparence, Dock et options de sécurité reviennent aux valeurs par défaut.", "Appearance, Dock and security options go back to their defaults."),
            tr("Réinitialiser", "Reset"),
            move || {
                c.set_prefs(|p| *p = Prefs::default());
                tick.update(|t| *t += 1);
            },
        )));
    };
    let wipe = move |_| {
        c.dialog.set(Some(
            Dialog::confirm(
                tr("Effacer toutes les données locales ?", "Erase all local data?"),
                tr(
                    "Réglages, dernier serveur, choix Nginx et présentation sont effacés, puis l'interface est rechargée. Les serveurs enregistrés et leurs secrets ne sont pas touchés.",
                    "Settings, last server, Nginx choices and tour are erased, then the interface reloads. Saved servers and their secrets are not touched.",
                ),
                tr("Effacer", "Erase"),
                move || {
                    for k in storage_keys() {
                        storage_remove(&k);
                    }
                    let _ = window().location().reload();
                },
            )
            .danger(),
        ));
    };
    view! {
        {group(
            tr("Mémorisé sur cet ordinateur", "Remembered on this computer"),
            view! {
                {row(
                    tr("Dernier serveur actif", "Last active server"),
                    "",
                    view! {
                        <span class="muted small">{move || last().unwrap_or_else(|| tr("aucun", "none").to_string())}</span>
                        <button disabled=move || last().is_none() on:click=forget_last>{tr("Oublier", "Forget")}</button>
                    },
                )}
                {row(
                    tr("Choix Nginx / NPM par serveur", "Nginx / NPM choice per server"),
                    "",
                    view! {
                        <span class="muted small">{move || nginx_keys().len()}</span>
                        <button disabled=move || nginx_keys().is_empty() on:click=reset_nginx>{tr("Réinitialiser", "Reset")}</button>
                    },
                )}
                {row(
                    tr("Réglages", "Settings"),
                    tr("Apparence, Dock, sécurité.", "Appearance, Dock, security."),
                    view! { <button on:click=reset_prefs>{tr("Valeurs par défaut", "Defaults")}</button> },
                )}
            },
        )}
        {group(
            tr("Tout effacer", "Erase everything"),
            view! {
                {row(
                    tr("Données locales de sshOs", "sshOs local data"),
                    tr(
                        "Les serveurs enregistrés et leurs secrets (trousseau du système) ne sont pas concernés.",
                        "Saved servers and their secrets (OS keychain) are not affected.",
                    ),
                    view! {
                        <span class="muted small">{move || format!("{} {}", keys().len(), tr("élément(s)", "item(s)"))}</span>
                        <button class="danger" on:click=wipe>{tr("Effacer…", "Erase…")}</button>
                    },
                )}
            },
        )}
    }
}
