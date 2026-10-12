//! The panel's shortcut. Windows, macOS and X11 desktops let an app grab a key combination for itself (Tauri's global
//! shortcut). Wayland doesn't let any app do that: there a shortcut of the desktop's own runs `DigUp --toggle`, which
//! asks the DigUp already running to show or hide the panel. On GNOME (Wayland or X11) DigUp adds that shortcut itself,
//! as a custom shortcut in GNOME's keyboard settings, where the person can change or remove it; on other Wayland
//! desktops the person adds the command to their desktop's shortcuts.

use std::path::PathBuf;

/// How the shortcut reaches DigUp on this desktop.
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))] // Windows and macOS always grab it
pub enum Method {
    /// DigUp grabs the keys itself.
    Grab,
    /// A custom shortcut in GNOME's keyboard settings runs `--toggle`.
    Gnome,
    /// The desktop lets no app set a shortcut: the person adds one that runs `--toggle`.
    ByHand,
}

impl Method {
    pub fn name(self) -> &'static str {
        match self {
            Method::Grab => "grab",
            Method::Gnome => "gnome",
            Method::ByHand => "by_hand",
        }
    }
}

pub fn method() -> Method {
    #[cfg(target_os = "linux")]
    {
        if gnome::available() {
            return Method::Gnome;
        }
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some_and(|display| !display.is_empty())
            || std::env::var("XDG_SESSION_TYPE").is_ok_and(|session| session == "wayland");
        if wayland {
            return Method::ByHand;
        }
    }
    Method::Grab
}

/// What a desktop shortcut runs: this program (the AppImage itself, when it runs from one) with `--toggle`.
pub fn toggle_command() -> String {
    let program = std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| PathBuf::from("digup"));
    format!("{} --toggle", quoted(&program.display().to_string()))
}

/// A word as a shell reads it (GNOME splits a shortcut's command the same way).
fn quoted(word: &str) -> String {
    if !word.is_empty() && word.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+:@%,=".contains(c)) {
        return word.to_string();
    }
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// `--toggle` while DigUp runs: the toggle goes to it over D-Bus, as a second launch's single-instance plugin would
/// send it, but before this process starts GTK (the panel shows ~35 ms sooner). False when no DigUp answers.
#[cfg(target_os = "linux")]
pub fn hand_over() -> bool {
    // tauri-plugin-single-instance 2.5's object: the app's identifier + ".SingleInstance", and that as a path.
    const NAME: &str = "com.abdurrahim.DigUp.SingleInstance";
    const PATH: &str = "/com/abdurrahim/DigUp/SingleInstance";
    let Ok(connection) = zbus::blocking::Connection::session() else { return false };
    let args: Vec<String> = std::env::args().collect();
    let folder = std::env::current_dir().map(|folder| folder.display().to_string()).unwrap_or_default();
    connection.call_method(Some(NAME), PATH, Some("org.SingleInstance.DBus"), "ExecuteCallback", &(args, folder)).is_ok()
}

/// GNOME's custom shortcuts (Settings → Keyboard → View and Customize Shortcuts → Custom Shortcuts), which its
/// settings daemon runs on X11 and Wayland alike. DigUp keeps one entry of its own there.
#[cfg(target_os = "linux")]
pub mod gnome {
    use gio::prelude::*;

    const MEDIA_KEYS: &str = "org.gnome.settings-daemon.plugins.media-keys";
    const CUSTOM: &str = "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding";
    const OURS: &str = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/digup/";

    /// A GNOME session with its settings daemon's shortcut settings.
    pub fn available() -> bool {
        let gnome = std::env::var("XDG_CURRENT_DESKTOP")
            .is_ok_and(|desktops| desktops.split(':').any(|desktop| desktop.eq_ignore_ascii_case("GNOME")));
        gnome
            && gio::SettingsSchemaSource::default()
                .is_some_and(|source| source.lookup(MEDIA_KEYS, true).is_some() && source.lookup(CUSTOM, true).is_some())
    }

    fn listed(list: &gio::Settings) -> bool {
        list.strv("custom-keybindings").iter().any(|path| path.as_str() == OURS)
    }

    /// DigUp's shortcut as GNOME has it now ("<Control><Shift>space"), if it's there.
    pub fn binding() -> Option<String> {
        if !listed(&gio::Settings::new(MEDIA_KEYS)) {
            return None;
        }
        Some(gio::Settings::with_path(CUSTOM, OURS).string("binding").to_string()).filter(|binding| !binding.is_empty())
    }

