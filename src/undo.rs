//! Undo and redo for event changes, one event or a whole series.
//!
//! A change records both sides of an edit — what it held before, what it held
//! after — so undo and redo are one operation run in opposite directions, and
//! a create, an edit and a delete are the same shape with one side left empty.
//!
//! Nothing here touches the database or a provider. A change only *describes*
//! the write that would reverse it, and refuses when what is there now is no
//! longer what the change left behind. That refusal is the whole point: it
//! keeps an undo from throwing away a newer edit, which is the same rule the
//! failed-drag rollback in `window.rs` follows, generalized to every kind of
//! change.
//!
//! A series is not one row. CalDAV keeps the master and its per-occurrence
//! overrides in a single iCalendar resource; Google keeps the overrides as
//! their own events beside the master. Either way the undo entry stores that
//! whole set, not just the repeat rule, and reversing it puts the set back.

use crate::store::{EventDraft, Store};

/// How many changes back the user can go. Deep enough that reaching the end is
/// not something anyone runs into by accident, bounded so a long session can't
/// grow this without limit.
const DEPTH: usize = 100;

/// One reversible change to a single event.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    /// Which calendar the row belongs to. Needed to put a deleted row back,
    /// since the row itself is gone by then.
    pub calendar_id: i64,
    /// The row's id while it exists — `None` once it has been deleted. A row
    /// that comes back comes back with a new id, so this is updated as undo
    /// and redo move it; see [`History::commit_undo`].
    pub id: Option<i64>,
    /// What the row held before the change; `None` when it didn't exist yet.
    pub before: Option<EventDraft>,
    /// What it held afterwards; `None` when the change deleted it.
    pub after: Option<EventDraft>,
}

impl Change {
    /// An event that was created.
    pub fn created(calendar_id: i64, id: i64, draft: EventDraft) -> Self {
        Self {
            calendar_id,
            id: Some(id),
            before: None,
            after: Some(draft),
        }
    }

    /// An event that was edited in place.
    pub fn edited(calendar_id: i64, id: i64, before: EventDraft, after: EventDraft) -> Self {
        Self {
            calendar_id,
            id: Some(id),
            before: Some(before),
            after: Some(after),
        }
    }

    /// An event that was deleted.
    pub fn deleted(calendar_id: i64, draft: EventDraft) -> Self {
        Self {
            calendar_id,
            id: None,
            before: Some(draft),
            after: None,
        }
    }

    /// The write that takes the row back to how it was before this change,
    /// given what the row holds now.
    pub fn undo(&self, current: Option<&EventDraft>) -> Result<Write, Stale> {
        self.step(self.after.as_ref(), self.before.as_ref(), current)
    }

    /// The write that applies this change again, given what the row holds now.
    pub fn redo(&self, current: Option<&EventDraft>) -> Result<Write, Stale> {
        self.step(self.before.as_ref(), self.after.as_ref(), current)
    }

    /// One step in either direction: `expected` is what this change left the
    /// row holding, `target` is where the step should leave it.
    fn step(
        &self,
        expected: Option<&EventDraft>,
        target: Option<&EventDraft>,
        current: Option<&EventDraft>,
    ) -> Result<Write, Stale> {
        // The row must still hold exactly what this change left there. Anything
        // else means someone — another edit, or a sync — has been through since,
        // and their version is the one to keep.
        match (expected, current) {
            (Some(expected), Some(current)) if same_edit(expected, current) => {}
            (None, None) => {}
            (Some(_), None) => return Err(Stale::Gone),
            _ => return Err(Stale::ChangedSince),
        }
        match (target, self.id) {
            (Some(draft), Some(id)) => Ok(Write::Update {
                id,
                draft: draft.clone(),
            }),
            (Some(draft), None) => Ok(Write::Insert {
                calendar_id: self.calendar_id,
                draft: draft.clone(),
            }),
            (None, Some(id)) => Ok(Write::Delete { id }),
            // Nothing to remove and nothing to write: the row is already where
            // this step wanted it.
            (None, None) => Err(Stale::Gone),
        }
    }
}

/// Whether a row still holds what a change left in it.
///
/// Attendees are deliberately not compared. They belong to the provider rather
/// than to an edit: a sync or an invitation response rewrites them at any
/// moment, and `Store::update_event` does not touch them at all (that is
/// `update_event_attendees`, kept separate so editing an event cannot wipe what
/// was synced). Comparing them would make an undo refuse itself because someone
/// accepted an invitation in the meantime.
fn same_edit(a: &EventDraft, b: &EventDraft) -> bool {
    a.title == b.title
        && a.start == b.start
        && a.end == b.end
        && a.all_day == b.all_day
        && a.location == b.location
        && a.notes == b.notes
        && a.recurrence == b.recurrence
        && a.reminder_minutes == b.reminder_minutes
        && a.additional_reminders == b.additional_reminders
}

