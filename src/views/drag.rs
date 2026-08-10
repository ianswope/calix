use crate::views::press_hits_button;
use chrono::{NaiveDate, NaiveTime};
use gtk::gdk;
use gtk::glib;
use gtk::glib::clone;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DragKind {
    Move,
    ResizeStart,
    ResizeEnd,
}

impl DragKind {
    pub(crate) fn as_prefix(self) -> &'static str {
        match self {
            Self::Move => "move",
            Self::ResizeStart => "resize-start",
            Self::ResizeEnd => "resize-end",
        }
    }

    pub(crate) fn from_prefix(prefix: &str) -> Option<Self> {
        match prefix {
            "move" => Some(Self::Move),
            "resize-start" => Some(Self::ResizeStart),
            "resize-end" => Some(Self::ResizeEnd),
            _ => None,
        }
    }
}

pub(crate) fn drag_payload(kind: DragKind, event_id: i64) -> String {
    format!("{}:{event_id}", kind.as_prefix())
}

pub(crate) fn parse_drag_payload(value: &str) -> Option<(DragKind, i64)> {
    let (kind, event_id) = value.split_once(':')?;
    Some((DragKind::from_prefix(kind)?, event_id.parse().ok()?))
}

/// Snap granularity for interactive drag/resize, in minutes.
const SNAP_MINUTES: f64 = 15.0;
/// Pointer travel (px) before a press is treated as a drag rather than a click.
const DRAG_THRESHOLD: f64 = 4.0;
/// Shortest event a resize will produce.
const MIN_BLOCK_MINUTES: f64 = 15.0;
const MINUTES_PER_DAY: f64 = 24.0 * 60.0;
/// Distance from the scroll viewport's top/bottom edge (px) at which a drag
/// starts auto-scrolling toward off-screen hours.
const AUTOSCROLL_EDGE: f64 = 32.0;
/// Fastest auto-scroll step, in px per frame.
const AUTOSCROLL_MAX_STEP: f64 = 14.0;
/// The preview block is too short to fit its time-range label below this.
const PREVIEW_LABEL_MIN_PX: f64 = 22.0;

/// Callback fired when an interactive drag/resize commits: the event id, the
/// target day column, and the new time for the moved edge (or the moved
/// event's start).
pub(crate) type CommitFn = Rc<dyn Fn(DragKind, i64, NaiveDate, Option<NaiveTime>)>;

/// Where a timed block sits in the week/day grid, and which of its edges are
/// the event's own start/end. An event spanning midnight renders as one
/// clipped block per day; a clipped edge is the day boundary, not the event's,
/// so it must not be movable or resizable (the commit math would treat the
/// rendered position as the event's real time).
pub struct BlockPlacement {
    pub col: usize,
    pub top_px: f64,
    pub height_px: f64,
    pub starts_here: bool,
    pub ends_here: bool,
}

/// Live state for the in-flight drag. Only one exists at a time; the first
/// gesture to cross the threshold owns it until release.
struct Session {
    kind: DragKind,
    event_id: i64,
    orig_col: usize,
    top_px: f64,
    height_px: f64,
    block: gtk::Widget,
    /// Where the press landed on the day-area's y axis; auto-scroll follows
    /// the pointer, not the block's edges.
    anchor_y: f64,
    /// Last raw gesture offset, so auto-scroll can keep re-deriving the
    /// target while the pointer holds still.
    last_offset: (f64, f64),
    /// Scroll position when `last_offset` was recorded. Gesture offsets are
    /// measured in the (scrolling) content's frame of reference, so any
    /// scroll since then is extra pointer travel the gesture hasn't seen.
    scroll_baseline: f64,
    vadj: Option<gtk::Adjustment>,
    // Live, snapped result in day-area coordinates.
    col: usize,
    new_top_px: f64,
    new_height_px: f64,
}

/// Direct-manipulation drag/resize for timed event blocks in the week/day
/// grid. Unlike GTK's data-transfer drag-and-drop, this tracks the pointer
/// continuously and renders a snapped preview of where the event will land,
/// only committing on release.
pub(crate) struct TimedGrid {
    day_area: gtk::Grid,
    preview: gtk::Fixed,
    preview_block: gtk::Box,
    preview_label: gtk::Label,
    days: Vec<NaiveDate>,
    hour_height: f64,
    session: RefCell<Option<Session>>,
    autoscroll_running: Cell<bool>,
    on_move: CommitFn,
}

