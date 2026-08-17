//! Bridges the active Omarchy theme into libadwaita's named colors.
//!
//! Omarchy publishes the current theme's palette at a stable path,
//! `~/.local/state/omarchy/current/theme/colors.toml` (a symlink that Omarchy
//! re-points when you switch themes; older installs kept it under
//! `~/.config/omarchy`, which is still checked). Calix already styles itself entirely
//! through libadwaita named colors (`@accent_bg_color`, `@window_fg_color`,
//! `@borders`, …), so matching the desktop theme is just a matter of reading
//! that palette and overriding those colors — in both spellings libadwaita
//! understands: the legacy `@define-color` names and the modern `:root`
//! custom properties (`--accent-bg-color`, …) that 1.6+ widgets read.
//!
//! This is read once at startup. Switching themes while Calix is open won't
//! recolor the running window until it's relaunched. On a machine without
//! Omarchy the file is simply absent and we fall back to stock Adwaita.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

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
///
/// Omarchy names the semantic hues (`red`, `green`, `yellow`); the aliases
/// accept the indexed spelling older palettes used, so either generation reads.
#[derive(serde::Deserialize)]
struct Palette {
    /// `"dark"` or `"light"` — the theme's own declaration of which it is,
    /// which beats inferring it from the background.
    mode: Option<String>,
    accent: Option<String>,
    background: Option<String>,
    foreground: Option<String>,
    #[serde(alias = "color1")]
    red: Option<String>, // -> destructive / error
    #[serde(alias = "color2")]
    green: Option<String>, // -> success
    #[serde(alias = "color3")]
    yellow: Option<String>, // -> warning
}

/// Reads the active Omarchy theme and returns libadwaita color overrides, or
/// `None` if Omarchy isn't present or the palette is unusable (in which case
/// the app keeps its stock Adwaita colors).
pub fn theme_overrides() -> Option<ThemeOverrides> {
    let path = palette_paths(&crate::xdg::state_home(), &crate::xdg::config_home())
        .into_iter()
        .find(|path| path.exists())?;
    let contents = std::fs::read_to_string(&path).ok()?;
    let overrides = overrides_from_toml(&contents);
    if overrides.is_none() {
        eprintln!("calix: no usable palette in {}", path.display());
    }
    overrides
}

/// Where `colors.toml` may live, newest layout first. Omarchy moved the
/// `current` symlink out of `~/.config/omarchy` and into `~/.local/state/omarchy`;
/// both are checked so the theming works either side of that move.
fn palette_paths(state_home: &Path, config_home: &Path) -> [PathBuf; 2] {
    [
        state_home.join("omarchy/current/theme/colors.toml"),
        config_home.join("omarchy/current/theme/colors.toml"),
    ]
}

