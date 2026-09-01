use gtk::gdk;

const CSS: &str = "
/* ── Terminal grammar ──────────────────────────────────────────────────────
   Calix sits on an Omarchy desktop, where every other window is a terminal.
   The house style there is built from three things: a monospace face, 1px
   rules, and inverse video for emphasis — never rounding, gradients or
   elevation. These rules strip GTK's defaults back to that vocabulary; the
   layout rules further down were already line-based and mostly just work.

   This provider loads at APPLICATION priority, which outranks libadwaita's
   own THEME-priority rules regardless of selector specificity — which is why
   a bare `*` is enough to square everything off. */

* {
    border-radius: 0;
    text-shadow: none;
    -gtk-icon-shadow: none;
}

/* Elevation is the other half of the GTK look: chrome lifts off the page with
   a shadow. Flatten it, and let borders do the separating. Entries and buttons
   are included because libadwaita draws their outline as an inset shadow,
   which is replaced with a real border below. */
window,
headerbar,
popover > contents,
.card,
.boxed-list,
dialog,
toast,
.toast,
.osd,
button,
entry,
spinbutton {
    box-shadow: none;
}

/* Controls read as bracketed regions rather than raised chips: flat ground,
   one hairline, and inverse video on hover the way a selected line inverts. */
button {
    background-image: none;
    background-color: transparent;
    border: 1px solid @borders;
}

button:hover {
    background-color: alpha(@window_fg_color, 0.08);
}

button:active,
button:checked {
    background-color: @accent_bg_color;
    color: @accent_fg_color;
    border-color: @accent_bg_color;
}

/* Flat buttons carry no border at all until pointed at — used for the grid's
   own event blocks and the icon buttons in the header, where a box around
   every one would out-shout the calendar. */
button.flat {
    border-color: transparent;
}

entry,
spinbutton,
popover > contents,
.card,
.boxed-list,
toast,
.toast {
    background-image: none;
    border: 1px solid @borders;
}

entry:focus-within {
    border-color: @accent_bg_color;
}

/* A terminal's selected line inverts; it doesn't tint. */
:selected,
row:selected {
    background-color: @accent_bg_color;
    color: @accent_fg_color;
}

/* Today's date is the one piece of inverse video in the grid — a filled block
   the way a block cursor sits on a character cell, not a pill. */
.today-badge {
    background-color: @accent_bg_color;
    color: @accent_fg_color;
    min-width: 24px;
    min-height: 22px;
}

.month-cell {
    border-right: 1px solid @borders;
    border-bottom: 1px solid @borders;
}

/* Year view. A thumbnail is read as a shape rather than a list, so the
   day buttons shed their padding and a busy day is weighted instead of
   dotted — at this size a dot is indistinguishable from an artifact. */
.year-month button,
.mini-month button {
    padding: 0;
    min-width: 22px;
    min-height: 20px;
}

.year-day-busy {
    font-weight: bold;
    color: @accent_color;
}

/* The sidebar's mini month reuses the year thumbnail, so it inherits the
   rules above; this only gives it breathing room and a separating line. */
.mini-month {
    padding: 10px 12px;
    border-bottom: 1px solid @borders;
}

.selected-slot {
    /* The paste target, and the only thing on the grid that says where Ctrl+V
       will land. Drawn as a filled wash with an inset outline so it reads on
       both the month grid and an hour row, and so it survives sitting next to
       the today highlight. */
    background-color: alpha(@accent_bg_color, 0.16);
    box-shadow: inset 0 0 0 2px alpha(@accent_bg_color, 0.85);
}

.selected-event {
    /* The event Ctrl+C copies and Ctrl+X cuts. A ring rather than a fill: the
       block already carries its calendar's color, and filling it would hide
       which calendar it is on. */
    box-shadow: inset 0 0 0 2px @accent_bg_color;
}

.today-cell {
    background-color: alpha(@accent_bg_color, 0.08);
}

.week-header-cell {
    padding: 6px 0;
    border-right: 1px solid @borders;
    border-bottom: 1px solid @borders;
}

.week-gutter {
    border-right: 1px solid @borders;
}

.week-day-column {
    border-right: 1px solid @borders;
}

.today-column {
    background-color: alpha(@accent_bg_color, 0.06);
}

.all-day-row {
    border-bottom: 1px solid @borders;
}

.all-day-cell {
    border-right: 1px solid @borders;
    min-height: 18px;
    padding: 1px 0;
}

.hour-cell {
    border-bottom: 1px solid alpha(@borders, 0.6);
}

/* The current time reads as a rule across the day with a square tick in the
   gutter — the shape a terminal draws a marker with. */
.now-line {
    background-color: @destructive_bg_color;
    min-height: 1px;
}

.now-dot {
    background-color: @destructive_bg_color;
    min-width: 6px;
    min-height: 6px;
}

.event-chip {
    background-color: transparent;
    color: @window_fg_color;
    padding: 0;
    margin: 0 2px;
    font-size: 0.85em;
    min-height: 20px;
    border: none;
}

