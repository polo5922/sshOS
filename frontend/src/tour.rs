//! Quick tour: highlights one zone of the desktop at a time (a hole in a dimmed overlay) with a
//! card that explains it. Started from the sshOs menu, and once after the welcome assistant.

use leptos::prelude::*;

use crate::icons::Icon;
use crate::state::*;

/// Set once the tour was completed or skipped.
pub const TOUR_KEY: &str = "sshos.tour_done";

struct Step {
    /// CSS selector of the zone to highlight; `None` centers the card.
    sel: Option<&'static str>,
    icon: &'static str,
    title: fn() -> &'static str,
    text: fn() -> &'static str,
}

const STEPS: &[Step] = &[
    Step {
        sel: None,
        icon: "server",
        title: || tr("Bienvenue sur sshOs", "Welcome to sshOs"),
        text: || tr(
            "Une visite rapide des zones du bureau. Vous pouvez la relancer à tout moment depuis le menu sshOs.",
            "A quick look at the desktop. You can start it again at any time from the sshOs menu.",
        ),
    },
    Step {
        sel: Some("[data-tour=host]"),
        icon: "server",
        title: || tr("Serveur actif", "Active server"),
        text: || tr(
            "Le serveur sur lequel travaillent les apps. Cliquez pour en changer, vous connecter à un autre ou vous déconnecter.",
            "The server the apps work on. Click to switch, connect another one or disconnect.",
        ),
    },
    Step {
        sel: Some("[data-tour=menus]"),
        icon: "list",
        title: || tr("Menus de l'app", "App menus"),
        text: || tr(
            "Comme sur macOS, la barre du haut affiche les menus de la fenêtre au premier plan : actions, présentation, raccourcis.",
            "Like on macOS, the top bar shows the menus of the frontmost window: actions, view options, shortcuts.",
        ),
    },
    Step {
        sel: Some(".live"),
        icon: "activity",
        title: || tr("Ressources en direct", "Live resources"),
        text: || tr(
            "CPU, mémoire, disque et débit réseau du serveur actif. Cliquez pour ouvrir le Moniteur.",
            "CPU, memory, disk and network speed of the active server. Click to open the Monitor.",
        ),
    },
    Step {
        sel: Some("[data-tour=bell]"),
        icon: "bell",
        title: || tr("Notifications", "Notifications"),
        text: || tr(
            "Alertes du serveur (disque plein, service en échec, conteneur arrêté…) et résultats des actions.",
            "Server alerts (full disk, failed service, stopped container…) and results of your actions.",
        ),
    },
    Step {
        sel: Some("[data-tour=spotlight]"),
        icon: "search",
        title: || tr("Spotlight", "Spotlight"),
        text: || tr(
            "Ctrl+K : ouvrez une app, un serveur ou un réglage en tapant quelques lettres.",
            "Ctrl+K: open an app, a server or a setting by typing a few letters.",
        ),
    },
    Step {
        sel: Some("[data-tour=lang]"),
        icon: "globe",
        title: || tr("Langue", "Language"),
        text: || tr(
            "Français, English, Deutsch ou Español. Par défaut, la langue du système.",
            "Français, English, Deutsch or Español. Defaults to the system language.",
        ),
    },
    Step {
        sel: Some("[data-tour=dock]"),
        icon: "layers",
        title: || tr("Le Dock", "The Dock"),
        text: || tr(
            "Vos apps favorites ; les apps ouvertes s'ajoutent après le séparateur. Clic droit sur une icône : ses fenêtres, Garder dans le Dock / Retirer du Dock.",
            "Your favourite apps; open apps appear after the separator. Right-click an icon: its windows, Keep in Dock / Remove from Dock.",
        ),
    },
    Step {
        sel: Some("[data-tour=launcher]"),
        icon: "grid",
        title: || tr("Applications", "Applications"),
        text: || tr(
            "Toutes les apps, rangées par catégorie, avec une recherche (F4). Le bouton + d'une app l'ajoute au Dock.",
            "Every app, sorted by category, with a search (F4). The + button of an app adds it to the Dock.",
        ),
    },
    Step {
        sel: None,
        icon: "lock",
        title: || tr("Rien ne se fait dans votre dos", "Nothing happens behind your back"),
        text: || tr(
            "Chaque commande est affichée avant d'être lancée, sudo est toujours tapé dans un terminal sans être exécuté, et l'app Historique liste tout ce que sshOs a exécuté.",
            "Every command is shown before it runs, sudo is always typed in a terminal without being run, and the Command log app lists everything sshOs ran.",
        ),
    },
];

