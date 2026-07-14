//! Bridges the active Omarchy theme into libadwaita's named colors.
//!
//! Omarchy publishes the current theme's palette at a stable path,
//! `~/.config/omarchy/current/theme/colors.toml` (a symlink that Omarchy
//! re-points when you switch themes). Calix already styles itself entirely
//! through libadwaita named colors (`@accent_bg_color`, `@window_fg_color`,
//! `@borders`, …), so matching the desktop theme is just a matter of reading
//! that palette and overriding those colors — in both spellings libadwaita
//! understands: the legacy `@define-color` names and the modern `:root`
//! custom properties (`--accent-bg-color`, …) that 1.6+ widgets read.
//!
//! This is read once at startup. Switching themes while Calix is open won't
//! recolor the running window until it's relaunched. On a machine without
//! Omarchy the file is simply absent and we fall back to stock Adwaita.

use gtk::glib;
use std::fmt::Write as _;

pub struct ThemeOverrides {
    /// CSS `@define-color` block overriding libadwaita's named colors.
    pub css: String,
    /// Whether the theme's background is dark, so the caller can force the
    /// matching libadwaita color scheme (symbolic icons and dark-aware
    /// widgets key off this rather than the overridden colors).
    pub dark: bool,
}

/// Only the keys we map. Everything is optional so a partial or unfamiliar
/// `colors.toml` degrades gracefully instead of failing the whole read.
#[derive(serde::Deserialize)]
struct Palette {
    accent: Option<String>,
    background: Option<String>,
    foreground: Option<String>,
    color1: Option<String>, // red   -> destructive / error
    color2: Option<String>, // green -> success
    color3: Option<String>, // yellow-ish -> warning
}

/// Reads the active Omarchy theme and returns libadwaita color overrides, or
/// `None` if Omarchy isn't present or the palette is unusable (in which case
/// the app keeps its stock Adwaita colors).
pub fn theme_overrides() -> Option<ThemeOverrides> {
    let path = glib::user_config_dir().join("omarchy/current/theme/colors.toml");
    let contents = std::fs::read_to_string(&path).ok()?;
    let palette: Palette = toml::from_str(&contents)
        .inspect_err(|e| eprintln!("calix: failed to parse {}: {e}", path.display()))
        .ok()?;

    // Accent, background and foreground are the load-bearing three; without
    // all of them there's nothing coherent to theme, so bail to defaults.
    let accent = palette.accent.as_deref().and_then(parse_hex)?;
    let background = palette.background.as_deref().and_then(parse_hex)?;
    let foreground = palette.foreground.as_deref().and_then(parse_hex)?;

    let dark = luminance(background) < 0.5;

    // Each named color is emitted in both spellings libadwaita understands:
    // the legacy `@define-color name` (which Calix's own CSS references, and
    // which libadwaita < 1.6 widgets read) and the modern `--name-color`
    // custom property in `:root` (which libadwaita >= 1.6 widgets read via
    // `var()`). Overriding only one leaves the other half of the UI on stock
    // Adwaita colors, so we set both to the same value.
    let mut legacy = String::new();
    let mut root = String::new();
    macro_rules! set {
        ($name:literal, $c:expr) => {{
            let hex = to_hex($c);
            let _ = writeln!(legacy, "@define-color {} {};", $name, hex);
            let _ = writeln!(root, "  --{}: {};", $name.replace('_', "-"), hex);
        }};
    }

    // Content surfaces sit flat on the theme background — matching Omarchy's
    // own terminal-derived, largely-flat look — while chrome (headerbar,
    // sidebar, popovers, cards, dialogs) is nudged a few percent off the
    // background so it separates without inventing colors the palette lacks.
    set!("window_bg_color", background);
    set!("window_fg_color", foreground);
    set!("view_bg_color", background);
    set!("view_fg_color", foreground);
    set!("headerbar_bg_color", elevate(background, 0.05, dark));
    set!("headerbar_fg_color", foreground);
    set!("sidebar_bg_color", elevate(background, 0.04, dark));
    set!("sidebar_fg_color", foreground);
    set!("card_bg_color", elevate(background, 0.06, dark));
    set!("card_fg_color", foreground);
    set!("popover_bg_color", elevate(background, 0.06, dark));
    set!("popover_fg_color", foreground);
    set!("dialog_bg_color", elevate(background, 0.05, dark));
    set!("dialog_fg_color", foreground);

    // Only the `*-bg`/`*-fg` pairs are set; libadwaita derives the standalone
    // text colors (`accent_color`, `destructive_color`, …) from these via
    // oklab, which keeps them legible on both light and dark themes.
    set!("accent_bg_color", accent);
    set!("accent_fg_color", on_color(accent));

    if let Some(c) = palette.color1.as_deref().and_then(parse_hex) {
        set!("destructive_bg_color", c);
        set!("destructive_fg_color", on_color(c));
        set!("error_bg_color", c);
        set!("error_fg_color", on_color(c));
    }
    if let Some(c) = palette.color2.as_deref().and_then(parse_hex) {
        set!("success_bg_color", c);
        set!("success_fg_color", on_color(c));
    }
    if let Some(c) = palette.color3.as_deref().and_then(parse_hex) {
        set!("warning_bg_color", c);
        set!("warning_fg_color", on_color(c));
    }

    // Grid lines: a faint wash of the foreground, matching how Adwaita defines
    // `borders` as low-alpha ink over the surface. Only the legacy name is
    // needed — Calix's grid CSS reads `@borders`, and libadwaita's own
    // `--border-color` already derives from the (now themed) foreground.
    let _ = writeln!(
        legacy,
        "@define-color borders rgba({}, {}, {}, 0.15);",
        foreground.r, foreground.g, foreground.b
    );

    let css = format!("{legacy}\n:root {{\n{root}}}\n");
    Some(ThemeOverrides { css, dark })
}

#[derive(Clone, Copy)]
struct Rgb {
    r: u8,
    g: u8,
    b: u8,
}

const WHITE: Rgb = Rgb {
    r: 255,
    g: 255,
    b: 255,
};
const BLACK: Rgb = Rgb { r: 0, g: 0, b: 0 };

fn parse_hex(s: &str) -> Option<Rgb> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    Some(Rgb {
        r: u8::from_str_radix(&s[0..2], 16).ok()?,
        g: u8::from_str_radix(&s[2..4], 16).ok()?,
        b: u8::from_str_radix(&s[4..6], 16).ok()?,
    })
}

fn to_hex(c: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
}

/// Perceived relative luminance in 0.0..=1.0 (Rec. 709 weights).
fn luminance(c: Rgb) -> f32 {
    (0.2126 * c.r as f32 + 0.7152 * c.g as f32 + 0.0722 * c.b as f32) / 255.0
}

/// Blend `a` toward `b`; `t` of 0.0 yields `a`, 1.0 yields `b`.
fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let lerp = |x: u8, y: u8| {
        (x as f32 + (y as f32 - x as f32) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Rgb {
        r: lerp(a.r, b.r),
        g: lerp(a.g, b.g),
        b: lerp(a.b, b.b),
    }
}

/// Raise a surface off the background: lighter on dark themes, darker on light
/// ones, so stacked chrome reads as elevated in either mode.
fn elevate(base: Rgb, level: f32, dark: bool) -> Rgb {
    mix(base, if dark { WHITE } else { BLACK }, level)
}

/// A legible ink color to place *on* `c` — black over light fills, white over
/// dark ones (the palette only gives us the fill, not its contrasting pair).
fn on_color(c: Rgb) -> Rgb {
    if luminance(c) > 0.6 { BLACK } else { WHITE }
}