    /// Adds DigUp's shortcut (`accelerator` in Tauri's form) running `command`. When it's there already, only the
    /// command is brought up to date: a key the person chose in GNOME's settings stays.
    pub fn bind(accelerator: &str, command: &str) -> Result<(), String> {
        let list = gio::Settings::new(MEDIA_KEYS);
        let ours = gio::Settings::with_path(CUSTOM, OURS);
        let set = |key: &str, value: &str| ours.set_string(key, value).map_err(|error| error.to_string());
        let new = !listed(&list);
        if new {
            set("binding", &to_gnome(accelerator)?)?;
        }
        set("name", "DigUp")?;
        set("command", command)?;
        if new {
            // Listed last, so GNOME never reads half an entry.
            let mut paths: Vec<String> = list.strv("custom-keybindings").iter().map(|path| path.to_string()).collect();
            paths.push(OURS.to_string());
            list.set_strv("custom-keybindings", paths.iter().map(String::as_str).collect::<Vec<_>>())
                .map_err(|error| error.to_string())?;
        }
        gio::Settings::sync();
        Ok(())
    }

    /// Brings the command of DigUp's shortcut up to date when it's there (DigUp moved: a new AppImage, a new install).
    pub fn refresh(command: &str) {
        if listed(&gio::Settings::new(MEDIA_KEYS)) {
            let ours = gio::Settings::with_path(CUSTOM, OURS);
            if ours.string("command") != command && ours.set_string("command", command).is_ok() {
                gio::Settings::sync();
            }
        }
    }

    /// Tauri's form of a shortcut ("CommandOrControl+Shift+Space") in GNOME's ("<Control><Shift>space").
    pub fn to_gnome(accelerator: &str) -> Result<String, String> {
        let parts: Vec<&str> = accelerator.split('+').map(str::trim).collect();
        let (key, modifiers) = parts.split_last().ok_or("no key")?;
        let mut text = String::new();
        for modifier in modifiers {
            text.push_str(match modifier.to_ascii_lowercase().as_str() {
                "commandorcontrol" | "cmdorctrl" | "control" | "ctrl" => "<Control>",
                "shift" => "<Shift>",
                "alt" | "option" => "<Alt>",
                "super" | "meta" | "command" | "cmd" => "<Super>",
                _ => return Err(format!("{accelerator}: {modifier} isn't a key GNOME knows")),
            });
        }
        let lower = key.to_ascii_lowercase();
        let named = match lower.as_str() {
            "space" => Some("space"),
            "enter" | "return" => Some("Return"),
            "tab" => Some("Tab"),
            "backquote" => Some("grave"),
            "slash" => Some("slash"),
            "period" => Some("period"),
            "comma" => Some("comma"),
            _ => None,
        };
        let function = lower.strip_prefix('f').and_then(|number| number.parse::<u8>().ok()).filter(|n| (1..=24).contains(n));
        let plain = lower.chars().count() == 1 && lower.chars().all(|c| c.is_ascii_alphanumeric());
        match (named, function) {
            (Some(name), _) => text.push_str(name),
            (None, Some(number)) => text.push_str(&format!("F{number}")),
            _ if plain => text.push_str(&lower),
            _ => return Err(format!("{accelerator}: {key} isn't a key GNOME knows")),
        }
        Ok(text)
    }

    /// GNOME's form in the words of the panel and setup: "<Control><Shift>space" → "Ctrl+Shift+Space".
    pub fn shown(binding: &str) -> String {
        let mut parts = Vec::new();
        let mut rest = binding;
        while let Some(end) = rest.strip_prefix('<').and_then(|inner| inner.find('>')) {
            parts.push(match &rest[1..=end] {
                "Control" | "Primary" | "Ctrl" => "Ctrl".to_string(),
                "Mod1" | "Alt" => "Alt".to_string(),
                "Mod4" | "Super" => "Super".to_string(),
                other => other.to_string(),
            });
            rest = &rest[end + 2..];
        }
        let mut key = rest.chars();
        parts.push(key.next().map(|first| first.to_uppercase().chain(key).collect()).unwrap_or_default());
        parts.join("+")
    }
}
