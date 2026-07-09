use crate::config::GoogleConfig;
use crate::google;
use crate::icloud;
use crate::store::{Event, EventDraft, Store};
use adw::prelude::*;
use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
use gtk::glib;
use gtk::glib::clone;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

const DATETIME_FORMAT: &str = "%Y-%m-%d %H:%M";
const DATE_FORMAT: &str = "%Y-%m-%d";

/// The network details needed to change an existing remote event. This is
/// owned and `Send`, so event dialogs can do remote work off the GTK thread.
#[derive(Clone)]
pub enum RemoteEvent {
    Unavailable(String),
    Google {
        config: GoogleConfig,
        token_key: String,
        calendar_id: String,
        event_id: String,
    },
    Icloud {
        apple_id: String,
        token_key: String,
        event_href: String,
    },
}

impl RemoteEvent {
    fn update(&self, draft: &EventDraft) -> Result<(), String> {
        match self {
            Self::Unavailable(error) => Err(error.clone()),
            Self::Google {
                config,
                token_key,
                calendar_id,
                event_id,
            } => {
                let access_token = google::oauth::get_access_token(config, token_key)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| "Google account is not connected".to_string())?;
                google::calendar_api::update_event(&access_token, calendar_id, event_id, draft)
            }
            Self::Icloud {
                apple_id,
                token_key,
                event_href,
            } => {
                let app_password = icloud::credentials::app_password(token_key)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| "iCloud account is not connected".to_string())?;
                let credentials = icloud::caldav::Credentials {
                    apple_id: apple_id.clone(),
                    app_password,
                };
                icloud::caldav::update_event(&credentials, event_href, draft)
            }
        }
    }

    fn delete(&self) -> Result<(), String> {
        match self {
            Self::Unavailable(error) => Err(error.clone()),
            Self::Google {
                config,
                token_key,
                calendar_id,
                event_id,
            } => {
                let access_token = google::oauth::get_access_token(config, token_key)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| "Google account is not connected".to_string())?;
                google::calendar_api::delete_event(&access_token, calendar_id, event_id)
            }
            Self::Icloud {
                apple_id,
                token_key,
                event_href,
            } => {
                let app_password = icloud::credentials::app_password(token_key)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| "iCloud account is not connected".to_string())?;
                let credentials = icloud::caldav::Credentials {
                    apple_id: apple_id.clone(),
                    app_password,
                };
                icloud::caldav::delete_event(&credentials, event_href)
            }
        }
    }
}

