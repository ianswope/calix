use crate::store::{Event, EventDraft, Store};
use adw::prelude::*;
use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, NaiveTime};
use gtk::glib;
use gtk::glib::clone;
use std::rc::Rc;

const DATETIME_FORMAT: &str = "%Y-%m-%d %H:%M";
const DATE_FORMAT: &str = "%Y-%m-%d";

/// Opens a create/edit dialog for an event. Pass `editing: Some(event)` to
/// edit (adds a Delete button); `None` to create a new one starting at
/// `initial_start`. `on_saved` is called after any successful save or
/// delete so the caller can refresh whatever's showing the event list.
pub fn open(
    parent: &impl IsA<gtk::Widget>,
    store: Rc<Store>,
    calendar_id: i64,
    editing: Option<Event>,
    initial_start: DateTime<Local>,
    on_saved: impl Fn() + 'static,
    remote_update: Option<Rc<dyn Fn(&EventDraft) -> Result<(), String>>>,
    remote_delete: Option<Rc<dyn Fn() -> Result<(), String>>>,
) {
    let on_saved = Rc::new(on_saved);

    let dialog = adw::Dialog::builder()
        .title(if editing.is_some() {
            "Edit Event"
        } else {
            "New Event"
        })
        .content_width(420)
        .build();

    let cancel_button = gtk::Button::with_label("Cancel");
    let save_button = gtk::Button::builder()
        .label("Save")
        .css_classes(["suggested-action"])
        .build();

    let header = adw::HeaderBar::new();
    header.pack_start(&cancel_button);
    header.pack_end(&save_button);

    let title_row = adw::EntryRow::builder().title("Title").build();
    let all_day_row = adw::SwitchRow::builder().title("All day").build();
    let start_row = adw::EntryRow::builder()
        .title("Start (YYYY-MM-DD HH:MM)")
        .build();
    let end_row = adw::EntryRow::builder()
        .title("End (YYYY-MM-DD HH:MM)")
        .build();
    let location_row = adw::EntryRow::builder().title("Location").build();
    let notes_row = adw::EntryRow::builder().title("Notes").build();
    let error_label = gtk::Label::new(None);
    error_label.add_css_class("error");
    error_label.set_xalign(0.0);
    error_label.set_wrap(true);
    let calendar_name = editing
        .as_ref()
        .map(|event| event.calendar_name.clone())
        .unwrap_or_else(|| "Local".to_string());

    match &editing {
        Some(ev) => {
            title_row.set_text(&ev.title);
            all_day_row.set_active(ev.all_day);
            start_row.set_text(&ev.start.format(DATETIME_FORMAT).to_string());
            end_row.set_text(&ev.end.format(DATETIME_FORMAT).to_string());
            location_row.set_text(ev.location.as_deref().unwrap_or(""));
            notes_row.set_text(ev.notes.as_deref().unwrap_or(""));
        }
        None => {
            let end = initial_start + chrono::Duration::hours(1);
            start_row.set_text(&initial_start.format(DATETIME_FORMAT).to_string());
            end_row.set_text(&end.format(DATETIME_FORMAT).to_string());
        }
    }

    let group = adw::PreferencesGroup::new();
    let calendar_row = adw::ActionRow::builder()
        .title("Calendar")
        .subtitle(gtk::glib::markup_escape_text(&calendar_name))
        .build();
    calendar_row.set_subtitle_lines(1);
    group.add(&calendar_row);
    group.add(&title_row);
    group.add(&all_day_row);
    group.add(&start_row);
    group.add(&end_row);
    group.add(&location_row);
    group.add(&notes_row);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);
    content.append(&group);
    content.append(&error_label);

    if let Some(ev) = &editing {
        let event_id = ev.id;
        let delete_button = gtk::Button::builder()
            .label("Delete Event")
            .css_classes(["destructive-action"])
            .build();
        delete_button.connect_clicked(clone!(
            #[weak]
            dialog,
            #[strong]
            store,
            #[strong]
            on_saved,
            #[strong]
            remote_delete,
            #[weak]
            error_label,
            move |_| {
                if let Some(remote_delete) = &remote_delete {
                    if let Err(error) = remote_delete() {
                        error_label.set_label(&error);
                        return;
                    }
                }
                let _ = store.delete_event(event_id);
                on_saved();
                dialog.close();
            }
        ));
        content.append(&delete_button);
    }

    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&content));
    dialog.set_child(Some(&toolbar_view));

    cancel_button.connect_clicked(clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));

    save_button.connect_clicked(clone!(
        #[weak]
        dialog,
        #[strong]
        store,
        #[strong]
        on_saved,
        #[strong]
        editing,
        #[strong]
        remote_update,
        #[weak]
        error_label,
        move |_| {
            let all_day = all_day_row.is_active();
            let Some(start) = parse_datetime(&start_row.text(), all_day) else {
                return;
            };
            let Some(end) = parse_datetime(&end_row.text(), all_day) else {
                return;
            };
            let title = title_row.text().trim().to_string();
            if title.is_empty() {
                return;
            }

            let draft = EventDraft {
                title,
                start,
                end,
                all_day,
                location: non_empty(location_row.text().to_string()),
                notes: non_empty(notes_row.text().to_string()),
            };

            let result = match &editing {
                Some(ev) => {
                    if let Some(remote_update) = &remote_update {
                        if let Err(error) = remote_update(&draft) {
                            error_label.set_label(&error);
                            return;
                        }
                    }
                    store.update_event(ev.id, &draft)
                }
                None => store.create_event(calendar_id, &draft).map(|_| ()),
            };

            if result.is_ok() {
                on_saved();
                dialog.close();
            }
        }
    ));

    dialog.present(Some(parent));
}

fn parse_datetime(text: &str, all_day: bool) -> Option<DateTime<Local>> {
    let text = text.trim();
    let naive = if all_day {
        NaiveDate::parse_from_str(text, DATE_FORMAT)
            .ok()
            .map(|d| NaiveDateTime::new(d, NaiveTime::MIN))
    } else {
        NaiveDateTime::parse_from_str(text, DATETIME_FORMAT).ok()
    }?;
    naive.and_local_timezone(Local).single()
}

fn non_empty(s: String) -> Option<String> {
    let s = s.trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}
