use crate::caldav;
use crate::config::GoogleConfig;
use crate::google;
use crate::icloud;
use crate::notify;
use crate::recurrence::Frequency;
use crate::store::{Event, EventDraft, Store};
use crate::undo;
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
        series_id: Option<String>,
    },
    Caldav {
        base_url: String,
        username: String,
        token_key: String,
        event_href: String,
    },
}

/// What the local cache is worth once a save has landed, which is what decides
/// whether the caller can simply redraw or has to go back to the provider first.
///
/// A remote series is cached as the individual occurrences its provider expanded
/// for Calix, never as one row with a repeat rule. So any operation that acts on
/// a series *as a whole* leaves the cache holding one row where the server now
/// has many — or still holding the occurrences the server just moved or deleted.
/// Only a sync settles that; redrawing from the cache in the meantime shows a new
/// weekly event as a single one, or an edit that only took on the occurrence that
/// was clicked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Saved {
    /// The cache already matches the provider; redraw and stop.
    Cached,
    /// Redraw, then refresh from the provider.
    StaleUntilSync,
}

/// [`Saved`] for one completed save: `remote` is whether the event lives on a
/// provider, `whole_series` whether the operation covered a whole series rather
/// than a single event or occurrence.
/// The repeat rule the dialog shows. A stored exception carries none of its
/// own; "All events" has to edit the series rule, not clear it.
fn repeat_rule(store: &Store, event: &Event) -> Option<Frequency> {
    if event.recurrence.is_some() || event.instance_of.is_none() {
        return event.recurrence;
    }
    event
        .instance_of
        .and_then(|id| store.event_by_id(id).ok().flatten())
        .and_then(|master| master.recurrence)
}

fn saved_state(remote: bool, whole_series: bool) -> Saved {
    if remote && whole_series {
        Saved::StaleUntilSync
    } else {
        Saved::Cached
    }
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

    fn create(&self, draft: &EventDraft) -> Result<Option<CreatedRemote>, String> {
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
                let series = draft.recurrence.map(|_| {
                    google::calendar_api::created_event_json(draft)
                        .map(|master| undo::SeriesDocument::google(master, Vec::new()))
                });
                let series = match series {
                    Some(result) => Some(result?),
                    None => None,
                };
                Ok(Some(CreatedRemote {
                    is_google: true,
                    remote_id: event_id,
                    series,
                }))
            }
            Self::Caldav {
                base_url,
                username,
                token_key,
                calendar_href,
                ..
            } => {
                let credentials = caldav_credentials(base_url, username, token_key)?;
                let (event_id, ics) = caldav::create_event(&credentials, calendar_href, draft)?;
                Ok(Some(CreatedRemote {
                    is_google: false,
                    remote_id: event_id,
                    series: draft.recurrence.map(|_| undo::SeriesDocument::ics(ics)),
                }))
            }
            Self::Unavailable { error, .. } => Err(error.clone()),
        }
    }

    /// Where a series created on this calendar can be read back.
    fn series_locator(&self, remote_id: &str) -> Option<undo::SeriesLocator> {
        match self {
            Self::Google {
                token_key,
                google_calendar_id,
                ..
            } => Some(undo::SeriesLocator::Google {
                token_key: token_key.clone(),
                google_calendar_id: google_calendar_id.clone(),
                series_id: remote_id.to_string(),
            }),
            Self::Caldav {
                base_url,
                username,
                token_key,
                ..
            } => Some(undo::SeriesLocator::Caldav {
                base_url: base_url.clone(),
                username: username.clone(),
                token_key: token_key.clone(),
                href: remote_id.to_string(),
            }),
            Self::Local { .. } | Self::Unavailable { .. } => None,
        }
    }
}

/// A provider event that was just created, and the series document when the
/// draft repeats — undoing that deletes the series, not the one cached row.
struct CreatedRemote {
    is_google: bool,
    remote_id: String,
    series: Option<undo::SeriesDocument>,
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
    pub fn can_respond(&self, event: &Event) -> bool {
        match self {
            Self::Google { .. } => event.attendees.iter().any(|attendee| attendee.is_self),
            Self::Caldav { username, .. } => event
                .attendees
                .iter()
                .any(|attendee| attendee.email.eq_ignore_ascii_case(username)),
            Self::Unavailable(_) => false,
        }
    }