impl TimedGrid {
    pub(crate) fn new(
        day_area: &gtk::Grid,
        days: Vec<NaiveDate>,
        hour_height: i32,
        on_move: CommitFn,
    ) -> Rc<Self> {
        let preview = gtk::Fixed::new();
        preview.set_can_target(false);
        preview.add_css_class("drag-preview-layer");

        let preview_block = gtk::Box::new(gtk::Orientation::Vertical, 0);
        preview_block.add_css_class("drag-preview");
        preview_block.set_can_target(false);
        preview_block.set_visible(false);

        let preview_label = gtk::Label::new(None);
        preview_label.add_css_class("drag-preview-label");
        preview_label.set_halign(gtk::Align::Start);
        preview_label.set_valign(gtk::Align::Start);
        preview_label.set_margin_start(8);
        preview_label.set_margin_top(2);
        preview_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        preview_block.append(&preview_label);

        preview.put(&preview_block, 0.0, 0.0);

        Rc::new(Self {
            day_area: day_area.clone(),
            preview,
            preview_block,
            preview_label,
            days,
            hour_height: hour_height as f64,
            session: RefCell::new(None),
            autoscroll_running: Cell::new(false),
            on_move,
        })
    }

    /// The overlay layer that renders the drag preview; the caller must place
    /// this on top of the day grid.
    pub(crate) fn preview_layer(&self) -> &gtk::Fixed {
        &self.preview
    }

    /// Wire a draggable region (the event block for `Move`, or a resize handle
    /// for the resize kinds) to this controller. `block` is the event block to
    /// fade while its preview is shown; for a move it is the region itself.
    pub(crate) fn install(
        self: &Rc<Self>,
        region: &impl IsA<gtk::Widget>,
        block: &impl IsA<gtk::Widget>,
        kind: DragKind,
        event_id: i64,
        placement: &BlockPlacement,
    ) {
        let gesture = gtk::GestureDrag::new();
        gesture.set_button(gdk::BUTTON_PRIMARY);

        let (orig_col, top_px, height_px) = (placement.col, placement.top_px, placement.height_px);
        let block = block.clone().upcast::<gtk::Widget>();
        let this = self.clone();
        gesture.connect_drag_update(move |gesture, offset_x, offset_y| {
            // Ignore updates while another block owns the drag.
            if let Some(active) = this.session.borrow().as_ref()
                && (active.event_id != event_id || active.kind != kind)
            {
                return;
            }

            let starting = this.session.borrow().is_none();
            if starting {
                if offset_x.hypot(offset_y) < DRAG_THRESHOLD {
                    return;
                }
                gesture.set_state(gtk::EventSequenceState::Claimed);
                block.set_opacity(0.35);
                block.set_cursor_from_name(Some("grabbing"));
                let press_y = gesture.start_point().map(|(_, y)| y).unwrap_or(0.0);
                let anchor_y = match kind {
                    // The press offset is in the grabbed region's own
                    // coordinates: the whole block for a move, a thin edge
                    // handle for a resize.
                    DragKind::Move => top_px + press_y,
                    DragKind::ResizeStart => top_px,
                    DragKind::ResizeEnd => top_px + height_px,
                };
                let vadj = this
                    .day_area
                    .ancestor(gtk::ScrolledWindow::static_type())
                    .and_downcast::<gtk::ScrolledWindow>()
                    .map(|scrolled| scrolled.vadjustment());
                let scroll_baseline = vadj.as_ref().map(gtk::Adjustment::value).unwrap_or(0.0);
                *this.session.borrow_mut() = Some(Session {
                    kind,
                    event_id,
                    orig_col,
                    top_px,
                    height_px,
                    block: block.clone(),
                    anchor_y,
                    last_offset: (offset_x, offset_y),
                    scroll_baseline,
                    vadj,
                    col: orig_col,
                    new_top_px: top_px,
                    new_height_px: height_px,
                });
                this.start_autoscroll();
            } else {
                let mut session = this.session.borrow_mut();
                if let Some(session) = session.as_mut() {
                    session.last_offset = (offset_x, offset_y);
                    session.scroll_baseline = session
                        .vadj
                        .as_ref()
                        .map(gtk::Adjustment::value)
                        .unwrap_or(0.0);
                }
            }

            this.update(offset_x, offset_y);
        });

        let this = self.clone();
        gesture.connect_drag_end(move |_, _, _| {
            let session = this.session.borrow_mut().take();
            if let Some(session) = session {
                this.finish(&session);
                this.commit(&session);
            }
        });

        let this = self.clone();
        gesture.connect_cancel(move |_, _| {
            let session = this.session.borrow_mut().take();
            if let Some(session) = session {
                this.finish(&session);
            }
        });

        region.add_controller(gesture);
    }

