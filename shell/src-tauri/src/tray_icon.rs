//! The tray icon: DigUp's logo (Resources/AppIcon/tray.svg and its tray*.png, as the Mac app's menubar shows it). While
//! the model downloads or files index, the tiles are dimmed and get their color back in a sweep from the top, clockwise,
//! as far as it has come; while indexing is paused, two bars take the D's place.

use std::f64::consts::TAU;
use tauri::image::Image;

const LOGO: &[u8] = include_bytes!("../../../Resources/AppIcon/tray.png");
const DIMMED: &[u8] = include_bytes!("../../../Resources/AppIcon/tray-dim.png");
const PAUSED: &[u8] = include_bytes!("../../../Resources/AppIcon/tray-paused.png");

/// What the icon shows: progress (0…1) or none, and whether indexing is paused.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    /// In steps of a twenty-fourth of a turn: a tray icon redraws only when the sweep visibly moves.
    pub step: Option<u8>,
    pub paused: bool,
}

impl Look {
    pub fn new(progress: Option<f64>, paused: bool) -> Look {
        Look { step: progress.map(|progress| (progress.clamp(0.0, 1.0) * 24.0).round() as u8), paused }
    }
}

pub fn image(look: Look) -> Image<'static> {
    let picture = |png: &[u8]| Image::from_bytes(png).expect("the tray's pictures are PNGs");
    if look.paused {
        return picture(PAUSED);
    }
    let Some(step) = look.step else { return picture(LOGO) };
    let (logo, dimmed) = (picture(LOGO), picture(DIMMED));
    let (width, height) = (logo.width(), logo.height());
    let reached = f64::from(step) / 24.0;
    let mut rgba = dimmed.rgba().to_vec();
    for (index, pixel) in rgba.chunks_mut(4).enumerate() {
        let x = (index as u32 % width) as f64 + 0.5 - f64::from(width) / 2.0;
        let y = (index as u32 / width) as f64 + 0.5 - f64::from(height) / 2.0;
        // How far round from the top, clockwise (0…1).
        if (x.atan2(-y) / TAU).rem_euclid(1.0) < reached {
            pixel.copy_from_slice(&logo.rgba()[index * 4..index * 4 + 4]);
        }
    }
    Image::new_owned(rgba, width, height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_each_look() {
        let pixels = |look: Look| image(look).rgba().to_vec();
        let idle = pixels(Look::new(None, false));
        let none = pixels(Look::new(Some(0.0), false));
        let paused = pixels(Look::new(None, true));
        assert_ne!(idle, none);
        assert_ne!(none, paused);
        assert_eq!(pixels(Look::new(Some(1.0), false)), idle);   // all the way round: the logo
        // Half way: the right half is the logo's, the left half still dimmed.
        let half = pixels(Look::new(Some(0.5), false));
        let width = image(Look::new(None, false)).width() as usize;
        let at = |rgba: &[u8], x: usize, y: usize| rgba[(y * width + x) * 4..(y * width + x) * 4 + 4].to_vec();
        assert_eq!(at(&half, width * 3 / 4, width / 4), at(&idle, width * 3 / 4, width / 4));
        assert_eq!(at(&half, width / 4, width * 3 / 4), at(&none, width / 4, width * 3 / 4));
        assert_eq!(Look::new(Some(0.5), false), Look::new(Some(0.51), false));
    }
}
