//! The location field's type-ahead: a suggestion list that drops out of the
//! Location row while you type.
//!
//! What to suggest is decided in [`crate::places`] and [`crate::store`], both of
//! which are testable without a display. What is left here is the part that
//! genuinely needs widgets — when to ask, where to put the list, and which key
//! does what — kept as thin as it can be.
//!
//! The list is a popover that deliberately does not take the focus
//! (`autohide` off): a type-ahead the user cannot keep typing into is no use.
//! That trade means every way of dismissing it is wired by hand below.

use crate::config::Config;
use crate::places;
use crate::store::Store;
use gtk::glib;
use gtk::glib::clone;
use gtk::prelude::*;
use gtk::{gdk, pango};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

/// How long typing has to pause before the geocoder is asked. Long enough that
/// a normally typed word is one request rather than six, short enough that the
/// list arrives while the user is still looking at the field.
const DEBOUNCE: Duration = Duration::from_millis(300);

/// How often the main loop checks a running lookup for its answer, matching the
/// other provider calls in Calix.
const POLL: Duration = Duration::from_millis(100);

/// State shared between the entry, the timer, and the worker's poll.
///
/// The row is held weakly on purpose. The controllers below live on the row and
/// hold this strongly, so a strong reference back would be a cycle: the row, its
/// popover and this state would all outlive the dialog they belong to, one set
/// per event opened.
struct Completion {
    row: glib::WeakRef<adw::EntryRow>,
    popover: gtk::Popover,
    list: gtk::ListBox,
    store: Rc<Store>,
    /// The geocoder to ask, or `None` when `[places] enabled = false` leaves
    /// only the local half running.
    endpoint: Option<String>,
    /// Bumped by every keystroke. A debounce timer or a finished lookup that
    /// finds the count moved on is answering a question nobody is asking any
    /// more, and drops its result.
    generation: Cell<u64>,
    /// Set while a suggestion is being written into the entry, so the change it
    /// causes doesn't start a fresh search for the text just accepted.
    accepting: Cell<bool>,
    /// What the list currently holds, in the order shown.
    shown: RefCell<Vec<String>>,
}

/// Attaches the type-ahead to an existing Location row.
pub fn attach(row: &adw::EntryRow, store: Rc<Store>) {
    // Read here rather than threaded down from the window: the dialog already
    // takes nine arguments, this is one small file read per dialog open, and it
    // means an edit to config.toml takes effect on the next event opened rather
    // than on the next launch.
    let endpoint = Config::load().places_endpoint().map(str::to_string);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.add_css_class("boxed-list");

    let popover = gtk::Popover::new();
    popover.set_parent(row);
    popover.set_position(gtk::PositionType::Bottom);
    popover.set_has_arrow(false);
    // The list must not take the focus away from the entry the user is still
    // typing into, which is exactly what an autohiding popover does.
    popover.set_autohide(false);
    popover.set_child(Some(&list));

    let completion = Rc::new(Completion {
        row: row.downgrade(),
        popover: popover.clone(),
        list,
        store,
        endpoint,
        generation: Cell::new(0),
        accepting: Cell::new(false),
        shown: RefCell::new(Vec::new()),
    });

    // Clicking a row commits it. `row-activated` covers the click and the
    // keyboard alike, so the two paths can't disagree — and it is connected
    // once here, not on every rebuild of the list. Weak, because the list is
    // owned by the completion: a strong reference back would be the one cycle
    // the weak `row` above doesn't cover, and it would keep the popover and
    // list alive after their dialog was gone.
    completion.list.connect_row_activated(clone!(
        #[weak]
        completion,
        move |_, row| completion.accept(row.index())
    ));

    row.connect_changed(clone!(
        #[strong]
        completion,
        move |_| completion.text_changed()
    ));

    // Focus is what dismisses the list: moving to another field, or out of the
    // dialog entirely, means the user is done with it. A popover that isn't
    // autohiding will otherwise hang over whatever comes next.
    let focus = gtk::EventControllerFocus::new();
    focus.connect_leave(clone!(
        #[strong]
        completion,
        move |_| completion.dismiss()
    ));
    focus.connect_enter(clone!(
        #[strong]
        completion,
        move |_| completion.text_changed()
    ));
    row.add_controller(focus);

    // Capture phase: Down/Up/Enter/Escape belong to the list while it is up,
    // and the entry (and the dialog's default Save) must not see them first.
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed(clone!(
        #[strong]
        completion,
        move |_, key, _, _| completion.key_pressed(key)
    ));
    row.add_controller(keys);

    // A popover parented to a widget outlives its own dismissal unless it is
    // explicitly unparented, and the row is gone with the dialog.
    row.connect_destroy(move |_| popover.unparent());
}

impl Completion {
    /// A keystroke landed in the field: answer from what this calendar already
    /// knows straight away, and start the clock on asking the geocoder.
    fn text_changed(self: &Rc<Self>) {
        let Some(row) = self.row.upgrade() else {
            return;
        };
        if self.accepting.get() {
            return;
        }
        let query = row.text().to_string();
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);

        let history = self
            .store
            .recent_locations(&query, places::MAX_SUGGESTIONS)
            .unwrap_or_default();
        self.show(places::suggestions(
            history.clone(),
            Vec::new(),
            places::MAX_SUGGESTIONS,
        ));

