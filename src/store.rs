use chrono::{DateTime, Local, NaiveDate, SecondsFormat, Utc};
use rusqlite::{Connection, params};
use std::path::PathBuf;

/// The first instant of `date` in the local timezone, for turning a
/// `NaiveDate` range (as used by the calendar grids) into the `DateTime` range
/// `events_between` expects. Delegates to [`crate::date_util::local_day_start`],
/// which has an explicit policy for civil dates whose midnight was skipped or
/// repeated by a DST transition instead of panicking.
pub fn day_start(date: NaiveDate) -> DateTime<Local> {
    crate::date_util::local_day_start(date)
}

#[derive(Clone)]
pub struct Event {
    pub id: i64,
    pub calendar_id: i64,
    pub calendar_name: String,
    pub calendar_color: String,
    pub account_provider: Option<String>,
    pub account_provider_id: Option<String>,
    pub account_token_key: Option<String>,
    pub google_calendar_id: Option<String>,
    pub title: String,
    pub start: DateTime<Local>,
    pub end: DateTime<Local>,
    pub all_day: bool,
    pub location: Option<String>,
    pub notes: Option<String>,
    pub google_event_id: Option<String>,
    pub icloud_event_id: Option<String>,
    /// CalDAV server base URL for a generic `caldav` event's account; `None`
    /// for google, icloud, and local events.
    pub account_server_url: Option<String>,
}

/// Fields for creating or updating an event; `id`/`calendar_id` are handled
/// separately since callers building this don't yet know or can't change
/// them.
#[derive(Clone)]
pub struct EventDraft {
    pub title: String,
    pub start: DateTime<Local>,
    pub end: DateTime<Local>,
    pub all_day: bool,
    pub location: Option<String>,
    pub notes: Option<String>,
}

#[derive(Clone)]
pub struct Account {
    pub id: i64,
    pub provider_account_id: String,
    pub display_name: String,
    pub token_key: String,
    /// CalDAV server base URL for generic `caldav` accounts; `None` for
    /// google and icloud.
    pub server_url: Option<String>,
}

#[derive(Clone)]
pub struct Calendar {
    pub id: i64,
    pub name: String,
    pub color: String,
    pub visible: bool,
    pub google_calendar_id: Option<String>,
    pub icloud_calendar_id: Option<String>,
}