/// Opens a create/edit dialog for an event. Remote changes are completed in a
/// worker thread before the local cache is updated.
pub fn open(
    parent: &impl IsA<gtk::Widget>,
    store: Rc<Store>,
    calendar_id: i64,
    editing: Option<Event>,
    initial_start: DateTime<Local>,
    on_saved: impl Fn() + 'static,
    remote_event: Option<RemoteEvent>,
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
    let start_row = adw::EntryRow::new();
    let end_row = adw::EntryRow::new();
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
        Some(event) => {
            title_row.set_text(&event.title);
            all_day_row.set_active(event.all_day);
            set_time_rows(&start_row, &end_row, event.start, event.end, event.all_day);
            location_row.set_text(event.location.as_deref().unwrap_or(""));
            notes_row.set_text(event.notes.as_deref().unwrap_or(""));
        }
        None => {
            set_time_rows(
                &start_row,
                &end_row,
                initial_start,
                initial_start + chrono::Duration::hours(1),
                false,
            );
        }
    }

    all_day_row.connect_active_notify(clone!(
        #[weak]
        start_row,
        #[weak]
        end_row,
        move |row| switch_time_format(&start_row, &end_row, row.is_active())
    ));

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

    if let Some(event) = &editing {
        let event_id = event.id;
        let delete_button = gtk::Button::builder()
            .label("Delete Event")
            .css_classes(["destructive-action"])
            .build();
        delete_button.connect_clicked(clone!(
            #[strong]
            dialog,
            #[strong]
            store,
            #[strong]
            on_saved,
            #[strong]
            remote_event,
            #[strong]
            delete_button,
            #[weak]
            error_label,
            move |_| {
                if let Some(remote_event) = remote_event.clone() {
                    delete_button.set_sensitive(false);
                    let (tx, rx) = mpsc::channel();
                    std::thread::spawn(move || {
                        let _ = tx.send(remote_event.delete());
                    });
                    glib::timeout_add_local(
                        Duration::from_millis(100),
                        clone!(
                            #[strong]
                            dialog,
                            #[strong]
                            store,
                            #[strong]
                            on_saved,
                            #[strong]
                            delete_button,
                            #[strong]
                            error_label,
                            move || match rx.try_recv() {
                                Ok(Ok(())) => match store.delete_event(event_id) {
                                    Ok(()) => {
                                        on_saved();
                                        dialog.close();
                                        glib::ControlFlow::Break
                                    }
                                    Err(error) => {
                                        error_label.set_label(&format!(
                                            "Remote event deleted, but the local cache could not be updated: {error}"
                                        ));
                                        delete_button.set_sensitive(true);
                                        glib::ControlFlow::Break
                                    }
                                },
                                Ok(Err(error)) => {
                                    error_label.set_label(&error);
                                    delete_button.set_sensitive(true);
                                    glib::ControlFlow::Break
                                }
                                Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                                Err(mpsc::TryRecvError::Disconnected) => {
                                    error_label.set_label("Remote delete stopped unexpectedly");
                                    delete_button.set_sensitive(true);
                                    glib::ControlFlow::Break
                                }
                            }
                        ),
                    );
                } else {
                    match store.delete_event(event_id) {
                        Ok(()) => {
                            on_saved();
                            dialog.close();
                        }
                        Err(error) => {
                            error_label.set_label(&format!("Couldn't delete event: {error}"))
                        }
                    }
                }
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
        #[strong]
        dialog,
        #[strong]
        store,
        #[strong]
        on_saved,
        #[strong]
        editing,
        #[strong]
        remote_event,
        #[strong]
        save_button,
        #[weak]
        error_label,
        move |_| {
            let all_day = all_day_row.is_active();
            let Some(start) = parse_datetime(&start_row.text(), all_day) else {
                error_label.set_label(&invalid_date_message(all_day));
                return;
            };
            let Some(end) = parse_datetime(&end_row.text(), all_day) else {
                error_label.set_label(&invalid_date_message(all_day));
                return;
            };
            let title = title_row.text().trim().to_string();
            if title.is_empty() {
                error_label.set_label("A title is required");
                return;
            }
            if end <= start {
                error_label.set_label(if all_day {
                    "The end date must be after the start date"
                } else {
                    "The end time must be after the start time"
                });
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

            let Some(event) = editing.as_ref() else {
                match store.create_event(calendar_id, &draft) {
                    Ok(_) => {
                        on_saved();
                        dialog.close();
                    }
                    Err(error) => error_label.set_label(&format!("Couldn't save event: {error}")),
                }
                return;
            };

            if let Some(remote_event) = remote_event.clone() {
                save_button.set_sensitive(false);
                let event_id = event.id;
                let (tx, rx) = mpsc::channel();
                let remote_draft = draft.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(remote_event.update(&remote_draft));
                });
                glib::timeout_add_local(
                    Duration::from_millis(100),
                    clone!(
                        #[strong]
                        dialog,
                        #[strong]
                        store,
                        #[strong]
                        on_saved,
                        #[strong]
                        save_button,
                        #[strong]
                        error_label,
                        move || match rx.try_recv() {
                            Ok(Ok(())) => match store.update_event(event_id, &draft) {
                                Ok(()) => {
                                    on_saved();
                                    dialog.close();
                                    glib::ControlFlow::Break
                                }
                                Err(error) => {
                                    error_label.set_label(&format!(
                                        "Remote event saved, but the local cache could not be updated: {error}"
                                    ));
                                    save_button.set_sensitive(true);
                                    glib::ControlFlow::Break
                                }
                            },
                            Ok(Err(error)) => {
                                error_label.set_label(&error);
                                save_button.set_sensitive(true);
                                glib::ControlFlow::Break
                            }
                            Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                            Err(mpsc::TryRecvError::Disconnected) => {
                                error_label.set_label("Remote save stopped unexpectedly");
                                save_button.set_sensitive(true);
                                glib::ControlFlow::Break
                            }
                        }
                    ),
                );
            } else {
                match store.update_event(event.id, &draft) {
                    Ok(()) => {
                        on_saved();
                        dialog.close();
                    }
                    Err(error) => error_label.set_label(&format!("Couldn't save event: {error}")),
                }
            }
        }
    ));

    dialog.present(Some(parent));
}

