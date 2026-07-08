use crate::date_util::{month_start, shift_months, shift_weeks, week_dates, week_start};
use crate::views::{month_view, week_view};
use adw::prelude::*;
use chrono::{Local, NaiveDate};
use gtk::glib;
use gtk::glib::clone;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    Month,
    Week,
}

struct State {
    view_mode: ViewMode,
    current_date: NaiveDate,
}

impl State {
    fn period_anchor(&self) -> NaiveDate {
        match self.view_mode {
            ViewMode::Month => month_start(self.current_date),
            ViewMode::Week => week_start(self.current_date),
        }
    }

    fn shift(&self, delta: i32) -> NaiveDate {
        self.shift_from(self.current_date, delta)
    }

    fn shift_from(&self, date: NaiveDate, delta: i32) -> NaiveDate {
        match self.view_mode {
            ViewMode::Month => shift_months(date, delta),
            ViewMode::Week => shift_weeks(date, delta),
        }
    }

    fn build_page(&self, date: NaiveDate) -> gtk::Widget {
        match self.view_mode {
            ViewMode::Month => month_view::build(date),
            ViewMode::Week => week_view::build(date),
        }
    }

    fn title(&self) -> String {
        match self.view_mode {
            ViewMode::Month => self.period_anchor().format("%B %Y").to_string(),
            ViewMode::Week => {
                let days = week_dates(self.current_date);
                let (start, end) = (days[0], days[6]);
                if start.format("%b").to_string() == end.format("%b").to_string() {
                    format!("{} – {}", start.format("%b %-d"), end.format("%-d, %Y"))
                } else {
                    format!("{} – {}", start.format("%b %-d"), end.format("%b %-d, %Y"))
                }
            }
        }
    }
}

/// Bundles the widgets `reset` and the interactive handlers both need, so
/// they don't have to be threaded through as a long parameter list.
struct Ui {
    carousel: adw::Carousel,
    title_label: gtk::Label,
    state: Rc<RefCell<State>>,
    rebuilding: Rc<Cell<bool>>,
}

impl Ui {
    /// Clears the carousel and rebuilds it with prev/current/next pages
    /// centered on the current period. Deliberately avoids `scroll_to`:
    /// in this libadwaita version it's unreliable when the target was
    /// just appended in the same call (confirmed by trial — it works
    /// maybe half the time, silently leaving the carousel parked on the
    /// wrong page the rest). `append` on an empty carousel making position
    /// 0 correct by construction, then `prepend`ing `prev`, sidesteps the
    /// bug entirely: no jump is ever requested, only structural inserts.
    fn reset(&self) {
        self.rebuilding.set(true);

        let mut child = self.carousel.first_child();
        while let Some(widget) = child {
            let next = widget.next_sibling();
            self.carousel.remove(&widget);
            child = next;
        }

        let state = self.state.borrow();
        let anchor = state.period_anchor();
        let current_page = state.build_page(anchor);
        let prev_page = state.build_page(state.shift_from(anchor, -1));
        let next_page = state.build_page(state.shift_from(anchor, 1));
        let title = state.title();
        drop(state);

        self.carousel.append(&current_page);
        self.carousel.prepend(&prev_page);
        self.carousel.append(&next_page);

        self.rebuilding.set(false);
        self.title_label.set_label(&title);
    }
}

