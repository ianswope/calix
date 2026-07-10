use crate::caldav;
use crate::config::GoogleConfig;
use crate::google;
use crate::icloud;
use crate::store::{Event, EventDraft, Store};
use adw::prelude::*;
use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
use gtk::glib;
use gtk::glib::clone;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;
use url::Url;

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
    Caldav {
        base_url: String,
        username: String,
        token_key: String,
        event_href: String,
    },
}

/// A calendar the dialog can create events on, plus whether it belongs to
/// the default picker set (the calendars currently shown in the sidebar —
/// accounts can carry dozens of calendars, and the hidden ones shouldn't
/// crowd the dropdown).
pub struct TargetChoice {
    pub target: CreateTarget,
    pub visible: bool,
}

#[derive(Clone)]
pub enum CreateTarget {
    Local {
        calendar_id: i64,
        name: String,
    },
    Google {
        calendar_id: i64,
        name: String,
        config: GoogleConfig,
        token_key: String,
        google_calendar_id: String,
    },
    Caldav {
        calendar_id: i64,
        name: String,
        base_url: String,
        username: String,
        token_key: String,
        calendar_href: String,
    },
    Unavailable {
        calendar_id: i64,
        name: String,
        error: String,
    },
}

impl CreateTarget {
    pub fn calendar_id(&self) -> i64 {
        match self {
            Self::Local { calendar_id, .. }
            | Self::Google { calendar_id, .. }
            | Self::Caldav { calendar_id, .. }
            | Self::Unavailable { calendar_id, .. } => *calendar_id,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Self::Local { name, .. }
            | Self::Google { name, .. }
            | Self::Caldav { name, .. }
            | Self::Unavailable { name, .. } => name,
        }
    }

    fn create(&self, draft: &EventDraft) -> Result<Option<(bool, String)>, String> {
        match self {
            Self::Local { .. } => Ok(None),
            Self::Google {
                config,
                token_key,
                google_calendar_id,
                ..
            } => {
                let token = google::oauth::get_access_token(config, token_key)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| "Google account is not connected".to_string())?;
                let event_id =
                    google::calendar_api::create_event(&token, google_calendar_id, draft)?;
                Ok(Some((true, event_id)))
            }
            Self::Caldav {
                base_url,
                username,
                token_key,
                calendar_href,
                ..
            } => {
                let credentials = caldav_credentials(base_url, username, token_key)?;
                let event_id = caldav::create_event(&credentials, calendar_href, draft)?;
                Ok(Some((false, event_id)))
            }
            Self::Unavailable { error, .. } => Err(error.clone()),
        }
    }
}

/// Rebuilds CalDAV connection details from a stored account, reading the
/// password from the keyring. Shared by create/update/delete.
fn caldav_credentials(
    base_url: &str,
    username: &str,
    token_key: &str,
) -> Result<caldav::Credentials, String> {
    let password = icloud::credentials::app_password(token_key)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "CalDAV account is not connected".to_string())?;
    Ok(caldav::Credentials {
        base_url: base_url.to_string(),
        username: username.to_string(),
        password,
    })
}

impl RemoteEvent {
    pub fn update(&self, draft: &EventDraft) -> Result<(), String> {
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
            Self::Caldav {
                base_url,
                username,
                token_key,
                event_href,
            } => {
                let credentials = caldav_credentials(base_url, username, token_key)?;
                caldav::update_event(&credentials, event_href, draft)
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
            Self::Caldav {
                base_url,
                username,
                token_key,
                event_href,
            } => {
                let credentials = caldav_credentials(base_url, username, token_key)?;
                caldav::delete_event(&credentials, event_href)
            }
        }
    }
}