#[derive(Clone)]
pub struct CalendarConnection {
    pub id: i64,
    pub name: String,
    pub provider: Option<String>,
    pub provider_account_id: Option<String>,
    pub token_key: Option<String>,
    pub google_calendar_id: Option<String>,
    pub icloud_calendar_id: Option<String>,
    pub visible: bool,
    /// CalDAV server base URL for generic `caldav` calendars; `None` otherwise.
    pub server_url: Option<String>,
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
        // The UI thread and each background sync worker hold their own
        // connection; WAL plus a busy timeout lets them overlap instead of
        // failing immediately with `database is locked`.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS calendars (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                color TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS accounts (
                id INTEGER PRIMARY KEY,
                provider TEXT NOT NULL,
                provider_account_id TEXT NOT NULL,
                display_name TEXT NOT NULL,
                token_key TEXT NOT NULL
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
            CREATE TABLE IF NOT EXISTS app_settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS events_start_at ON events(start_at);
            ",
        )?;

        // SQLite has no `ADD COLUMN IF NOT EXISTS` — these two columns were
        // added after the tables above shipped, so existing databases need
        // an explicit existence check before altering.
        ensure_column(
            &conn,
            "calendars",
            "account_id",
            "INTEGER REFERENCES accounts(id)",
        )?;
        ensure_column(&conn, "calendars", "google_calendar_id", "TEXT")?;
        ensure_column(&conn, "calendars", "visible", "INTEGER NOT NULL DEFAULT 1")?;
        ensure_column(&conn, "events", "google_event_id", "TEXT")?;
        // The `icloud_*` columns hold CalDAV hrefs. iCloud was the first
        // CalDAV provider, so they kept its name; generic CalDAV accounts
        // (provider = 'caldav') store their calendar/event hrefs in the same
        // columns and are told apart by `accounts.provider`.
        ensure_column(&conn, "calendars", "icloud_calendar_id", "TEXT")?;
        ensure_column(&conn, "events", "icloud_event_id", "TEXT")?;
        // Base URL for a generic CalDAV account's server; NULL for google and
        // icloud (iCloud uses a fixed well-known root).
        ensure_column(&conn, "accounts", "server_url", "TEXT")?;

        conn.execute_batch(
            "
            -- Account identity includes the server so the same username on
            -- two different CalDAV servers is two accounts, not one. google
            -- and icloud leave server_url NULL, and COALESCE('') keeps their
            -- identity effectively (provider, provider_account_id) as before.
            DROP INDEX IF EXISTS accounts_provider_remote_id;
            CREATE UNIQUE INDEX IF NOT EXISTS accounts_identity
                ON accounts(provider, provider_account_id, COALESCE(server_url, ''));
            CREATE UNIQUE INDEX IF NOT EXISTS accounts_token_key
                ON accounts(token_key);
            DROP INDEX IF EXISTS calendars_google_id;
            CREATE UNIQUE INDEX IF NOT EXISTS calendars_google_account_id
                ON calendars(account_id, google_calendar_id)
                WHERE account_id IS NOT NULL AND google_calendar_id IS NOT NULL;
            DROP INDEX IF EXISTS events_google_id;
            CREATE UNIQUE INDEX IF NOT EXISTS events_google_calendar_id
                ON events(calendar_id, google_event_id) WHERE google_event_id IS NOT NULL;
            CREATE UNIQUE INDEX IF NOT EXISTS calendars_icloud_account_id
                ON calendars(account_id, icloud_calendar_id)
                WHERE account_id IS NOT NULL AND icloud_calendar_id IS NOT NULL;
            CREATE UNIQUE INDEX IF NOT EXISTS events_icloud_calendar_id
                ON events(calendar_id, icloud_event_id) WHERE icloud_event_id IS NOT NULL;
            ",
        )?;

        // Rows written before schema version 1 stored local-offset RFC3339
        // text; rewrite them once so TEXT comparisons are chronological.
        let schema_version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if schema_version < 1 {
            normalize_event_timestamps(&conn)?;
            conn.pragma_update(None, "user_version", 1)?;
        }

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

    #[cfg(test)]
    pub fn default_calendar_id(&self) -> i64 {
        1
    }

    pub fn setting(&self, key: &str) -> rusqlite::Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT value FROM app_settings WHERE key = ?1")?;
        let mut rows = stmt.query(params![key])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    pub fn set_setting(&self, key: &str, value: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO app_settings (key, value)
             VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = ?2",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn local_calendars(&self) -> rusqlite::Result<Vec<Calendar>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, color, visible, google_calendar_id, icloud_calendar_id
             FROM calendars
             WHERE account_id IS NULL
             ORDER BY name",
        )?;
        let rows = stmt.query_map([], row_to_calendar)?;
        rows.collect()
    }

    pub fn calendars_for_account(&self, account_id: i64) -> rusqlite::Result<Vec<Calendar>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, color, visible, google_calendar_id, icloud_calendar_id
             FROM calendars
             WHERE account_id = ?1
             ORDER BY name",
        )?;
        let rows = stmt.query_map(params![account_id], row_to_calendar)?;
        rows.collect()
    }

    pub fn calendar_connections(&self) -> rusqlite::Result<Vec<CalendarConnection>> {
        let mut stmt = self.conn.prepare(
            "SELECT calendars.id, calendars.name, accounts.provider,
                    accounts.provider_account_id, accounts.token_key,
                    calendars.google_calendar_id, calendars.icloud_calendar_id,
                    calendars.visible, accounts.server_url
             FROM calendars
             LEFT JOIN accounts ON accounts.id = calendars.account_id
             ORDER BY accounts.provider IS NOT NULL, accounts.display_name, calendars.name",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(CalendarConnection {
                id: row.get(0)?,
                name: row.get(1)?,
                provider: row.get(2)?,
                provider_account_id: row.get(3)?,
                token_key: row.get(4)?,
                google_calendar_id: row.get(5)?,
                icloud_calendar_id: row.get(6)?,
                visible: row.get::<_, i64>(7)? != 0,
                server_url: row.get(8)?,
            })
        })?;
        rows.collect()
    }

    pub fn set_calendar_visible(&self, calendar_id: i64, visible: bool) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE calendars SET visible = ?1 WHERE id = ?2",
            params![visible as i64, calendar_id],
        )?;
        Ok(())
    }

    pub fn google_accounts(&self) -> rusqlite::Result<Vec<Account>> {
        self.accounts_for_provider("google")
    }

    pub fn icloud_accounts(&self) -> rusqlite::Result<Vec<Account>> {
        self.accounts_for_provider("icloud")
    }

    pub fn caldav_accounts(&self) -> rusqlite::Result<Vec<Account>> {
        self.accounts_for_provider("caldav")
    }

    fn accounts_for_provider(&self, provider: &str) -> rusqlite::Result<Vec<Account>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, provider_account_id, display_name, token_key, server_url
             FROM accounts
             WHERE provider = ?1
             ORDER BY display_name",
        )?;
        let rows = stmt.query_map(params![provider], |row| {
            Ok(Account {
                id: row.get(0)?,
                provider_account_id: row.get(1)?,
                display_name: row.get(2)?,
                token_key: row.get(3)?,
                server_url: row.get(4)?,
            })
        })?;
        rows.collect()
    }

    /// Creates or updates a Google account row. `token_key` names the
    /// keyring entry holding the refresh token; the token itself stays out
    /// of SQLite.
    pub fn upsert_google_account(
        &self,
        provider_account_id: &str,
        display_name: &str,
        token_key: &str,
    ) -> rusqlite::Result<i64> {
        self.conn.query_row(
            "INSERT INTO accounts (provider, provider_account_id, display_name, token_key)
             VALUES ('google', ?1, ?2, ?3)
             ON CONFLICT(provider, provider_account_id, COALESCE(server_url, ''))
             DO UPDATE SET display_name = ?2, token_key = ?3
             RETURNING id",
            params![provider_account_id, display_name, token_key],
            |row| row.get(0),
        )
    }

    pub fn upsert_icloud_account(
        &self,
        apple_id: &str,
        display_name: &str,
        token_key: &str,
    ) -> rusqlite::Result<i64> {
        self.conn.query_row(
            "INSERT INTO accounts (provider, provider_account_id, display_name, token_key)
             VALUES ('icloud', ?1, ?2, ?3)
             ON CONFLICT(provider, provider_account_id, COALESCE(server_url, ''))
             DO UPDATE SET display_name = ?2, token_key = ?3
             RETURNING id",
            params![apple_id, display_name, token_key],
            |row| row.get(0),
        )
    }

    /// Creates or updates a generic CalDAV account, keyed on the
    /// `(username, server_url)` pair: reconnecting the same login to the same
    /// server updates that row, while the same username on a different server
    /// is a distinct account.
    pub fn upsert_caldav_account(
        &self,
        username: &str,
        server_url: &str,
        display_name: &str,
        token_key: &str,
    ) -> rusqlite::Result<i64> {
        self.conn.query_row(
            "INSERT INTO accounts (provider, provider_account_id, display_name, token_key, server_url)
             VALUES ('caldav', ?1, ?2, ?3, ?4)
             ON CONFLICT(provider, provider_account_id, COALESCE(server_url, ''))
             DO UPDATE SET display_name = ?2, token_key = ?3, server_url = ?4
             RETURNING id",
            params![username, display_name, token_key, server_url],
            |row| row.get(0),
        )
    }

    /// Creates a Google-sourced calendar if `google_calendar_id` hasn't
    /// been seen before for `account_id`, or updates its name/color if it
    /// has. Returns the local calendar id either way.
    pub fn upsert_google_calendar(
        &self,
        account_id: i64,
        google_calendar_id: &str,
        name: &str,
        color: &str,
        visible: bool,
    ) -> rusqlite::Result<i64> {
        self.conn.execute(
            "UPDATE calendars
             SET account_id = ?1, name = ?3, color = ?4
             WHERE account_id IS NULL AND google_calendar_id = ?2",
            params![account_id, google_calendar_id, name, color],
        )?;

        self.conn.query_row(
            "INSERT INTO calendars (account_id, name, color, google_calendar_id, visible)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(account_id, google_calendar_id)
             WHERE account_id IS NOT NULL AND google_calendar_id IS NOT NULL
             DO UPDATE SET name = ?2, color = ?3
             RETURNING id",
            params![account_id, name, color, google_calendar_id, visible as i64],
            |row| row.get(0),
        )
    }

    /// Upserts a CalDAV-sourced calendar (iCloud or generic `caldav`). The
    /// href is stored in the `icloud_calendar_id` column (see the schema
    /// note); the account's provider distinguishes the source.
    pub fn upsert_caldav_calendar(
        &self,
        account_id: i64,
        icloud_calendar_id: &str,
        name: &str,
        color: &str,
        visible: bool,
    ) -> rusqlite::Result<i64> {
        self.conn.query_row(
            "INSERT INTO calendars (account_id, name, color, icloud_calendar_id, visible)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(account_id, icloud_calendar_id)
             WHERE account_id IS NOT NULL AND icloud_calendar_id IS NOT NULL
             DO UPDATE SET name = ?2, color = ?3
             RETURNING id",
            params![account_id, name, color, icloud_calendar_id, visible as i64],
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
            "SELECT events.id, events.calendar_id, calendars.name, calendars.color,
                    accounts.provider, accounts.provider_account_id, accounts.token_key,
                    calendars.google_calendar_id,
                    events.title, events.start_at,
                    events.end_at, events.all_day, events.location, events.notes,
                    events.google_event_id, events.icloud_event_id,
                    accounts.server_url
             FROM events
             JOIN calendars ON calendars.id = events.calendar_id
             LEFT JOIN accounts ON accounts.id = calendars.account_id
             WHERE events.start_at < ?1 AND events.end_at > ?2
             AND calendars.visible != 0
             ORDER BY events.start_at",
        )?;
        let rows = stmt.query_map(
            params![stored_timestamp(&range_end), stored_timestamp(&range_start)],
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
                stored_timestamp(&draft.start),
                stored_timestamp(&draft.end),
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
                stored_timestamp(&draft.start),
                stored_timestamp(&draft.end),
                draft.all_day as i64,
                draft.location,
                draft.notes,
                id,
            ],
        )?;
        Ok(())
    }

    pub fn delete_event(&self, id: i64) -> rusqlite::Result<()> {
        self.conn
            .execute("DELETE FROM events WHERE id = ?1", params![id])?;
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
             ON CONFLICT(calendar_id, google_event_id) WHERE google_event_id IS NOT NULL
             DO UPDATE SET title = ?2, start_at = ?3, end_at = ?4, all_day = ?5, location = ?6, notes = ?7",
            params![
                calendar_id,
                draft.title,
                stored_timestamp(&draft.start),
                stored_timestamp(&draft.end),
                draft.all_day as i64,
                draft.location,
                draft.notes,
                google_event_id,
            ],
        )?;
        Ok(())
    }

    pub fn upsert_caldav_event(
        &self,
        calendar_id: i64,
        icloud_event_id: &str,
        draft: &EventDraft,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO events (calendar_id, title, start_at, end_at, all_day, location, notes, icloud_event_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(calendar_id, icloud_event_id) WHERE icloud_event_id IS NOT NULL
             DO UPDATE SET title = ?2, start_at = ?3, end_at = ?4, all_day = ?5, location = ?6, notes = ?7",
            params![
                calendar_id,
                draft.title,
                stored_timestamp(&draft.start),
                stored_timestamp(&draft.end),
                draft.all_day as i64,
                draft.location,
                draft.notes,
                icloud_event_id,
            ],
        )?;
        Ok(())
    }

    /// Removes previously-synced events for `calendar_id` that are no
    /// longer in `keep_google_ids` — i.e. deleted on Google's side since
    /// the last sync.
    pub fn prune_google_events(
        &self,
        calendar_id: i64,
        keep_google_ids: &[String],
        range_start: DateTime<Local>,
        range_end: DateTime<Local>,
    ) -> rusqlite::Result<()> {
        if keep_google_ids.is_empty() {
            self.conn.execute(
                "DELETE FROM events
                 WHERE calendar_id = ?1 AND google_event_id IS NOT NULL
                   AND start_at < ?2 AND end_at > ?3",
                params![
                    calendar_id,
                    stored_timestamp(&range_end),
                    stored_timestamp(&range_start)
                ],
            )?;
            return Ok(());
        }

        let placeholders = keep_google_ids
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "DELETE FROM events
             WHERE calendar_id = ? AND google_event_id IS NOT NULL
               AND start_at < ? AND end_at > ?
               AND google_event_id NOT IN ({placeholders})"
        );
        let range_end = stored_timestamp(&range_end);
        let range_start = stored_timestamp(&range_start);
        let mut params: Vec<&dyn rusqlite::ToSql> = vec![&calendar_id, &range_end, &range_start];
        params.extend(keep_google_ids.iter().map(|id| id as &dyn rusqlite::ToSql));
        self.conn.execute(&sql, params.as_slice())?;
        Ok(())
    }

    pub fn prune_caldav_events(
        &self,
        calendar_id: i64,
        keep_icloud_ids: &[String],
        range_start: DateTime<Local>,
        range_end: DateTime<Local>,
    ) -> rusqlite::Result<()> {
        if keep_icloud_ids.is_empty() {
            self.conn.execute(
                "DELETE FROM events
                 WHERE calendar_id = ?1 AND icloud_event_id IS NOT NULL
                   AND start_at < ?2 AND end_at > ?3",
                params![
                    calendar_id,
                    stored_timestamp(&range_end),
                    stored_timestamp(&range_start)
                ],
            )?;
            return Ok(());
        }

        let placeholders = keep_icloud_ids
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "DELETE FROM events
             WHERE calendar_id = ? AND icloud_event_id IS NOT NULL
               AND start_at < ? AND end_at > ?
               AND icloud_event_id NOT IN ({placeholders})"
        );
        let range_end = stored_timestamp(&range_end);
        let range_start = stored_timestamp(&range_start);
        let mut params: Vec<&dyn rusqlite::ToSql> = vec![&calendar_id, &range_end, &range_start];
        params.extend(keep_icloud_ids.iter().map(|id| id as &dyn rusqlite::ToSql));
        self.conn.execute(&sql, params.as_slice())?;
        Ok(())
    }

    pub fn prune_caldav_calendars(
        &self,
        account_id: i64,
        keep_icloud_ids: &[String],
    ) -> rusqlite::Result<()> {
        if keep_icloud_ids.is_empty() {
            self.conn.execute(
                "DELETE FROM events
                 WHERE calendar_id IN (
                     SELECT id FROM calendars
                     WHERE account_id = ?1 AND icloud_calendar_id IS NOT NULL
                 )",
                params![account_id],
            )?;
            self.conn.execute(
                "DELETE FROM calendars
                 WHERE account_id = ?1 AND icloud_calendar_id IS NOT NULL",
                params![account_id],
            )?;
            return Ok(());
        }

        let placeholders = keep_icloud_ids
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let event_sql = format!(
            "DELETE FROM events
             WHERE calendar_id IN (
                 SELECT id FROM calendars
                 WHERE account_id = ? AND icloud_calendar_id IS NOT NULL
                   AND icloud_calendar_id NOT IN ({placeholders})
             )"
        );
        let mut event_params: Vec<&dyn rusqlite::ToSql> = vec![&account_id];
        event_params.extend(keep_icloud_ids.iter().map(|id| id as &dyn rusqlite::ToSql));
        self.conn.execute(&event_sql, event_params.as_slice())?;

        let calendar_sql = format!(
            "DELETE FROM calendars
             WHERE account_id = ? AND icloud_calendar_id IS NOT NULL
               AND icloud_calendar_id NOT IN ({placeholders})"
        );
        let mut calendar_params: Vec<&dyn rusqlite::ToSql> = vec![&account_id];
        calendar_params.extend(keep_icloud_ids.iter().map(|id| id as &dyn rusqlite::ToSql));
        self.conn
            .execute(&calendar_sql, calendar_params.as_slice())?;
        Ok(())
    }

    pub fn prune_google_calendars(
        &self,
        account_id: i64,
        keep_google_ids: &[String],
    ) -> rusqlite::Result<()> {
        if keep_google_ids.is_empty() {
            self.conn.execute(
                "DELETE FROM events
                 WHERE calendar_id IN (
                     SELECT id FROM calendars
                     WHERE account_id = ?1 AND google_calendar_id IS NOT NULL
                 )",
                params![account_id],
            )?;
            self.conn.execute(
                "DELETE FROM calendars
                 WHERE account_id = ?1 AND google_calendar_id IS NOT NULL",
                params![account_id],
            )?;
            return Ok(());
        }

        let placeholders = keep_google_ids
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let event_sql = format!(
            "DELETE FROM events
             WHERE calendar_id IN (
                 SELECT id FROM calendars
                 WHERE account_id = ? AND google_calendar_id IS NOT NULL
                   AND google_calendar_id NOT IN ({placeholders})
             )"
        );
        let mut event_params: Vec<&dyn rusqlite::ToSql> = vec![&account_id];
        event_params.extend(keep_google_ids.iter().map(|id| id as &dyn rusqlite::ToSql));
        self.conn.execute(&event_sql, event_params.as_slice())?;

        let calendar_sql = format!(
            "DELETE FROM calendars
             WHERE account_id = ? AND google_calendar_id IS NOT NULL
               AND google_calendar_id NOT IN ({placeholders})"
        );
        let mut calendar_params: Vec<&dyn rusqlite::ToSql> = vec![&account_id];
        calendar_params.extend(keep_google_ids.iter().map(|id| id as &dyn rusqlite::ToSql));
        self.conn
            .execute(&calendar_sql, calendar_params.as_slice())?;
        Ok(())
    }

    #[cfg(test)]
    fn calendar_row(&self, id: i64) -> rusqlite::Result<(String, String)> {
        self.conn.query_row(
            "SELECT name, color FROM calendars WHERE id = ?1",
            params![id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
    }
}

fn ensure_column(
    conn: &Connection,
    table: &str,
    column: &str,
    ddl_type: &str,
) -> rusqlite::Result<()> {
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
        params![table, column],
        |row| row.get(0),
    )?;
    if exists == 0 {
        conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {ddl_type}"),
            [],
        )?;
    }
    Ok(())
}