pub fn build(app: &adw::Application) {
    let state = Rc::new(RefCell::new(State {
        view_mode: ViewMode::Month,
        current_date: Local::now().date_naive(),
    }));

    let carousel = adw::Carousel::builder()
        .allow_scroll_wheel(true)
        .hexpand(true)
        .vexpand(true)
        .build();

    let ui = Rc::new(Ui {
        carousel: carousel.clone(),
        title_label: gtk::Label::builder().css_classes(["title"]).build(),
        state,
        // Guards against `page-changed`/`toggled` firing (and reentering
        // `rebuild`) as a side effect of our own programmatic changes.
        rebuilding: Rc::new(Cell::new(false)),
    });

    let today_button = gtk::Button::builder().label("Today").build();
    let prev_button = gtk::Button::from_icon_name("go-previous-symbolic");
    let next_button = gtk::Button::from_icon_name("go-next-symbolic");
    let nav_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    nav_box.add_css_class("linked");
    nav_box.append(&prev_button);
    nav_box.append(&next_button);

    let month_toggle = gtk::ToggleButton::builder()
        .label("Month")
        .active(true)
        .build();
    let week_toggle = gtk::ToggleButton::builder()
        .label("Week")
        .group(&month_toggle)
        .build();
    let view_toggle_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    view_toggle_box.add_css_class("linked");
    view_toggle_box.append(&month_toggle);
    view_toggle_box.append(&week_toggle);

    let header = adw::HeaderBar::new();
    header.pack_start(&today_button);
    header.pack_start(&nav_box);
    header.set_title_widget(Some(&ui.title_label));
    header.pack_end(&view_toggle_box);

    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&carousel));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Calix")
        .default_width(1100)
        .default_height(750)
        .content(&toolbar_view)
        .build();

    window.present();

    // Defer everything interactive until the carousel has actually been
    // allocated real geometry. Two problems otherwise: (1) scroll_to()
    // computes its jump as a pixel offset (position * width); called while
    // width is still 0 it silently resolves to an offset of 0 for any
    // target and never leaves the first page. (2) GTK's own startup
    // machinery (toggle-group resolution, the carousel's initial position
    // notify) fires a flurry of signals while the window first realizes;
    // connecting our handlers only after that settles keeps them from
    // being mistaken for real user input.
    carousel.add_tick_callback(clone!(
        #[strong]
        ui,
        #[strong]
        today_button,
        #[strong]
        prev_button,
        #[strong]
        next_button,
        #[strong]
        month_toggle,
        #[strong]
        week_toggle,
        move |carousel, _clock| {
            if carousel.width() <= 0 {
                return glib::ControlFlow::Continue;
            }

            ui.reset();
            connect_handlers(
                &ui,
                &today_button,
                &prev_button,
                &next_button,
                &month_toggle,
                &week_toggle,
            );
            glib::ControlFlow::Break
        }
    ));
}

fn connect_handlers(
    ui: &Rc<Ui>,
    today_button: &gtk::Button,
    prev_button: &gtk::Button,
    next_button: &gtk::Button,
    month_toggle: &gtk::ToggleButton,
    week_toggle: &gtk::ToggleButton,
) {
    ui.carousel.connect_page_changed(clone!(
        #[strong]
        ui,
        move |_, index| {
            if ui.rebuilding.get() || (index != 0 && index != 2) {
                return;
            }
            let delta = if index == 0 { -1 } else { 1 };
            let mut s = ui.state.borrow_mut();
            s.current_date = s.shift(delta);
            drop(s);
            ui.reset();
        }
    ));

    today_button.connect_clicked(clone!(
        #[strong]
        ui,
        move |_| {
            ui.state.borrow_mut().current_date = Local::now().date_naive();
            ui.reset();
        }
    ));

    prev_button.connect_clicked(clone!(
        #[strong]
        ui,
        move |_| {
            let mut s = ui.state.borrow_mut();
            s.current_date = s.shift(-1);
            drop(s);
            ui.reset();
        }
    ));

    next_button.connect_clicked(clone!(
        #[strong]
        ui,
        move |_| {
            let mut s = ui.state.borrow_mut();
            s.current_date = s.shift(1);
            drop(s);
            ui.reset();
        }
    ));

    month_toggle.connect_toggled(clone!(
        #[strong]
        ui,
        move |btn| {
            if btn.is_active() {
                ui.state.borrow_mut().view_mode = ViewMode::Month;
                ui.reset();
            }
        }
    ));

    week_toggle.connect_toggled(clone!(
        #[strong]
        ui,
        move |btn| {
            if btn.is_active() {
                ui.state.borrow_mut().view_mode = ViewMode::Week;
                ui.reset();
            }
        }
    ));
}