#[derive(Clone, Copy, PartialEq)]
struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

fn rect_of(sel: &str) -> Option<Rect> {
    let el = document().query_selector(sel).ok()??;
    let r = el.get_bounding_client_rect();
    (r.width() > 0.0 && r.height() > 0.0).then(|| Rect { x: r.left(), y: r.top(), w: r.width(), h: r.height() })
}

fn viewport() -> (f64, f64) {
    let w = window();
    let iw = w.inner_width().ok().and_then(|v| v.as_f64()).unwrap_or(1280.0);
    let ih = w.inner_height().ok().and_then(|v| v.as_f64()).unwrap_or(800.0);
    (iw, ih)
}

#[component]
pub fn Tour() -> impl IntoView {
    let c = ctx();
    let tick = RwSignal::new(0u32);
    let resize = window_event_listener(leptos::ev::resize, move |_| tick.update(|t| *t += 1));
    on_cleanup(move || resize.remove());

    let finish = move || {
        storage_set(TOUR_KEY, "1");
        c.tour.set(None);
    };
    let idx = move || c.tour.get().unwrap_or(0).min(STEPS.len() - 1);
    let go = move |d: isize| {
        let i = idx() as isize + d;
        if i < 0 {
            return;
        }
        if i as usize >= STEPS.len() {
            finish();
        } else {
            c.tour.set(Some(i as usize));
        }
    };
    let keys = window_event_listener(leptos::ev::keydown, move |e| match e.key().as_str() {
        "Escape" => finish(),
        "ArrowRight" | "Enter" => go(1),
        "ArrowLeft" => go(-1),
        _ => {}
    });
    on_cleanup(move || keys.remove());

    // Zone to highlight (padded), recomputed on step change and window resize.
    let hole = Memo::new(move |_| {
        tick.track();
        STEPS[idx()].sel.and_then(rect_of).map(|r| Rect { x: r.x - 6.0, y: r.y - 6.0, w: r.w + 12.0, h: r.h + 12.0 })
    });
    let hole_style = move || match hole.get() {
        Some(r) => format!("left:{}px;top:{}px;width:{}px;height:{}px", r.x, r.y, r.w, r.h),
        // No zone: a zero-size hole in the middle keeps the dimmed backdrop.
        None => {
            let (w, h) = viewport();
            format!("left:{}px;top:{}px;width:0;height:0", w / 2.0, h / 2.0)
        }
    };
    // Card below the zone, or above it when the zone sits at the bottom (Dock).
    let card_style = move || {
        const CW: f64 = 340.0;
        let (vw, vh) = viewport();
        match hole.get() {
            Some(r) => {
                let x = (r.x + r.w / 2.0 - CW / 2.0).clamp(12.0, (vw - CW - 12.0).max(12.0));
                if r.y + r.h / 2.0 > vh / 2.0 {
                    format!("left:{x}px;bottom:{}px;width:{CW}px", vh - r.y + 14.0)
                } else {
                    format!("left:{x}px;top:{}px;width:{CW}px", r.y + r.h + 14.0)
                }
            }
            None => format!("left:{}px;top:{}px;width:{CW}px", (vw - CW) / 2.0, vh / 2.0 - 110.0),
        }
    };

    view! {
        <div class="tour" on:click=move |e| e.stop_propagation()>
            <div class="tour-hole" class:empty=move || hole.get().is_none() style=hole_style></div>
            <div class="tour-card" style=card_style>
                {move || {
                    let i = idx();
                    let s = &STEPS[i];
                    view! {
                        <div class="tour-head">
                            <span class="tour-icon"><Icon name=s.icon size=16 /></span>
                            <b class="grow">{(s.title)()}</b>
                            <span class="muted small">{format!("{} / {}", i + 1, STEPS.len())}</span>
                        </div>
                        <p>{(s.text)()}</p>
                        <div class="tour-dots">
                            {(0..STEPS.len()).map(|j| view! { <span class:on=j == i></span> }).collect_view()}
                        </div>
                    }
                }}
                <div class="tour-actions">
                    <button class="link" on:click=move |_| finish()>{tr("Passer", "Skip")}</button>
                    <span class="grow"></span>
                    <button disabled=move || idx() == 0 on:click=move |_| go(-1)>{tr("Précédent", "Back")}</button>
                    <button class="primary" on:click=move |_| go(1)>
                        {move || if idx() + 1 >= STEPS.len() { tr("Terminer", "Done") } else { tr("Suivant", "Next") }}
                    </button>
                </div>
            </div>
        </div>
    }
}