fn row_to_event(row: &rusqlite::Row) -> rusqlite::Result<Event> {
    let start_at: String = row.get(9)?;
    let end_at: String = row.get(10)?;
    Ok(Event {
        id: row.get(0)?,
        calendar_id: row.get(1)?,
        calendar_name: row.get(2)?,
        calendar_color: row.get(3)?,
        account_provider: row.get(4)?,
        account_provider_id: row.get(5)?,
        account_token_key: row.get(6)?,
        google_calendar_id: row.get(7)?,
        title: row.get(8)?,
        start: parse_rfc3339(&start_at),
        end: parse_rfc3339(&end_at),
        all_day: row.get::<_, i64>(11)? != 0,
        location: row.get(12)?,
        notes: row.get(13)?,
        google_event_id: row.get(14)?,
        icloud_event_id: row.get(15)?,
        account_server_url: row.get(16)?,
    })
}

fn row_to_calendar(row: &rusqlite::Row) -> rusqlite::Result<Calendar> {
    Ok(Calendar {
        id: row.get(0)?,
        name: row.get(1)?,
        color: row.get(2)?,
        visible: row.get::<_, i64>(3)? != 0,
        google_calendar_id: row.get(4)?,
        icloud_calendar_id: row.get(5)?,
    })
}

fn parse_rfc3339(s: &str) -> DateTime<Local> {
    DateTime::parse_from_rfc3339(s)
        .expect("dates stored by this app are always valid RFC3339")
        .with_timezone(&Local)
}