/// The database write that carries out one undo or redo step.
#[derive(Clone, Debug, PartialEq)]
pub enum Write {
    /// Put a deleted event back. It returns with a new id.
    Insert {
        calendar_id: i64,
        draft: EventDraft,
    },
    Update {
        id: i64,
        draft: EventDraft,
    },
    Delete {
        id: i64,
    },
}

/// Why a change can no longer be applied in the direction asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stale {
    /// The row holds something this change didn't write — something else has
    /// edited it since, and reversing now would discard that.
    ChangedSince,
    /// The row this change expected is gone.
    Gone,
}

/// Carries out one undo or redo step, returning the id the row has afterwards:
/// a new one for a row put back, `None` for one removed.
///
/// This lives here rather than in the window so the whole cycle — record,
/// reverse, write, replay — can be exercised against a real database with no
/// display attached.
pub fn apply(store: &Store, write: &Write) -> rusqlite::Result<Option<i64>> {
    match write {
        Write::Insert { calendar_id, draft } => store.create_event(*calendar_id, draft).map(Some),
        Write::Update { id, draft } => store.update_event(*id, draft).map(|()| Some(*id)),
        Write::Delete { id } => store.delete_event(*id).map(|()| None),
    }
}

/// Caches a provider-confirmed creation, retaining its identity for later sync and redo.
pub fn cache_remote_creation(
    store: &Store,
    calendar_id: i64,
    draft: &EventDraft,
    is_google: bool,
    remote_id: &str,
) -> rusqlite::Result<i64> {
    if is_google {
        store.upsert_google_event(calendar_id, remote_id, draft, &draft.attendees)
    } else {
        store.upsert_caldav_event(calendar_id, remote_id, draft, &draft.attendees)
    }
}

/// Whether this delete removes the whole series.
///
/// "All events" does. So does deleting the one cached row of a series that
/// sync has not expanded yet: that row still carries the repeat rule, and on
/// the provider it is the master, so a single-row delete would remove the
/// series and leave undo with nothing to put back.
pub fn deletes_whole_series(apply_to_all: bool, series_instance: bool, repeats: bool) -> bool {
    apply_to_all || (!series_instance && repeats)
}

/// Whether carrying out `write` would mean re-creating an event on a provider,
/// so it must go through the provider creation path before caching the row.
///
/// Restoring a deleted event is the only step that can hit this: putting the row
/// back in the local cache alone would leave it there until the next sync
/// deletes it again, so refusing is the honest answer.
pub fn needs_a_remote_create(write: &Write, calendar_is_local: bool) -> bool {
    matches!(write, Write::Insert { .. }) && !calendar_is_local
}

/// Which provider a series document came from. The two don't compare equal:
/// an iCalendar resource and a Google event are different shapes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeriesKind {
    /// One iCalendar resource. Overrides are further `VEVENT`s in `master`;
    /// `exceptions` is empty.
    Ics,
    /// A Google series event in `master`, plus each modified instance in
    /// `exceptions`.
    Google,
}

/// The provider's copy of a series: the master and any per-occurrence overrides.
///
/// This is the whole set an "All events" change rewrites, which is what an undo
/// has to put back. A repeat rule on its own would restore the schedule and
/// lose a moved or cancelled occurrence.
#[derive(Clone, Debug, PartialEq)]
pub struct SeriesDocument {
    pub kind: SeriesKind,
    pub master: String,
    pub exceptions: Vec<String>,
}

impl SeriesDocument {
    pub fn ics(master: impl Into<String>) -> Self {
        Self {
            kind: SeriesKind::Ics,
            master: master.into(),
            exceptions: Vec::new(),
        }
    }

    pub fn google(master: impl Into<String>, exceptions: Vec<String>) -> Self {
        Self {
            kind: SeriesKind::Google,
            master: master.into(),
            exceptions,
        }
    }

    /// Whether `other` is still the series this document left behind.
    ///
    /// Providers rewrite timestamps, etags and sequence numbers on every save,
    /// so a byte compare would refuse an undo of our own edit. The comparison
    /// is the fields an all-events change actually writes, and the overrides.
    pub fn matches(&self, other: &Self) -> bool {
        self.kind == other.kind && series_signature(self) == series_signature(other)
    }
}

/// Where a series lives on its provider. Enough to read it back and write it,
/// without the secret itself — `token_key` is the keyring entry.
#[derive(Clone, Debug, PartialEq)]
pub enum SeriesLocator {
    Google {
        token_key: String,
        google_calendar_id: String,
        series_id: String,
    },
    Caldav {
        base_url: String,
        username: String,
        token_key: String,
        /// The resource href, without an `#instance` suffix.
        href: String,
    },
}

