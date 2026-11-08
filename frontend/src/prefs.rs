//! User preferences (Settings app): appearance and security options, saved in localStorage.
//!
//! The safety rules inherited from Davish (no silent sudo, exact command shown, `nginx -t` /
//! `fail2ban-client -t` before reloads, NPM only through its API) are NOT preferences: they
//! cannot be turned off and the Settings app only shows them.

use serde::{Deserialize, Serialize};

use crate::state::{storage_get, storage_set, tr};

pub const PREFS_KEY: &str = "sshos.settings";

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    Dark,
    Light,
    /// Follows the OS (prefers-color-scheme).
    Auto,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Accent {
    Blue,
    Purple,
    Pink,
    Red,
    Orange,
    Green,
    Graphite,
}

impl Accent {
    pub const ALL: [Accent; 7] = [Accent::Blue, Accent::Purple, Accent::Pink, Accent::Red, Accent::Orange, Accent::Green, Accent::Graphite];
    pub fn id(self) -> &'static str {
        match self {
            Accent::Blue => "blue",
            Accent::Purple => "purple",
            Accent::Pink => "pink",
            Accent::Red => "red",
            Accent::Orange => "orange",
            Accent::Green => "green",
            Accent::Graphite => "graphite",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Accent::Blue => tr("Bleu", "Blue"),
            Accent::Purple => tr("Violet", "Purple"),
            Accent::Pink => tr("Rose", "Pink"),
            Accent::Red => tr("Rouge", "Red"),
            Accent::Orange => "Orange",
            Accent::Green => tr("Vert", "Green"),
            Accent::Graphite => tr("Graphite", "Graphite"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Wallpaper {
    Aurora,
    Ocean,
    Sunset,
    Forest,
    Plain,
}

impl Wallpaper {
    pub const ALL: [Wallpaper; 5] = [Wallpaper::Aurora, Wallpaper::Ocean, Wallpaper::Sunset, Wallpaper::Forest, Wallpaper::Plain];
    pub fn id(self) -> &'static str {
        match self {
            Wallpaper::Aurora => "aurora",
            Wallpaper::Ocean => "ocean",
            Wallpaper::Sunset => "sunset",
            Wallpaper::Forest => "forest",
            Wallpaper::Plain => "plain",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Wallpaper::Aurora => tr("Aurore", "Aurora"),
            Wallpaper::Ocean => tr("Océan", "Ocean"),
            Wallpaper::Sunset => tr("Crépuscule", "Sunset"),
            Wallpaper::Forest => tr("Forêt", "Forest"),
            Wallpaper::Plain => tr("Uni", "Plain"),
        }
    }
}

/// How see-through windows, Dock and menus are.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Glass {
    Clear,
    Normal,
    Opaque,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DockSize {
    Small,
    Medium,
    Large,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    // --- appearance
    pub theme: ThemeMode,
    pub accent: Accent,
    pub wallpaper: Wallpaper,
    pub glass: Glass,
    pub dock_size: DockSize,
    pub dock_magnify: bool,
    pub reduce_motion: bool,
    pub menu_stats: bool,
    pub clock_seconds: bool,
    // --- security
    /// Reconnect the last active server at launch.
    pub auto_reconnect: bool,
    /// Disconnect every server after this many minutes without keyboard/mouse activity (0 = never).
    pub idle_disconnect_min: u32,
    /// Blur user@host addresses (screen sharing, demos).
    pub hide_addresses: bool,
    /// Ask before closing sshOs.
    pub confirm_quit: bool,
    // --- dock
    /// Apps kept in the Dock (catalog ids, see launcher.rs), in order.
    pub dock: Vec<String>,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            theme: ThemeMode::Dark,
            accent: Accent::Blue,
            wallpaper: Wallpaper::Aurora,
            glass: Glass::Normal,
            dock_size: DockSize::Medium,
            dock_magnify: true,
            reduce_motion: false,
            menu_stats: true,
            clock_seconds: false,
            auto_reconnect: true,
            idle_disconnect_min: 0,
            hide_addresses: false,
            confirm_quit: true,
            dock: crate::launcher::DEFAULT_DOCK.iter().map(|s| s.to_string()).collect(),
        }
    }
}

impl Prefs {
    pub fn load() -> Self {
        if let Some(p) = storage_get(PREFS_KEY).and_then(|s| serde_json::from_str::<Prefs>(&s).ok()) {
            return p;
        }
        // Older versions only stored the theme.
        let mut p = Prefs::default();
        if storage_get("sshos.theme").as_deref() == Some("light") {
            p.theme = ThemeMode::Light;
        }
        p
    }

    pub fn save(&self) {
        if let Ok(s) = serde_json::to_string(self) {
            storage_set(PREFS_KEY, &s);
        }
    }

    /// Dark or not, given what the OS prefers (for `Auto`).
    pub fn is_dark(&self, os_dark: bool) -> bool {
        match self.theme {
            ThemeMode::Dark => true,
            ThemeMode::Light => false,
            ThemeMode::Auto => os_dark,
        }
    }

    /// `data-*` attributes set on <html>; the stylesheet does the rest.
    pub fn attributes(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("data-accent", self.accent.id()),
            ("data-wallpaper", self.wallpaper.id()),
            (
                "data-glass",
                match self.glass {
                    Glass::Clear => "clear",
                    Glass::Normal => "normal",
                    Glass::Opaque => "opaque",
                },
            ),
            (
                "data-dock",
                match self.dock_size {
                    DockSize::Small => "small",
                    DockSize::Medium => "medium",
                    DockSize::Large => "large",
                },
            ),
            ("data-magnify", if self.dock_magnify { "on" } else { "off" }),
            ("data-motion", if self.reduce_motion { "reduced" } else { "full" }),
            ("data-privacy", if self.hide_addresses { "on" } else { "off" }),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefs_roundtrip_and_defaults() {
        let p = Prefs { accent: Accent::Green, idle_disconnect_min: 15, ..Prefs::default() };
        let s = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<Prefs>(&s).unwrap(), p);
        // Missing fields (older or newer versions) fall back to the defaults.
        let partial: Prefs = serde_json::from_str(r#"{"theme":"auto"}"#).unwrap();
        assert_eq!(partial.theme, ThemeMode::Auto);
        assert!(partial.auto_reconnect);
        assert_eq!(partial.accent, Accent::Blue);
    }

    #[test]
    fn auto_theme_follows_os() {
        let p = Prefs { theme: ThemeMode::Auto, ..Prefs::default() };
        assert!(p.is_dark(true));
        assert!(!p.is_dark(false));
        assert!(Prefs::default().is_dark(false));
    }
}