/// Serializes an instant for storage: UTC, whole seconds, `Z` suffix. The
/// SQL above compares `start_at`/`end_at` as TEXT (range queries, pruning,
/// ORDER BY), which is only chronological if every stored value shares one
/// offset and precision — local-offset RFC3339 text does not sort by instant.
fn stored_timestamp(instant: &DateTime<Local>) -> String {
    instant
        .with_timezone(&Utc)
        .to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Rewrites rows stored by earlier versions as local-offset RFC3339 text
/// (`2026-11-01T01:30:00-04:00`) into the form `stored_timestamp` writes, so
/// TEXT comparisons stay chronological across the whole table.
fn normalize_event_timestamps(conn: &Connection) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare("SELECT id, start_at, end_at FROM events")?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, start, end) in rows {
        let normalized_start = normalized_timestamp(&start);
        let normalized_end = normalized_timestamp(&end);
        if normalized_start.is_some() || normalized_end.is_some() {
            conn.execute(
                "UPDATE events SET start_at = ?1, end_at = ?2 WHERE id = ?3",
                params![
                    normalized_start.unwrap_or(start),
                    normalized_end.unwrap_or(end),
                    id
                ],
            )?;
        }
    }
    Ok(())
}

/// `Some(normalized)` when `value` parses and isn't already in stored form;
/// `None` leaves unparseable or already-normalized text untouched.
fn normalized_timestamp(value: &str) -> Option<String> {
    let normalized = DateTime::parse_from_rfc3339(value)
        .ok()?
        .with_timezone(&Utc)
        .to_rfc3339_opts(SecondsFormat::Secs, true);
    (normalized != value).then_some(normalized)
}

