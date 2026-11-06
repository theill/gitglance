//! Light or dark: picks the few colors that are drawn as backgrounds so they suit the terminal.
//!
//! The terminal is asked for its background color (OSC 11). Terminals that follow the system
//! appearance answer with whichever palette they show right now, so gitglance asks again every few
//! seconds and follows a switch while it runs. A terminal that doesn't answer is asked once; then
//! `COLORFGBG` or, on macOS, the system appearance decides.

use ratatui::style::Color;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use terminal_colorsaurus::{theme_mode, QueryOptions, ThemeMode};

static LIGHT: AtomicBool = AtomicBool::new(false);
static ANSWERS: AtomicBool = AtomicBool::new(false);

/// How often a terminal that answers is asked again.
pub const RECHECK: Duration = Duration::from_secs(3);

/// Whether the background is light.
pub fn light() -> bool {
    LIGHT.load(Ordering::Relaxed)
}

/// Background of the selected row: a step off the terminal's own background.
pub fn selected_bg() -> Color {
    if light() { Color::Indexed(254) } else { Color::Indexed(237) }
}

/// Asks the terminal at startup, before the TUI takes the screen. Falls back when it can't answer.
pub fn detect() {
    match theme_mode(QueryOptions::default()) {
        Ok(mode) => {
            ANSWERS.store(true, Ordering::Relaxed);
            LIGHT.store(mode == ThemeMode::Light, Ordering::Relaxed);
        }
        Err(_) => LIGHT.store(fallback_light(), Ordering::Relaxed),
    }
}

/// Asks again, if the terminal answered before. Returns true when light and dark flipped.
pub fn recheck() -> bool {
    if !ANSWERS.load(Ordering::Relaxed) {
        return false;
    }
    match theme_mode(QueryOptions::default()) {
        Ok(mode) => LIGHT.swap(mode == ThemeMode::Light, Ordering::Relaxed) != (mode == ThemeMode::Light),
        Err(_) => {
            // Stop asking, so a terminal that has gone quiet never gets a late answer read as keys.
            ANSWERS.store(false, Ordering::Relaxed);
            false
        }
    }
}

fn fallback_light() -> bool {
    if let Some(light) = std::env::var("COLORFGBG").ok().as_deref().and_then(colorfgbg_light) {
        return light;
    }
    system_light()
}

/// `COLORFGBG` is `fg;bg` (sometimes `fg;default;bg`) in ANSI color numbers; 7 and 9–15 are light.
fn colorfgbg_light(v: &str) -> Option<bool> {
    let bg: u8 = v.rsplit(';').next()?.parse().ok()?;
    Some(bg == 7 || (9..=15).contains(&bg))
}

#[cfg(target_os = "macos")]
fn system_light() -> bool {
    // Prints "Dark" in dark mode and fails (no such key) in light mode.
    std::process::Command::new("defaults")
        .args(["read", "-g", "AppleInterfaceStyle"])
        .output()
        .map(|o| !String::from_utf8_lossy(&o.stdout).contains("Dark"))
        .unwrap_or(false)
}

#[cfg(not(target_os = "macos"))]
fn system_light() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colorfgbg_reads_the_last_field_as_the_background() {
        assert_eq!(colorfgbg_light("15;0"), Some(false));
        assert_eq!(colorfgbg_light("0;15"), Some(true));
        assert_eq!(colorfgbg_light("0;default;7"), Some(true));
        assert_eq!(colorfgbg_light("12;8"), Some(false));
        assert_eq!(colorfgbg_light("junk"), None);
    }
}
