//! The tray icon, drawn here from the Mac app's menubar mark (Sources/DigUpApp/StatusMenu.swift: a boxed D on a 64-unit
//! grid): while the model downloads or files index, a ring shows how far along it is in place of the box; while indexing
//! is paused, two bars take the D's place. One color, as trays draw their icons: white on Linux's dark panels, black or
//! white on Windows as its taskbar is light or dark, black on a Mac (which colors a template itself).

use tauri::image::Image;

/// What the icon shows: progress (0…1) or none, and whether indexing is paused.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    /// In steps of a twenty-fourth of a turn: a tray icon redraws only when the ring visibly moves.
    pub step: Option<u8>,
    pub paused: bool,
}

impl Look {
    pub fn new(progress: Option<f64>, paused: bool) -> Look {
        Look { step: progress.map(|progress| (progress.clamp(0.0, 1.0) * 24.0).round() as u8), paused }
    }
}

const SIZE: u32 = 32;
const SAMPLES: u32 = 4;

pub fn image(look: Look) -> Image<'static> {
    let color = color();
    let scale = 64.0 / SIZE as f64;
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let mut alpha = 0.0;
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let px = (x as f64 + (sx as f64 + 0.5) / SAMPLES as f64) * scale;
                    let py = (y as f64 + (sy as f64 + 0.5) / SAMPLES as f64) * scale;
                    alpha += coverage(look, px, py);
                }
            }
            alpha /= (SAMPLES * SAMPLES) as f64;
            rgba.extend_from_slice(&[color, color, color, (alpha * 255.0).round() as u8]);
        }
    }
    Image::new_owned(rgba, SIZE, SIZE)
}

/// How opaque the mark is at (x, y) on the 64-unit grid, y down.
fn coverage(look: Look, x: f64, y: f64) -> f64 {
    let mut alpha: f64 = 0.0;
    match look.step {
        Some(step) => {
            // The ring (radius 27, 5.3 wide): a faint track, and the part done from twelve o'clock clockwise, with
            // round ends (a sliver at the least).
            let (dx, dy) = (x - 32.0, y - 32.0);
            let on_ring = ((dx * dx + dy * dy).sqrt() - 27.0).abs() <= 2.65;
            if on_ring {
                alpha = 0.3;
            }
            let done = (step as f64 / 24.0).max(0.03);
            let angle = (dx.atan2(-dy) / std::f64::consts::TAU).rem_euclid(1.0);   // 0 at the top, clockwise
            let end = |turn: f64| {
                let (cx, cy) = (32.0 + 27.0 * (std::f64::consts::TAU * turn).sin(), 32.0 - 27.0 * (std::f64::consts::TAU * turn).cos());
                (x - cx).powi(2) + (y - cy).powi(2) <= 2.65 * 2.65
            };
            if on_ring && angle <= done || end(0.0) || end(done) {
                alpha = 1.0;
            }
        }
        None => {
            // The box (55 wide at 4.5, corners of 12, a 4.5 stroke), with a tick in the middle of each side (the
            // icon's four tiles).
            if rounded_rect(x, y, 2.25, 2.25, 61.75, 61.75, 14.25) && !rounded_rect(x, y, 6.75, 6.75, 57.25, 57.25, 9.75) {
                alpha = 1.0;
            }
            let vertical = (x - 32.0).abs() <= 2.25 && (5.0..=16.0).contains(&y) || (x - 32.0).abs() <= 2.25 && (48.0..=59.0).contains(&y);
            let horizontal = (y - 32.0).abs() <= 2.25 && ((5.0..=17.0).contains(&x) || (47.0..=59.0).contains(&x));
            if vertical || horizontal {
                alpha = 1.0;
            }
        }
    }
    if look.paused && look.step.is_none() {
        for left in [23.5, 34.5] {
            if rounded_rect(x, y, left, 21.0, left + 6.0, 43.0, 1.8) {
                alpha = 1.0;
            }
        }
    } else {
        // The D: a stem (rounded on the left) and a half circle of radius 11 around (30, 32).
        let corner = |cy: f64| (x - 25.0).powi(2) + (y - cy).powi(2) <= 4.0;
        let stem = (23.0..=30.5).contains(&x) && (21.0..=43.0).contains(&y)
            && (x >= 25.0 || (23.0..=41.0).contains(&y) && y >= 23.0 || y < 23.0 && corner(23.0) || y > 41.0 && corner(41.0));
        let bowl = x >= 30.0 && (x - 30.0).powi(2) + (y - 32.0).powi(2) <= 11.0 * 11.0;
        if stem || bowl {
            alpha = 1.0;
        }
    }
    alpha
}

