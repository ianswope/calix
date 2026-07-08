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

.week-header-cell {
    padding: 6px 0;
    border-bottom: 1px solid @borders;
}

.hour-cell {
    border-bottom: 1px solid alpha(@borders, 0.6);
}

.now-line {
    background-color: @destructive_bg_color;
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
