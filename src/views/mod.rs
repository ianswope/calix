use crate::store::Event;
use chrono::{DateTime, Local, NaiveDate, NaiveTime, Timelike};
use gtk::prelude::*;
use gtk::{gdk, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub(crate) mod drag;
mod event_widget;
pub mod month_view;
pub mod week_view;
pub mod year_view;

/// Opens the new-event dialog at `start`. The second argument is the end time
/// when the caller knows one — a create-drag draws an explicit span — and
/// `None` when it should fall back to the default duration.
pub(crate) type CreateFn = Rc<dyn Fn(DateTime<Local>, Option<DateTime<Local>>)>;

/// Opens the inspector for an event. The widget is the chip or block that was
/// clicked, which the popover anchors itself to — a popover with no anchor has
/// nowhere meaningful to point.
pub(crate) type EditFn = Rc<dyn Fn(Event, gtk::Widget)>;

/// Commits a finished move or resize: which edge was dragged, which event, and
/// the day and (in a timed view) the time it was dropped on.
pub(crate) type MoveFn = Rc<dyn Fn(drag::DragKind, i64, NaiveDate, Option<NaiveTime>)>;

/// A place on the grid: the day, and — where the view draws one — the hour
/// within it. This is what a click on empty space selects, what the highlight
/// is drawn around, and what a paste lands on.
///
/// `time` is `None` in month view, whose cells name a date and nothing finer.
/// A paste onto one of those keeps the copied event's own time of day, while a
/// paste onto an hour cell takes the hour that is highlighted — what is on
/// screen and what happens are then the same thing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Slot {
    pub day: NaiveDate,
    pub time: Option<NaiveTime>,
}

/// How a view turns the moment under the pointer into a [`Slot`]: a month cell
/// names only the day it stands for, while an hour row names the hour it draws.
#[derive(Clone, Copy)]
pub(crate) enum SlotGrain {
    Day,
    Hour,
}

impl SlotGrain {
    fn slot_at(self, moment: DateTime<Local>) -> Slot {
        Slot {
            day: moment.date_naive(),
            time: match self {
                Self::Day => None,
                Self::Hour => NaiveTime::from_hms_opt(moment.time().hour(), 0, 0),
            },
        }
    }
}

/// A "this is the one you picked" mark that moves between widgets.
///
/// It is moved directly rather than by rebuilding the grid: a rebuild would
/// close any open popover and re-query the store for what is a purely visual
/// change. The previous holder is remembered weakly, because a redraw can
/// finalize it while this still points at it.
#[derive(Clone)]
pub(crate) struct Highlight {
    class: &'static str,
    marked: Rc<RefCell<Option<glib::WeakRef<gtk::Widget>>>>,
}

impl Highlight {
    fn new(class: &'static str) -> Self {
        Self {
            class,
            marked: Rc::new(RefCell::new(None)),
        }
    }

    /// Marks `widget`, unmarking whatever held the mark before it.
    pub fn move_to(&self, widget: &impl IsA<gtk::Widget>) {
        let widget = widget.clone().upcast::<gtk::Widget>();
        self.clear();
        widget.add_css_class(self.class);
        *self.marked.borrow_mut() = Some(widget.downgrade());
    }

    pub fn clear(&self) {
        if let Some(previous) = self.marked.borrow_mut().take()
            && let Some(previous) = previous.upgrade()
        {
            previous.remove_css_class(self.class);
        }
    }
}

/// Which slot is selected, and the mark drawn on it.
#[derive(Clone)]
pub(crate) struct SlotSelection {
    slot: Rc<Cell<Option<Slot>>>,
    highlight: Highlight,
}

impl Default for SlotSelection {
    fn default() -> Self {
        Self {
            slot: Rc::new(Cell::new(None)),
            highlight: Highlight::new("selected-slot"),
        }
    }
}

impl SlotSelection {
    pub fn selected(&self) -> Option<Slot> {
        self.slot.get()
    }

    pub fn select(&self, slot: Slot, widget: &impl IsA<gtk::Widget>) {
        self.slot.set(Some(slot));
        self.highlight.move_to(widget);
    }

    pub fn clear(&self) {
        self.slot.set(None);
        self.highlight.clear();
    }

    /// Re-marks a freshly built cell that stands for the slot still selected.
    /// A redraw makes new widgets for the same days, and a selection the user
    /// can no longer see is one Ctrl+V would surprise them with.
    pub fn restore(&self, slot: Slot, widget: &impl IsA<gtk::Widget>) {
        if self.slot.get() == Some(slot) {
            self.highlight.move_to(widget);
        }
    }
}

/// Which event is selected — the one Ctrl+C copies and Ctrl+X cuts — and the
/// ring drawn around it. The id is what survives a redraw; the widget doesn't.
#[derive(Clone)]
pub(crate) struct EventSelection {
    id: Rc<Cell<Option<i64>>>,
    highlight: Highlight,
}

impl Default for EventSelection {
    fn default() -> Self {
        Self {
            id: Rc::new(Cell::new(None)),
            highlight: Highlight::new("selected-event"),
        }
    }
}

impl EventSelection {
    pub fn select(&self, id: i64, widget: &impl IsA<gtk::Widget>) {
        self.id.set(Some(id));
        self.highlight.move_to(widget);
    }

