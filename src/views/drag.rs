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
    /// A move dragged into the band above the hours. Not a gesture target;
    /// the grid reports it when a move commits there.
    ToAllDay,
}

impl DragKind {
    pub(crate) fn as_prefix(self) -> &'static str {
        match self {
            Self::Move => "move",
            Self::ResizeStart => "resize-start",
            Self::ResizeEnd => "resize-end",
            Self::ToAllDay => "to-all-day",
        }
    }

    pub(crate) fn from_prefix(prefix: &str) -> Option<Self> {
        match prefix {
            "move" => Some(Self::Move),
            "resize-start" => Some(Self::ResizeStart),
            "resize-end" => Some(Self::ResizeEnd),
            "to-all-day" => Some(Self::ToAllDay),
            _ => None,
        }
    }
}

pub(crate) fn drag_payload(kind: DragKind, event_id: i64) -> String {
    format!("{}:{event_id}", kind.as_prefix())
}

/// A chip drag names the occurrence it came from. Several copies of a series
/// share one id; the day is what tells the grabbed copy from the others.
pub(crate) fn occurrence_payload(kind: DragKind, event_id: i64, day: NaiveDate) -> String {
    format!(
        "{}:{}",
        drag_payload(kind, event_id),
        day.format("%Y-%m-%d")
    )
}