/// Opens a create/edit dialog for an event. Remote changes are completed in a
/// worker thread before the local cache is updated.
pub fn open(
    parent: &impl IsA<gtk::Widget>,
    store: Rc<Store>,
    create_targets: Vec<TargetChoice>,
    editing: Option<Event>,
    initial_start: DateTime<Local>,
    on_saved: impl Fn() + 'static,
    remote_event: Option<RemoteEvent>,
) {
    let on_saved = Rc::new(on_saved);

    // The calendar picker starts with just the default set (sidebar-visible
    // calendars) and expands via a trailing "Show all calendars…" item.
    // Hidden targets are ordered after visible ones so a dropdown index maps
    // straight into `create_targets` whether or not the list is expanded.
    let (visible_targets, hidden_targets): (Vec<_>, Vec<_>) = create_targets
        .into_iter()
        .partition(|choice| choice.visible);
    let visible_count = visible_targets.len();
    let create_targets: Vec<CreateTarget> = visible_targets
        .into_iter()
        .chain(hidden_targets)
        .map(|choice| choice.target)
        .collect();
    let collapsible = visible_count > 0 && visible_count < create_targets.len();

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
    let target_names: Vec<String> = create_targets
        .iter()
        .map(|target| target.name().to_string())
        .collect();
    let initial_names: Vec<&str> = if collapsible {
        target_names[..visible_count]
            .iter()
            .map(String::as_str)
            .chain(std::iter::once("Show all calendars…"))
            .collect()
    } else {
        target_names.iter().map(String::as_str).collect()
    };
    let calendar_selector = gtk::DropDown::from_strings(&initial_names);
    let selected_target = create_targets
        .iter()
        .position(|target| {
            target.calendar_id() == editing.as_ref().map_or(1, |event| event.calendar_id)
        })
        .filter(|position| !collapsible || *position < visible_count)
        .unwrap_or(0);
    calendar_selector.set_selected(selected_target as u32);

    // Until expanded, the index `visible_count` is the "Show all" item, not a
    // calendar; selecting it swaps in the full list and restores the previous
    // pick. The guard flag keeps the swap's own notifications inert and lets
    // the save handler reject the sentinel in the unexpanded state.
    let picker_expanded = Rc::new(Cell::new(!collapsible));
    if collapsible {
        let last_pick = Cell::new(selected_target as u32);
        let expanded = picker_expanded.clone();
        let all_names = target_names.clone();
        calendar_selector.connect_selected_notify(move |selector| {
            if expanded.get() {
                return;
            }
            if (selector.selected() as usize) != visible_count {
                last_pick.set(selector.selected());
                return;
            }
            expanded.set(true);
            let names: Vec<&str> = all_names.iter().map(String::as_str).collect();
            selector.set_model(Some(&gtk::StringList::new(&names)));
            selector.set_selected(last_pick.get());
            // Reopen the popup so the full list is immediately in front of
            // the user instead of needing a second click.
            selector.activate();
        });
    }

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
    if editing.is_none() {
        calendar_row.add_suffix(&calendar_selector);
        calendar_row.set_activatable_widget(Some(&calendar_selector));
    }
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
    if let Some(event) = &editing {
        let links = event_links(event);
        if !links.is_empty() {
            let links_group = adw::PreferencesGroup::builder()
                .title("Meeting links")
                .build();
            for link in links {
                let label = meeting_link_label(&link);
                let button = gtk::LinkButton::with_label(&link, &label);
                button.set_halign(gtk::Align::Start);
                button.set_tooltip_text(Some(&link));
                links_group.add(&button);
            }
            content.append(&links_group);
        }
    }
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
        create_targets,
        #[strong]
        calendar_selector,
        #[strong]
        picker_expanded,
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
                let selected = calendar_selector.selected() as usize;
                if !picker_expanded.get() && selected >= visible_count {
                    error_label.set_label("Choose a calendar");
                    return;
                }
                let Some(target) = create_targets.get(selected).cloned() else {
                    error_label.set_label("Choose a calendar");
                    return;
                };
                save_button.set_sensitive(false);
                let remote_draft = draft.clone();
                let (tx, rx) = mpsc::channel();
                let remote_target = target.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(remote_target.create(&remote_draft));
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
                            Ok(Ok(None)) => match store.create_event(target.calendar_id(), &draft) {
                                Ok(_) => {
                                    on_saved();
                                    dialog.close();
                                    glib::ControlFlow::Break
                                }
                                Err(error) => {
                                    error_label.set_label(&format!("Couldn't save event: {error}"));
                                    save_button.set_sensitive(true);
                                    glib::ControlFlow::Break
                                }
                            },
                            Ok(Ok(Some((is_google, remote_id)))) => {
                                let result = if is_google {
                                    store.upsert_google_event(target.calendar_id(), &remote_id, &draft)
                                } else {
                                    store.upsert_caldav_event(target.calendar_id(), &remote_id, &draft)
                                };
                                match result {
                                    Ok(()) => {
                                        on_saved();
                                        dialog.close();
                                        glib::ControlFlow::Break
                                    }
                                    Err(error) => {
                                        error_label.set_label(&format!("Remote event created, but the local cache could not be updated: {error}"));
                                        save_button.set_sensitive(true);
                                        glib::ControlFlow::Break
                                    }
                                }
                            }
                            Ok(Err(error)) => {
                                error_label.set_label(&error);
                                save_button.set_sensitive(true);
                                glib::ControlFlow::Break
                            }
                            Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                            Err(mpsc::TryRecvError::Disconnected) => {
                                error_label.set_label("Event creation stopped unexpectedly");
                                save_button.set_sensitive(true);
                                glib::ControlFlow::Break
                            }
                        }
                    ),
                );
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

fn event_links(event: &Event) -> Vec<String> {
    [event.location.as_deref(), event.notes.as_deref()]
        .into_iter()
        .flatten()
        .flat_map(urls_in_text)
        .fold(Vec::new(), |mut links, link| {
            if !links.contains(&link) {
                links.push(link);
            }
            links
        })
}

fn urls_in_text(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split_whitespace().filter_map(|word| {
        let candidate = word.trim_matches(|character: char| {
            matches!(
                character,
                '(' | ')' | '[' | ']' | '{' | '}' | ',' | '.' | ';' | ':'
            )
        });
        let url = Url::parse(candidate).ok()?;
        matches!(url.scheme(), "http" | "https").then(|| candidate.to_string())
    })
}

fn meeting_link_label(link: &str) -> String {
    Url::parse(link)
        .ok()
        .and_then(|url| url.host_str().map(|host| format!("Join via {host}")))
        .unwrap_or_else(|| "Open meeting link".to_string())
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

    #[test]
    fn url_detection_keeps_meeting_links_and_drops_surrounding_punctuation() {
        let links =
            urls_in_text("Join https://meet.google.com/abc-defg-hij, or https://zoom.us/j/123.")
                .collect::<Vec<_>>();

        assert_eq!(
            links,
            vec![
                "https://meet.google.com/abc-defg-hij",
                "https://zoom.us/j/123"
            ]
        );
    }
}