    /// Scroll the grid while the drag holds the pointer near the viewport's
    /// top or bottom edge, so events can reach off-screen hours. Runs as a
    /// frame tick for the lifetime of the session — continuing to scroll
    /// doesn't require the pointer to keep moving — and removes itself once
    /// the session ends.
    fn start_autoscroll(self: &Rc<Self>) {
        if self.autoscroll_running.get() {
            return;
        }
        self.autoscroll_running.set(true);
        let this = self.clone();
        self.day_area.add_tick_callback(move |_, _| {
            if this.autoscroll_tick() {
                glib::ControlFlow::Continue
            } else {
                this.autoscroll_running.set(false);
                glib::ControlFlow::Break
            }
        });
    }

    fn autoscroll_tick(&self) -> bool {
        let (vadj, anchor_y, last_offset, scroll_baseline) = {
            let session = self.session.borrow();
            let Some(session) = session.as_ref() else {
                return false;
            };
            let Some(vadj) = session.vadj.clone() else {
                return false;
            };
            (
                vadj,
                session.anchor_y,
                session.last_offset,
                session.scroll_baseline,
            )
        };

        // The day area sits at the top of the scrolled content, so the
        // adjustment's value/page directly bound the visible slice of it.
        let pointer_y = anchor_y + last_offset.1 + (vadj.value() - scroll_baseline);
        let visible_top = vadj.value();
        let visible_bottom = visible_top + vadj.page_size();
        let step = if pointer_y < visible_top + AUTOSCROLL_EDGE {
            -((visible_top + AUTOSCROLL_EDGE - pointer_y) * 0.25).min(AUTOSCROLL_MAX_STEP)
        } else if pointer_y > visible_bottom - AUTOSCROLL_EDGE {
            ((pointer_y - (visible_bottom - AUTOSCROLL_EDGE)) * 0.25).min(AUTOSCROLL_MAX_STEP)
        } else {
            0.0
        };

        if step != 0.0 {
            let before = vadj.value();
            vadj.set_value(before + step);
            if vadj.value() != before {
                self.update(
                    last_offset.0,
                    last_offset.1 + (vadj.value() - scroll_baseline),
                );
            }
        }
        true
    }

    /// Recompute the snapped target from the current pointer offset and repaint
    /// the preview.
    fn update(&self, offset_x: f64, offset_y: f64) {
        let column_count = self.days.len().max(1);
        let column_width = (self.day_area.width() as f64 / column_count as f64).max(1.0);
        let px_per_minute = self.hour_height / 60.0;
        let snap_px = SNAP_MINUTES * px_per_minute;
        let min_block_px = MIN_BLOCK_MINUTES * px_per_minute;
        let day_px = MINUTES_PER_DAY * px_per_minute;

        let mut session = self.session.borrow_mut();
        let Some(session) = session.as_mut() else {
            return;
        };

        match session.kind {
            DragKind::Move => {
                let top = snap(session.top_px + offset_y, snap_px)
                    .clamp(0.0, (day_px - session.height_px).max(0.0));
                session.new_top_px = top;
                session.new_height_px = session.height_px;
                let columns_moved = (offset_x / column_width).round() as i64;
                session.col = (session.orig_col as i64 + columns_moved)
                    .clamp(0, column_count as i64 - 1) as usize;
            }
            DragKind::ResizeStart => {
                let bottom = session.top_px + session.height_px;
                let top =
                    snap(session.top_px + offset_y, snap_px).clamp(0.0, bottom - min_block_px);
                session.new_top_px = top;
                session.new_height_px = bottom - top;
                session.col = session.orig_col;
            }
            DragKind::ResizeEnd => {
                let bottom = snap(session.top_px + session.height_px + offset_y, snap_px)
                    .clamp(session.top_px + min_block_px, day_px);
                session.new_top_px = session.top_px;
                session.new_height_px = bottom - session.top_px;
                session.col = session.orig_col;
            }
        }

        let col = session.col;
        let top = session.new_top_px;
        let height = session.new_height_px;

        let x = col as f64 * column_width + 2.0;
        let width = (column_width - 4.0).max(1.0);
        self.preview_block
            .set_size_request(width as i32, height.max(1.0) as i32);
        self.preview.move_(&self.preview_block, x, top);
        self.preview_label.set_text(&format!(
            "{} – {}",
            format_minutes(top / px_per_minute),
            format_minutes((top + height) / px_per_minute),
        ));
        // On a block too short for the label, showing it would stretch the
        // preview taller than the event it stands for.
        self.preview_label
            .set_visible(height >= PREVIEW_LABEL_MIN_PX);
        self.preview_block.set_visible(true);
    }

