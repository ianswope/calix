use crate::store::Event;
use gtk::prelude::*;
use std::f64::consts::PI;

pub fn event_button(event: &Event, css_class: &str, min_height: i32) -> gtk::Button {
    event_button_with_padding(event, css_class, min_height, 2)
}

pub fn compact_event_button(event: &Event, css_class: &str, min_height: i32) -> gtk::Button {
    event_button_with_padding(event, css_class, min_height, 0)
}

fn event_button_with_padding(
    event: &Event,
    css_class: &str,
    min_height: i32,
    vertical_padding: i32,
) -> gtk::Button {
    let color = gtk::gdk::RGBA::parse(event.calendar_color.as_str())
        .unwrap_or_else(|_| gtk::gdk::RGBA::new(0.2, 0.52, 0.89, 1.0));

    let background = gtk::DrawingArea::new();
    background.set_hexpand(true);
    background.set_vexpand(false);
    background.set_content_height(min_height);
    background.set_draw_func(move |_, cr, width, height| {
        let width = width as f64;
        let height = height as f64;
        let radius = 6.0_f64.min(height / 2.0);

        cr.set_source_rgba(
            color.red() as f64,
            color.green() as f64,
            color.blue() as f64,
            0.22,
        );
        rounded_rect(cr, 0.0, 0.0, width, height, radius);
        let _ = cr.fill();

        let _ = cr.save();
        rounded_rect(cr, 0.0, 0.0, width, height, radius);
        cr.clip();
        cr.set_source_rgba(
            color.red() as f64,
            color.green() as f64,
            color.blue() as f64,
            0.92,
        );
        cr.rectangle(0.0, 0.0, 5.0_f64.min(width), height);
        let _ = cr.fill();
        let _ = cr.restore();
    });

    let label = gtk::Label::new(None);
    label.set_markup(&gtk::glib::markup_escape_text(event.title.as_str()));
    label.set_xalign(0.0);
    label.set_hexpand(true);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_single_line_mode(true);
    label.set_width_chars(1);
    label.set_max_width_chars(1);

    let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    content.set_margin_start(10);
    content.set_margin_end(7);
    content.set_margin_top(vertical_padding);
    content.set_margin_bottom(vertical_padding);
    content.append(&label);

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&background));
    overlay.add_overlay(&content);

    let button = gtk::Button::builder()
        .label("")
        .css_classes([css_class])
        .build();
    button.set_label("");
    button.set_child(Some(&overlay));
    button
}

fn rounded_rect(cr: &gtk::cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -PI / 2.0, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, PI / 2.0);
    cr.arc(x + r, y + h - r, r, PI / 2.0, PI);
    cr.arc(x + r, y + r, r, PI, 3.0 * PI / 2.0);
    cr.close_path();
}