/// One reversible change to a whole series.
///
/// `before` is `None` when the change created the series, `after` when it
/// deleted one. [`SeriesLocator`] says where to read and write it; the
/// decision of *what* to write doesn't use it.
#[derive(Clone, Debug, PartialEq)]
pub struct SeriesChange {
    pub before: Option<SeriesDocument>,
    pub after: Option<SeriesDocument>,
    pub locator: SeriesLocator,
    /// The local row a create cached. Series undo talks to the provider and
    /// leaves this alone; a single-event undo that had to recreate a repeating
    /// event uses it to follow the new row.
    pub cached_id: Option<i64>,
}

impl SeriesChange {
    pub fn edited(before: SeriesDocument, after: SeriesDocument) -> Self {
        Self {
            before: Some(before),
            after: Some(after),
            locator: SeriesLocator::unlocated(),
            cached_id: None,
        }
    }

    pub fn deleted(before: SeriesDocument) -> Self {
        Self {
            before: Some(before),
            after: None,
            locator: SeriesLocator::unlocated(),
            cached_id: None,
        }
    }

    pub fn created(after: SeriesDocument) -> Self {
        Self {
            before: None,
            after: Some(after),
            locator: SeriesLocator::unlocated(),
            cached_id: None,
        }
    }

    /// The provider coordinates the window uses to carry the write out.
    pub fn locate(mut self, locator: SeriesLocator) -> Self {
        self.locator = locator;
        self
    }

    /// Remembers the row this create cached, so a caller updating an existing
    /// history entry can retarget it.
    pub fn remember_row(mut self, id: i64) -> Self {
        self.cached_id = Some(id);
        self
    }

    pub fn undo(&self, current: Option<&SeriesDocument>) -> Result<SeriesWrite, Stale> {
        self.step(self.after.as_ref(), self.before.as_ref(), current)
    }

    pub fn redo(&self, current: Option<&SeriesDocument>) -> Result<SeriesWrite, Stale> {
        self.step(self.before.as_ref(), self.after.as_ref(), current)
    }

    fn step(
        &self,
        expected: Option<&SeriesDocument>,
        target: Option<&SeriesDocument>,
        current: Option<&SeriesDocument>,
    ) -> Result<SeriesWrite, Stale> {
        // The series must still be what this change left it as. Anything else
        // — a later edit, or the series coming back after a delete — is kept.
        match (expected, current) {
            (None, None) => {}
            (Some(expected), Some(current)) if expected.matches(current) => {}
            (Some(_), None) => return Err(Stale::Gone),
            _ => return Err(Stale::ChangedSince),
        }
        match target {
            // The series is gone, so this step has to recreate it, overrides
            // included. Rewriting a series that is still there only puts the
            // master back; its overrides were never removed.
            Some(document) if expected.is_none() => Ok(SeriesWrite::RestoreAll(document.clone())),
            Some(document) => Ok(SeriesWrite::RestoreMaster(document.clone())),
            None => Ok(SeriesWrite::Delete),
        }
    }
}

impl SeriesLocator {
    /// A placeholder for a change whose write is being decided and not sent.
    fn unlocated() -> Self {
        Self::Google {
            token_key: String::new(),
            google_calendar_id: String::new(),
            series_id: String::new(),
        }
    }
}

/// The provider write that carries out one series undo or redo.
#[derive(Clone, Debug, PartialEq)]
pub enum SeriesWrite {
    /// The series is still there. Put the master back and leave its overrides
    /// in place — the change never removed them.
    RestoreMaster(SeriesDocument),
    /// The series is gone. Put the master and its overrides back.
    RestoreAll(SeriesDocument),
    /// Remove the series.
    Delete,
}

/// One entry in the undo stack: a single row, or a whole series.
#[derive(Clone, Debug, PartialEq)]
pub enum Record {
    Event(Change),
    Series(SeriesChange),
}

impl Record {
    #[cfg(test)]
    fn as_event(&self) -> Option<&Change> {
        match self {
            Self::Event(change) => Some(change),
            Self::Series(_) => None,
        }
    }
}

/// The parts of a series an all-events change writes, stable across the
/// bookkeeping a provider adds on save.
fn series_signature(document: &SeriesDocument) -> Option<String> {
    match document.kind {
        SeriesKind::Ics => Some(ics_signature(&document.master)),
        SeriesKind::Google => google_signature(&document.master, &document.exceptions),
    }
}

/// Properties that distinguish one series from a later edit of it. A server's
/// `DTSTAMP` and `SEQUENCE` change on every save and say nothing about the
/// series, so they are left out.
const ICS_FIELDS: &[&str] = &[
    "UID",
    "SUMMARY",
    "LOCATION",
    "DESCRIPTION",
    "DTSTART",
    "DTEND",
    "RRULE",
    "EXDATE",
    "STATUS",
    "RECURRENCE-ID",
];