    /// Hide the preview and restore the faded block.
    fn finish(&self, session: &Session) {
        self.preview_block.set_visible(false);
        session.block.set_opacity(1.0);
        session.block.set_cursor_from_name(Some("grab"));
    }

    /// Apply the result if the event actually moved.
    fn commit(&self, session: &Session) {
        let px_per_minute = self.hour_height / 60.0;
        let moved = session.col != session.orig_col
            || (session.new_top_px - session.top_px).abs() >= 0.5
            || (session.new_height_px - session.height_px).abs() >= 0.5;
        if !moved {
            return;
        }

        let start_minutes = (session.new_top_px / px_per_minute).round();
        let end_minutes = ((session.new_top_px + session.new_height_px) / px_per_minute).round();

        match session.kind {
            DragKind::Move => {
                let date = self.days[session.col];
                (self.on_move)(session.kind, session.event_id, date, time_of(start_minutes));
            }
            DragKind::ResizeStart => {
                let date = self.days[session.orig_col];
                (self.on_move)(session.kind, session.event_id, date, time_of(start_minutes));
            }
            DragKind::ResizeEnd => {
                // An edge dragged to the very bottom means midnight of the next
                // day, which has no same-day NaiveTime.
                let date = self.days[session.orig_col];
                if end_minutes >= MINUTES_PER_DAY {
                    let next = date.succ_opt().unwrap_or(date);
                    (self.on_move)(
                        session.kind,
                        session.event_id,
                        next,
                        NaiveTime::from_hms_opt(0, 0, 0),
                    );
                } else {
                    (self.on_move)(session.kind, session.event_id, date, time_of(end_minutes));
                }
            }
        }
    }
}

/// Fired when a create-drag commits, with the snapped span it drew.
pub(crate) type CreateRangeFn = Rc<dyn Fn(NaiveTime, NaiveTime)>;