        let Some(endpoint) = self.endpoint.clone() else {
            return;
        };
        if !places::should_search(&query) {
            return;
        }
        glib::timeout_add_local_once(
            DEBOUNCE,
            clone!(
                #[strong(rename_to = completion)]
                self,
                move || {
                    // Typing carried on, so this prefix is no longer the
                    // question — and no request is made for it.
                    if completion.generation.get() != generation {
                        return;
                    }
                    completion.search(endpoint, query, history, generation);
                }
            ),
        );
    }

    /// Asks the geocoder on a worker thread, and folds the answer in behind the
    /// local suggestions if it is still wanted when it lands.
    fn search(
        self: &Rc<Self>,
        endpoint: String,
        query: String,
        history: Vec<String>,
        generation: u64,
    ) {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(places::search(&endpoint, &query));
        });
        glib::timeout_add_local(
            POLL,
            clone!(
                #[strong(rename_to = completion)]
                self,
                move || {
                    let found = match rx.try_recv() {
                        Ok(Ok(found)) => found,
                        // A geocoder that is unreachable, rate-limiting, or
                        // talking nonsense leaves the local suggestions
                        // standing. Nothing here is worth an error banner over
                        // a half-typed word.
                        Ok(Err(_)) | Err(mpsc::TryRecvError::Disconnected) => {
                            return glib::ControlFlow::Break;
                        }
                        Err(mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
                    };
                    if completion.generation.get() == generation {
                        completion.show(places::suggestions(
                            history.clone(),
                            found,
                            places::MAX_SUGGESTIONS,
                        ));
                    }
                    glib::ControlFlow::Break
                }
            ),
        );
    }

    /// Puts `suggestions` in the list, or takes the list away when there are
    /// none. A single suggestion identical to what has been typed is no
    /// suggestion at all, so it counts as none.
    fn show(self: &Rc<Self>, suggestions: Vec<String>) {
        let Some(row) = self.row.upgrade() else {
            return;
        };
        let typed = row.text().to_string();
        let suggestions: Vec<String> = suggestions
            .into_iter()
            .filter(|suggestion| !suggestion.eq_ignore_ascii_case(typed.trim()))
            .collect();
        // Filling in an event being edited sets this text before the dialog is
        // on screen; a popover has nowhere to point until the row it hangs off
        // has been mapped.
        if !row.is_mapped() {
            *self.shown.borrow_mut() = suggestions;
            return;
        }
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        if suggestions.is_empty() {
            *self.shown.borrow_mut() = suggestions;
            self.dismiss();
            return;
        }
        for suggestion in &suggestions {
            let label = gtk::Label::new(Some(suggestion));
            label.set_xalign(0.0);
            label.set_ellipsize(pango::EllipsizeMode::End);
            label.set_margin_top(8);
            label.set_margin_bottom(8);
            label.set_margin_start(10);
            label.set_margin_end(10);
            let list_row = gtk::ListBoxRow::new();
            list_row.set_child(Some(&label));
            self.list.append(&list_row);
        }
        *self.shown.borrow_mut() = suggestions;

        self.popover.set_width_request(row.width().max(240));
        self.popover.popup();
    }

    /// Down/Up walk the list, Enter takes the highlighted row, Escape puts the
    /// list away. Everything else belongs to the entry.
    fn key_pressed(self: &Rc<Self>, key: gdk::Key) -> glib::Propagation {
        if !self.popover.is_visible() {
            return glib::Propagation::Proceed;
        }
        let count = self.shown.borrow().len();
        if count == 0 {
            return glib::Propagation::Proceed;
        }
        let current = self.list.selected_row().map(|row| row.index());
        match key {
            gdk::Key::Down | gdk::Key::KP_Down => {
                let next = current.map_or(0, |index| (index + 1) % count as i32);
                self.highlight(next);
                glib::Propagation::Stop
            }
            gdk::Key::Up | gdk::Key::KP_Up => {
                let previous = current.map_or(count as i32 - 1, |index| {
                    (index + count as i32 - 1) % count as i32
                });
                self.highlight(previous);
                glib::Propagation::Stop
            }
            // Enter with nothing highlighted is the user finishing the field,
            // not choosing a suggestion — it must still reach Save.
            gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::ISO_Enter => match current {
                Some(index) => {
                    self.accept(index);
                    glib::Propagation::Stop
                }
                None => {
                    self.dismiss();
                    glib::Propagation::Proceed
                }
            },
            // Escape closes the list rather than the whole dialog, which is
            // what it would otherwise do with an unfinished event in it.
            gdk::Key::Escape => {
                self.dismiss();
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        }
    }

    fn highlight(self: &Rc<Self>, index: i32) {
        if let Some(row) = self.list.row_at_index(index) {
            self.list.select_row(Some(&row));
        }
    }

    /// Writes the chosen suggestion into the field and puts the list away,
    /// without letting the write start a search for what was just accepted.
    fn accept(self: &Rc<Self>, index: i32) {
        let Some(suggestion) = self
            .shown
            .borrow()
            .get(usize::try_from(index).unwrap_or(usize::MAX))
            .cloned()
        else {
            return;
        };
        let Some(row) = self.row.upgrade() else {
            return;
        };
        self.accepting.set(true);
        row.set_text(&suggestion);
        row.set_position(-1);
        self.accepting.set(false);
        // The accepted text is the answer, so nothing in flight for the older
        // prefix may reopen the list behind it.
        self.generation.set(self.generation.get().wrapping_add(1));
        self.dismiss();
    }

    fn dismiss(self: &Rc<Self>) {
        self.list.unselect_all();
        self.popover.popdown();
    }
}