fn data_file_path() -> PathBuf {
    gtk::glib::user_data_dir()
        .join("calix")
        .join("calix.sqlite3")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

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
    fn events_are_stored_as_utc_z_timestamps() {
        let store = Store::open_in_memory().unwrap();
        let start = Local
            .with_ymd_and_hms(2026, 7, 6, 9, 0, 0)
            .single()
            .unwrap();
        let id = store
            .create_event(
                store.default_calendar_id(),
                &draft("Meeting", start, start + Duration::hours(1)),
            )
            .unwrap();

        let (start_at, end_at): (String, String) = store
            .conn
            .query_row(
                "SELECT start_at, end_at FROM events WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();

        assert_eq!(start_at, stored_timestamp(&start));
        assert!(start_at.ends_with('Z'));
        assert!(end_at.ends_with('Z'));

        // Round-trips back to the same instant through events_between.
        let events = store
            .events_between(start - Duration::minutes(1), start + Duration::hours(2))
            .unwrap();
        assert_eq!(events[0].start, start);
    }

    #[test]
    fn migration_normalizes_legacy_local_offset_timestamps() {
        let store = Store::open_in_memory().unwrap();
        store
            .conn
            .execute(
                "INSERT INTO events (calendar_id, title, start_at, end_at)
                 VALUES (?1, 'Legacy', '2026-11-01T01:30:00-04:00', '2026-11-01T02:30:00-05:00')",
                params![store.default_calendar_id()],
            )
            .unwrap();

        normalize_event_timestamps(&store.conn).unwrap();

        let (start_at, end_at): (String, String) = store
            .conn
            .query_row(
                "SELECT start_at, end_at FROM events WHERE title = 'Legacy'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(start_at, "2026-11-01T05:30:00Z");
        assert_eq!(end_at, "2026-11-01T07:30:00Z");
    }

    #[test]
    fn normalized_timestamp_leaves_normalized_and_invalid_text_alone() {
        assert_eq!(normalized_timestamp("2026-11-01T05:30:00Z"), None);
        assert_eq!(normalized_timestamp("not a timestamp"), None);
        assert_eq!(
            normalized_timestamp("2026-11-01T01:30:00-04:00").as_deref(),
            Some("2026-11-01T05:30:00Z")
        );
    }

    #[test]
    fn create_list_update_delete_roundtrip() {
        let store = Store::open_in_memory().unwrap();
        let start = Local::now();
        let end = start + Duration::hours(1);
        let calendar_id = store.default_calendar_id();

        let id = store
            .create_event(calendar_id, &draft("Test", start, end))
            .unwrap();
        let events = store
            .events_between(start - Duration::minutes(1), end + Duration::minutes(1))
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, id);
        assert_eq!(events[0].title, "Test");
        assert_eq!(events[0].calendar_name, "Local");
        assert_eq!(events[0].calendar_color, "#3584e4");
        assert_eq!(events[0].account_provider, None);

        let mut updated = draft("Updated", start, end);
        updated.location = Some("Home".to_string());
        store.update_event(id, &updated).unwrap();
        let events = store
            .events_between(start - Duration::minutes(1), end + Duration::minutes(1))
            .unwrap();
        assert_eq!(events[0].title, "Updated");
        assert_eq!(events[0].location.as_deref(), Some("Home"));

        store.delete_event(id).unwrap();
        let events = store
            .events_between(start - Duration::minutes(1), end + Duration::minutes(1))
            .unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn app_settings_roundtrip() {
        let store = Store::open_in_memory().unwrap();

        assert_eq!(store.setting("view_mode").unwrap(), None);
        store.set_setting("view_mode", "week").unwrap();
        assert_eq!(store.setting("view_mode").unwrap().as_deref(), Some("week"));
        store.set_setting("view_mode", "day").unwrap();
        assert_eq!(store.setting("view_mode").unwrap().as_deref(), Some("day"));
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
                &draft(
                    "Next week",
                    start + Duration::days(7),
                    end + Duration::days(7),
                ),
            )
            .unwrap();

        let events = store.events_between(start, end).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn events_between_excludes_hidden_calendars() {
        let store = Store::open_in_memory().unwrap();
        let start = Local::now();
        let end = start + Duration::hours(1);
        let calendar_id = store.default_calendar_id();

        store
            .create_event(calendar_id, &draft("Hidden", start, end))
            .unwrap();
        store.set_calendar_visible(calendar_id, false).unwrap();

        let events = store
            .events_between(start - Duration::minutes(1), end + Duration::minutes(1))
            .unwrap();
        assert!(events.is_empty());

        store.set_calendar_visible(calendar_id, true).unwrap();
        let events = store
            .events_between(start - Duration::minutes(1), end + Duration::minutes(1))
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].title, "Hidden");
    }

    #[test]
    fn list_calendars_returns_visibility_and_source_ids() {
        let store = Store::open_in_memory().unwrap();
        let account_id = store
            .upsert_google_account(
                "person@example.com",
                "person@example.com",
                "google-refresh-token:person@example.com",
            )
            .unwrap();
        let google_calendar_id = store
            .upsert_google_calendar(account_id, "cal-abc", "Work", "#ff0000", true)
            .unwrap();
        store
            .set_calendar_visible(google_calendar_id, false)
            .unwrap();

        let local = store.local_calendars().unwrap();
        let remote = store.calendars_for_account(account_id).unwrap();

        assert_eq!(local.len(), 1);
        assert!(local[0].visible);
        assert_eq!(remote.len(), 1);
        assert!(!remote[0].visible);
        assert_eq!(remote[0].google_calendar_id.as_deref(), Some("cal-abc"));
    }

    #[test]
    fn upsert_google_calendar_is_idempotent_by_google_id() {
        let store = Store::open_in_memory().unwrap();
        let account_id = store
            .upsert_google_account(
                "person@example.com",
                "person@example.com",
                "google-refresh-token:person@example.com",
            )
            .unwrap();
        let id1 = store
            .upsert_google_calendar(account_id, "cal-abc", "Work", "#ff0000", true)
            .unwrap();
        let id2 = store
            .upsert_google_calendar(account_id, "cal-abc", "Work Renamed", "#00ff00", true)
            .unwrap();
        assert_eq!(id1, id2);

        let (name, color) = store.calendar_row(id1).unwrap();
        assert_eq!(name, "Work Renamed");
        assert_eq!(color, "#00ff00");
    }

    #[test]
    fn upsert_google_calendar_sets_initial_visibility_but_preserves_user_choice() {
        let store = Store::open_in_memory().unwrap();
        let account_id = store
            .upsert_google_account(
                "person@example.com",
                "person@example.com",
                "google-refresh-token:person@example.com",
            )
            .unwrap();

        let calendar_id = store
            .upsert_google_calendar(account_id, "cal-abc", "Work", "#ff0000", false)
            .unwrap();
        assert!(!store.calendars_for_account(account_id).unwrap()[0].visible);

        store.set_calendar_visible(calendar_id, true).unwrap();
        store
            .upsert_google_calendar(account_id, "cal-abc", "Work Renamed", "#00ff00", false)
            .unwrap();

        let calendar = store.calendars_for_account(account_id).unwrap().remove(0);
        assert!(calendar.visible);
        assert_eq!(calendar.name, "Work Renamed");
        assert_eq!(calendar.color, "#00ff00");
    }

    #[test]
    fn upsert_google_calendar_is_scoped_to_account() {
        let store = Store::open_in_memory().unwrap();
        let first_account_id = store
            .upsert_google_account("first@example.com", "First", "google-refresh-token:first")
            .unwrap();
        let second_account_id = store
            .upsert_google_account(
                "second@example.com",
                "Second",
                "google-refresh-token:second",
            )
            .unwrap();

        let first_calendar_id = store
            .upsert_google_calendar(
                first_account_id,
                "primary",
                "First primary",
                "#ff0000",
                true,
            )
            .unwrap();
        let second_calendar_id = store
            .upsert_google_calendar(
                second_account_id,
                "primary",
                "Second primary",
                "#00ff00",
                true,
            )
            .unwrap();

        assert_ne!(first_calendar_id, second_calendar_id);
    }

    #[test]
    fn upsert_google_calendar_claims_legacy_unscoped_calendar() {
        let store = Store::open_in_memory().unwrap();
        store
            .conn
            .execute(
                "INSERT INTO calendars (name, color, google_calendar_id)
                 VALUES ('Legacy Work', '#000000', 'cal-abc')",
                [],
            )
            .unwrap();
        let account_id = store
            .upsert_google_account(
                "person@example.com",
                "person@example.com",
                "google-refresh-token:person@example.com",
            )
            .unwrap();

        let calendar_id = store
            .upsert_google_calendar(account_id, "cal-abc", "Work", "#ff0000", true)
            .unwrap();

        let calendars: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM calendars WHERE google_calendar_id = 'cal-abc'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let (name, color) = store.calendar_row(calendar_id).unwrap();
        assert_eq!(calendars, 1);
        assert_eq!(name, "Work");
        assert_eq!(color, "#ff0000");
    }

    #[test]
    fn upsert_google_event_updates_in_place_and_marks_google_source() {
        let store = Store::open_in_memory().unwrap();
        let account_id = store
            .upsert_google_account(
                "person@example.com",
                "person@example.com",
                "google-refresh-token:person@example.com",
            )
            .unwrap();
        let calendar_id = store
            .upsert_google_calendar(account_id, "cal-abc", "Work", "#ff0000", true)
            .unwrap();
        let start = Local::now();
        let end = start + Duration::hours(1);

        store
            .upsert_google_event(calendar_id, "evt-1", &draft("Standup", start, end))
            .unwrap();
        store
            .upsert_google_event(calendar_id, "evt-1", &draft("Standup (moved)", start, end))
            .unwrap();

        let events = store
            .events_between(start - Duration::minutes(1), end + Duration::minutes(1))
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].title, "Standup (moved)");
        assert_eq!(events[0].calendar_name, "Work");
        assert_eq!(events[0].account_provider.as_deref(), Some("google"));
        assert_eq!(events[0].google_event_id.as_deref(), Some("evt-1"));
    }

    #[test]
    fn upsert_caldav_event_updates_in_place_and_marks_caldav_source() {
        let store = Store::open_in_memory().unwrap();
        let account_id = store
            .upsert_icloud_account(
                "person@example.com",
                "person@example.com",
                "icloud-app-password:person@example.com",
            )
            .unwrap();
        let calendar_id = store
            .upsert_caldav_calendar(account_id, "/calendars/work/", "Work", "#ff9500", true)
            .unwrap();
        let start = Local::now();
        let end = start + Duration::hours(1);

        store
            .upsert_caldav_event(
                calendar_id,
                "/calendars/work/evt-1.ics",
                &draft("Lunch", start, end),
            )
            .unwrap();
        store
            .upsert_caldav_event(
                calendar_id,
                "/calendars/work/evt-1.ics",
                &draft("Lunch (moved)", start, end),
            )
            .unwrap();

        let events = store
            .events_between(start - Duration::minutes(1), end + Duration::minutes(1))
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].title, "Lunch (moved)");
        assert_eq!(
            events[0].icloud_event_id.as_deref(),
            Some("/calendars/work/evt-1.ics")
        );
    }

    #[test]
    fn caldav_account_stores_server_url_and_surfaces_it_on_events() {
        let store = Store::open_in_memory().unwrap();
        let account_id = store
            .upsert_caldav_account(
                "me",
                "https://caldav.fastmail.com/",
                "me (caldav.fastmail.com)",
                "caldav-password:https://caldav.fastmail.com|me",
            )
            .unwrap();

        let accounts = store.caldav_accounts().unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(
            accounts[0].server_url.as_deref(),
            Some("https://caldav.fastmail.com/")
        );
        // A CalDAV account must not leak into the iCloud provider list.
        assert!(store.icloud_accounts().unwrap().is_empty());

        let calendar_id = store
            .upsert_caldav_calendar(
                account_id,
                "/dav/calendars/me/work/",
                "Work",
                "#123456",
                true,
            )
            .unwrap();
        let start = Local::now();
        let end = start + Duration::hours(1);
        store
            .upsert_caldav_event(
                calendar_id,
                "/dav/calendars/me/work/evt.ics",
                &draft("Standup", start, end),
            )
            .unwrap();

        let events = store
            .events_between(start - Duration::minutes(1), end + Duration::minutes(1))
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].account_provider.as_deref(), Some("caldav"));
        assert_eq!(
            events[0].account_server_url.as_deref(),
            Some("https://caldav.fastmail.com/")
        );
        assert_eq!(
            events[0].icloud_event_id.as_deref(),
            Some("/dav/calendars/me/work/evt.ics")
        );
    }

    #[test]
    fn caldav_identity_is_scoped_to_server() {
        let store = Store::open_in_memory().unwrap();
        // Same username, two different servers → two distinct accounts.
        let fastmail = store
            .upsert_caldav_account(
                "me",
                "https://caldav.fastmail.com/",
                "me (fastmail)",
                "caldav-password:https://caldav.fastmail.com|me",
            )
            .unwrap();
        let nextcloud = store
            .upsert_caldav_account(
                "me",
                "https://cloud.example.com/remote.php/dav",
                "me (nextcloud)",
                "caldav-password:https://cloud.example.com/remote.php/dav|me",
            )
            .unwrap();
        assert_ne!(fastmail, nextcloud);
        assert_eq!(store.caldav_accounts().unwrap().len(), 2);

        // Reconnecting the same username+server updates in place, and can move
        // the display name without spawning a third row.
        let fastmail_again = store
            .upsert_caldav_account(
                "me",
                "https://caldav.fastmail.com/",
                "me (fastmail, renamed)",
                "caldav-password:https://caldav.fastmail.com|me",
            )
            .unwrap();
        assert_eq!(fastmail_again, fastmail);
        assert_eq!(store.caldav_accounts().unwrap().len(), 2);
    }

    #[test]
    fn google_account_identity_unaffected_by_server_url_column() {
        // Re-authing the same Google account must still update in place, not
        // create a duplicate, now that the identity index coalesces the
        // (always-NULL for Google) server_url.
        let store = Store::open_in_memory().unwrap();
        let first = store
            .upsert_google_account("person@example.com", "Person", "google:person")
            .unwrap();
        let second = store
            .upsert_google_account("person@example.com", "Person Renamed", "google:person")
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(store.google_accounts().unwrap().len(), 1);
    }

    #[test]
    fn upsert_google_event_is_scoped_to_calendar() {
        let store = Store::open_in_memory().unwrap();
        let account_id = store
            .upsert_google_account(
                "person@example.com",
                "person@example.com",
                "google-refresh-token:person@example.com",
            )
            .unwrap();
        let work_calendar_id = store
            .upsert_google_calendar(account_id, "cal-work", "Work", "#ff0000", true)
            .unwrap();
        let home_calendar_id = store
            .upsert_google_calendar(account_id, "cal-home", "Home", "#00ff00", true)
            .unwrap();
        let start = Local::now();
        let end = start + Duration::hours(1);

        store
            .upsert_google_event(
                work_calendar_id,
                "shared-id",
                &draft("Work event", start, end),
            )
            .unwrap();
        store
            .upsert_google_event(
                home_calendar_id,
                "shared-id",
                &draft("Home event", start, end),
            )
            .unwrap();

        let events = store
            .events_between(start - Duration::minutes(1), end + Duration::minutes(1))
            .unwrap();
        let titles: Vec<&str> = events.iter().map(|e| e.title.as_str()).collect();
        assert_eq!(events.len(), 2);
        assert!(titles.contains(&"Work event"));
        assert!(titles.contains(&"Home event"));
    }

    #[test]
    fn prune_google_events_removes_only_stale_synced_ones() {
        let store = Store::open_in_memory().unwrap();
        let account_id = store
            .upsert_google_account(
                "person@example.com",
                "person@example.com",
                "google-refresh-token:person@example.com",
            )
            .unwrap();
        let calendar_id = store
            .upsert_google_calendar(account_id, "cal-abc", "Work", "#ff0000", true)
            .unwrap();
        let start = Local::now();
        let end = start + Duration::hours(1);

        store
            .upsert_google_event(calendar_id, "keep", &draft("Keep", start, end))
            .unwrap();
        store
            .upsert_google_event(calendar_id, "gone", &draft("Gone", start, end))
            .unwrap();
        store
            .create_event(calendar_id, &draft("Local one", start, end))
            .unwrap();

        store
            .prune_google_events(
                calendar_id,
                &["keep".to_string()],
                start - Duration::minutes(1),
                end + Duration::minutes(1),
            )
            .unwrap();

        let events = store
            .events_between(start - Duration::minutes(1), end + Duration::minutes(1))
            .unwrap();
        let titles: Vec<&str> = events.iter().map(|e| e.title.as_str()).collect();
        assert!(titles.contains(&"Keep"));
        assert!(titles.contains(&"Local one"));
        assert!(!titles.contains(&"Gone"));
    }

    #[test]
    fn prune_google_events_with_empty_keep_list_removes_all_synced_events() {
        let store = Store::open_in_memory().unwrap();
        let account_id = store
            .upsert_google_account(
                "person@example.com",
                "person@example.com",
                "google-refresh-token:person@example.com",
            )
            .unwrap();
        let calendar_id = store
            .upsert_google_calendar(account_id, "cal-abc", "Work", "#ff0000", true)
            .unwrap();
        let start = Local::now();
        let end = start + Duration::hours(1);

        store
            .upsert_google_event(calendar_id, "gone-1", &draft("Gone 1", start, end))
            .unwrap();
        store
            .upsert_google_event(calendar_id, "gone-2", &draft("Gone 2", start, end))
            .unwrap();
        store
            .create_event(calendar_id, &draft("Local one", start, end))
            .unwrap();

        store
            .prune_google_events(
                calendar_id,
                &[],
                start - Duration::minutes(1),
                end + Duration::minutes(1),
            )
            .unwrap();

        let events = store
            .events_between(start - Duration::minutes(1), end + Duration::minutes(1))
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].title, "Local one");
        assert!(events[0].google_event_id.is_none());
    }

    #[test]
    fn pruning_a_sync_window_preserves_cached_events_outside_it() {
        let store = Store::open_in_memory().unwrap();
        let account_id = store
            .upsert_google_account("person@example.com", "Person", "token")
            .unwrap();
        let calendar_id = store
            .upsert_google_calendar(account_id, "cal-abc", "Work", "#ff0000", true)
            .unwrap();
        let now = Local::now();
        let old_start = now - Duration::days(365);

        store
            .upsert_google_event(
                calendar_id,
                "old-event",
                &draft("Old", old_start, old_start + Duration::hours(1)),
            )
            .unwrap();
        store
            .upsert_google_event(
                calendar_id,
                "stale-current-event",
                &draft("Stale", now, now + Duration::hours(1)),
            )
            .unwrap();

        store
            .prune_google_events(
                calendar_id,
                &[],
                now - Duration::days(1),
                now + Duration::days(1),
            )
            .unwrap();

        let old_events = store
            .events_between(
                old_start - Duration::minutes(1),
                old_start + Duration::hours(2),
            )
            .unwrap();
        assert_eq!(old_events.len(), 1);
        let current_events = store
            .events_between(now - Duration::minutes(1), now + Duration::hours(2))
            .unwrap();
        assert!(current_events.is_empty());
    }

    #[test]
    fn pruning_google_calendars_removes_unsubscribed_calendars() {
        let store = Store::open_in_memory().unwrap();
        let account_id = store
            .upsert_google_account("person@example.com", "Person", "token")
            .unwrap();
        store
            .upsert_google_calendar(account_id, "keep", "Keep", "#ff0000", true)
            .unwrap();
        store
            .upsert_google_calendar(account_id, "remove", "Remove", "#00ff00", true)
            .unwrap();

        store
            .prune_google_calendars(account_id, &["keep".to_string()])
            .unwrap();

        let calendars = store.calendars_for_account(account_id).unwrap();
        assert_eq!(calendars.len(), 1);
        assert_eq!(calendars[0].google_calendar_id.as_deref(), Some("keep"));
    }
}