    pub fn clear(&self) {
        self.id.set(None);
        self.highlight.clear();
    }

    pub fn restore(&self, id: i64, widget: &impl IsA<gtk::Widget>) {
        if self.id.get() == Some(id) {
            self.highlight.move_to(widget);
        }
    }
}

/// The clipboard half of a page's right-click menu: whether an event is on
/// Calix's clipboard at this moment, and dropping a copy of it onto a slot.
///
/// Readiness is a callback rather than a flag because a menu is built when its
/// page is — usually well before the copy it will go on to offer.
#[derive(Clone)]
pub(crate) struct PasteAction {
    pub ready: Rc<dyn Fn() -> bool>,
    pub paste: Rc<dyn Fn(Slot)>,
}

/// Everything a page does when it is clicked, in one bundle.
///
/// Every view threads the identical set down to its day cells, so they travel
/// together rather than as four positional arguments that each hop has to
/// repeat in the right order.
#[derive(Clone)]
pub(crate) struct PageActions {
    pub on_create: CreateFn,
    pub on_edit: EditFn,
    pub on_move: MoveFn,
    pub paste: PasteAction,
    /// What the page should draw as selected, and what its cells and chips
    /// hand a click to. Selection outlives any one page: the grid is rebuilt
    /// constantly, so it cannot live on the widgets it is drawn on.
    pub slots: SlotSelection,
    pub events: EventSelection,
}

/// Whether an event's half-open time range includes a calendar date.
pub(crate) fn event_occurs_on_day(event: &Event, day: NaiveDate) -> bool {
    event.start.date_naive() <= day
        && day <= crate::date_util::last_covered_day(event.start, event.end)
}

/// Attach the right-click menu for empty calendar space to `widget`: New Event
/// always, and Paste Event whenever something has been copied. `day` is the
/// date this widget stands for, and `moment_at` maps the press position (in
/// `widget` coordinates) to the start time a new event would get. Presses that
/// land on event chips/blocks (buttons) are left alone — those may grow a
/// context menu of their own someday.
pub(crate) fn add_slot_menu(
    widget: &impl IsA<gtk::Widget>,
    grain: SlotGrain,
    moment_at: impl Fn(f64, f64) -> Option<DateTime<Local>> + 'static,
    on_create: CreateFn,
    paste: PasteAction,
) {
    let gesture = gtk::GestureClick::new();
    gesture.set_button(gdk::BUTTON_SECONDARY);
    gesture.connect_pressed(move |gesture, _, x, y| {
        // Read off the gesture rather than captured: a closure on a widget's
        // own controller that holds that widget is a reference cycle GTK never
        // breaks, and this runs on every cell of every rebuilt page.
        let Some(target) = gesture.widget() else {
            return;
        };
        if press_hits_button(&target, x, y) {
            return;
        }
        let Some(start) = moment_at(x, y) else {
            return;
        };
        gesture.set_state(gtk::EventSequenceState::Claimed);
        show_slot_menu(
            &target,
            x,
            y,
            grain,
            start,
            on_create.clone(),
            paste.clone(),
        );
    });
    widget.add_controller(gesture);
}

/// Whether the press landed on a button (an event chip/block) rather than
/// empty calendar space.
pub(crate) fn press_hits_button(root: &gtk::Widget, x: f64, y: f64) -> bool {
    let mut widget = root.pick(x, y, gtk::PickFlags::DEFAULT);
    while let Some(current) = widget {
        if current == *root {
            return false;
        }
        if current.is::<gtk::Button>() {
            return true;
        }
        widget = current.parent();
    }
    false
}

fn show_slot_menu(
    parent: &gtk::Widget,
    x: f64,
    y: f64,
    grain: SlotGrain,
    start: DateTime<Local>,
    on_create: CreateFn,
    paste: PasteAction,
) {
    let popover = gtk::Popover::new();
    popover.set_parent(parent);
    popover.set_has_arrow(false);
    popover.add_css_class("menu");
    popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));

    let items = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let new_item = menu_item("New Event");
    let weak = popover.downgrade();
    new_item.connect_clicked(move |_| {
        if let Some(popover) = weak.upgrade() {
            popover.popdown();
        }
        on_create(start, None);
    });
    items.append(&new_item);

    // Offered only when there is something to paste: a permanently dead menu
    // entry teaches nothing, and the clipboard is empty for most of a session.
    if (paste.ready)() {
        let paste_item = menu_item("Paste Event");
        let weak = popover.downgrade();
        paste_item.connect_clicked(move |_| {
            if let Some(popover) = weak.upgrade() {
                popover.popdown();
            }
            (paste.paste)(grain.slot_at(start));
        });
        items.append(&paste_item);
    }
    popover.set_child(Some(&items));

    // A dismissed popover must be manually unparented or it (and everything
    // its closures captured) lives as long as its parent widget; deferred to
    // idle so it isn't yanked out from under the `closed` emission.
    popover.connect_closed(|popover| {
        let popover = popover.clone();
        glib::idle_add_local_once(move || popover.unparent());
    });
    popover.popup();
}

fn menu_item(label: &str) -> gtk::Button {
    let item = gtk::Button::with_label(label);
    item.add_css_class("flat");
    if let Some(label) = item.child().and_downcast::<gtk::Label>() {
        label.set_halign(gtk::Align::Start);
    }
    item
}