fn set_time_rows(
    start_row: &adw::EntryRow,
    end_row: &adw::EntryRow,
    start: DateTime<Local>,
    end: DateTime<Local>,
    all_day: bool,
) {
    let format = if all_day {
        DATE_FORMAT
    } else {
        DATETIME_FORMAT
    };
    start_row.set_title(if all_day {
        "Start (YYYY-MM-DD)"
    } else {
        "Start (YYYY-MM-DD HH:MM)"
    });
    end_row.set_title(if all_day {
        "End (YYYY-MM-DD)"
    } else {
        "End (YYYY-MM-DD HH:MM)"
    });
    start_row.set_text(&start.format(format).to_string());
    end_row.set_text(&end.format(format).to_string());
}

fn switch_time_format(start_row: &adw::EntryRow, end_row: &adw::EntryRow, all_day: bool) {
    if all_day {
        let Some(start) = parse_datetime(&start_row.text(), false) else {
            return;
        };
        let Some(end) = parse_datetime(&end_row.text(), false) else {
            return;
        };
        let mut end_date = end.date_naive();
        if end.time() != NaiveTime::MIN || end_date <= start.date_naive() {
            end_date = end_date.succ_opt().unwrap_or(end_date);
        }
        set_time_rows(
            start_row,
            end_row,
            start,
            Local
                .from_local_datetime(&end_date.and_time(NaiveTime::MIN))
                .single()
                .unwrap_or(end),
            true,
        );
    } else {
        let Some(start_date) = NaiveDate::parse_from_str(start_row.text().trim(), DATE_FORMAT).ok()
        else {
            return;
        };
        let Some(end_date) = NaiveDate::parse_from_str(end_row.text().trim(), DATE_FORMAT).ok()
        else {
            return;
        };
        let timed_end_date = end_date.pred_opt().unwrap_or(end_date).max(start_date);
        let Some(start) = Local
            .from_local_datetime(&start_date.and_hms_opt(9, 0, 0).expect("valid time"))
            .single()
        else {
            return;
        };
        let Some(end) = Local
            .from_local_datetime(&timed_end_date.and_hms_opt(10, 0, 0).expect("valid time"))
            .single()
        else {
            return;
        };
        set_time_rows(start_row, end_row, start, end, false);
    }
}

fn invalid_date_message(all_day: bool) -> String {
    if all_day {
        "Enter dates as YYYY-MM-DD".to_string()
    } else {
        "Enter dates and times as YYYY-MM-DD HH:MM".to_string()
    }
}

fn parse_datetime(text: &str, all_day: bool) -> Option<DateTime<Local>> {
    let text = text.trim();
    let naive = if all_day {
        NaiveDate::parse_from_str(text, DATE_FORMAT)
            .ok()
            .map(|date| NaiveDateTime::new(date, NaiveTime::MIN))
    } else {
        NaiveDateTime::parse_from_str(text, DATETIME_FORMAT).ok()
    }?;
    naive.and_local_timezone(Local).single()
}

fn non_empty(value: String) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_day_values_use_date_only_input() {
        assert!(parse_datetime("2026-07-09", true).is_some());
        assert!(parse_datetime("2026-07-09 00:00", true).is_none());
        assert!(parse_datetime("2026-07-09 09:00", false).is_some());
    }
}
