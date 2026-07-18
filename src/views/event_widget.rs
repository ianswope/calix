use crate::store::Event;
use crate::views::drag::{BlockPlacement, DragKind, TimedGrid, drag_payload};
use gtk::gdk;
use gtk::prelude::*;
use std::f64::consts::PI;
use std::rc::Rc;

pub fn event_button(event: &Event, css_class: &str, min_height: i32) -> gtk::Button {
    event_button_with_padding(event, css_class, min_height, 2, true)
}

pub fn compact_event_button(event: &Event, css_class: &str, min_height: i32) -> gtk::Button {
    event_button_with_padding(event, css_class, min_height, 0, true)
}

/// A timed event block for the week/day grid. Moving and resizing are driven
/// by `grid` (a `GestureDrag` controller with a live preview) rather than
/// GTK's data-transfer drag-and-drop, so `placement` describes where the
/// block currently sits in the grid.
pub fn timed_event_widget(
    event: &Event,
    css_class: &str,
    min_height: i32,
    on_click: Rc<dyn Fn(Event)>,
    grid: &Rc<TimedGrid>,
    placement: &BlockPlacement,
) -> gtk::Widget {
    let overlay = gtk::Overlay::new();
    let button = event_button_with_padding(event, css_class, min_height, 0, false);
    let ev = event.clone();
    let click = on_click.clone();
    button.connect_clicked(move |_| click(ev.clone()));
    overlay.set_child(Some(&button));

    // A local recurring event is drawn as many occurrences sharing one id;
    // moving or resizing one would rewrite the whole series, so it stays
    // read-only until per-instance recurrence editing exists.
    let movable = event.recurrence.is_none();

    // Moving commits the block's top edge as the event's new start, so only
    // a block whose top really is the start may move.
    if placement.starts_here && movable {
        button.set_cursor_from_name(Some("grab"));
        grid.install(&button, &button, DragKind::Move, event.id, placement);
    }

    // Keep a clickable/movable band in the middle even for short blocks by
    // shrinking the resize handles rather than letting them swallow the block.
    let handle_height = (placement.height_px / 3.0).clamp(4.0, 10.0) as i32;
    for (kind, is_own_edge) in [
        (DragKind::ResizeStart, placement.starts_here),
        (DragKind::ResizeEnd, placement.ends_here),
    ] {
        if !is_own_edge || !movable {
            continue;
        }
        let handle = resize_handle(kind, handle_height);
        grid.install(&handle, &button, kind, event.id, placement);

        // The handle covers the button's edge, so plain clicks on it would
        // otherwise go dead; forward them to the event's click action. A
        // drag that crosses the threshold claims the sequence, which cancels
        // this click before it can fire.
        let click_gesture = gtk::GestureClick::new();
        let ev = event.clone();
        let on_click = on_click.clone();
        click_gesture.connect_released(move |_, _, _, _| on_click(ev.clone()));
        handle.add_controller(click_gesture);

        overlay.add_overlay(&handle);
    }

    overlay.upcast()
}

fn event_button_with_padding(
    event: &Event,
    css_class: &str,
    min_height: i32,
    vertical_padding: i32,
    draggable: bool,
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
    if !event.title.is_empty() {
        button.set_tooltip_text(Some(event.title.as_str()));
    }
    // A local recurring event is drawn as many occurrences sharing one id, so
    // dragging one would move the whole series to that date; leave it read-only
    // until per-instance recurrence editing exists.
    if draggable && event.recurrence.is_none() {
        make_draggable(&button, event.id, DragKind::Move);
    }
    button
}

fn resize_handle(kind: DragKind, height: i32) -> gtk::Box {
    let handle = gtk::Box::new(gtk::Orientation::Vertical, 0);
    handle.add_css_class("event-resize-handle");
    handle.add_css_class(match kind {
        DragKind::ResizeStart => "event-resize-handle-start",
        DragKind::ResizeEnd => "event-resize-handle-end",
        DragKind::Move => unreachable!("move drags never use resize handles"),
    });
    handle.set_hexpand(true);
    handle.set_size_request(-1, height);
    handle.set_halign(gtk::Align::Fill);
    handle.set_valign(match kind {
        DragKind::ResizeStart => gtk::Align::Start,
        DragKind::ResizeEnd => gtk::Align::End,
        DragKind::Move => gtk::Align::Start,
    });
    handle.set_cursor_from_name(Some("ns-resize"));
    handle
}

fn make_draggable(widget: &impl IsA<gtk::Widget>, event_id: i64, kind: DragKind) {
    let drag = gtk::DragSource::builder()
        .actions(gdk::DragAction::MOVE)
        .build();
    let payload = drag_payload(kind, event_id);
    drag.connect_prepare(move |_, _, _| Some(gdk::ContentProvider::for_value(&payload.to_value())));
    widget.add_controller(drag);
}

fn rounded_rect(cr: &gtk::cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -PI / 2.0, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, PI / 2.0);
    cr.arc(x + r, y + h - r, r, PI / 2.0, PI);
    cr.arc(x + r, y + r, r, PI, 3.0 * PI / 2.0);
    cr.close_path();
}