fn overrides_from_toml(contents: &str) -> Option<ThemeOverrides> {
    let palette: Palette = toml::from_str(contents)
        .inspect_err(|e| eprintln!("calix: failed to parse Omarchy palette: {e}"))
        .ok()?;

    // Accent, background and foreground are the load-bearing three; without
    // all of them there's nothing coherent to theme, so bail to defaults.
    let accent = palette.accent.as_deref().and_then(parse_hex)?;
    let background = palette.background.as_deref().and_then(parse_hex)?;
    let foreground = palette.foreground.as_deref().and_then(parse_hex)?;

    // The theme says which it is; every other themed app on the desktop reads
    // that declaration, so we follow it rather than second-guessing from the
    // background. Luminance is only the fallback for a palette without `mode`.
    let dark = match palette.mode.as_deref().map(str::trim) {
        Some("dark") => true,
        Some("light") => false,
        _ => luminance(background) < 0.5,
    };

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

    if let Some(c) = palette.red.as_deref().and_then(parse_hex) {
        set!("destructive_bg_color", c);
        set!("destructive_fg_color", on_color(c));
        set!("error_bg_color", c);
        set!("error_fg_color", on_color(c));
    }
    if let Some(c) = palette.green.as_deref().and_then(parse_hex) {
        set!("success_bg_color", c);
        set!("success_fg_color", on_color(c));
    }
    if let Some(c) = palette.yellow.as_deref().and_then(parse_hex) {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The load-bearing part of a real Omarchy palette (Everforest), verbatim —
    /// including the key spellings, which are what this module has to match.
    const EVERFOREST: &str = r##"
mode = "dark"

accent = "#7fbbb3"
selection = "#3d484d"
muted = "#475258"

background = "#2d353b"
dark_background = "#21272c"
lighter_background = "#343f44"

foreground = "#d3c6aa"
light_foreground = "#9da9a0"

red = "#e67e80"
yellow = "#dbbc7f"
green = "#a7c080"
blue = "#7fbbb3"
"##;

    /// The value of a `@define-color` declaration in the emitted CSS.
    fn color(css: &str, name: &str) -> Option<String> {
        let prefix = format!("@define-color {name} ");
        css.lines().find_map(|line| {
            Some(
                line.trim()
                    .strip_prefix(&prefix)?
                    .trim_end_matches(';')
                    .to_string(),
            )
        })
    }

    #[test]
    fn the_three_load_bearing_colors_reach_the_surfaces_and_the_accent() {
        let overrides = overrides_from_toml(EVERFOREST).expect("a usable palette");
        assert_eq!(
            color(&overrides.css, "window_bg_color").as_deref(),
            Some("#2d353b")
        );
        assert_eq!(
            color(&overrides.css, "window_fg_color").as_deref(),
            Some("#d3c6aa")
        );
        assert_eq!(
            color(&overrides.css, "accent_bg_color").as_deref(),
            Some("#7fbbb3")
        );
    }

    #[test]
    fn a_themes_semantic_colors_map_to_the_libadwaita_roles() {
        let overrides = overrides_from_toml(EVERFOREST).expect("a usable palette");
        let css = &overrides.css;
        // Omarchy names these by hue, not by index.
        assert_eq!(
            color(css, "destructive_bg_color").as_deref(),
            Some("#e67e80")
        );
        assert_eq!(color(css, "error_bg_color").as_deref(), Some("#e67e80"));
        assert_eq!(color(css, "success_bg_color").as_deref(), Some("#a7c080"));
        assert_eq!(color(css, "warning_bg_color").as_deref(), Some("#dbbc7f"));
    }

    #[test]
    fn an_explicit_mode_wins_over_guessing_from_the_background() {
        // A theme can declare itself dark while carrying a light background
        // (Omarchy's own light themes do the reverse); the declaration is the
        // authority, since it's what every other themed app on the desktop reads.
        let palette = r##"
mode = "dark"
accent = "#7fbbb3"
background = "#eeeeee"
foreground = "#111111"
"##;
        assert!(overrides_from_toml(palette).expect("a usable palette").dark);
    }

    #[test]
    fn a_palette_without_a_mode_falls_back_to_the_backgrounds_luminance() {
        let light = r##"
accent = "#7fbbb3"
background = "#eeeeee"
foreground = "#111111"
"##;
        assert!(!overrides_from_toml(light).expect("a usable palette").dark);

        let dark = light.replace("#eeeeee", "#222222");
        assert!(overrides_from_toml(&dark).expect("a usable palette").dark);
    }

    #[test]
    fn a_palette_missing_a_load_bearing_color_yields_no_overrides() {
        // No accent: there's nothing coherent to theme, so the app keeps stock
        // Adwaita rather than half-recoloring itself.
        let palette = r##"
background = "#2d353b"
foreground = "#d3c6aa"
"##;
        assert!(overrides_from_toml(palette).is_none());
        assert!(overrides_from_toml("").is_none());
    }

    #[test]
    fn the_state_dir_layout_is_preferred_over_the_legacy_config_dir() {
        // Omarchy moved `current` from ~/.config/omarchy to ~/.local/state/omarchy;
        // the newer location has to win, or a machine carrying both reads a stale theme.
        let paths = palette_paths(
            Path::new("/home/ian/.local/state"),
            Path::new("/home/ian/.config"),
        );
        assert_eq!(
            paths[0],
            PathBuf::from("/home/ian/.local/state/omarchy/current/theme/colors.toml")
        );
        assert_eq!(
            paths[1],
            PathBuf::from("/home/ian/.config/omarchy/current/theme/colors.toml")
        );
    }

    #[test]
    fn parse_hex_accepts_both_spellings_and_case() {
        assert_eq!(to_hex(parse_hex("#ff8800").unwrap()), "#ff8800");
        // A leading '#' is optional, and casing is normalized on the way out.
        assert_eq!(to_hex(parse_hex("FF8800").unwrap()), "#ff8800");
        // Surrounding whitespace (as toml values sometimes carry) is trimmed.
        assert_eq!(to_hex(parse_hex("  #ff8800  ").unwrap()), "#ff8800");
    }

    #[test]
    fn parse_hex_rejects_malformed_input() {
        assert!(parse_hex("#fff").is_none()); // 3-digit shorthand unsupported
        assert!(parse_hex("#ff88000").is_none()); // too long
        assert!(parse_hex("#gg8800").is_none()); // non-hex digit
        assert!(parse_hex("").is_none());
    }

    #[test]
    fn to_hex_zero_pads_each_channel() {
        assert_eq!(to_hex(Rgb { r: 0, g: 5, b: 16 }), "#000510");
    }

    #[test]
    fn luminance_weights_green_over_red_over_blue() {
        assert_eq!(luminance(WHITE), 1.0);
        assert_eq!(luminance(BLACK), 0.0);
        let pure = |r, g, b| luminance(Rgb { r, g, b });
        // Rec. 709: for equal-intensity primaries, green reads brightest and
        // blue darkest — this ordering is what drives the dark/light decision.
        assert!(pure(0, 255, 0) > pure(255, 0, 0));
        assert!(pure(255, 0, 0) > pure(0, 0, 255));
    }

    #[test]
    fn mix_interpolates_between_endpoints() {
        assert_eq!(to_hex(mix(BLACK, WHITE, 0.0)), "#000000");
        assert_eq!(to_hex(mix(BLACK, WHITE, 1.0)), "#ffffff");
        // 0.5 lands on the rounded midpoint (127.5 rounds to 128).
        assert_eq!(to_hex(mix(BLACK, WHITE, 0.5)), "#808080");
    }

    #[test]
    fn elevate_lightens_on_dark_themes_and_darkens_on_light() {
        let gray = Rgb {
            r: 128,
            g: 128,
            b: 128,
        };
        assert!(luminance(elevate(gray, 0.1, true)) > luminance(gray));
        assert!(luminance(elevate(gray, 0.1, false)) < luminance(gray));
    }

    #[test]
    fn on_color_picks_a_legible_ink() {
        // Black text over a light fill, white text over a dark one.
        assert_eq!(to_hex(on_color(WHITE)), to_hex(BLACK));
        assert_eq!(to_hex(on_color(BLACK)), to_hex(WHITE));
    }
}