fn ics_signature(ics: &str) -> String {
    let mut components = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut in_event = false;
    for line in unfold_lines(ics) {
        let upper = line.to_ascii_uppercase();
        if upper.starts_with("BEGIN:VEVENT") {
            in_event = true;
            current.clear();
            continue;
        }
        if upper.starts_with("END:VEVENT") {
            current.sort();
            components.push(current.join("\n"));
            in_event = false;
            continue;
        }
        if in_event && ics_field(&line).is_some_and(|name| ICS_FIELDS.contains(&name)) {
            current.push(line);
        }
    }
    components.sort();
    components.join("\n---\n")
}

fn ics_field(line: &str) -> Option<&str> {
    let name = line.split_once([';', ':'])?.0;
    Some(name)
}

fn unfold_lines(ics: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for line in ics.replace("\r\n", "\n").lines() {
        if (line.starts_with(' ') || line.starts_with('\t'))
            && let Some(previous) = lines.last_mut()
        {
            previous.push_str(line.trim_start());
        } else if !line.is_empty() {
            lines.push(line.to_string());
        }
    }
    lines
}

fn google_signature(master: &str, exceptions: &[String]) -> Option<String> {
    let master: serde_json::Value = serde_json::from_str(master).ok()?;
    let mut parts = vec![google_event_signature(&master)];
    let mut overrides: Vec<String> = exceptions
        .iter()
        .map(|exception| {
            serde_json::from_str::<serde_json::Value>(exception)
                .map(|event| google_event_signature(&event))
                .unwrap_or_else(|_| exception.clone())
        })
        .collect();
    overrides.sort();
    parts.extend(overrides);
    Some(parts.join("\n---\n"))
}