.event-chip label {
    color: @window_fg_color;
}

.all-day-event {
    font-size: 0.78em;
    min-height: 14px;
    padding: 0;
    margin-top: 0;
    margin-bottom: 0;
}

.all-day-event > * {
    min-height: 14px;
}

.event-block {
    background-color: transparent;
    color: @window_fg_color;
    padding: 0;
    font-size: 0.85em;
    border: none;
}

.event-block label {
    color: @window_fg_color;
}

.event-resize-handle {
    min-height: 10px;
    background-color: transparent;
    transition: background-color 120ms ease;
}

.event-resize-handle:hover {
    background-color: alpha(@accent_bg_color, 0.45);
}

.drag-preview {
    background-color: alpha(@accent_bg_color, 0.9);
    border: 1px solid @accent_bg_color;
}

.drag-preview-label {
    color: @accent_fg_color;
    font-size: 0.8em;
    font-weight: bold;
}

/* Compact text: window.rs toggles this class on the window below its width
   breakpoint, stepping calendar-grid text down a size so narrow day columns
   stay readable instead of ellipsizing everything away. */
window.compact-text .event-chip,
window.compact-text .event-block {
    font-size: 0.75em;
}

window.compact-text .all-day-event {
    font-size: 0.7em;
}

window.compact-text .day-number {
    font-size: 0.85em;
}

window.compact-text .month-weekday {
    font-size: 0.7em;
}

window.compact-text .month-cell .caption {
    font-size: 0.72em;
}

window.compact-text .week-gutter label {
    font-size: 0.72em;
}

window.compact-text .week-header-cell .caption-heading {
    font-size: 0.68em;
}

window.compact-text .week-header-cell .title-3 {
    font-size: 1.1em;
}

window.compact-text .today-badge {
    min-width: 22px;
    min-height: 22px;
}

window.compact-text .drag-preview-label {
    font-size: 0.7em;
}

/* The header is a rule with controls on it, not a raised bar: its shadow is
   gone, so a hairline does the separating and the height comes down to
   something closer to a status line. */
headerbar {
    border-bottom: 1px solid @borders;
    min-height: 34px;
}

/* Header controls (Today, Month/Week/Day) sized down from GTK's default
   header-bar button bulk. */
.header-small {
    min-height: 0;
    padding: 3px 10px;
    font-size: 0.9em;
}

.calendar-sidebar {
    background-color: @sidebar_bg_color;
    border-right: 1px solid @borders;
}

.sidebar-actions {
    border-bottom: 1px solid @borders;
    padding-bottom: 10px;
}

.sidebar-action-button {
    min-height: 30px;
    padding-left: 8px;
    padding-right: 8px;
}
";

/// The rule that sets the whole app in the desktop's monospace face.
///
/// Kept separate from `CSS` because the family is discovered at runtime. The
/// generic `monospace` stays on the end as a fallback, so a machine with no
/// alacritty config still gets a fixed-width face rather than the UI sans.
fn font_css(family: Option<&str>) -> String {
    match family {
        // The family is quoted: real names have spaces in them.
        Some(family) => format!("* {{ font-family: \"{family}\", monospace; }}\n"),
        None => "* { font-family: monospace; }\n".to_string(),
    }
}

pub fn load() {
    let display = gdk::Display::default().expect("a display is available");

    let provider = gtk::CssProvider::new();
    provider.load_from_string(&format!(
        "{}{CSS}",
        font_css(crate::omarchy::terminal_font_family().as_deref())
    ));
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    // If Omarchy is present, recolor libadwaita from the active theme. The
    // overrides load at USER priority so they win over libadwaita's own
    // (theme-priority) color definitions, and force the matching color scheme
    // so symbolic icons and dark-aware widgets line up with the palette. The
    // layout CSS above keeps referencing the same color names either way.
    if let Some(overrides) = crate::omarchy::theme_overrides() {
        let color_provider = gtk::CssProvider::new();
        color_provider.load_from_string(&overrides.css);
        gtk::style_context_add_provider_for_display(
            &display,
            &color_provider,
            gtk::STYLE_PROVIDER_PRIORITY_USER,
        );
        adw::StyleManager::default().set_color_scheme(if overrides.dark {
            adw::ColorScheme::ForceDark
        } else {
            adw::ColorScheme::ForceLight
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_discovered_family_is_quoted_and_backed_by_the_generic_monospace() {
        assert_eq!(
            font_css(Some("JetBrainsMono Nerd Font")),
            "* { font-family: \"JetBrainsMono Nerd Font\", monospace; }\n"
        );
    }

    #[test]
    fn without_a_discovered_family_the_generic_monospace_stands_alone() {
        // Still fixed-width: falling back to the UI sans would lose the look
        // entirely on a machine with no alacritty config.
        assert_eq!(font_css(None), "* { font-family: monospace; }\n");
    }
}