/// (x, y) inside the rectangle from (left, top) to (right, bottom) with corners of `radius`.
fn rounded_rect(x: f64, y: f64, left: f64, top: f64, right: f64, bottom: f64, radius: f64) -> bool {
    if x < left || x > right || y < top || y > bottom {
        return false;
    }
    let cx = x.clamp(left + radius, right - radius);
    let cy = y.clamp(top + radius, bottom - radius);
    (x - cx).powi(2) + (y - cy).powi(2) <= radius * radius
}

/// The mark's color: black on a Mac (a template), on a light Windows taskbar and on a light KDE panel, white elsewhere.
fn color() -> u8 {
    if cfg!(target_os = "macos") {
        return 0;
    }
    #[cfg(windows)]
    if light_taskbar() {
        return 0;
    }
    #[cfg(target_os = "linux")]
    if light_kde_panel() {
        return 0;
    }
    255
}

/// Linux: KDE Plasma's panel is light (its default, Breeze with the Breeze Light colors). GNOME's top bar is dark whatever
/// the theme, and other desktops' panels are taken for dark. Looked at once.
#[cfg(target_os = "linux")]
fn light_kde_panel() -> bool {
    static LIGHT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *LIGHT.get_or_init(|| {
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
        let config = std::env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()).map(std::path::PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".config")));
        config.is_some_and(|config| kde_panel_is_light(&desktop, &config))
    })
}

/// The Plasma style decides (plasmarc's [Theme] name); its default follows the color scheme, whose windows' background
/// (kdeglobals' [Colors:Window] BackgroundNormal) says light or dark, and with none set it's Breeze Light.
#[cfg(any(target_os = "linux", test))]
fn kde_panel_is_light(desktop: &str, config: &std::path::Path) -> bool {
    if !desktop.split(':').any(|name| name.eq_ignore_ascii_case("KDE")) {
        return false;
    }
    let style = ini_value(&config.join("plasmarc"), "Theme", "name").unwrap_or_else(|| "default".into());
    match style.as_str() {
        "breeze-dark" | "oxygen" => false,
        "breeze-light" | "air" => true,
        _ => ini_value(&config.join("kdeglobals"), "Colors:Window", "BackgroundNormal")
            .and_then(|rgb| {
                let parts: Vec<f64> = rgb.split(',').filter_map(|part| part.trim().parse().ok()).collect();
                (parts.len() >= 3).then(|| 0.299 * parts[0] + 0.587 * parts[1] + 0.114 * parts[2])
            })
            .map_or(true, |brightness| brightness > 128.0),
    }
}

/// A value in an INI-style file (KDE's config files): `key=value` under `[group]`.
#[cfg(any(target_os = "linux", test))]
fn ini_value(file: &std::path::Path, group: &str, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(file).ok()?;
    let mut inside = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            inside = line == format!("[{group}]");
        } else if inside {
            if let Some((name, value)) = line.split_once('=') {
                if name.trim() == key {
                    return Some(value.trim().to_string());
                }
            }
        }
    }
    None
}

/// Windows: the taskbar is light (Settings → Personalization → Colors, "Choose your default Windows mode").
#[cfg(windows)]
fn light_taskbar() -> bool {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
    let wide = |text: &str| text.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let name = wide("SystemUsesLightTheme");
    let mut value: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: the strings are NUL-terminated and live through the call; `value` is a DWORD of `size` bytes.
    let status = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr(), RRF_RT_REG_DWORD, std::ptr::null_mut(),
                     &mut value as *mut u32 as *mut _, &mut size)
    };
    status == 0 && value == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kde_panels_light_or_dark() {
        let config = std::env::temp_dir().join(format!("digup-kde-{}", std::process::id()));
        std::fs::create_dir_all(&config).unwrap();
        // No style or colors set: Breeze Light. GNOME: dark whatever the files say.
        assert!(kde_panel_is_light("KDE", &config));
        assert!(!kde_panel_is_light("ubuntu:GNOME", &config));
        std::fs::write(config.join("kdeglobals"), "[General]\nColorScheme=BreezeDark\n\n[Colors:Window]\nBackgroundNormal=32,35,38\n").unwrap();
        assert!(!kde_panel_is_light("KDE", &config));
        std::fs::write(config.join("plasmarc"), "[Theme]\nname=breeze-light\n").unwrap();
        assert!(kde_panel_is_light("KDE", &config));
        std::fs::remove_dir_all(config).unwrap();
    }

    #[test]
    fn draws_each_look() {
        let opaque = |look: Look| image(look).rgba().chunks(4).filter(|pixel| pixel[3] > 200).count();
        let idle = opaque(Look::new(None, false));
        let half = opaque(Look::new(Some(0.5), false));
        let paused = opaque(Look::new(None, true));
        assert!(idle > 100 && half > 50 && paused > 100, "{idle} {half} {paused}");
        assert_ne!(opaque(Look::new(Some(0.25), false)), half);
        assert_eq!(Look::new(Some(0.5), false), Look::new(Some(0.51), false));
    }
}