/// Attach drag-to-create to one day column's empty space.
///
/// Deliberately per column rather than folded into [`TimedGrid`]: a create
/// drag never crosses a day, so none of that type's column mapping,
/// cross-column commit math or block bookkeeping applies. The day is fixed by
/// which column the gesture is on.
///
/// Presses landing on an event block are left alone — that block owns its own
/// move/resize gesture, and starting a create underneath it would fight for
/// the same pointer.
pub(crate) fn install_create_drag(
    column: &gtk::Overlay,
    hour_height: f64,
    on_create_range: CreateRangeFn,
) {
    let preview = gtk::Fixed::new();
    preview.set_can_target(false);
    preview.add_css_class("drag-preview-layer");

    let block = gtk::Box::new(gtk::Orientation::Vertical, 0);
    block.add_css_class("drag-preview");
    block.set_can_target(false);
    block.set_visible(false);

    let label = gtk::Label::new(None);
    label.add_css_class("drag-preview-label");
    label.set_halign(gtk::Align::Start);
    label.set_valign(gtk::Align::Start);
    label.set_margin_start(8);
    label.set_margin_top(2);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    block.append(&label);
    preview.put(&block, 0.0, 0.0);
    column.add_overlay(&preview);

    let gesture = gtk::GestureDrag::new();
    gesture.set_button(gdk::BUTTON_PRIMARY);

    // Set once the press has travelled far enough to be a drag rather than a
    // click, so a plain click still falls through to the hour cell beneath.
    let dragging = Rc::new(Cell::new(false));
    let root = column.clone().upcast::<gtk::Widget>();

    gesture.connect_drag_update(clone!(
        #[strong]
        block,
        #[strong]
        label,
        #[strong]
        dragging,
        #[strong]
        root,
        move |gesture, offset_x, offset_y| {
            let Some((press_x, press_y)) = gesture.start_point() else {
                return;
            };
            if !dragging.get() {
                if offset_x.hypot(offset_y) < DRAG_THRESHOLD {
                    return;
                }
                if press_hits_button(&root, press_x, press_y) {
                    return;
                }
                // Claiming denies the hour cell's click gesture, which would
                // otherwise also fire on release and open a second dialog.
                gesture.set_state(gtk::EventSequenceState::Claimed);
                dragging.set(true);
            }
            let Some((start, end)) = create_range(press_y, press_y + offset_y, hour_height) else {
                return;
            };
            let top = start / 60.0 * hour_height;
            let height = (end - start) / 60.0 * hour_height;
            block.set_size_request(root.width().max(1), height as i32);
            block.set_visible(true);
            label.set_visible(height >= PREVIEW_LABEL_MIN_PX);
            label.set_label(&format!(
                "{} – {}",
                format_minutes(start),
                format_minutes(end)
            ));
            if let Some(fixed) = block.parent().and_downcast::<gtk::Fixed>() {
                fixed.move_(&block, 0.0, top);
            }
        }
    ));

    gesture.connect_drag_end(clone!(
        #[strong]
        block,
        #[strong]
        dragging,
        move |gesture, _, offset_y| {
            block.set_visible(false);
            if !dragging.replace(false) {
                return;
            }
            let Some((_, press_y)) = gesture.start_point() else {
                return;
            };
            let Some((start, end)) = create_range(press_y, press_y + offset_y, hour_height) else {
                return;
            };
            // `time_of` clamps 1440 to 23:59, so a drag to the very bottom
            // ends at the last minute of the day rather than failing.
            if let (Some(start), Some(end)) = (time_of(start), time_of(end)) {
                on_create_range(start, end);
            }
        }
    ));

    column.add_controller(gesture);
}

/// The snapped time range a create-drag covers, in minutes from midnight.
///
/// `from_y` is where the press landed, `to_y` where the pointer has reached,
/// both on the day column's y axis. Dragging upward is the same gesture as
/// dragging downward — people reach for the end of a meeting as readily as its
/// start — so the pair is ordered rather than treated as invalid.
///
/// Never returns a zero-length range. A press that barely moves is a click,
/// and the caller still wants a usable event out of it, so the range widens to
/// `MIN_BLOCK_MINUTES` rather than collapsing. Both edges are clamped into the
/// day, so dragging off the top or bottom of the grid stops at midnight
/// instead of producing a time that doesn't exist.
pub(crate) fn create_range(from_y: f64, to_y: f64, hour_height: f64) -> Option<(f64, f64)> {
    if hour_height <= 0.0 {
        return None;
    }
    let minutes_at = |y: f64| snap(y / hour_height * 60.0, SNAP_MINUTES);
    let (a, b) = (minutes_at(from_y), minutes_at(to_y));
    // Clamping start first leaves room for the minimum block even when the
    // press lands in the day's last quarter hour.
    let start = a.min(b).clamp(0.0, MINUTES_PER_DAY - MIN_BLOCK_MINUTES);
    let end = a.max(b).clamp(start + MIN_BLOCK_MINUTES, MINUTES_PER_DAY);
    Some((start, end))
}

fn snap(value: f64, snap_px: f64) -> f64 {
    if snap_px <= 0.0 {
        value
    } else {
        (value / snap_px).round() * snap_px
    }
}

fn time_of(minutes: f64) -> Option<NaiveTime> {
    let minutes = minutes.clamp(0.0, MINUTES_PER_DAY - 1.0) as u32;
    NaiveTime::from_hms_opt(minutes / 60, minutes % 60, 0)
}

