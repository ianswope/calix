//! Eased mouse-wheel scrolling for the timed grid.
//!
//! GTK applies each wheel detent as an instant jump — `page_size^(2/3)` px,
//! around seventy pixels for a grid the height of this window — and only
//! animates keyboard scrolling. A touchpad already arrives as a stream of
//! small smooth deltas with GTK's own kinetic deceleration, so this leaves
//! those alone and intercepts only wheel detents, easing the adjustment
//! toward where GTK would have put it. The distance per detent is GTK's own,
//! so the wheel covers exactly as much ground as before; it just gets there
//! over a few frames instead of in one.

use gtk::prelude::*;
use gtk::{gdk, glib};
use std::cell::Cell;
use std::rc::Rc;

/// How quickly the ease closes the remaining distance: the time, in
/// microseconds, over which about two thirds of what is left gets covered.
/// Short enough that a detent still reads as immediate, long enough that it
/// is not a jump.
const EASE_TAU_US: f64 = 50_000.0;
/// Within this many pixels of the target the ease snaps there and stops.
const SETTLE_PX: f64 = 0.5;
/// The frame interval assumed on the first tick, before the clock has a
/// previous frame to measure from.
const FIRST_FRAME_US: f64 = 16_667.0;

/// Attach eased wheel scrolling to a timed page's scroll container.
pub(crate) fn install(scrolled: &gtk::ScrolledWindow) {
    let controller = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
    // Capture, so a detent is seen here before the scrolled window's own
    // controller applies it as a jump.
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let ease = Rc::new(Ease::default());
    controller.connect_scroll(move |controller, _dx, dy| {
        // Read off the controller rather than captured: a handler that holds
        // the widget its controller is attached to is a reference cycle GTK
        // never breaks, and this runs on every page of every rebuild.
        let Some(scrolled) = controller.widget().and_downcast::<gtk::ScrolledWindow>() else {
            return glib::Propagation::Proceed;
        };
        let vadjustment = scrolled.vadjustment();
        let is_wheel = controller.unit() == gdk::ScrollUnit::Wheel;
        let shifted = controller
            .current_event_state()
            .contains(gdk::ModifierType::SHIFT_MASK);
        if !wheel_handles(
            is_wheel,
            shifted,
            dy,
            vadjustment.page_size(),
            vadjustment.upper(),
        ) {
            // Anything else moves the grid on its own terms; an ease still
            // running would fight it.
            ease.cancel();
            return glib::Propagation::Proceed;
        }
        // Detents queue up from where the last one was already heading, so a
        // quick flick of three covers three steps rather than restarting the
        // ease from wherever the first had got to.
        let from = ease.target.get().unwrap_or_else(|| vadjustment.value());
        ease.target.set(Some(wheel_target(
            from,
            dy,
            vadjustment.page_size(),
            vadjustment.upper(),
        )));
        ease.run(&scrolled);
        glib::Propagation::Stop
    });
    scrolled.add_controller(controller);
}

/// Whether a scroll event on the timed grid is a wheel detent for the ease to
/// take, or something GTK should keep handling exactly as before: a touchpad
/// stream, a Shift+wheel the carousel pages on, a sideways tilt, or a grid
/// with nothing to scroll (where the wheel pages the carousel instead).
fn wheel_handles(is_wheel: bool, shifted: bool, dy: f64, page_size: f64, upper: f64) -> bool {
    is_wheel && !shifted && dy != 0.0 && upper > page_size
}

/// Where one detent of `dy` wheel units sends the adjustment from `from`:
/// GTK's own distance per detent, clamped to the scrollable range.
fn wheel_target(from: f64, dy: f64, page_size: f64, upper: f64) -> f64 {
    let detent = page_size.powf(2.0 / 3.0);
    (from + dy * detent).clamp(0.0, (upper - page_size).max(0.0))
}

/// The adjustment value one frame of `dt_us` microseconds later, easing from
/// `value` toward `target`: exponential, so it is frame-rate independent and
/// never overshoots, and snapped to the target once within `SETTLE_PX`.
fn ease_toward(value: f64, target: f64, dt_us: f64) -> f64 {
    let remaining = target - value;
    if remaining.abs() <= SETTLE_PX {
        return target;
    }
    value + remaining * (1.0 - (-dt_us / EASE_TAU_US).exp())
}

/// The ease in flight, if any. One per scroll container.
#[derive(Default)]
struct Ease {
    /// Where the adjustment is heading; `None` when idle.
    target: Cell<Option<f64>>,
    /// The value the last tick wrote. A reading that differs means something
    /// else — a scrollbar drag, a touchpad — moved the grid since, and the
    /// ease stands down rather than drag it back.
    written: Cell<Option<f64>>,
    last_frame_us: Cell<Option<i64>>,
    ticking: Cell<bool>,
}

impl Ease {
    fn cancel(&self) {
        self.target.set(None);
        self.written.set(None);
        self.last_frame_us.set(None);
    }