pub(crate) fn parse_drag_payload(value: &str) -> Option<(DragKind, i64, Option<NaiveDate>)> {
    let (kind, rest) = value.split_once(':')?;
    let kind = DragKind::from_prefix(kind)?;
    let (id_text, day_text) = match rest.split_once(':') {
        Some((id, day)) => (id, Some(day)),
        None => (rest, None),
    };
    let event_id = id_text.parse().ok()?;
    let day = match day_text {
        Some(text) => Some(NaiveDate::parse_from_str(text, "%Y-%m-%d").ok()?),
        None => None,
    };
    Some((kind, event_id, day))
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
/// target day, the new time for the moved edge (or the moved event's start),
/// and the day the grabbed occurrence started on.
pub(crate) type CommitFn =
    Rc<dyn Fn(DragKind, i64, NaiveDate, Option<NaiveTime>, Option<NaiveDate>)>;

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
    /// Where the press landed on the day-area's x axis. The column is the one
    /// under the pointer (`origin_x + offset_x`), not a count of columns the
    /// grab point has travelled.
    origin_x: f64,
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
    /// Whole days to add on a one-column day page. Zero in week view, where
    /// horizontal travel is a column change instead.
    day_shift: i64,
    /// The pointer is in the band above the hour grid, so a move commits as
    /// all-day. Resize never sets this.
    all_day: bool,
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
        // Everything these gestures capture is held weakly. The controller
        // owns the block (for a move, the block *is* the region), and this grid
        // owns the day area the block sits in, so a strong reference to either
        // from the gesture's own closure is a cycle that would keep every
        // rendered page alive after it was swapped out.
        let block = block.clone().upcast::<gtk::Widget>().downgrade();
        let this = Rc::downgrade(self);
        gesture.connect_drag_update(move |gesture, offset_x, offset_y| {
            let (Some(this), Some(block)) = (this.upgrade(), block.upgrade()) else {
                return;
            };
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
                let (press_x, press_y) = gesture.start_point().unwrap_or((0.0, 0.0));
                let anchor_y = match kind {
                    // The press offset is in the grabbed region's own
                    // coordinates: the whole block for a move, a thin edge
                    // handle for a resize.
                    DragKind::Move | DragKind::ToAllDay => top_px + press_y,
                    DragKind::ResizeStart => top_px,
                    DragKind::ResizeEnd => top_px + height_px,
                };
                // Map the press into the day area so later offsets describe the
                // pointer, not travel from wherever on the block was grabbed.
                // A grid that has not been allocated yet (the leak test builds
                // one off-screen) has no coordinate space; falling back to the
                // column keeps that drag on its own day.
                let origin_x = gesture
                    .widget()
                    .and_then(|widget| {
                        widget.compute_point(
                            &this.day_area,
                            &gtk::graphene::Point::new(press_x as f32, press_y as f32),
                        )
                    })
                    .map(|point| f64::from(point.x()))
                    .unwrap_or_else(|| orig_col as f64 * this.column_width() + press_x);
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
                    origin_x,
                    anchor_y,
                    last_offset: (offset_x, offset_y),
                    scroll_baseline,
                    vadj,
                    col: orig_col,
                    day_shift: 0,
                    all_day: false,
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

        let this = Rc::downgrade(self);
        gesture.connect_drag_end(move |_, _, _| {
            let Some(this) = this.upgrade() else {
                return;
            };
            let session = this.session.borrow_mut().take();
            if let Some(session) = session {
                this.finish(&session);
                this.commit(&session);
            }
        });

        let this = Rc::downgrade(self);
        gesture.connect_cancel(move |_, _| {
            let Some(this) = this.upgrade() else {
                return;
            };
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
        let this = Rc::downgrade(self);
        self.day_area.add_tick_callback(move |_, _| {
            let Some(this) = this.upgrade() else {
                return glib::ControlFlow::Break;
            };
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

    /// Width of one day column, or zero when the grid has not been allocated.
    ///
    /// Zero must stay zero. Treating an unallocated grid as one pixel wide
    /// would turn a vertical drag on the leak test's off-screen page into a
    /// day change.
    fn column_width(&self) -> f64 {
        let count = self.days.len();
        let width = self.day_area.width() as f64;
        if count == 0 || width <= 0.0 {
            0.0
        } else {
            width / count as f64
        }
    }

    /// Recompute the snapped target from the current pointer offset and repaint
    /// the preview.
    fn update(&self, offset_x: f64, offset_y: f64) {
        let column_count = self.days.len();
        let column_width = self.column_width();
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
                let pointer_x = session.origin_x + offset_x;
                let pointer_y = session.anchor_y + offset_y;
                // Content y grows downward from midnight. The all-day row sits
                // above the viewport, so "above the hours" is above whatever
                // slice is on screen, not above midnight.
                let visible_top = session
                    .vadj
                    .as_ref()
                    .map(gtk::Adjustment::value)
                    .unwrap_or(0.0);
                let aim = aim_move(
                    pointer_x,
                    pointer_y,
                    column_width,
                    column_count,
                    visible_top,
                );
                // No allocation yet: keep the column the gesture started on.
                if column_width <= 0.0 {
                    session.col = session.orig_col;
                    session.day_shift = 0;
                } else {
                    session.col = aim.col;
                    session.day_shift = aim.day_shift;
                }
                session.all_day = aim.all_day;
                if !session.all_day {
                    let top = snap(session.top_px + offset_y, snap_px)
                        .clamp(0.0, (day_px - session.height_px).max(0.0));
                    session.new_top_px = top;
                    session.new_height_px = session.height_px;
                }
            }
            DragKind::ResizeStart => {
                let bottom = session.top_px + session.height_px;
                let top =
                    snap(session.top_px + offset_y, snap_px).clamp(0.0, bottom - min_block_px);
                session.new_top_px = top;
                session.new_height_px = bottom - top;
                session.col = session.orig_col;
                session.day_shift = 0;
                session.all_day = false;
            }
            DragKind::ResizeEnd => {
                let bottom = snap(session.top_px + session.height_px + offset_y, snap_px)
                    .clamp(session.top_px + min_block_px, day_px);
                session.new_top_px = session.top_px;
                session.new_height_px = bottom - session.top_px;
                session.col = session.orig_col;
                session.day_shift = 0;
                session.all_day = false;
            }
            // Reported by a committed move. A session never has this kind.
            DragKind::ToAllDay => {}
        }

        let col = session.col;
        let all_day = session.all_day;
        let day_shift = session.day_shift;
        let (top, height) = if all_day {
            (0.0, ALL_DAY_PREVIEW_PX)
        } else {
            (session.new_top_px, session.new_height_px)
        };

        let layout_width = if column_width <= 0.0 {
            1.0
        } else {
            column_width
        };
        let x = col as f64 * layout_width + 2.0;
        let width = (layout_width - 4.0).max(1.0);
        self.preview_block
            .set_size_request(width as i32, height.max(1.0) as i32);
        self.preview.move_(&self.preview_block, x, top);
        let landing = self
            .days
            .get(col)
            .copied()
            .map(|day| landing_date(day, day_shift));
        let label = match landing {
            Some(day) => move_preview_label(
                all_day,
                day_shift,
                day,
                &format_minutes(top / px_per_minute),
                &format_minutes((top + height) / px_per_minute),
            ),
            None => String::new(),
        };
        self.preview_label.set_text(&label);
        // On a block too short for the label, showing it would stretch the
        // preview taller than the event it stands for. The all-day bar is
        // drawn tall enough to carry its label.
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
        let geometry_moved = session.col != session.orig_col
            || (session.new_top_px - session.top_px).abs() >= 0.5
            || (session.new_height_px - session.height_px).abs() >= 0.5;
        // A day-page shift or a conversion to all-day changes the event even
        // when the snapped top of the block did not.
        let moved = match session.kind {
            DragKind::Move => geometry_moved || session.day_shift != 0 || session.all_day,
            DragKind::ResizeStart | DragKind::ResizeEnd | DragKind::ToAllDay => geometry_moved,
        };
        if !moved {
            return;
        }

        let Some(column_day) = self.days.get(session.col).copied() else {
            return;
        };
        let Some(source_day) = self.days.get(session.orig_col).copied() else {
            return;
        };
        let start_minutes = (session.new_top_px / px_per_minute).round();
        let end_minutes = ((session.new_top_px + session.new_height_px) / px_per_minute).round();

        match session.kind {
            DragKind::Move => {
                let date = landing_date(column_day, session.day_shift);
                if session.all_day {
                    (self.on_move)(
                        DragKind::ToAllDay,
                        session.event_id,
                        date,
                        None,
                        Some(source_day),
                    );
                } else {
                    (self.on_move)(
                        DragKind::Move,
                        session.event_id,
                        date,
                        time_of(start_minutes),
                        Some(source_day),
                    );
                }
            }
            DragKind::ResizeStart => {
                (self.on_move)(
                    session.kind,
                    session.event_id,
                    source_day,
                    time_of(start_minutes),
                    Some(source_day),
                );
            }
            DragKind::ResizeEnd => {
                // An edge dragged to the very bottom means midnight of the next
                // day, which has no same-day NaiveTime.
                if end_minutes >= MINUTES_PER_DAY {
                    let next = source_day.succ_opt().unwrap_or(source_day);
                    (self.on_move)(
                        session.kind,
                        session.event_id,
                        next,
                        NaiveTime::from_hms_opt(0, 0, 0),
                        Some(source_day),
                    );
                } else {
                    (self.on_move)(
                        session.kind,
                        session.event_id,
                        source_day,
                        time_of(end_minutes),
                        Some(source_day),
                    );
                }
            }
            DragKind::ToAllDay => {}
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

    gesture.connect_drag_update(clone!(
        #[strong]
        block,
        #[strong]
        label,
        #[strong]
        dragging,
        move |gesture, offset_x, offset_y| {
            let Some((press_x, press_y)) = gesture.start_point() else {
                return;
            };
            // The column is the gesture's own widget; capturing it would be a
            // cycle that kept every day column of every page alive.
            let Some(root) = gesture.widget() else {
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

/// How far above the day grid a move has to travel before it becomes all-day.
/// The all-day row sits above the hours, so this is that gesture without
/// needing the pointer to hit that row's widgets.
pub(crate) const ALL_DAY_ENTER_PX: f64 = 16.0;
/// Height of the all-day preview bar. Tall enough for its label.
const ALL_DAY_PREVIEW_PX: f64 = 28.0;

/// Where a move is aimed: the column under the pointer, a day-page shift, and
/// whether the pointer has crossed into the all-day band.
#[derive(Debug, PartialEq)]
pub(crate) struct MoveAim {
    pub col: usize,
    pub day_shift: i64,
    pub all_day: bool,
}

pub(crate) fn aim_move(
    pointer_x: f64,
    pointer_y: f64,
    column_width: f64,
    column_count: usize,
    visible_top: f64,
) -> MoveAim {
    MoveAim {
        col: column_under_pointer(pointer_x, column_width, column_count),
        day_shift: day_page_shift(pointer_x, column_width, column_count),
        all_day: dragged_above_the_day(pointer_y, visible_top),
    }
}

/// The column under the pointer, not the column a grab-point offset would imply.
///
/// A wide block grabbed at its left edge crosses into the next day as soon as
/// the pointer does. Measuring travel from the grab point waits until the hand
/// has moved half a column past where it started, which lands the event on the
/// day beside the cursor.
pub(crate) fn column_under_pointer(
    pointer_x: f64,
    column_width: f64,
    column_count: usize,
) -> usize {
    if column_count == 0 || column_width <= 0.0 {
        return 0;
    }
    let index = (pointer_x / column_width).floor() as i64;
    index.clamp(0, column_count as i64 - 1) as usize
}

/// Days to add on a one-column day page. Week view already has one column per
/// day, so horizontal travel there is a column change and this stays zero.
pub(crate) fn day_page_shift(pointer_x: f64, column_width: f64, column_count: usize) -> i64 {
    if column_count != 1 || column_width <= 0.0 {
        return 0;
    }
    (pointer_x / column_width).floor() as i64
}

/// Whether a move has been dragged into the band above the timed grid.
///
/// `pointer_y` and `visible_top` are both in the day column's coordinates,
/// where midnight is 0 and the scrolled viewport starts at `visible_top`.
/// The all-day row is above that viewport, so a grid scrolled to the
/// afternoon still converts when the pointer crosses its top edge.
pub(crate) fn dragged_above_the_day(pointer_y: f64, visible_top: f64) -> bool {
    pointer_y < visible_top - ALL_DAY_ENTER_PX
}

/// The day a drag commits to: the column it landed on, plus a day-page shift.
pub(crate) fn landing_date(day: NaiveDate, shift: i64) -> NaiveDate {
    // A pointer fling on a tiny allocation can ask for a shift chrono cannot
    // represent. Keeping the column's own day is safer than panicking.
    let shift = shift.clamp(-100_000, 100_000);
    day.checked_add_signed(chrono::Duration::days(shift))
        .unwrap_or(day)
}

/// Preview text for a move. A day-page shift has no column header under the
/// pointer, so the landing weekday is named; an all-day landing says so.
pub(crate) fn move_preview_label(
    all_day: bool,
    day_shift: i64,
    landing: NaiveDate,
    start_label: &str,
    end_label: &str,
) -> String {
    let when = if all_day {
        "All day".to_string()
    } else {
        format!("{start_label} – {end_label}")
    };
    if day_shift == 0 {
        when
    } else {
        format!("{} · {when}", landing.format("%a"))
    }
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
        for kind in [
            DragKind::Move,
            DragKind::ResizeStart,
            DragKind::ResizeEnd,
            DragKind::ToAllDay,
        ] {
            let payload = drag_payload(kind, 42);
            assert_eq!(parse_drag_payload(&payload), Some((kind, 42, None)));
        }
    }

    #[test]
    fn an_occurrence_payload_names_the_day_it_came_from() {
        let day = NaiveDate::from_ymd_opt(2026, 10, 21).unwrap();
        let payload = occurrence_payload(DragKind::Move, 42, day);
        assert_eq!(
            parse_drag_payload(&payload),
            Some((DragKind::Move, 42, Some(day)))
        );
    }

    #[test]
    fn parse_drag_payload_rejects_malformed_values() {
        assert!(parse_drag_payload("move").is_none()); // no ':' separator
        assert!(parse_drag_payload("teleport:42").is_none()); // unknown kind
        assert!(parse_drag_payload("move:notanumber").is_none()); // bad id
        assert!(parse_drag_payload("move:42:not-a-date").is_none()); // bad day
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
    fn the_column_is_the_one_under_the_pointer() {
        // 100px columns. The pointer just inside Wednesday (index 2) lands
        // there even if the grab started at the left edge of Tuesday.
        assert_eq!(column_under_pointer(200.0, 100.0, 7), 2);
        assert_eq!(column_under_pointer(199.0, 100.0, 7), 1);
        // Past either end stays on the page rather than inventing a column.
        assert_eq!(column_under_pointer(-40.0, 100.0, 7), 0);
        assert_eq!(column_under_pointer(10_000.0, 100.0, 7), 6);
        assert_eq!(column_under_pointer(50.0, 0.0, 7), 0);
    }

    #[test]
    fn a_day_page_shifts_to_the_neighbour_under_the_pointer() {
        // One column, 400px wide. Dragging a full column left or right is the
        // previous or next day; week view does not also shift.
        assert_eq!(day_page_shift(-1.0, 400.0, 1), -1);
        assert_eq!(day_page_shift(400.0, 400.0, 1), 1);
        assert_eq!(day_page_shift(850.0, 400.0, 1), 2);
        assert_eq!(day_page_shift(200.0, 400.0, 1), 0);
        assert_eq!(day_page_shift(-400.0, 400.0, 7), 0);
        assert_eq!(day_page_shift(50.0, 0.0, 1), 0);
    }

    #[test]
    fn dragging_above_the_hours_means_all_day() {
        assert!(!dragged_above_the_day(0.0, 0.0));
        assert!(!dragged_above_the_day(-ALL_DAY_ENTER_PX, 0.0));
        assert!(dragged_above_the_day(-ALL_DAY_ENTER_PX - 1.0, 0.0));
        // Scrolled to 9:00 (432px). The pointer can sit on a morning hour in
        // content coordinates and still be in the all-day row above the
        // viewport.
        assert!(dragged_above_the_day(400.0, 432.0));
        assert!(!dragged_above_the_day(420.0, 432.0));
        assert!(!dragged_above_the_day(432.0, 432.0));
    }

    #[test]
    fn the_landing_date_adds_a_day_page_shift() {
        let wednesday = NaiveDate::from_ymd_opt(2026, 10, 21).unwrap();
        assert_eq!(
            landing_date(wednesday, 0),
            NaiveDate::from_ymd_opt(2026, 10, 21).unwrap()
        );
        assert_eq!(
            landing_date(wednesday, 1),
            NaiveDate::from_ymd_opt(2026, 10, 22).unwrap()
        );
        assert_eq!(
            landing_date(wednesday, -1),
            NaiveDate::from_ymd_opt(2026, 10, 20).unwrap()
        );
    }

    #[test]
    fn a_move_aims_at_the_column_or_the_day_under_the_pointer() {
        assert_eq!(
            aim_move(250.0, 10.0, 100.0, 7, 0.0),
            MoveAim {
                col: 2,
                day_shift: 0,
                all_day: false
            }
        );
        // One column: leaving it to the left is the previous day, not a column.
        assert_eq!(
            aim_move(-1.0, 10.0, 400.0, 1, 0.0),
            MoveAim {
                col: 0,
                day_shift: -1,
                all_day: false
            }
        );
        assert_eq!(
            aim_move(850.0, 10.0, 400.0, 1, 0.0),
            MoveAim {
                col: 0,
                day_shift: 2,
                all_day: false
            }
        );
        assert!(aim_move(10.0, -ALL_DAY_ENTER_PX - 1.0, 400.0, 1, 0.0).all_day);
        assert!(aim_move(200.0, 400.0, 400.0, 1, 432.0).all_day);
        let unallocated = aim_move(50.0, 10.0, 0.0, 7, 0.0);
        assert_eq!(unallocated.col, 0);
        assert_eq!(unallocated.day_shift, 0);
    }

    #[test]
    fn the_preview_names_the_day_when_a_day_page_shifts() {
        let wednesday = NaiveDate::from_ymd_opt(2026, 10, 21).unwrap();
        let thursday = NaiveDate::from_ymd_opt(2026, 10, 22).unwrap();
        let tuesday = NaiveDate::from_ymd_opt(2026, 10, 20).unwrap();
        assert_eq!(
            move_preview_label(false, 0, wednesday, "9:00 AM", "10:00 AM"),
            "9:00 AM – 10:00 AM"
        );
        assert_eq!(
            move_preview_label(true, 0, wednesday, "9:00 AM", "10:00 AM"),
            "All day"
        );
        assert_eq!(
            move_preview_label(true, 1, thursday, "9:00 AM", "10:00 AM"),
            "Thu · All day"
        );
        assert_eq!(
            move_preview_label(false, -1, tuesday, "9:00 AM", "10:00 AM"),
            "Tue · 9:00 AM – 10:00 AM"
        );
    }

    #[test]
    fn format_minutes_uses_twelve_hour_clock() {
        assert_eq!(format_minutes(0.0), "12:00 AM"); // midnight
        assert_eq!(format_minutes(9.0 * 60.0 + 5.0), "9:05 AM");
        assert_eq!(format_minutes(12.0 * 60.0), "12:00 PM"); // noon
        assert_eq!(format_minutes(13.0 * 60.0 + 30.0), "1:30 PM");
    }
}
