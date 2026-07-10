use gtk::gdk;

const CSS: &str = "
.today-badge {
    background-color: @accent_bg_color;
    color: @accent_fg_color;
    border-radius: 999px;
    min-width: 26px;
    min-height: 26px;
}

.month-cell {
    border-right: 1px solid @borders;
    border-bottom: 1px solid @borders;
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

.now-line {
    background-color: @destructive_bg_color;
    min-height: 2px;
}

.now-dot {
    background-color: @destructive_bg_color;
    border-radius: 999px;
    min-width: 8px;
    min-height: 8px;
}

.event-chip {
    background-color: transparent;
    color: @window_fg_color;
    border-radius: 6px;
    box-shadow: none;
    padding: 0;
    margin: 0 4px;
    font-size: 0.85em;
    min-height: 20px;
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
    border-radius: 6px;
    box-shadow: none;
    padding: 0;
    font-size: 0.85em;
}

.event-block label {
    color: @window_fg_color;
}

.event-resize-handle {
    background-color: alpha(@accent_bg_color, 0.22);
    min-height: 8px;
}

.event-resize-handle:hover {
    background-color: alpha(@accent_bg_color, 0.35);
}

.event-resize-handle-start {
    border-top-left-radius: 6px;
    border-top-right-radius: 6px;
}

.event-resize-handle-end {
    border-bottom-left-radius: 6px;
    border-bottom-right-radius: 6px;
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

pub fn load() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    gtk::style_context_add_provider_for_display(
        &gdk::Display::default().expect("a display is available"),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}
