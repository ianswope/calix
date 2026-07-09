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
    /// `Some` for events synced from Google — editing these locally isn't
    /// supported yet (a sync would just overwrite the edit), so the UI
    /// uses this to show them read-only.
    pub google_event_id: Option<String>,
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

        // SQLite has no `ADD COLUMN IF NOT EXISTS` — these two columns were
        // added after the tables above shipped, so existing databases need
        // an explicit existence check before altering.
        ensure_column(&conn, "calendars", "google_calendar_id", "TEXT")?;
        ensure_column(&conn, "events", "google_event_id", "TEXT")?;

        conn.execute_batch(
            "
            CREATE UNIQUE INDEX IF NOT EXISTS calendars_google_id
                ON calendars(google_calendar_id) WHERE google_calendar_id IS NOT NULL;
            CREATE UNIQUE INDEX IF NOT EXISTS events_google_id
                ON events(google_event_id) WHERE google_event_id IS NOT NULL;
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

    /// Creates a Google-sourced calendar if `google_calendar_id` hasn't
    /// been seen before, or updates its name/color if it has. Returns the
    /// local calendar id either way.
    pub fn upsert_google_calendar(
        &self,
        google_calendar_id: &str,
        name: &str,
        color: &str,
    ) -> rusqlite::Result<i64> {
        self.conn.query_row(
            "INSERT INTO calendars (name, color, google_calendar_id) VALUES (?1, ?2, ?3)
             ON CONFLICT(google_calendar_id) WHERE google_calendar_id IS NOT NULL
             DO UPDATE SET name = ?1, color = ?2
             RETURNING id",
            params![name, color, google_calendar_id],
            |row| row.get(0),
        )
    }

    /// Events whose [start, end) span overlaps the given half-open range.
    pub fn events_between(
        &self,
        range_start: DateTime<Local>,
        range_end: DateTime<Local>,
    ) -> rusqlite::Result<Vec<Event>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, calendar_id, title, start_at, end_at, all_day, location, notes, google_event_id
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

    /// Creates or updates a Google-sourced event by its Google event id.
    pub fn upsert_google_event(
        &self,
        calendar_id: i64,
        google_event_id: &str,
        draft: &EventDraft,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO events (calendar_id, title, start_at, end_at, all_day, location, notes, google_event_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(google_event_id) WHERE google_event_id IS NOT NULL
             DO UPDATE SET title = ?2, start_at = ?3, end_at = ?4, all_day = ?5, location = ?6, notes = ?7",
            params![
                calendar_id,
                draft.title,
                draft.start.to_rfc3339(),
                draft.end.to_rfc3339(),
                draft.all_day as i64,
                draft.location,
                draft.notes,
                google_event_id,
            ],
        )?;
        Ok(())
    }

    /// Removes previously-synced events for `calendar_id` that are no
    /// longer in `keep_google_ids` — i.e. deleted on Google's side since
    /// the last sync.
    pub fn prune_google_events(&self, calendar_id: i64, keep_google_ids: &[String]) -> rusqlite::Result<()> {
        let placeholders = keep_google_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "DELETE FROM events WHERE calendar_id = ? AND google_event_id IS NOT NULL
             AND google_event_id NOT IN ({placeholders})"
        );
        let mut params: Vec<&dyn rusqlite::ToSql> = vec![&calendar_id];
        params.extend(keep_google_ids.iter().map(|id| id as &dyn rusqlite::ToSql));
        self.conn.execute(&sql, params.as_slice())?;
        Ok(())
    }

    #[cfg(test)]
    fn calendar_row(&self, id: i64) -> rusqlite::Result<(String, String)> {
        self.conn
            .query_row("SELECT name, color FROM calendars WHERE id = ?1", params![id], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
    }
}

fn ensure_column(conn: &Connection, table: &str, column: &str, ddl_type: &str) -> rusqlite::Result<()> {
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
        params![table, column],
        |row| row.get(0),
    )?;
    if exists == 0 {
        conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {column} {ddl_type}"), [])?;
    }
    Ok(())
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
        google_event_id: row.get(8)?,
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

    #[test]
    fn upsert_google_calendar_is_idempotent_by_google_id() {
        let store = Store::open_in_memory().unwrap();
        let id1 = store.upsert_google_calendar("cal-abc", "Work", "#ff0000").unwrap();
        let id2 = store.upsert_google_calendar("cal-abc", "Work Renamed", "#00ff00").unwrap();
        assert_eq!(id1, id2);

        let (name, color) = store.calendar_row(id1).unwrap();
        assert_eq!(name, "Work Renamed");
        assert_eq!(color, "#00ff00");
    }

    #[test]
    fn upsert_google_event_updates_in_place_and_marks_google_source() {
        let store = Store::open_in_memory().unwrap();
        let calendar_id = store.upsert_google_calendar("cal-abc", "Work", "#ff0000").unwrap();
        let start = Local::now();
        let end = start + Duration::hours(1);

        store.upsert_google_event(calendar_id, "evt-1", &draft("Standup", start, end)).unwrap();
        store
            .upsert_google_event(calendar_id, "evt-1", &draft("Standup (moved)", start, end))
            .unwrap();

        let events = store.events_between(start - Duration::minutes(1), end + Duration::minutes(1)).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].title, "Standup (moved)");
        assert_eq!(events[0].google_event_id.as_deref(), Some("evt-1"));
    }

    #[test]
    fn prune_google_events_removes_only_stale_synced_ones() {
        let store = Store::open_in_memory().unwrap();
        let calendar_id = store.upsert_google_calendar("cal-abc", "Work", "#ff0000").unwrap();
        let start = Local::now();
        let end = start + Duration::hours(1);

        store.upsert_google_event(calendar_id, "keep", &draft("Keep", start, end)).unwrap();
        store.upsert_google_event(calendar_id, "gone", &draft("Gone", start, end)).unwrap();
        store.create_event(calendar_id, &draft("Local one", start, end)).unwrap();

        store.prune_google_events(calendar_id, &["keep".to_string()]).unwrap();

        let events = store.events_between(start - Duration::minutes(1), end + Duration::minutes(1)).unwrap();
        let titles: Vec<&str> = events.iter().map(|e| e.title.as_str()).collect();
        assert!(titles.contains(&"Keep"));
        assert!(titles.contains(&"Local one"));
        assert!(!titles.contains(&"Gone"));
    }

}