/// Fields of one Google event that an edit can change. An id, an etag and a
/// time zone the server filled in are not among them. A missing status is the
/// same as `confirmed`, which is what Google writes back onto a create.
fn google_event_signature(event: &serde_json::Value) -> String {
    let status = match event.get("status").and_then(|value| value.as_str()) {
        None | Some("confirmed") => "confirmed",
        Some(other) => other,
    };
    let mut rules: Vec<String> = event
        .get("recurrence")
        .and_then(|value| value.as_array())
        .map(|rules| {
            rules
                .iter()
                .filter_map(|rule| rule.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    rules.sort();
    format!(
        "summary={}\nlocation={}\ndescription={}\nstatus={status}\nstart={}\nend={}\noriginal={}\nrecurrence={}",
        json_text(event.get("summary")),
        json_text(event.get("location")),
        json_text(event.get("description")),
        json_text(
            event
                .pointer("/start/date")
                .or_else(|| event.pointer("/start/dateTime"))
        ),
        json_text(
            event
                .pointer("/end/date")
                .or_else(|| event.pointer("/end/dateTime"))
        ),
        json_text(
            event
                .pointer("/originalStartTime/date")
                .or_else(|| event.pointer("/originalStartTime/dateTime"))
        ),
        rules.join(",")
    )
}

fn json_text(value: Option<&serde_json::Value>) -> String {
    match value {
        None | Some(serde_json::Value::Null) => String::new(),
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    }
}

/// The undo and redo stacks for one window.
#[derive(Default)]
pub struct History {
    done: Vec<Record>,
    undone: Vec<Record>,
}

impl History {
    /// Records a single-event change the user just made. This is a new branch
    /// of history, so anything that had been undone can no longer be redone.
    pub fn record(&mut self, change: Change) {
        self.push(Record::Event(change));
    }

    /// Records a whole-series change. Same branching rule as [`Self::record`].
    pub fn record_series(&mut self, change: SeriesChange) {
        self.push(Record::Series(change));
    }

    fn push(&mut self, record: Record) {
        self.done.push(record);
        if self.done.len() > DEPTH {
            self.done.remove(0);
        }
        self.undone.clear();
    }

    /// The change an undo would reverse, without taking it off the stack.
    pub fn peek_undo(&self) -> Option<&Record> {
        self.done.last()
    }

    /// The change a redo would re-apply.
    pub fn peek_redo(&self) -> Option<&Record> {
        self.undone.last()
    }

    /// Moves the top change over to the redo side after its write went
    /// through. `id` is the row's id now — a re-created row has a new one, and
    /// a deleted row has none. A series keeps its provider identity, so `id`
    /// is ignored for one.
    pub fn commit_undo(&mut self, id: Option<i64>) {
        if let Some(mut record) = self.done.pop() {
            if let Record::Event(change) = &mut record {
                change.id = id;
            }
            self.undone.push(record);
        }
    }

    /// Moves the top redo back to the undo side after its write went through.
    pub fn commit_redo(&mut self, id: Option<i64>) {
        if let Some(mut record) = self.undone.pop() {
            if let Record::Event(change) = &mut record {
                change.id = id;
            }
            self.done.push(record);
        }
    }

    /// Drops a change that can no longer be applied, so the next attempt
    /// reaches the one behind it instead of retrying this one forever.
    pub fn discard_undo(&mut self) {
        self.done.pop();
    }

    /// Drops an unapplicable redo.
    pub fn discard_redo(&mut self) {
        self.undone.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Duration, Local, TimeZone};

    fn at(hour: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(2026, 8, 24, hour, 0, 0)
            .single()
            .expect("an unambiguous local time")
    }

    fn draft(title: &str, hour: u32) -> EventDraft {
        EventDraft {
            title: title.to_string(),
            start: at(hour),
            end: at(hour) + Duration::hours(1),
            all_day: false,
            location: None,
            notes: None,
            recurrence: None,
            reminder_minutes: None,
            additional_reminders: Vec::new(),
            attendees: Vec::new(),
        }
    }

    /// The sequence `Ui::step_history` runs, with the GTK parts left out: read
    /// the row as it stands, ask for the reversing write, carry it out, and move
    /// the change to the other stack with the id the row has now.
    fn undo_once(store: &Store, history: &mut History) -> Result<(), Stale> {
        let change = history
            .peek_undo()
            .and_then(Record::as_event)
            .cloned()
            .expect("a change waiting to be undone");
        let current = current_draft(store, &change);
        let write = change.undo(current.as_ref())?;
        let id = apply(store, &write).expect("the write to land");
        history.commit_undo(id);
        Ok(())
    }

    fn redo_once(store: &Store, history: &mut History) -> Result<(), Stale> {
        let change = history
            .peek_redo()
            .and_then(Record::as_event)
            .cloned()
            .expect("a change waiting to be redone");
        let current = current_draft(store, &change);
        let write = change.redo(current.as_ref())?;
        let id = apply(store, &write).expect("the write to land");
        history.commit_redo(id);
        Ok(())
    }

    fn current_draft(store: &Store, change: &Change) -> Option<EventDraft> {
        change
            .id
            .and_then(|id| store.event_by_id(id).expect("the query to run"))
            .map(|event| event.draft())
    }

    #[test]
    fn restoring_a_remote_event_keeps_its_provider_identity_for_redo_and_sync() {
        for google in [true, false] {
            let store = Store::open_in_memory().unwrap();
            let written = draft("Dentist", 15);
            let mut history = History::default();
            history.record(Change::deleted(1, written.clone()));
            let id = cache_remote_creation(&store, 1, &written, google, "restored-id").unwrap();
            history.commit_undo(Some(id));
            let restored = store.event_by_id(id).unwrap().unwrap();
            let remote_id = if google {
                &restored.google_event_id
            } else {
                &restored.icloud_event_id
            };
            assert_eq!(remote_id.as_deref(), Some("restored-id"));
            assert_eq!(
                history
                    .peek_redo()
                    .and_then(Record::as_event)
                    .unwrap()
                    .redo(Some(&restored.draft())),
                Ok(Write::Delete { id })
            );
        }
    }

    #[test]
    fn a_created_event_is_gone_after_an_undo_and_back_after_a_redo() {
        let store = Store::open_in_memory().expect("an in-memory database");
        let mut history = History::default();
        let written = draft("Standup", 9);
        let id = store.create_event(1, &written).expect("the event to store");
        history.record(Change::created(1, id, written.clone()));

        undo_once(&store, &mut history).expect("the row is untouched, so it can be undone");
        assert!(
            store.event_by_id(id).unwrap().is_none(),
            "undoing a create removes the row it wrote"
        );

        redo_once(&store, &mut history).expect("nothing has taken its place");
        let back = history
            .peek_undo()
            .and_then(Record::as_event)
            .and_then(|change| change.id)
            .and_then(|id| store.event_by_id(id).unwrap())
            .expect("the event is back, at whatever id SQLite gave it");
        assert_eq!(back.draft(), written);
    }

    #[test]
    fn an_edit_is_put_back_and_can_be_reapplied() {
        let store = Store::open_in_memory().expect("an in-memory database");
        let mut history = History::default();
        let before = draft("Standup", 9);
        let after = draft("Standup", 11);
        let id = store.create_event(1, &before).expect("the event to store");
        store.update_event(id, &after).expect("the edit to land");
        history.record(Change::edited(1, id, before.clone(), after.clone()));

        undo_once(&store, &mut history).expect("the row still holds the edit");
        assert_eq!(store.event_by_id(id).unwrap().unwrap().draft(), before);

        redo_once(&store, &mut history).expect("the row still holds the original");
        assert_eq!(store.event_by_id(id).unwrap().unwrap().draft(), after);
    }

    #[test]
    fn a_deleted_event_comes_back_and_can_be_removed_again() {
        let store = Store::open_in_memory().expect("an in-memory database");
        let mut history = History::default();
        let written = draft("Dentist", 15);
        let id = store.create_event(1, &written).expect("the event to store");
        store.delete_event(id).expect("the delete to land");
        history.record(Change::deleted(1, written.clone()));

        undo_once(&store, &mut history).expect("nothing is in its place");
        let restored_id = history
            .peek_redo()
            .and_then(Record::as_event)
            .and_then(|change| change.id)
            .expect("the restored row has an id");
        assert_eq!(
            store.event_by_id(restored_id).unwrap().unwrap().draft(),
            written
        );

        redo_once(&store, &mut history).expect("the restored row is untouched");
        assert!(store.event_by_id(restored_id).unwrap().is_none());
    }

    #[test]
    fn an_edit_made_since_is_not_thrown_away_by_an_undo() {
        let store = Store::open_in_memory().expect("an in-memory database");
        let mut history = History::default();
        let before = draft("Standup", 9);
        let after = draft("Standup", 11);
        let id = store.create_event(1, &before).expect("the event to store");
        store.update_event(id, &after).expect("the edit to land");
        history.record(Change::edited(1, id, before, after));

        // Someone renames it — a sync, or the user in the dialog.
        let mut renamed = draft("Standup", 11);
        renamed.title = "Standup (moved room)".to_string();
        store
            .update_event(id, &renamed)
            .expect("the rename to land");

        assert_eq!(undo_once(&store, &mut history), Err(Stale::ChangedSince));
        assert_eq!(
            store.event_by_id(id).unwrap().unwrap().draft(),
            renamed,
            "the newer edit survives the refused undo"
        );
    }

    #[test]
    fn undoing_a_create_deletes_the_row_it_wrote() {
        let change = Change::created(1, 7, draft("Standup", 9));

        assert_eq!(
            change.undo(Some(&draft("Standup", 9))),
            Ok(Write::Delete { id: 7 })
        );
    }

    #[test]
    fn undoing_an_edit_puts_back_what_was_there_before() {
        let change = Change::edited(1, 7, draft("Standup", 9), draft("Standup", 11));

        assert_eq!(
            change.undo(Some(&draft("Standup", 11))),
            Ok(Write::Update {
                id: 7,
                draft: draft("Standup", 9)
            })
        );
    }

    #[test]
    fn undoing_a_delete_puts_the_event_back_on_its_calendar() {
        let change = Change::deleted(4, draft("Dentist", 15));

        assert_eq!(
            change.undo(None),
            Ok(Write::Insert {
                calendar_id: 4,
                draft: draft("Dentist", 15)
            })
        );
    }

    #[test]
    fn an_undo_refuses_once_something_else_has_edited_the_row() {
        let change = Change::edited(1, 7, draft("Standup", 9), draft("Standup", 11));

        assert_eq!(
            change.undo(Some(&draft("Renamed since", 11))),
            Err(Stale::ChangedSince)
        );
    }

    #[test]
    fn an_undo_refuses_when_the_row_is_gone() {
        let change = Change::edited(1, 7, draft("Standup", 9), draft("Standup", 11));

        assert_eq!(change.undo(None), Err(Stale::Gone));
    }

    #[test]
    fn undoing_a_delete_refuses_when_a_sync_already_brought_it_back() {
        let change = Change::deleted(4, draft("Dentist", 15));

        assert_eq!(
            change.undo(Some(&draft("Dentist", 15))),
            Err(Stale::ChangedSince)
        );
    }

    #[test]
    fn an_invitation_response_arriving_since_does_not_block_the_undo() {
        let change = Change::edited(1, 7, draft("Standup", 9), draft("Standup", 11));
        let mut answered = draft("Standup", 11);
        answered.attendees = vec![crate::store::Attendee {
            email: "ada@example.com".to_string(),
            name: None,
            status: Some("accepted".to_string()),
            is_self: true,
        }];

        assert_eq!(
            change.undo(Some(&answered)),
            Ok(Write::Update {
                id: 7,
                draft: draft("Standup", 9)
            }),
            "attendees belong to the provider, not to the edit being undone"
        );
    }

    #[test]
    fn redoing_an_edit_applies_it_again() {
        let change = Change::edited(1, 7, draft("Standup", 9), draft("Standup", 11));

        assert_eq!(
            change.redo(Some(&draft("Standup", 9))),
            Ok(Write::Update {
                id: 7,
                draft: draft("Standup", 11)
            })
        );
    }

    #[test]
    fn restoring_a_deleted_event_onto_a_synced_calendar_needs_a_remote_create() {
        let write = Write::Insert {
            calendar_id: 4,
            draft: draft("Dentist", 15),
        };

        assert!(needs_a_remote_create(&write, false));
        assert!(
            !needs_a_remote_create(&write, true),
            "a local calendar takes the row straight back"
        );
    }

    #[test]
    fn changing_or_removing_a_row_never_needs_a_remote_create() {
        for write in [
            Write::Update {
                id: 7,
                draft: draft("Standup", 9),
            },
            Write::Delete { id: 7 },
        ] {
            assert!(!needs_a_remote_create(&write, false));
        }
    }

    #[test]
    fn a_recreated_row_is_redone_at_the_id_it_came_back_with() {
        let mut history = History::default();
        history.record(Change::deleted(4, draft("Dentist", 15)));

        // The undo re-inserted it, and SQLite handed out a fresh id.
        history.commit_undo(Some(31));

        assert_eq!(
            history
                .peek_redo()
                .and_then(Record::as_event)
                .expect("the undone delete can be redone")
                .redo(Some(&draft("Dentist", 15))),
            Ok(Write::Delete { id: 31 })
        );
    }

    #[test]
    fn making_a_new_change_gives_up_the_redo_branch() {
        let mut history = History::default();
        history.record(Change::created(1, 7, draft("Standup", 9)));
        history.commit_undo(None);
        assert!(history.peek_redo().is_some());

        history.record(Change::created(1, 8, draft("Something else", 14)));

        assert!(
            history.peek_redo().is_none(),
            "history branched, so the old redo is unreachable"
        );
    }

    #[test]
    fn undo_then_redo_leaves_the_stacks_as_they_started() {
        let mut history = History::default();
        let change = Change::edited(1, 7, draft("Standup", 9), draft("Standup", 11));
        history.record(change.clone());

        history.commit_undo(Some(7));
        history.commit_redo(Some(7));

        assert_eq!(
            history.peek_undo().and_then(Record::as_event),
            Some(&change)
        );
        assert!(history.peek_redo().is_none());
    }

    #[test]
    fn a_change_that_can_no_longer_be_applied_is_dropped_not_retried() {
        let mut history = History::default();
        history.record(Change::created(1, 7, draft("First", 9)));
        history.record(Change::created(1, 8, draft("Second", 10)));

        history.discard_undo();

        assert_eq!(
            history
                .peek_undo()
                .and_then(Record::as_event)
                .and_then(|change| change.id),
            Some(7),
            "the next undo reaches the change behind the one dropped"
        );
    }

    #[test]
    fn history_stops_growing_at_its_depth() {
        let mut history = History::default();
        for index in 0..DEPTH + 10 {
            history.record(Change::created(1, index as i64, draft("Filler", 9)));
        }

        assert_eq!(history.done.len(), DEPTH);
        assert_eq!(
            history
                .peek_undo()
                .and_then(Record::as_event)
                .and_then(|change| change.id),
            Some((DEPTH + 9) as i64),
            "the newest change is still the one an undo reaches first"
        );
    }

    fn weekly_with_override(summary: &str) -> SeriesDocument {
        SeriesDocument::ics(format!(
            "BEGIN:VCALENDAR\r\n\
             BEGIN:VEVENT\r\n\
             UID:s\r\n\
             DTSTAMP:20260101T000000Z\r\n\
             SUMMARY:{summary}\r\n\
             DTSTART:20260701T090000Z\r\n\
             DTEND:20260701T100000Z\r\n\
             RRULE:FREQ=WEEKLY\r\n\
             END:VEVENT\r\n\
             BEGIN:VEVENT\r\n\
             UID:s\r\n\
             RECURRENCE-ID:20260708T090000Z\r\n\
             SUMMARY:Moved one\r\n\
             DTSTART:20260708T110000Z\r\n\
             DTEND:20260708T120000Z\r\n\
             END:VEVENT\r\n\
             END:VCALENDAR\r\n"
        ))
    }

    #[test]
    fn undoing_an_all_events_edit_restores_the_previous_series_and_its_override() {
        let before = weekly_with_override("Standup");
        let after = weekly_with_override("Moved");
        let change = SeriesChange::edited(before.clone(), after.clone());

        assert_eq!(
            change.undo(Some(&after)),
            Ok(SeriesWrite::RestoreMaster(before))
        );
    }

    #[test]
    fn undoing_an_all_events_edit_refuses_once_the_series_has_changed_again() {
        let before = weekly_with_override("Standup");
        let after = weekly_with_override("Moved");
        let since = weekly_with_override("Renamed on the phone");
        let change = SeriesChange::edited(before, after);

        assert_eq!(change.undo(Some(&since)), Err(Stale::ChangedSince));
    }

    #[test]
    fn a_server_timestamp_does_not_make_our_own_series_edit_look_newer() {
        let before = weekly_with_override("Standup");
        let after = weekly_with_override("Moved");
        let mut fetched = after
            .master
            .replace("DTSTAMP:20260101T000000Z", "DTSTAMP:20260202T030405Z");
        fetched = fetched.replace("SUMMARY:Moved\r\n", "SUMMARY:Moved\r\nSEQUENCE:3\r\n");
        let change = SeriesChange::edited(before.clone(), after);

        assert_eq!(
            change.undo(Some(&SeriesDocument::ics(fetched))),
            Ok(SeriesWrite::RestoreMaster(before))
        );
    }

    #[test]
    fn undoing_a_deleted_series_puts_back_the_whole_set_when_it_is_gone() {
        let before = weekly_with_override("Standup");
        let change = SeriesChange::deleted(before.clone());

        assert_eq!(change.undo(None), Ok(SeriesWrite::RestoreAll(before)));
    }

    #[test]
    fn undoing_a_deleted_series_refuses_when_it_has_already_come_back() {
        let before = weekly_with_override("Standup");
        let change = SeriesChange::deleted(before.clone());

        assert_eq!(change.undo(Some(&before)), Err(Stale::ChangedSince));
    }

    #[test]
    fn undoing_a_created_series_deletes_it_while_it_still_matches() {
        let created = SeriesDocument::google(
            r#"{"summary":"Standup","start":{"dateTime":"2026-07-01T09:00:00-04:00"},"end":{"dateTime":"2026-07-01T10:00:00-04:00"},"recurrence":["RRULE:FREQ=WEEKLY"]}"#,
            Vec::new(),
        );
        // What Google hands back adds an id, an etag and a status the create
        // body never sent. None of those is a later edit.
        let fetched = SeriesDocument::google(
            r#"{"id":"series-1","etag":"\"abc\"","status":"confirmed","summary":"Standup","start":{"dateTime":"2026-07-01T09:00:00-04:00","timeZone":"America/New_York"},"end":{"dateTime":"2026-07-01T10:00:00-04:00","timeZone":"America/New_York"},"recurrence":["RRULE:FREQ=WEEKLY"]}"#,
            Vec::new(),
        );
        let change = SeriesChange::created(created);

        assert_eq!(change.undo(Some(&fetched)), Ok(SeriesWrite::Delete));
    }

    #[test]
    fn undoing_a_created_series_refuses_once_someone_has_edited_it() {
        let created = SeriesDocument::google(
            r#"{"summary":"Standup","recurrence":["RRULE:FREQ=WEEKLY"]}"#,
            Vec::new(),
        );
        let edited = SeriesDocument::google(
            r#"{"summary":"Standup (moved)","recurrence":["RRULE:FREQ=WEEKLY"]}"#,
            Vec::new(),
        );
        let change = SeriesChange::created(created);

        assert_eq!(change.undo(Some(&edited)), Err(Stale::ChangedSince));
    }

    #[test]
    fn a_changed_google_exception_blocks_undoing_the_series() {
        let master = r#"{"summary":"Standup","recurrence":["RRULE:FREQ=WEEKLY"]}"#;
        let exception = r#"{"id":"ex-1","recurringEventId":"series-1","originalStartTime":{"dateTime":"2026-07-08T09:00:00-04:00"},"summary":"Moved one","status":"confirmed"}"#;
        let renamed = r#"{"id":"ex-1","etag":"\"new\"","recurringEventId":"series-1","originalStartTime":{"dateTime":"2026-07-08T09:00:00-04:00"},"summary":"Renamed one","status":"confirmed"}"#;
        let change = SeriesChange::edited(
            SeriesDocument::google(master, vec![exception.into()]),
            SeriesDocument::google(master, vec![exception.into()]),
        );

        assert_eq!(
            change.undo(Some(&SeriesDocument::google(master, vec![renamed.into()]))),
            Err(Stale::ChangedSince)
        );
    }

    #[test]
    fn redoing_a_deleted_series_removes_it_again() {
        let before = weekly_with_override("Standup");
        let change = SeriesChange::deleted(before.clone());

        assert_eq!(change.redo(Some(&before)), Ok(SeriesWrite::Delete));
    }

    #[test]
    fn deleting_the_unexpanded_master_removes_the_whole_series() {
        // Right after a create, the only cached row still carries the rule and
        // it is the master on the provider. There is no "this event" to keep.
        assert!(deletes_whole_series(false, false, true));
    }

    #[test]
    fn deleting_one_occurrence_keeps_the_rest_of_the_series() {
        assert!(!deletes_whole_series(false, true, false));
        // A repeat picked in the dialog does not turn "this event" into "all".
        assert!(!deletes_whole_series(false, true, true));
    }

    #[test]
    fn choosing_all_events_removes_the_whole_series() {
        assert!(deletes_whole_series(true, true, false));
    }

    #[test]
    fn a_series_change_is_what_undo_reaches_ahead_of_an_older_single_event() {
        let mut history = History::default();
        history.record(Change::edited(
            1,
            7,
            draft("Standup", 9),
            draft("Standup", 11),
        ));
        history.record_series(SeriesChange::created(weekly_with_override("Standup")));

        assert!(
            matches!(history.peek_undo(), Some(Record::Series(_))),
            "the series change is the last thing that happened, so it is undone first"
        );
    }
}