    /// Drives the adjustment toward `target` on the frame clock until it
    /// arrives, is cancelled, or finds the grid moved out from under it.
    fn run(self: &Rc<Self>, scrolled: &gtk::ScrolledWindow) {
        if self.ticking.replace(true) {
            return;
        }
        let ease = Rc::clone(self);
        scrolled.add_tick_callback(move |scrolled, clock| {
            let Some(target) = ease.target.get() else {
                ease.ticking.set(false);
                return glib::ControlFlow::Break;
            };
            let vadjustment = scrolled.vadjustment();
            let value = vadjustment.value();
            if ease
                .written
                .get()
                .is_some_and(|written| (written - value).abs() > SETTLE_PX)
            {
                ease.cancel();
                ease.ticking.set(false);
                return glib::ControlFlow::Break;
            }
            let now = clock.frame_time();
            let dt_us = ease
                .last_frame_us
                .get()
                .map_or(FIRST_FRAME_US, |last| (now - last) as f64);
            ease.last_frame_us.set(Some(now));

            vadjustment.set_value(ease_toward(value, target, dt_us));
            let landed = vadjustment.value();
            ease.written.set(Some(landed));
            // Arrived, or pinned by a range that shrank since the target was
            // chosen: either way there is nothing left to ease.
            if (landed - target).abs() <= SETTLE_PX || landed == value {
                ease.cancel();
                ease.ticking.set(false);
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: f64 = 600.0;
    const DAY: f64 = 1152.0;
    const FRAME_US: f64 = 16_667.0;

    #[test]
    fn a_wheel_detent_on_a_scrollable_grid_is_eased() {
        assert!(wheel_handles(true, false, 1.0, PAGE, DAY));
        assert!(wheel_handles(true, false, -1.0, PAGE, DAY));
    }

    #[test]
    fn touchpad_scrolling_is_left_to_gtk() {
        // Smooth deltas already arrive many times a frame with GTK's own
        // kinetic deceleration behind them.
        assert!(!wheel_handles(false, false, 0.3, PAGE, DAY));
    }

    #[test]
    fn shift_wheel_is_left_for_the_carousel_to_page_on() {
        assert!(!wheel_handles(true, true, 1.0, PAGE, DAY));
    }

    #[test]
    fn a_sideways_tilt_is_left_alone() {
        assert!(!wheel_handles(true, false, 0.0, PAGE, DAY));
    }

    #[test]
    fn a_grid_that_fits_its_viewport_is_left_alone() {
        // Nothing to scroll, so the wheel falls through to the carousel and
        // pages the period, as it did before.
        assert!(!wheel_handles(true, false, 1.0, PAGE, 480.0));
        assert!(!wheel_handles(true, false, 1.0, PAGE, PAGE));
    }

    #[test]
    fn a_detent_moves_exactly_as_far_as_gtk_would() {
        let gtk_step = PAGE.powf(2.0 / 3.0);
        assert!((wheel_target(100.0, 1.0, PAGE, DAY) - (100.0 + gtk_step)).abs() < 1e-9);
        assert!((wheel_target(300.0, -1.0, PAGE, DAY) - (300.0 - gtk_step)).abs() < 1e-9);
    }

    #[test]
    fn a_half_detent_from_a_high_resolution_wheel_moves_half_as_far() {
        let gtk_step = PAGE.powf(2.0 / 3.0);
        assert!((wheel_target(100.0, 0.5, PAGE, DAY) - (100.0 + gtk_step / 2.0)).abs() < 1e-9);
    }

    #[test]
    fn a_detent_cannot_scroll_past_either_end() {
        assert_eq!(wheel_target(20.0, -1.0, PAGE, DAY), 0.0);
        assert_eq!(wheel_target(540.0, 1.0, PAGE, DAY), DAY - PAGE);
    }

    #[test]
    fn the_ease_never_overshoots_and_always_advances() {
        let mut value = 0.0;
        while value < 100.0 {
            let next = ease_toward(value, 100.0, FRAME_US);
            assert!(next > value, "each frame moves closer");
            assert!(next <= 100.0, "never past the target");
            value = next;
        }
    }

    #[test]
    fn the_ease_closes_most_of_the_gap_within_a_few_frames() {
        let mut value = 0.0;
        for _ in 0..6 {
            value = ease_toward(value, 100.0, FRAME_US);
        }
        // Six frames at 60 Hz is a tenth of a second: nearly there, so a detent
        // still reads as immediate.
        assert!(value > 85.0, "got to {value}");
    }

    #[test]
    fn the_ease_snaps_when_within_half_a_pixel() {
        assert_eq!(ease_toward(99.7, 100.0, FRAME_US), 100.0);
        assert_eq!(ease_toward(100.3, 100.0, FRAME_US), 100.0);
    }

    #[test]
    fn the_ease_is_frame_rate_independent() {
        // Two 120 Hz frames cover the same ground as one 60 Hz frame.
        let one = ease_toward(0.0, 100.0, FRAME_US);
        let half = ease_toward(0.0, 100.0, FRAME_US / 2.0);
        let two = ease_toward(half, 100.0, FRAME_US / 2.0);
        assert!((one - two).abs() < 1e-6, "{one} vs {two}");
    }

    #[test]
    fn the_ease_works_downward_too() {
        let next = ease_toward(500.0, 400.0, FRAME_US);
        assert!(next < 500.0 && next > 400.0);
    }
}