fn format_minutes(minutes: f64) -> String {
    let minutes = minutes.clamp(0.0, MINUTES_PER_DAY) as u32;
    let hour = (minutes / 60) % 24;
    let minute = minutes % 60;
    let (display_hour, suffix) = match hour {
        0 => (12, "AM"),
        1..=11 => (hour, "AM"),
        12 => (12, "PM"),
        _ => (hour - 12, "PM"),
    };
    format!("{display_hour}:{minute:02} {suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drag_payload_round_trips_through_parse() {
        for kind in [DragKind::Move, DragKind::ResizeStart, DragKind::ResizeEnd] {
            let payload = drag_payload(kind, 42);
            assert_eq!(parse_drag_payload(&payload), Some((kind, 42)));
        }
    }

    #[test]
    fn parse_drag_payload_rejects_malformed_values() {
        assert!(parse_drag_payload("move").is_none()); // no ':' separator
        assert!(parse_drag_payload("teleport:42").is_none()); // unknown kind
        assert!(parse_drag_payload("move:notanumber").is_none()); // bad id
    }

    #[test]
    fn snap_rounds_to_the_nearest_gridline() {
        // 15px grid: 17 rounds down to 15, 23 rounds up to 30.
        assert_eq!(snap(17.0, 15.0), 15.0);
        assert_eq!(snap(23.0, 15.0), 30.0);
        // A non-positive grid disables snapping (leaves the value untouched)
        // rather than dividing by zero.
        assert_eq!(snap(17.0, 0.0), 17.0);
    }

    #[test]
    fn time_of_maps_minutes_to_time_and_clamps() {
        assert_eq!(time_of(0.0), NaiveTime::from_hms_opt(0, 0, 0));
        assert_eq!(time_of(90.0), NaiveTime::from_hms_opt(1, 30, 0));
        // Midnight-of-next-day (1440) has no same-day NaiveTime, so it clamps
        // to the last representable minute of the day.
        assert_eq!(time_of(MINUTES_PER_DAY), NaiveTime::from_hms_opt(23, 59, 0));
    }

    /// One pixel per minute, so a y of 540 reads as 9:00 in these tests.
    const MINUTE_PX: f64 = 60.0;

    #[test]
    fn a_downward_create_drag_spans_press_to_release() {
        // 9:00 down to 10:30.
        assert_eq!(create_range(540.0, 630.0, MINUTE_PX), Some((540.0, 630.0)));
    }

    #[test]
    fn dragging_upward_produces_the_same_range_as_dragging_down() {
        // Grabbing 10:30 and pulling up to 9:00 means the same meeting.
        assert_eq!(
            create_range(630.0, 540.0, MINUTE_PX),
            create_range(540.0, 630.0, MINUTE_PX)
        );
    }

    #[test]
    fn both_edges_snap_to_the_quarter_hour() {
        // 9:07 -> 9:00 and 10:23 -> 10:30, matching the move/resize grid.
        assert_eq!(create_range(547.0, 623.0, MINUTE_PX), Some((540.0, 630.0)));
    }

    #[test]
    fn a_press_that_barely_moves_still_yields_a_usable_event() {
        // Otherwise a click would create a zero-length event.
        let (start, end) = create_range(540.0, 542.0, MINUTE_PX).expect("a range");
        assert_eq!(start, 540.0);
        assert_eq!(end - start, MIN_BLOCK_MINUTES);
    }

    #[test]
    fn a_drag_off_the_bottom_stops_at_the_end_of_the_day() {
        let (start, end) = create_range(1380.0, 5000.0, MINUTE_PX).expect("a range");
        assert_eq!(start, 1380.0); // 23:00
        assert_eq!(end, MINUTES_PER_DAY);
    }

    #[test]
    fn a_drag_off_the_top_stops_at_midnight() {
        let (start, end) = create_range(60.0, -500.0, MINUTE_PX).expect("a range");
        assert_eq!(start, 0.0);
        assert_eq!(end, 60.0);
    }

    #[test]
    fn a_press_at_the_very_bottom_still_leaves_room_for_the_minimum_block() {
        // Starting inside the last quarter hour must not produce an event that
        // ends before it starts.
        let (start, end) = create_range(1439.0, 1439.0, MINUTE_PX).expect("a range");
        assert!(end > start, "{start} -> {end}");
        assert_eq!(end, MINUTES_PER_DAY);
        assert_eq!(end - start, MIN_BLOCK_MINUTES);
    }

    #[test]
    fn a_grid_with_no_height_has_no_range_rather_than_dividing_by_zero() {
        assert_eq!(create_range(540.0, 630.0, 0.0), None);
    }

    #[test]
    fn format_minutes_uses_twelve_hour_clock() {
        assert_eq!(format_minutes(0.0), "12:00 AM"); // midnight
        assert_eq!(format_minutes(9.0 * 60.0 + 5.0), "9:05 AM");
        assert_eq!(format_minutes(12.0 * 60.0), "12:00 PM"); // noon
        assert_eq!(format_minutes(13.0 * 60.0 + 30.0), "1:30 PM");
    }
}