    pub fn respond(&self, event: &Event, response: &str) -> Result<(), String> {
        match self {
            Self::Google {
                config,
                token_key,
                calendar_id,
                event_id,
                ..
            } => {
                let token = google::oauth::get_access_token(config, token_key)
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| "Google account is not connected".to_string())?;
                google::calendar_api::respond_to_event(
                    &token,
                    calendar_id,
                    event_id,
                    &event.attendees,
                    response,
                )
            }
            Self::Caldav {
                base_url,
                username,
                token_key,
                event_href,
            } => {
                let credentials = caldav_credentials(base_url, username, token_key)?;
                caldav::respond_to_event(&credentials, event_href, username, response)
            }
            Self::Unavailable(error) => Err(error.clone()),
        }
    }

    pub fn update(&self, draft: &EventDraft) -> Result<(), String> {
        match self {
            Self::Unavailable(error) => Err(error.clone()),
            Self::Google {
                config,
                token_key,
                calendar_id,
                event_id,
                ..
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

    /// Whether this is one occurrence of a CalDAV series (an `href#instance`),
    /// i.e. a case where "this event" vs "all events" is a meaningful choice.
    pub fn is_series_instance(&self) -> bool {
        matches!(self, Self::Caldav { event_href, .. } if event_href.contains('#'))
            || matches!(
                self,
                Self::Google {
                    series_id: Some(_),
                    ..
                }
            )
    }

    /// Applies an edit to every occurrence of a series, moving the series by
    /// `shift` (how far the edited occurrence's start moved).
    pub fn update_all_events(
        &self,
        draft: &EventDraft,
        shift: caldav::SeriesShift,
    ) -> Result<Option<CapturedSeries>, String> {
        match self {
            Self::Google {
                config,
                token_key,
                calendar_id,
                series_id: Some(series_id),
                ..
            } => {
                let token = google::oauth::get_access_token(config, token_key)
                    .map_err(|error| error.to_string())?
                    .ok_or("Google account is not connected")?;
                let (before, exceptions, after) = google::calendar_api::update_series(
                    &token,
                    calendar_id,
                    series_id,
                    shift,
                    draft,
                )?;
                Ok(Some(CapturedSeries {
                    before: undo::SeriesDocument::google(before, exceptions.clone()),
                    after: undo::SeriesDocument::google(after, exceptions),
                }))
            }

            Self::Caldav {
                base_url,
                username,
                token_key,
                event_href,
            } => {
                let credentials = caldav_credentials(base_url, username, token_key)?;
                let (before, after) =
                    caldav::update_series(&credentials, event_href, shift, draft)?;
                Ok(Some(CapturedSeries {
                    before: undo::SeriesDocument::ics(before),
                    after: undo::SeriesDocument::ics(after),
                }))
            }
            // Not a series. The caller records an ordinary single-event edit.
            _ => self.update(draft).map(|()| None),
        }
    }

    /// Where this series can be read back. `None` when the event has no provider.
    ///
    /// A Google series that has not been expanded yet has no separate series id;
    /// its event id is the master.
    fn series_locator(&self) -> Option<undo::SeriesLocator> {
        match self {
            Self::Google {
                token_key,
                calendar_id,
                event_id,
                series_id,
                ..
            } => Some(undo::SeriesLocator::Google {
                token_key: token_key.clone(),
                google_calendar_id: calendar_id.clone(),
                // An expanded instance points at its master. A series created
                // here and not yet expanded is that master, under its event id.
                series_id: series_id.clone().unwrap_or_else(|| event_id.clone()),
            }),
            Self::Caldav {
                base_url,
                username,
                token_key,
                event_href,
            } => Some(undo::SeriesLocator::Caldav {
                base_url: base_url.clone(),
                username: username.clone(),
                token_key: token_key.clone(),
                href: event_href
                    .split_once('#')
                    .map(|(href, _)| href)
                    .unwrap_or(event_href)
                    .to_string(),
            }),
            _ => None,
        }
    }

    pub fn delete(&self) -> Result<(), String> {
        match self {
            Self::Unavailable(error) => Err(error.clone()),
            Self::Google {
                config,
                token_key,
                calendar_id,
                event_id,
                ..
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

    /// Deletes an entire series (an "all events" delete) rather than excluding
    /// the one occurrence [`Self::delete`] would drop for a CalDAV instance.
    ///
    /// The document is what was removed, so an undo can put the whole set back.
    /// `None` means this wasn't a series and the caller records a single-row delete.
    fn delete_all_events(&self) -> Result<Option<undo::SeriesDocument>, String> {
        match self {
            Self::Google {
                config,
                token_key,
                calendar_id,
                event_id,
                series_id,
                ..
            } => {
                let token = google::oauth::get_access_token(config, token_key)
                    .map_err(|error| error.to_string())?
                    .ok_or("Google account is not connected")?;
                let series_id = series_id.as_deref().unwrap_or(event_id.as_str());
                let (master, exceptions) =
                    google::calendar_api::delete_series(&token, calendar_id, series_id)?;
                Ok(Some(undo::SeriesDocument::google(master, exceptions)))
            }

            Self::Caldav {
                base_url,
                username,
                token_key,
                event_href,
            } => {
                let credentials = caldav_credentials(base_url, username, token_key)?;
                // The bare resource href removes the whole series in one delete.
                let resource_href = event_href
                    .split_once('#')
                    .map_or(event_href.as_str(), |(href, _)| href);
                let ics = caldav::delete_resource(&credentials, resource_href)?;
                Ok(Some(undo::SeriesDocument::ics(ics)))
            }
            _ => self.delete().map(|()| None),
        }
    }
}

/// The provider's series before and after an all-events edit.
pub(crate) struct CapturedSeries {
    before: undo::SeriesDocument,
    after: undo::SeriesDocument,
}

/// Opens a create/edit dialog for an event. Remote changes are completed in a
/// worker thread before the local cache is updated.
#[allow(clippy::too_many_arguments)]
pub fn open(
    parent: &impl IsA<gtk::Widget>,
    store: Rc<Store>,
    create_targets: Vec<TargetChoice>,
    editing: Option<Event>,
    initial_start: DateTime<Local>,
    initial_end: Option<DateTime<Local>>,
    on_saved: impl Fn(Saved) + 'static,
    on_change: impl Fn(undo::Record) + 'static,
    remote_event: Option<RemoteEvent>,
) {
    build(
        store,
        create_targets,
        editing,
        initial_start,
        initial_end,
        on_saved,
        on_change,
        remote_event,
    )
    .present(Some(parent));
}

/// The dialog, built but not yet presented. Separate from [`open`] so the
/// widget tree can be constructed — and checked for what it holds on to —
/// without a window to present it in.
// Eight arguments, one over clippy's threshold. They are the dialog's inputs
// and nothing here groups meaningfully: `editing` and `remote_event` describe
// an existing event, `create_targets` only a new one, and the start/end pair
// applies to both. A struct to hold them would be built inline at all three
// call sites and read no better than the argument list does.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build(
    store: Rc<Store>,
    create_targets: Vec<TargetChoice>,
    editing: Option<Event>,
    initial_start: DateTime<Local>,
    // End time for a new event. `None` takes the one-hour default; a
    // create-drag passes the span the user actually drew.
    initial_end: Option<DateTime<Local>>,
    // Called once a save, delete or create has landed, with what the local cache
    // is worth afterwards — see [`Saved`].
    on_saved: impl Fn(Saved) + 'static,
    // Called with the change the user just made, for the undo history.
    on_change: impl Fn(undo::Record) + 'static,
    remote_event: Option<RemoteEvent>,
) -> adw::Dialog {
    let on_saved = Rc::new(on_saved);
    let on_change = Rc::new(on_change);

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
    // Suggestions from this calendar's own locations and, unless it is switched
    // off, a geocoder — see `location_completion`.
    crate::location_completion::attach(&location_row, store.clone());
    let notes = NotesField::new();
    let invitees_row = adw::EntryRow::builder()
        .title("Invitees (email addresses, comma separated)")
        .build();
    // "Does not repeat" at index 0, then Frequency::ALL — matching
    // Frequency::picker_index / from_picker_index.
    let repeat_labels: Vec<&str> = std::iter::once("Does not repeat")
        .chain(Frequency::ALL.iter().map(|freq| freq.label()))
        .chain(std::iter::once("Custom…"))
        .collect();
    let repeat_row = adw::ComboRow::builder()
        .title("Repeat")
        .model(&gtk::StringList::new(&repeat_labels))
        .build();
    let custom_repeat = crate::recurrence_picker::CustomRepeat::new();
    custom_repeat.row.set_visible(false);
    let custom_row = custom_repeat.row.clone();
    let alert_row = adw::ExpanderRow::builder()
        .title("Alerts")
        .subtitle("Choose one or more reminders")
        .build();
    let alert_buttons: Vec<(i64, gtk::CheckButton)> = notify::ALERT_CHOICES
        .iter()
        .filter_map(|(label, minutes)| {
            minutes.map(|minutes| {
                let button = gtk::CheckButton::new();
                let row = adw::ActionRow::builder()
                    .title(*label)
                    .activatable_widget(&button)
                    .build();
                row.add_suffix(&button);
                alert_row.add_row(&row);
                (minutes, button)
            })
        })
        .collect();
    // Shown only when editing one occurrence of a series: governs whether Save
    // and Delete affect just this occurrence or the whole series (0 = this, 1
    // = all).
    let scope_row = adw::ComboRow::builder()
        .title("Apply to")
        .model(&gtk::StringList::new(&["This event", "All events"]))
        .build();
    let editing_series_instance = remote_event
        .as_ref()
        .is_some_and(RemoteEvent::is_series_instance)
        || editing
            .as_ref()
            .is_some_and(|event| event.instance_of.is_some());
    let local_occurrence = remote_event.is_none()
        && editing
            .as_ref()
            .is_some_and(|event| event.instance_of.is_some());
    if local_occurrence {
        // "This event" is the default. The rule belongs to the series, so it
        // stays visible and locked until the user chooses "All events".
        repeat_row.set_sensitive(false);
        custom_row.set_sensitive(false);
        scope_row.connect_selected_notify(clone!(
            #[weak]
            repeat_row,
            #[weak]
            custom_row,
            move |scope| {
                let editable = scope.selected() == 1;
                repeat_row.set_sensitive(editable);
                custom_row.set_sensitive(editable);
            }
        ));
    }
    let error_label = gtk::Label::new(None);
    error_label.add_css_class("error");
    error_label.set_xalign(0.0);
    error_label.set_wrap(true);
    repeat_row.connect_selected_notify(clone!(
        #[weak]
        custom_row,
        #[weak]
        error_label,
        move |repeat| {
            custom_row.set_visible(repeat.selected() == Frequency::CUSTOM_PICKER_INDEX);
            error_label.set_label("");
        }
    ));
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
            notes.set_text(event.notes.as_deref().unwrap_or(""));
            // Selecting the stored rule keeps Save from clearing it. A custom
            // rule also fills the controls the row reveals. A stored exception
            // has no rule of its own; the series rule is what "All events" edits.
            let recurrence = repeat_rule(&store, event);
            repeat_row.set_selected(Frequency::picker_index(recurrence));
            if let Some(Frequency::Custom(rule)) = recurrence {
                custom_repeat.set_rule(rule);
            }
            for (minutes, button) in &alert_buttons {
                button.set_active(
                    event.reminder_minutes == Some(*minutes)
                        || event.additional_reminders.contains(minutes),
                );
            }
        }
        None => {
            set_time_rows(
                &start_row,
                &end_row,
                initial_start,
                initial_end.unwrap_or_else(|| initial_start + chrono::Duration::hours(1)),
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
    // A local event, new or already saved, can set its own repeat. A synced
    // event keeps the rule its provider expanded, so the row stays off it.
    if remote_event.is_none() {
        group.add(&repeat_row);
        group.add(&custom_repeat.row);
    }
    if editing_series_instance {
        group.add(&scope_row);
    }
    group.add(&alert_row);
    group.add(&location_row);
    group.add(notes.widget());
    if editing.is_none() {
        group.add(&invitees_row);
    }

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);
    // Meeting links and invitees go first, so they're in view the moment the
    // event opens instead of being pushed below the fields (and off the
    // bottom). Both are read-only provider data rather than editable fields.
    if let Some(links_group) = editing.as_ref().and_then(meeting_links_group) {
        content.append(&links_group);
    }
    if let Some(invitees_group) = editing.as_ref().and_then(attendees_group) {
        content.append(&invitees_group);
    }
    content.append(&group);
    content.append(&error_label);

    if let Some(event) = &editing {
        let event_id = event.id;
        // What deleting one ordinary row takes away. A local series builds its
        // own entry at click time: this draft is one occurrence, and its id is
        // the master's, so recording it would undo the wrong row.
        let deleted = undo::Change::deleted(event.calendar_id, event.draft());
        let event_for_delete = event.clone();
        let delete_button = gtk::Button::builder()
            .label("Delete Event")
            .css_classes(["destructive-action"])
            .build();
        // The dialog and the button are held weakly: the button sits inside
        // the dialog, so a strong reference from its own handler to either
        // would be a cycle that outlives the dialog being closed. Both are
        // certainly alive while the click is being handled.
        delete_button.connect_clicked(clone!(
            #[weak]
            dialog,
            #[strong]
            store,
            #[strong]
            on_saved,
            #[strong]
            on_change,
            #[strong]
            deleted,
            #[strong]
            event_for_delete,
            #[strong]
            remote_event,
            #[weak]
            delete_button,
            #[strong]
            scope_row,
            #[weak]
            error_label,
            move |_| {
                delete_button.set_sensitive(false);
                if remote_event.is_none() {
                    match undo::local_removal(
                        scope_row.selected() == 1,
                        event_for_delete.instance_of.is_some(),
                        event_for_delete.recurrence.is_some(),
                    ) {
                        undo::LocalRemoval::OneOccurrence => {
                            let (Some(master_id), Some(original)) = (
                                event_for_delete.instance_of,
                                event_for_delete.original_start,
                            ) else {
                                error_label.set_label("Couldn't tell which occurrence this is");
                                delete_button.set_sensitive(true);
                                return;
                            };
                            let mut draft = event_for_delete.draft();
                            draft.cancelled = true;
                            match store.save_occurrence(master_id, original, &draft) {
                                Ok(saved) => {
                                    on_change(undo::Record::Event(undo::recorded_occurrence(
                                        event_for_delete.calendar_id,
                                        &saved,
                                    )));
                                    on_saved(Saved::Cached);
                                    dialog.close();
                                }
                                Err(error) => {
                                    error_label
                                        .set_label(&format!("Couldn't delete event: {error}"));
                                    delete_button.set_sensitive(true);
                                }
                            }
                            return;
                        }
                        undo::LocalRemoval::WholeSeries => {
                            let master_id = event_for_delete.instance_of.unwrap_or(event_id);
                            let master = store.event_by_id(master_id).ok().flatten();
                            let overrides = store.series_overrides(master_id).ok();
                            let (Some(master), Some(overrides)) = (master, overrides) else {
                                error_label.set_label("Couldn't load the repeating event");
                                delete_button.set_sensitive(true);
                                return;
                            };
                            match store.delete_event(master_id) {
                                Ok(()) => {
                                    on_change(undo::Record::Event(
                                        undo::Change::deleted(master.calendar_id, master.draft())
                                            .with_overrides(overrides),
                                    ));
                                    on_saved(Saved::Cached);
                                    dialog.close();
                                }
                                Err(error) => {
                                    error_label
                                        .set_label(&format!("Couldn't delete event: {error}"));
                                    delete_button.set_sensitive(true);
                                }
                            }
                            return;
                        }
                        undo::LocalRemoval::ThisRow => {}
                    }
                }
                let repeats = deleted
                    .before
                    .as_ref()
                    .is_some_and(|draft| draft.recurrence.is_some());
                let series_instance = remote_event
                    .as_ref()
                    .is_some_and(RemoteEvent::is_series_instance);
                delete_event_async(
                    store.clone(),
                    remote_event.clone(),
                    event_id,
                    deleted.clone(),
                    on_change.clone(),
                    // "All events" removes the whole series. So does deleting
                    // the one row of a series sync has not expanded yet.
                    undo::deletes_whole_series(scope_row.selected() == 1, series_instance, repeats),
                    clone!(
                        #[strong]
                        dialog,
                        #[strong]
                        on_saved,
                        #[strong]
                        delete_button,
                        #[weak]
                        error_label,
                        move |outcome| match outcome {
                            Ok(saved) => {
                                on_saved(saved);
                                dialog.close();
                            }
                            Err(message) => {
                                error_label.set_label(&message);
                                delete_button.set_sensitive(true);
                            }
                        }
                    ),
                );
            }
        ));
        content.append(&delete_button);
    }

    // Scroll the body so a long event — several meeting links, long notes —
    // can't push fields or the delete button out of reach. Natural-height
    // propagation keeps short events compact; only overflow scrolls.
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .vexpand(true)
        .child(&content)
        .build();
    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&scroller));
    dialog.set_child(Some(&toolbar_view));

    cancel_button.connect_clicked(clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));

    // Weak for the same reason as Delete's: Save is inside the dialog it would
    // otherwise pin. The async callbacks below take strong references of their
    // own, which is right — they must outlive the click, and they end.
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
        remote_event,
        #[strong]
        create_targets,
        #[strong]
        calendar_selector,
        #[strong]
        picker_expanded,
        #[weak]
        save_button,
        #[strong]
        scope_row,
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

            let recurrence = if repeat_row.selected() == Frequency::CUSTOM_PICKER_INDEX {
                match custom_repeat.value() {
                    Ok(rule) => Some(rule),
                    Err(message) => {
                        error_label.set_label(message);
                        return;
                    }
                }
            } else {
                Frequency::from_picker_index(repeat_row.selected())
            };
            let mut reminders = alert_buttons.iter().filter(|(_, button)| button.is_active()).map(|(minutes, _)| *minutes);
            let draft = EventDraft {
                title,
                start,
                end,
                all_day,
                location: non_empty(location_row.text().to_string()),
                notes: non_empty(notes.text()),
                recurrence,
                reminder_minutes: reminders.next(),
                additional_reminders: reminders.collect(),
                attendees: match editing.as_ref() {
                    Some(event) => event.attendees.clone(),
                    None => match parse_invitees(&invitees_row.text()) {
                        Ok(invitees) => invitees,
                        Err(message) => {
                            error_label.set_label(&message);
                            return;
                        }
                    },
                },
            instance_of: None,
            original_start: None,
            cancelled: false,};

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
                if !draft.attendees.is_empty() && !matches!(target, CreateTarget::Google { .. }) {
                    error_label.set_label("Sending invitations is currently supported for Google calendars");
                    return;
                }
                save_button.set_sensitive(false);
                create_event_async(
                    store.clone(),
                    target,
                    draft,
                    on_change.clone(),
                    clone!(
                        #[strong]
                        dialog,
                        #[strong]
                        on_saved,
                        #[strong]
                        save_button,
                        #[weak]
                        error_label,
                        move |outcome| match outcome {
                            Ok(saved) => {
                                on_saved(saved);
                                dialog.close();
                            }
                            Err((message, retryable)) => {
                                error_label.set_label(&message);
                                save_button.set_sensitive(retryable);
                            }
                        }
                    ),
                );
                return;
            };

            if let Some(remote_event) = remote_event.clone() {
                save_button.set_sensitive(false);
                let event_id = event.id;
                let edited_from = event.draft();
                let edited_calendar = event.calendar_id;
                // "All events" shifts the series by however far this occurrence's
                // start moved; anything else edits just this occurrence.
                let all_events = scope_row.selected() == 1;
                let shift = caldav::SeriesShift::between(event.start, draft.start);
                let series_locator = remote_event.series_locator();
                let (tx, rx) = mpsc::channel();
                let remote_draft = draft.clone();
                std::thread::spawn(move || {
                    let result = if all_events {
                        remote_event.update_all_events(&remote_draft, shift)
                    } else {
                        remote_event.update(&remote_draft).map(|()| None)
                    };
                    let _ = tx.send(result);
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
                        on_change,
                        #[strong]
                        save_button,
                        #[strong]
                        error_label,
                        #[strong]
                        series_locator,
                        move || match rx.try_recv() {
                            Ok(Ok(captured)) => match store
                                .update_event(event_id, &draft)
                                .and_then(|()| store.update_event_attendees(event_id, &draft.attendees))
                            {
                                Ok(()) => {
                                    match (captured, series_locator.clone()) {
                                        (Some(captured), Some(locator)) => {
                                            on_change(undo::Record::Series(
                                                undo::SeriesChange::edited(
                                                    captured.before,
                                                    captured.after,
                                                )
                                                .locate(locator),
                                            ));
                                        }
                                        _ => on_change(undo::Record::Event(undo::Change::edited(
                                            edited_calendar,
                                            event_id,
                                            edited_from.clone(),
                                            draft.clone(),
                                        ))),
                                    }
                                    // "All events" moved every occurrence on the
                                    // server; only the clicked row moved locally.
                                    on_saved(saved_state(true, all_events));
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
                match undo::local_save(event.instance_of.is_some(), scope_row.selected() == 1) {
                    undo::LocalSave::OneOccurrence => {
                        let (Some(master_id), Some(original)) =
                            (event.instance_of, event.original_start)
                        else {
                            error_label.set_label("Couldn't tell which occurrence this is");
                            return;
                        };
                        match store.save_occurrence(master_id, original, &draft) {
                            Ok(saved) => {
                                on_change(undo::Record::Event(undo::recorded_occurrence(
                                    event.calendar_id,
                                    &saved,
                                )));
                                on_saved(Saved::Cached);
                                dialog.close();
                            }
                            Err(error) => {
                                error_label.set_label(&format!("Couldn't save event: {error}"))
                            }
                        }
                    }
                    undo::LocalSave::AllEvents => {
                        let Some(master_id) = event.instance_of else {
                            error_label.set_label("Couldn't load the repeating event");
                            return;
                        };
                        let Some(master) = store.event_by_id(master_id).ok().flatten() else {
                            error_label.set_label("Couldn't load the repeating event");
                            return;
                        };
                        let shifted = crate::store::series_edit(&master, event, &draft);
                        match store.update_event(master_id, &shifted) {
                            Ok(()) => {
                                on_change(undo::Record::Event(undo::Change::edited(
                                    master.calendar_id,
                                    master_id,
                                    master.draft(),
                                    shifted,
                                )));
                                on_saved(Saved::Cached);
                                dialog.close();
                            }
                            Err(error) => {
                                error_label.set_label(&format!("Couldn't save event: {error}"))
                            }
                        }
                    }
                    undo::LocalSave::ThisRow => match store.update_event(event.id, &draft) {
                        Ok(()) => {
                            on_change(undo::Record::Event(undo::Change::edited(
                                event.calendar_id,
                                event.id,
                                event.draft(),
                                draft.clone(),
                            )));
                            on_saved(Saved::Cached);
                            dialog.close();
                        }
                        Err(error) => {
                            error_label.set_label(&format!("Couldn't save event: {error}"))
                        }
                    },
                }
            }
        }
    ));

    dialog
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

/// The dialog's notes field: a caption over a multi-line text box that grows
/// with what it holds. Invites carry whole paragraphs and links, which a
/// one-line entry ran together into a single strip that had to be scrolled
/// sideways to read. It has no scroller of its own: the dialog body already
/// scrolls, and a scroller inside a scroller sized itself to its minimum.
pub(crate) struct NotesField {
    row: adw::PreferencesRow,
    buffer: gtk::TextBuffer,
}

const NOTES_MIN_HEIGHT: i32 = 72;

impl NotesField {
    pub(crate) fn new() -> Self {
        let caption = gtk::Label::new(Some("Notes"));
        caption.add_css_class("caption");
        caption.add_css_class("dim-label");
        caption.set_xalign(0.0);

        let view = gtk::TextView::new();
        view.set_wrap_mode(gtk::WrapMode::WordChar);
        // Tab moves on to the next field, as it does from every other row.
        view.set_accepts_tab(false);
        view.add_css_class("inline");
        view.set_size_request(-1, NOTES_MIN_HEIGHT);
        let buffer = view.buffer();

        let content = gtk::Box::new(gtk::Orientation::Vertical, 4);
        content.set_margin_top(8);
        content.set_margin_bottom(8);
        content.set_margin_start(12);
        content.set_margin_end(12);
        content.append(&caption);
        content.append(&view);

        let row = adw::PreferencesRow::builder().title("Notes").build();
        row.set_activatable(false);
        row.set_child(Some(&content));
        Self { row, buffer }
    }

    pub(crate) fn widget(&self) -> &gtk::Widget {
        self.row.upcast_ref()
    }

    pub(crate) fn set_text(&self, text: &str) {
        self.buffer.set_text(text);
    }

    pub(crate) fn text(&self) -> String {
        let (start, end) = self.buffer.bounds();
        self.buffer.text(&start, &end, false).to_string()
    }
}

fn non_empty(value: String) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn parse_invitees(value: &str) -> Result<Vec<crate::store::Attendee>, String> {
    let mut invitees = Vec::new();
    for address in value
        .split([',', ';'])
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let valid = address.contains('@')
            && !address.starts_with('@')
            && !address.ends_with('@')
            && !address.chars().any(char::is_whitespace);
        if !valid {
            return Err(format!("{address} is not a valid email address"));
        }
        if !invitees
            .iter()
            .any(|attendee: &crate::store::Attendee| attendee.email.eq_ignore_ascii_case(address))
        {
            invitees.push(crate::store::Attendee {
                email: address.to_string(),
                name: None,
                status: Some("pending".to_string()),
                is_self: false,
            });
        }
    }
    Ok(invitees)
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

/// A "Meeting links" group listing every URL found in the event's location or
/// notes, or `None` if there are none.
fn meeting_links_group(event: &Event) -> Option<adw::PreferencesGroup> {
    let links = event_links(event);
    if links.is_empty() {
        return None;
    }
    let group = adw::PreferencesGroup::builder()
        .title("Meeting links")
        .build();
    for link in links {
        let label = meeting_link_label(&link);
        let button = gtk::LinkButton::with_label(&link, &label);
        button.set_halign(gtk::Align::Start);
        button.set_tooltip_text(Some(&link));
        group.add(&button);
    }
    Some(group)
}

/// An "Invitees" group listing everyone on the event, or `None` when it has no
/// attendees. Read-only: attendee lists come from the provider and Calix has no
/// invite-editing flow.
fn attendees_group(event: &Event) -> Option<adw::PreferencesGroup> {
    if event.attendees.is_empty() {
        return None;
    }
    let group = adw::PreferencesGroup::builder()
        .title(format!("Invitees ({})", event.attendees.len()))
        .build();
    for attendee in &event.attendees {
        let row = adw::ActionRow::builder()
            .title(gtk::glib::markup_escape_text(attendee.label()))
            .build();
        row.set_title_lines(1);
        // Only worth a subtitle when it isn't just repeating the title.
        if attendee.label() != attendee.email {
            row.set_subtitle(&gtk::glib::markup_escape_text(&attendee.email));
            row.set_subtitle_lines(1);
        }
        if let Some(status) = attendee.status.as_deref().map(attendee_status_label) {
            let label = gtk::Label::new(Some(status));
            label.add_css_class("dim-label");
            row.add_suffix(&label);
        }
        group.add(&row);
    }
    Some(group)
}

/// Human-readable form of the normalized response status the syncs store.
pub(crate) fn attendee_status_label(status: &str) -> &str {
    match status {
        "accepted" => "Accepted",
        "declined" => "Declined",
        "tentative" => "Maybe",
        "pending" => "No reply",
        other => other,
    }
}

/// The series at `locator` as the provider has it now, or `None` when it is gone.
pub(crate) fn current_series(
    config: Option<GoogleConfig>,
    locator: &undo::SeriesLocator,
) -> Result<Option<undo::SeriesDocument>, String> {
    match locator {
        undo::SeriesLocator::Google {
            token_key,
            google_calendar_id,
            series_id,
        } => {
            let config = config.ok_or("Google is not configured on this machine")?;
            let token = google::oauth::get_access_token(&config, token_key)
                .map_err(|error| error.to_string())?
                .ok_or("Google account is not connected")?;
            Ok(
                google::calendar_api::load_series(&token, google_calendar_id, series_id)?
                    .map(|(master, exceptions)| undo::SeriesDocument::google(master, exceptions)),
            )
        }
        undo::SeriesLocator::Caldav {
            base_url,
            username,
            token_key,
            href,
        } => {
            let credentials = caldav_credentials(base_url, username, token_key)?;
            Ok(caldav::read_resource(&credentials, href)?.map(undo::SeriesDocument::ics))
        }
    }
}

/// Carries out the provider write a series undo or redo decided on.
pub(crate) fn apply_series_write(
    config: Option<GoogleConfig>,
    locator: &undo::SeriesLocator,
    write: &undo::SeriesWrite,
) -> Result<(), String> {
    match locator {
        undo::SeriesLocator::Google {
            token_key,
            google_calendar_id,
            series_id,
        } => {
            let config = config.ok_or("Google is not configured on this machine")?;
            let token = google::oauth::get_access_token(&config, token_key)
                .map_err(|error| error.to_string())?
                .ok_or("Google account is not connected")?;
            match write {
                undo::SeriesWrite::RestoreMaster(document) => google::calendar_api::restore_master(
                    &token,
                    google_calendar_id,
                    series_id,
                    &document.master,
                ),
                undo::SeriesWrite::RestoreAll(document) => google::calendar_api::restore_series(
                    &token,
                    google_calendar_id,
                    series_id,
                    &document.master,
                    &document.exceptions,
                ),
                undo::SeriesWrite::Delete => {
                    google::calendar_api::delete_event(&token, google_calendar_id, series_id)
                }
            }
        }
        undo::SeriesLocator::Caldav {
            base_url,
            username,
            token_key,
            href,
        } => {
            let credentials = caldav_credentials(base_url, username, token_key)?;
            match write {
                undo::SeriesWrite::RestoreMaster(document)
                | undo::SeriesWrite::RestoreAll(document) => {
                    caldav::write_resource(&credentials, href, &document.master)
                }
                undo::SeriesWrite::Delete => caldav::delete_event(&credentials, href),
            }
        }
    }
}

/// Deletes an event: the provider first when it lives on one — on a worker
/// thread the main loop polls — and then this device's copy. `on_done` fires
/// exactly once, on the main thread.
///
/// `all_events` chooses between removing a whole series and dropping the one
/// occurrence. Both the dialog's Delete and Ctrl+X come through here, so
/// there is still only one delete path that knows what a series is.
#[allow(clippy::too_many_arguments)]
pub(crate) fn delete_event_async(
    store: Rc<Store>,
    remote_event: Option<RemoteEvent>,
    event_id: i64,
    deleted: undo::Change,
    on_change: Rc<dyn Fn(undo::Record)>,
    all_events: bool,
    on_done: impl Fn(Result<Saved, String>) + 'static,
) {
    let Some(remote_event) = remote_event else {
        on_done(match store.delete_event(event_id) {
            Ok(()) => {
                on_change(undo::Record::Event(deleted));
                Ok(Saved::Cached)
            }
            Err(error) => Err(format!("Couldn't delete event: {error}")),
        });
        return;
    };

    let series_locator = remote_event.series_locator();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = if all_events {
            remote_event.delete_all_events()
        } else {
            remote_event.delete().map(|()| None)
        };
        let _ = tx.send(result);
    });
    glib::timeout_add_local(Duration::from_millis(100), move || match rx.try_recv() {
        Ok(Ok(captured)) => {
            on_done(match store.delete_event(event_id) {
                Ok(()) => {
                    match (captured, series_locator.clone()) {
                        (Some(document), Some(locator)) => on_change(undo::Record::Series(
                            undo::SeriesChange::deleted(document).locate(locator),
                        )),
                        _ => on_change(undo::Record::Event(deleted.clone())),
                    }
                    // Deleting a whole remote series takes one cached
                    // occurrence with it; the rest are separate rows only a
                    // sync can prune.
                    Ok(saved_state(true, all_events))
                }
                Err(error) => Err(format!(
                    "Remote event deleted, but the local cache could not be updated: {error}"
                )),
            });
            glib::ControlFlow::Break
        }
        Ok(Err(error)) => {
            on_done(Err(error));
            glib::ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => {
            on_done(Err("Remote delete stopped unexpectedly".to_string()));
            glib::ControlFlow::Break
        }
    });
}

/// Creates `draft` on `target`: the provider first — on a worker thread that
/// the main loop polls, the way every other provider call here runs — and then
/// this device's copy. `on_done` fires exactly once, on the main thread.
///
/// The `bool` in the error says whether the failure is worth retrying. A create
/// that reached the server but not the local cache is not: a second attempt
/// would create the event twice.
///
/// Both the dialog's Save and a paste onto another day come through here, so
/// the two can't drift on what a half-completed create means.
pub(crate) fn create_event_async(
    store: Rc<Store>,
    target: CreateTarget,
    draft: EventDraft,
    on_change: Rc<dyn Fn(undo::Record)>,
    on_done: impl Fn(Result<Saved, (String, bool)>) + 'static,
) {
    let (tx, rx) = mpsc::channel();
    let remote_draft = draft.clone();
    let remote_target = target.clone();
    std::thread::spawn(move || {
        let _ = tx.send(remote_target.create(&remote_draft));
    });
    glib::timeout_add_local(Duration::from_millis(100), move || match rx.try_recv() {
        Ok(Ok(None)) => {
            match store.create_event(target.calendar_id(), &draft) {
                Ok(id) => {
                    on_change(undo::Record::Event(undo::Change::created(
                        target.calendar_id(),
                        id,
                        draft.clone(),
                    )));
                    on_done(Ok(Saved::Cached));
                }
                Err(error) => on_done(Err(save_failure(&error.to_string(), false))),
            }
            glib::ControlFlow::Break
        }
        Ok(Ok(Some(created))) => {
            let result = undo::cache_remote_creation(
                &store,
                target.calendar_id(),
                &draft,
                created.is_google,
                &created.remote_id,
            );
            match result {
                Ok(id) => {
                    match created.series {
                        Some(document) => match target.series_locator(&created.remote_id) {
                            Some(locator) => on_change(undo::Record::Series(
                                undo::SeriesChange::created(document)
                                    .locate(locator)
                                    .remember_row(id),
                            )),
                            None => on_change(undo::Record::Event(undo::Change::created(
                                target.calendar_id(),
                                id,
                                draft.clone(),
                            ))),
                        },
                        None => on_change(undo::Record::Event(undo::Change::created(
                            target.calendar_id(),
                            id,
                            draft.clone(),
                        ))),
                    }
                    // A recurring event was created on the server as a series;
                    // the row just cached is one occurrence of it.
                    on_done(Ok(saved_state(true, draft.recurrence.is_some())));
                }
                Err(error) => on_done(Err(save_failure(&error.to_string(), true))),
            }
            glib::ControlFlow::Break
        }
        Ok(Err(error)) => {
            on_done(Err((error, true)));
            glib::ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => {
            on_done(Err((
                "Event creation stopped unexpectedly".to_string(),
                true,
            )));
            glib::ControlFlow::Break
        }
    });
}

/// The message to show for a failed save, and whether Save may be pressed
/// again.
///
/// Once the provider has created the event, a retry is not a retry: creation
/// isn't idempotent and there is no key to repeat it under, so pressing Save
/// again produces a second event on the server. The remote copy is already
/// what the user asked for, so the local cache catches up at the next sync.
fn save_failure(error: &str, created_remotely: bool) -> (String, bool) {
    if created_remotely {
        return (
            format!(
                "Created on the server, but this device's copy couldn't be updated: {error}. \
                 It will appear here after the next sync."
            ),
            false,
        );
    }
    (format!("Couldn't save event: {error}"), true)
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
    fn a_stored_exception_shows_the_series_rule() {
        let store = Store::open_in_memory().unwrap();
        let start = Local
            .with_ymd_and_hms(2026, 7, 2, 9, 0, 0)
            .single()
            .unwrap();
        let weekly = EventDraft {
            title: "Standup".to_string(),
            start,
            end: start + chrono::Duration::hours(1),
            all_day: false,
            location: None,
            notes: None,
            recurrence: Some(Frequency::Weekly),
            reminder_minutes: None,
            additional_reminders: Vec::new(),
            attendees: Vec::new(),
            instance_of: None,
            original_start: None,
            cancelled: false,
        };
        let id = store
            .create_event(store.default_calendar_id(), &weekly)
            .unwrap();
        let mut exception = store.event_by_id(id).unwrap().unwrap();
        exception.recurrence = None;
        exception.instance_of = Some(id);
        exception.original_start = Some(start + chrono::Duration::weeks(1));

        assert_eq!(repeat_rule(&store, &exception), Some(Frequency::Weekly));
        assert_eq!(
            repeat_rule(&store, &store.event_by_id(id).unwrap().unwrap()),
            Some(Frequency::Weekly),
            "the series itself shows its own rule"
        );
    }

    #[test]
    fn a_local_save_failure_can_be_retried() {
        let (message, retryable) = save_failure("database is locked", false);
        assert!(retryable);
        assert_eq!(message, "Couldn't save event: database is locked");
    }

    #[test]
    fn a_cache_failure_after_a_remote_create_must_not_offer_a_retry() {
        // Save is the only control that creates the event, and the provider
        // already has it — pressing Save again would make a second one.
        let (message, retryable) = save_failure("database is locked", true);
        assert!(!retryable, "{message}");
        assert!(
            message.contains("Created on the server"),
            "the message must say the event exists remotely: {message}"
        );
    }

    #[test]
    fn a_purely_local_save_leaves_the_cache_up_to_date() {
        assert_eq!(saved_state(false, false), Saved::Cached);
        // A local recurring event is stored as one row with its rule and
        // expanded on read, so the cache is complete on its own.
        assert_eq!(saved_state(false, true), Saved::Cached);
    }

    #[test]
    fn a_single_remote_event_is_complete_in_the_cache() {
        assert_eq!(saved_state(true, false), Saved::Cached);
    }

    #[test]
    fn acting_on_a_whole_remote_series_needs_a_sync_before_the_grid_is_right() {
        assert_eq!(saved_state(true, true), Saved::StaleUntilSync);
    }

    #[test]
    fn an_unexpanded_google_series_is_addressed_by_its_event_id() {
        let remote = RemoteEvent::Google {
            config: crate::config::GoogleConfig {
                client_id: String::new(),
                client_secret: String::new(),
            },
            token_key: "token".into(),
            calendar_id: "cal".into(),
            event_id: "master-1".into(),
            series_id: None,
        };

        match remote.series_locator() {
            Some(undo::SeriesLocator::Google { series_id, .. }) => {
                assert_eq!(series_id, "master-1");
            }
            other => panic!("expected the event id as the series id, got {other:?}"),
        }
    }

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

    #[test]
    fn invitee_addresses_are_trimmed_and_deduplicated() {
        let invitees = parse_invitees("Ada@example.com, bob@example.com; ada@example.com").unwrap();
        assert_eq!(invitees.len(), 2);
        assert_eq!(invitees[0].email, "Ada@example.com");
        assert_eq!(invitees[1].email, "bob@example.com");
    }

    #[test]
    fn malformed_invitee_addresses_are_rejected() {
        assert!(parse_invitees("not-an-address").is_err());
    }
}
