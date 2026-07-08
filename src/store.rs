use chrono::{DateTime, Local, NaiveDate, NaiveTime, TimeZone};
use rusqlite::{params, Connection};
use std::path::PathBuf;

/// Midnight of `date` in the local timezone, for turning a `NaiveDate`
/// range (as used by the calendar grids) into the `DateTime` range
/// `events_between` expects.
pub fn day_start(date: NaiveDate) -> DateTime<Local> {
    Local
        .from_local_datetime(&date.and_time(NaiveTime::MIN))
        .single()
        .expect("midnight is never DST-ambiguous for any real timezone")
}

#[derive(Clone)]
pub struct Event {
    pub id: i64,
    pub calendar_id: i64,
    pub title: String,
    pub start: DateTime<Local>,
    pub end: DateTime<Local>,
    pub all_day: bool,
    pub location: Option<String>,
    pub notes: Option<String>,
}

/// Fields for creating or updating an event; `id`/`calendar_id` are handled
/// separately since callers building this don't yet know or can't change
/// them.
pub struct EventDraft {
    pub title: String,
    pub start: DateTime<Local>,
    pub end: DateTime<Local>,
    pub all_day: bool,
    pub location: Option<String>,
    pub notes: Option<String>,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open() -> rusqlite::Result<Self> {
        let path = data_file_path();
        std::fs::create_dir_all(path.parent().expect("data file has a parent dir"))
            .expect("can create Calix data directory");
        Self::from_connection(Connection::open(path)?)
    }

    #[cfg(test)]
    fn open_in_memory() -> rusqlite::Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(conn: Connection) -> rusqlite::Result<Self> {
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS calendars (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                color TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS events (
                id INTEGER PRIMARY KEY,
                calendar_id INTEGER NOT NULL REFERENCES calendars(id),
                title TEXT NOT NULL,
                start_at TEXT NOT NULL,
                end_at TEXT NOT NULL,
                all_day INTEGER NOT NULL DEFAULT 0,
                location TEXT,
                notes TEXT
            );
            CREATE INDEX IF NOT EXISTS events_start_at ON events(start_at);
            ",
        )?;

        let store = Store { conn };
        store.ensure_default_calendar()?;
        Ok(store)
    }

    fn ensure_default_calendar(&self) -> rusqlite::Result<()> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM calendars", [], |row| row.get(0))?;
        if count == 0 {
            self.conn.execute(
                "INSERT INTO calendars (id, name, color) VALUES (1, 'Local', '#3584e4')",
                [],
            )?;
        }
        Ok(())
    }

    pub fn default_calendar_id(&self) -> i64 {
        1
    }

    /// Events whose [start, end) span overlaps the given half-open range.
    pub fn events_between(
        &self,
        range_start: DateTime<Local>,
        range_end: DateTime<Local>,
    ) -> rusqlite::Result<Vec<Event>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, calendar_id, title, start_at, end_at, all_day, location, notes
             FROM events
             WHERE start_at < ?1 AND end_at > ?2
             ORDER BY start_at",
        )?;
        let rows = stmt.query_map(
            params![range_end.to_rfc3339(), range_start.to_rfc3339()],
            row_to_event,
        )?;
        rows.collect()
    }

    pub fn create_event(&self, calendar_id: i64, draft: &EventDraft) -> rusqlite::Result<i64> {
        self.conn.execute(
            "INSERT INTO events (calendar_id, title, start_at, end_at, all_day, location, notes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                calendar_id,
                draft.title,
                draft.start.to_rfc3339(),
                draft.end.to_rfc3339(),
                draft.all_day as i64,
                draft.location,
                draft.notes,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn update_event(&self, id: i64, draft: &EventDraft) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE events SET title = ?1, start_at = ?2, end_at = ?3, all_day = ?4,
             location = ?5, notes = ?6 WHERE id = ?7",
            params![
                draft.title,
                draft.start.to_rfc3339(),
                draft.end.to_rfc3339(),
                draft.all_day as i64,
                draft.location,
                draft.notes,
                id,
            ],
        )?;
        Ok(())
    }

    pub fn delete_event(&self, id: i64) -> rusqlite::Result<()> {
        self.conn.execute("DELETE FROM events WHERE id = ?1", params![id])?;
        Ok(())
    }
}

fn row_to_event(row: &rusqlite::Row) -> rusqlite::Result<Event> {
    let start_at: String = row.get(3)?;
    let end_at: String = row.get(4)?;
    Ok(Event {
        id: row.get(0)?,
        calendar_id: row.get(1)?,
        title: row.get(2)?,
        start: parse_rfc3339(&start_at),
        end: parse_rfc3339(&end_at),
        all_day: row.get::<_, i64>(5)? != 0,
        location: row.get(6)?,
        notes: row.get(7)?,
    })
}

fn parse_rfc3339(s: &str) -> DateTime<Local> {
    DateTime::parse_from_rfc3339(s)
        .expect("dates stored by this app are always valid RFC3339")
        .with_timezone(&Local)
}

fn data_file_path() -> PathBuf {
    gtk::glib::user_data_dir().join("calix").join("calix.sqlite3")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn draft(title: &str, start: DateTime<Local>, end: DateTime<Local>) -> EventDraft {
        EventDraft {
            title: title.to_string(),
            start,
            end,
            all_day: false,
            location: None,
            notes: None,
        }
    }

    #[test]
    fn create_list_update_delete_roundtrip() {
        let store = Store::open_in_memory().unwrap();
        let start = Local::now();
        let end = start + Duration::hours(1);
        let calendar_id = store.default_calendar_id();

        let id = store.create_event(calendar_id, &draft("Test", start, end)).unwrap();
        let events = store.events_between(start - Duration::minutes(1), end + Duration::minutes(1)).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, id);
        assert_eq!(events[0].title, "Test");

        let mut updated = draft("Updated", start, end);
        updated.location = Some("Home".to_string());
        store.update_event(id, &updated).unwrap();
        let events = store.events_between(start - Duration::minutes(1), end + Duration::minutes(1)).unwrap();
        assert_eq!(events[0].title, "Updated");
        assert_eq!(events[0].location.as_deref(), Some("Home"));

        store.delete_event(id).unwrap();
        let events = store.events_between(start - Duration::minutes(1), end + Duration::minutes(1)).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn events_between_excludes_non_overlapping_ranges() {
        let store = Store::open_in_memory().unwrap();
        let start = Local::now();
        let end = start + Duration::hours(1);
        let calendar_id = store.default_calendar_id();

        store
            .create_event(
                calendar_id,
                &draft("Next week", start + Duration::days(7), end + Duration::days(7)),
            )
            .unwrap();

        let events = store.events_between(start, end).unwrap();
        assert!(events.is_empty());
    }
}
