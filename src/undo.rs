//! Undo and redo for single-event changes.
//!
//! A change records the row on both sides of an edit — what it held before,
//! what it held after — so undo and redo are one operation run in opposite
//! directions, and a create, an edit and a delete are the same shape with one
//! side left empty.
//!
//! Nothing here touches the database. A change only *describes* the write that
//! would reverse it, and refuses when the row no longer holds what the change
//! left behind. That refusal is the whole point: it keeps an undo from throwing
//! away a newer edit, which is the same rule the failed-drag rollback in
//! `window.rs` follows, generalized to every kind of change.
//!
//! Whole-series edits are deliberately not recorded. Reversing one means
//! rewriting a provider's master recurrence resource, which is a different and
//! much larger problem than putting a single row back.

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

/// Whether carrying out `write` would mean re-creating an event on a provider,
/// which Calix cannot do yet.
///
/// Restoring a deleted event is the only step that can hit this: putting the row
/// back in the local cache alone would leave it there until the next sync
/// deletes it again, so refusing is the honest answer.
pub fn needs_a_remote_create(write: &Write, calendar_is_local: bool) -> bool {
    matches!(write, Write::Insert { .. }) && !calendar_is_local
}

/// The undo and redo stacks for one window.
#[derive(Default)]
pub struct History {
    done: Vec<Change>,
    undone: Vec<Change>,
}

impl History {
    /// Records a change the user just made. This is a new branch of history,
    /// so anything that had been undone can no longer be redone.
    pub fn record(&mut self, change: Change) {
        self.done.push(change);
        if self.done.len() > DEPTH {
            self.done.remove(0);
        }
        self.undone.clear();
    }

    /// The change an undo would reverse, without taking it off the stack.
    pub fn peek_undo(&self) -> Option<&Change> {
        self.done.last()
    }

    /// The change a redo would re-apply.
    pub fn peek_redo(&self) -> Option<&Change> {
        self.undone.last()
    }

    /// Moves the top change over to the redo side after its write went
    /// through. `id` is the row's id now — a re-created row has a new one, and
    /// a deleted row has none.
    pub fn commit_undo(&mut self, id: Option<i64>) {
        if let Some(mut change) = self.done.pop() {
            change.id = id;
            self.undone.push(change);
        }
    }

    /// Moves the top redo back to the undo side after its write went through.
    pub fn commit_redo(&mut self, id: Option<i64>) {
        if let Some(mut change) = self.undone.pop() {
            change.id = id;
            self.done.push(change);
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
            attendees: Vec::new(),
        }
    }

    /// The sequence `Ui::step_history` runs, with the GTK parts left out: read
    /// the row as it stands, ask for the reversing write, carry it out, and move
    /// the change to the other stack with the id the row has now.
    fn undo_once(store: &Store, history: &mut History) -> Result<(), Stale> {
        let change = history
            .peek_undo()
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

        assert_eq!(history.peek_undo(), Some(&change));
        assert!(history.peek_redo().is_none());
    }

    #[test]
    fn a_change_that_can_no_longer_be_applied_is_dropped_not_retried() {
        let mut history = History::default();
        history.record(Change::created(1, 7, draft("First", 9)));
        history.record(Change::created(1, 8, draft("Second", 10)));

        history.discard_undo();

        assert_eq!(
            history.peek_undo().and_then(|change| change.id),
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
            history.peek_undo().and_then(|change| change.id),
            Some((DEPTH + 9) as i64),
            "the newest change is still the one an undo reaches first"
        );
    }
}
