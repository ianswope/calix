use crate::recurrence::Frequency;
use chrono::{DateTime, Local, NaiveDate, SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Someone invited to an event. Attendee lists are read-only here: they come
/// from the remote provider, so they live outside [`EventDraft`] and a local
/// edit leaves them untouched.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Attendee {
    pub email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Invitation response, normalized across providers to one of `accepted`,
    /// `declined`, `tentative`, or `pending`. `None` when the provider didn't
    /// say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// This attendee represents the connected account's user.
    #[serde(default)]
    pub is_self: bool,
}

impl Attendee {
    /// The name to show, falling back to the email when the provider sent no
    /// display name.
    pub fn label(&self) -> &str {
        match self.name.as_deref() {
            Some(name) if !name.trim().is_empty() => name,
            _ => &self.email,
        }
    }
}

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
    /// How the event repeats; `None` for a one-off. Synced events arrive
    /// server-expanded (each occurrence a separate one-off row), so this is set
    /// only on locally-authored recurring events.
    pub recurrence: Option<Frequency>,
    /// Desktop-alert lead time in minutes before the start; `None` = no
    /// alert. Local to this machine — never pushed to Google/CalDAV, and the
    /// sync upserts leave it alone so it survives re-syncs.
    pub reminder_minutes: Option<i64>,
    /// Everyone invited, as last seen on the provider. Empty for local events
    /// and for remote events with no invitees.
    pub attendees: Vec<Attendee>,
}

/// Fields for creating or updating an event; `id`/`calendar_id` are handled
/// separately since callers building this don't yet know or can't change
/// them.
///
/// `PartialEq` is what lets undo tell "the row still holds what I wrote" from
/// "something changed it since" — see [`crate::undo`].
#[derive(Clone, Debug, PartialEq)]
pub struct EventDraft {
    pub title: String,
    pub start: DateTime<Local>,
    pub end: DateTime<Local>,
    pub all_day: bool,
    pub location: Option<String>,
    pub notes: Option<String>,
    pub recurrence: Option<Frequency>,
    pub reminder_minutes: Option<i64>,
    /// Invitees to send with a remote event. Local calendars retain these as
    /// event metadata but cannot deliver invitations.
    pub attendees: Vec<Attendee>,
}

impl Event {
    /// This event's editable fields, as a draft that would rewrite it
    /// unchanged. The seam between a stored row and a write of that row: undo
    /// compares against it, and an edit starts from it.
    pub fn draft(&self) -> EventDraft {
        EventDraft {
            title: self.title.clone(),
            start: self.start,
            end: self.end,
            all_day: self.all_day,
            location: self.location.clone(),
            notes: self.notes.clone(),
            recurrence: self.recurrence,
            reminder_minutes: self.reminder_minutes,
            attendees: self.attendees.clone(),
        }
    }
}

#[derive(Clone)]
pub struct Account {
    pub id: i64,
    /// `google`, `icloud`, or `caldav` — which sync path and keyring key
    /// scheme this account uses.
    pub provider: String,
    pub provider_account_id: String,
    pub display_name: String,
    pub token_key: String,
    /// CalDAV server base URL for generic `caldav` accounts; `None` for
    /// google and icloud.
    pub server_url: Option<String>,
    pub last_sync_at: Option<String>,
    pub last_sync_error: Option<String>,
}

impl Account {
    /// How to name this account to the user. Appends the provider's own id only
    /// when it says something the display name doesn't. Google names an account
    /// after its primary calendar, whose summary is normally the same address as
    /// its id, and CalDAV names one "<username> (<host>)" around the id, so in
    /// both cases the parenthetical would only repeat what's already there.
    pub fn label(&self) -> String {
        let name = self.display_name.trim();
        let id = self.provider_account_id.trim();
        if name.is_empty() {
            return id.to_string();
        }
        if id.is_empty() || name.to_lowercase().contains(&id.to_lowercase()) {
            return name.to_string();
        }
        format!("{name} ({id})")
    }
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
        let directory = path.parent().expect("data file has a parent dir");
        std::fs::create_dir_all(directory).map_err(sqlite_io_error)?;
        set_owner_only_permissions(directory, 0o700).map_err(sqlite_io_error)?;

        let connection = Connection::open(&path)?;
        // Tightened before the first write, not after it: SQLite copies the
        // database file's mode onto the -wal and -shm it creates, and those hold
        // real event data. Chmodding after `from_connection` ran the migrations
        // left both of them at whatever the umask allowed.
        set_owner_only_permissions(&path, 0o600).map_err(sqlite_io_error)?;
        Self::from_connection(connection)
    }

    #[cfg(test)]
    pub(crate) fn open_in_memory() -> rusqlite::Result<Self> {
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
        // Stores an event's recurrence as its iCalendar RRULE value (e.g.
        // "FREQ=WEEKLY"); NULL for one-off events. See `crate::recurrence`.
        ensure_column(&conn, "events", "recurrence", "TEXT")?;
        // Desktop-alert lead time in minutes; NULL = no alert. Local-only:
        // the sync upserts never write it, so it survives re-syncs.
        ensure_column(&conn, "events", "reminder_minutes", "INTEGER")?;
        // Base URL for a generic CalDAV account's server; NULL for google and
        // icloud (iCloud uses a fixed well-known root).
        ensure_column(&conn, "accounts", "server_url", "TEXT")?;
        ensure_column(&conn, "accounts", "last_sync_at", "TEXT")?;
        ensure_column(&conn, "accounts", "last_sync_error", "TEXT")?;
        // JSON array of `Attendee`, written only by sync. `update_event` never
        // touches it, so editing an event locally keeps the provider's list.
        ensure_column(&conn, "events", "attendees", "TEXT")?;

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

    pub fn accounts_for_provider(&self, provider: &str) -> rusqlite::Result<Vec<Account>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, provider, provider_account_id, display_name, token_key, server_url,
                    last_sync_at, last_sync_error
             FROM accounts
             WHERE provider = ?1
             ORDER BY display_name",
        )?;
        let rows = stmt.query_map(params![provider], row_to_account)?;
        rows.collect()
    }

    /// Every connected account across all providers, for the account-management
    /// UI. Ordered by provider then display name so the list is stable.
    pub fn all_accounts(&self) -> rusqlite::Result<Vec<Account>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, provider, provider_account_id, display_name, token_key, server_url,
                    last_sync_at, last_sync_error
             FROM accounts
             ORDER BY provider, display_name",
        )?;
        let rows = stmt.query_map([], row_to_account)?;
        rows.collect()
    }

    /// Forgets an account and everything cached under it: its events, then its
    /// calendars, then the row itself, in one transaction so a failure can't
    /// leave calendars orphaned from their account.
    ///
    /// Local only. Nothing is deleted on the provider, and the keyring entry is
    /// the caller's to remove — see the credential helpers.
    pub fn delete_account(&self, account_id: i64) -> rusqlite::Result<()> {
        self.conn.execute("BEGIN", [])?;
        let result = (|| -> rusqlite::Result<()> {
            self.conn.execute(
                "DELETE FROM events WHERE calendar_id IN
                     (SELECT id FROM calendars WHERE account_id = ?1)",
                params![account_id],
            )?;
            self.conn.execute(
                "DELETE FROM calendars WHERE account_id = ?1",
                params![account_id],
            )?;
            self.conn
                .execute("DELETE FROM accounts WHERE id = ?1", params![account_id])?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.conn.execute("COMMIT", [])?;
                Ok(())
            }
            Err(e) => {
                let _ = self.conn.execute("ROLLBACK", []);
                Err(e)
            }
        }
    }

    /// Records durable account health for the account center. A clean sync
    /// clears an older error; no credentials are included in the message.
    pub fn record_account_sync(
        &self,
        account_id: i64,
        error: Option<&str>,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE accounts SET last_sync_at = ?1, last_sync_error = ?2 WHERE id = ?3",
            params![
                Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
                error,
                account_id
            ],
        )?;
        Ok(())
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
    /// Recurring events are expanded into their occurrences within the range.
    pub fn events_between(
        &self,
        range_start: DateTime<Local>,
        range_end: DateTime<Local>,
    ) -> rusqlite::Result<Vec<Event>> {
        // Non-recurring rows — including server-expanded synced instances, whose
        // recurrence column is NULL — filtered to the range.
        let mut stmt = self.conn.prepare(&format!(
            "{EVENT_SELECT}
             WHERE events.recurrence IS NULL
               AND events.start_at < ?1 AND events.end_at > ?2
               AND calendars.visible != 0"
        ))?;
        let mut events: Vec<Event> = stmt
            .query_map(
                params![stored_timestamp(&range_end), stored_timestamp(&range_start)],
                row_to_event,
            )?
            .collect::<rusqlite::Result<Vec<Option<Event>>>>()?
            .into_iter()
            .flatten()
            .collect();

        // Recurring events have no server to expand them, and a master's own
        // stored span may fall outside the range, so fetch them all (unfiltered
        // by date) and expand client-side into the range.
        let mut recurring = self.conn.prepare(&format!(
            "{EVENT_SELECT} WHERE events.recurrence IS NOT NULL AND calendars.visible != 0"
        ))?;
        let masters = recurring
            .query_map([], row_to_event)?
            .collect::<rusqlite::Result<Vec<Option<Event>>>>()?;
        for master in masters.into_iter().flatten() {
            events.extend(expand_recurring(&master, range_start, range_end));
        }

        events.sort_by_key(|event| event.start);
        Ok(events)
    }

    /// Events whose title, location, or notes contain `query`, oldest first.
    ///
    /// Scoped to visible calendars, matching [`Self::events_between`]: a
    /// calendar switched off in the sidebar is off everywhere, and search
    /// turning up events from a calendar you deliberately hid would be its own
    /// small betrayal.
    ///
    /// Recurring events match on their stored master, so a weekly series
    /// surfaces once rather than as one hit per occurrence — the series is what
    /// the user is looking for.
    pub fn search_events(&self, query: &str, limit: usize) -> rusqlite::Result<Vec<Event>> {
        let query = query.trim();
        // Answered before it reaches SQL: the pattern for an empty query is
        // `%%`, which matches every row in the database.
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let pattern = format!("%{}%", like_escape(query));
        // LIKE is case-insensitive for ASCII in SQLite, which is the behavior
        // wanted here; `location` and `notes` are nullable, and NULL LIKE
        // anything is NULL rather than false, so they need a default.
        let mut stmt = self.conn.prepare(&format!(
            "{EVENT_SELECT}
             WHERE calendars.visible != 0
               AND (events.title LIKE ?1 ESCAPE '\\'
                    OR IFNULL(events.location, '') LIKE ?1 ESCAPE '\\'
                    OR IFNULL(events.notes, '') LIKE ?1 ESCAPE '\\')
             ORDER BY events.start_at
             LIMIT ?2"
        ))?;
        Ok(stmt
            .query_map(params![pattern, limit as i64], row_to_event)?
            .collect::<rusqlite::Result<Vec<Option<Event>>>>()?
            .into_iter()
            .flatten()
            .collect())
    }

    /// The stored event with `id`, if any. Unlike [`Self::events_between`] this
    /// returns the raw row — for a recurring event that's the series master, not
    /// an expanded occurrence.
    pub fn event_by_id(&self, id: i64) -> rusqlite::Result<Option<Event>> {
        self.conn
            .query_row(
                &format!("{EVENT_SELECT} WHERE events.id = ?1"),
                params![id],
                row_to_event,
            )
            .optional()
            .map(Option::flatten)
    }

    pub fn create_event(&self, calendar_id: i64, draft: &EventDraft) -> rusqlite::Result<i64> {
        self.conn.execute(
            "INSERT INTO events (calendar_id, title, start_at, end_at, all_day, location, notes, recurrence, reminder_minutes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                calendar_id,
                draft.title,
                stored_timestamp(&draft.start),
                stored_timestamp(&draft.end),
                draft.all_day as i64,
                draft.location,
                draft.notes,
                draft.recurrence.map(Frequency::to_rrule),
                draft.reminder_minutes,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn update_event(&self, id: i64, draft: &EventDraft) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE events SET title = ?1, start_at = ?2, end_at = ?3, all_day = ?4,
             location = ?5, notes = ?6, recurrence = ?7, reminder_minutes = ?8 WHERE id = ?9",
            params![
                draft.title,
                stored_timestamp(&draft.start),
                stored_timestamp(&draft.end),
                draft.all_day as i64,
                draft.location,
                draft.notes,
                draft.recurrence.map(Frequency::to_rrule),
                draft.reminder_minutes,
                id,
            ],
        )?;
        Ok(())
    }

    pub fn update_event_attendees(&self, id: i64, attendees: &[Attendee]) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE events SET attendees = ?1 WHERE id = ?2",
            params![attendees_to_json(attendees), id],
        )?;
        Ok(())
    }

    pub fn delete_event(&self, id: i64) -> rusqlite::Result<()> {
        self.conn
            .execute("DELETE FROM events WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Creates or updates a Google-sourced event by its Google event id.
    ///
    /// `reminder_minutes` is written on INSERT but deliberately left out of the
    /// `DO UPDATE`: an alert is Calix-local — nothing sends it to Google, and
    /// nothing reads one back — so the row created right after the user adds an
    /// event is the only chance to keep the alert they picked, while every later
    /// sync must leave the column alone rather than blank it. (An alert on a
    /// *recurring* remote event still can't outlive the first sync, which
    /// replaces the series row with the provider's expanded instances.)
    pub fn upsert_google_event(
        &self,
        calendar_id: i64,
        google_event_id: &str,
        draft: &EventDraft,
        attendees: &[Attendee],
    ) -> rusqlite::Result<i64> {
        // `RETURNING` rather than `last_insert_rowid`, which says nothing useful
        // when the upsert took the update branch.
        self.conn.query_row(
            "INSERT INTO events (calendar_id, title, start_at, end_at, all_day, location, notes, google_event_id, attendees, reminder_minutes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(calendar_id, google_event_id) WHERE google_event_id IS NOT NULL
             DO UPDATE SET title = ?2, start_at = ?3, end_at = ?4, all_day = ?5, location = ?6, notes = ?7, attendees = ?9
             RETURNING id",
            params![
                calendar_id,
                draft.title,
                stored_timestamp(&draft.start),
                stored_timestamp(&draft.end),
                draft.all_day as i64,
                draft.location,
                draft.notes,
                google_event_id,
                attendees_to_json(attendees),
                draft.reminder_minutes,
            ],
            |row| row.get(0),
        )
    }

    /// Creates or updates a CalDAV-sourced event by its href. Handles
    /// `reminder_minutes` exactly as [`Self::upsert_google_event`] does, and for
    /// the same reason.
    pub fn upsert_caldav_event(
        &self,
        calendar_id: i64,
        icloud_event_id: &str,
        draft: &EventDraft,
        attendees: &[Attendee],
    ) -> rusqlite::Result<i64> {
        self.conn.query_row(
            "INSERT INTO events (calendar_id, title, start_at, end_at, all_day, location, notes, icloud_event_id, attendees, reminder_minutes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(calendar_id, icloud_event_id) WHERE icloud_event_id IS NOT NULL
             DO UPDATE SET title = ?2, start_at = ?3, end_at = ?4, all_day = ?5, location = ?6, notes = ?7, attendees = ?9
             RETURNING id",
            params![
                calendar_id,
                draft.title,
                stored_timestamp(&draft.start),
                stored_timestamp(&draft.end),
                draft.all_day as i64,
                draft.location,
                draft.notes,
                icloud_event_id,
                attendees_to_json(attendees),
                draft.reminder_minutes,
            ],
            |row| row.get(0),
        )
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

        let placeholders = placeholders(keep_google_ids.len());
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

    /// Deletes cached CalDAV events in the sync window that the server no
    /// longer lists.
    ///
    /// `keep_icloud_ids` are the ids read back in full. `keep_resource_hrefs`
    /// are resources the sync couldn't read completely: every cached event
    /// under one of those survives, whatever its `href#instance` id, because
    /// the sync can't name the instance that went missing.
    pub fn prune_caldav_events(
        &self,
        calendar_id: i64,
        keep_icloud_ids: &[String],
        keep_resource_hrefs: &[String],
        range_start: DateTime<Local>,
        range_end: DateTime<Local>,
    ) -> rusqlite::Result<()> {
        let mut sql = String::from(
            "DELETE FROM events
             WHERE calendar_id = ? AND icloud_event_id IS NOT NULL
               AND start_at < ? AND end_at > ?",
        );
        let range_end = stored_timestamp(&range_end);
        let range_start = stored_timestamp(&range_start);
        let mut params: Vec<&dyn rusqlite::ToSql> = vec![&calendar_id, &range_end, &range_start];

        if !keep_icloud_ids.is_empty() {
            sql.push_str(&format!(
                " AND icloud_event_id NOT IN ({})",
                placeholders(keep_icloud_ids.len())
            ));
            params.extend(keep_icloud_ids.iter().map(|id| id as &dyn rusqlite::ToSql));
        }
        if !keep_resource_hrefs.is_empty() {
            // The resource href is the id up to its first '#'; appending one
            // makes instr() find a separator even for a plain, un-suffixed id.
            sql.push_str(&format!(
                " AND substr(icloud_event_id, 1, instr(icloud_event_id || '#', '#') - 1) \
                  NOT IN ({})",
                placeholders(keep_resource_hrefs.len())
            ));
            params.extend(
                keep_resource_hrefs
                    .iter()
                    .map(|href| href as &dyn rusqlite::ToSql),
            );
        }

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

        let placeholders = placeholders(keep_icloud_ids.len());
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

        let placeholders = placeholders(keep_google_ids.len());
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

/// The columns [`row_to_event`] reads, with the joins supplying the calendar
/// and account fields. Callers append their own `WHERE` (and any `ORDER BY`).
/// Escapes the characters SQL `LIKE` treats as wildcards, so a literal `%` or
/// `_` typed into the search box matches itself.
///
/// Without this, searching for `%` matches every event in the database and
/// `_` matches any single character — the two most likely ways for a search to
/// look broken while behaving exactly as written. The backslash must be escaped
/// first, or it would escape the escapes added after it.
fn like_escape(query: &str) -> String {
    query
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

const EVENT_SELECT: &str = "SELECT events.id, events.calendar_id, calendars.name, calendars.color,
            accounts.provider, accounts.provider_account_id, accounts.token_key,
            calendars.google_calendar_id,
            events.title, events.start_at,
            events.end_at, events.all_day, events.location, events.notes,
            events.google_event_id, events.icloud_event_id,
            accounts.server_url, events.recurrence, events.reminder_minutes,
            events.attendees
     FROM events
     JOIN calendars ON calendars.id = events.calendar_id
     LEFT JOIN accounts ON accounts.id = calendars.account_id";

/// The occurrences of a recurring `master` overlapping the range. Each is a
/// clone carrying the master's id and recurrence, so clicking any occurrence
/// opens the series and dragging stays disabled (see `event_widget`).
fn expand_recurring(
    master: &Event,
    range_start: DateTime<Local>,
    range_end: DateTime<Local>,
) -> Vec<Event> {
    let Some(freq) = master.recurrence else {
        return Vec::new();
    };
    let duration = master.end - master.start;
    crate::recurrence::occurrences_in(master.start, duration, freq, range_start, range_end)
        .into_iter()
        .map(|start| {
            let mut occurrence = master.clone();
            occurrence.end = if master.all_day {
                // Keep an all-day span a whole number of calendar days, the same
                // policy as a moved all-day draft, so DST can't nudge the end.
                let span_days = (master.end.date_naive() - master.start.date_naive())
                    .num_days()
                    .max(1);
                day_start(start.date_naive() + chrono::Duration::days(span_days))
            } else {
                start + duration
            };
            occurrence.start = start;
            occurrence
        })
        .collect()
}

/// `None` for a row whose stored timestamps can't be read — see
/// [`parse_rfc3339`]. Callers drop those rows instead of failing the query, so
/// one damaged row costs a single event rather than the whole calendar.
fn row_to_event(row: &rusqlite::Row) -> rusqlite::Result<Option<Event>> {
    let start_at: String = row.get(9)?;
    let end_at: String = row.get(10)?;
    let (Some(start), Some(end)) = (parse_rfc3339(&start_at), parse_rfc3339(&end_at)) else {
        return Ok(None);
    };
    Ok(Some(Event {
        id: row.get(0)?,
        calendar_id: row.get(1)?,
        calendar_name: row.get(2)?,
        calendar_color: row.get(3)?,
        account_provider: row.get(4)?,
        account_provider_id: row.get(5)?,
        account_token_key: row.get(6)?,
        google_calendar_id: row.get(7)?,
        title: row.get(8)?,
        start,
        end,
        all_day: row.get::<_, i64>(11)? != 0,
        location: row.get(12)?,
        notes: row.get(13)?,
        google_event_id: row.get(14)?,
        icloud_event_id: row.get(15)?,
        account_server_url: row.get(16)?,
        recurrence: row
            .get::<_, Option<String>>(17)?
            .as_deref()
            .and_then(Frequency::from_rrule),
        reminder_minutes: row.get(18)?,
        attendees: attendees_from_json(row.get(19)?),
    }))
}

/// Serializes an attendee list for storage. An empty list is stored as SQL NULL
/// rather than `"[]"`, so purely local events keep a NULL column.
fn attendees_to_json(attendees: &[Attendee]) -> Option<String> {
    (!attendees.is_empty()).then(|| {
        serde_json::to_string(attendees)
            .expect("attendee lists are plain strings and always encode")
    })
}

/// Parses a stored attendee list. Anything unreadable degrades to an empty
/// list — a malformed column shouldn't stop the event itself from loading.
fn attendees_from_json(raw: Option<String>) -> Vec<Attendee> {
    raw.and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
}

fn row_to_account(row: &rusqlite::Row) -> rusqlite::Result<Account> {
    Ok(Account {
        id: row.get(0)?,
        provider: row.get(1)?,
        provider_account_id: row.get(2)?,
        display_name: row.get(3)?,
        token_key: row.get(4)?,
        server_url: row.get(5)?,
        last_sync_at: row.get(6)?,
        last_sync_error: row.get(7)?,
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

/// Parses a stored timestamp, or `None` for a value this app could not have
/// written — a hand-edited row, a half-applied migration, a partial restore.
///
/// Reported and skipped rather than trusted: the alternatives are inventing a
/// date for the row or panicking, and a panic here takes the whole app down
/// while events are being loaded, which is every time the grid is drawn.
fn parse_rfc3339(s: &str) -> Option<DateTime<Local>> {
    match DateTime::parse_from_rfc3339(s) {
        Ok(parsed) => Some(parsed.with_timezone(&Local)),
        Err(error) => {
            eprintln!("calix: ignoring an event with an unreadable timestamp {s:?}: {error}");
            None
        }
    }
}

/// A comma-separated run of `n` SQL parameter placeholders, for the `IN (…)`
/// clauses whose length is only known at runtime.
fn placeholders(n: usize) -> String {
    std::iter::repeat_n("?", n).collect::<Vec<_>>().join(",")
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
    crate::xdg::data_home().join("calix").join("calix.sqlite3")
}

fn sqlite_io_error(error: std::io::Error) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(error))
}

#[cfg(unix)]
fn set_owner_only_permissions(path: &std::path::Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let permissions = std::fs::Permissions::from_mode(mode);
    std::fs::set_permissions(path, permissions)
}

#[cfg(not(unix))]
fn set_owner_only_permissions(_path: &std::path::Path, _mode: u32) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recurrence::Frequency;
    use chrono::{Duration, TimeZone};

    fn draft(title: &str, start: DateTime<Local>, end: DateTime<Local>) -> EventDraft {
        EventDraft {
            title: title.to_string(),
            start,
            end,
            all_day: false,
            location: None,
            notes: None,
            recurrence: None,
            reminder_minutes: None,
            attendees: Vec::new(),
        }
    }

    /// A store holding one event per (title, location, notes) triple given,
    /// each an hour long and a day apart so ordering is unambiguous.
    fn store_with(rows: &[(&str, Option<&str>, Option<&str>)]) -> Store {
        let store = Store::open_in_memory().expect("an in-memory database");
        let base = Local
            .with_ymd_and_hms(2026, 3, 1, 9, 0, 0)
            .single()
            .expect("an unambiguous local time");
        for (index, (title, location, notes)) in rows.iter().enumerate() {
            let start = base + Duration::days(index as i64);
            let mut event = draft(title, start, start + Duration::hours(1));
            event.location = location.map(str::to_string);
            event.notes = notes.map(str::to_string);
            store.create_event(1, &event).expect("the event to store");
        }
        store
    }

    fn titles(events: &[Event]) -> Vec<&str> {
        events.iter().map(|event| event.title.as_str()).collect()
    }

    #[test]
    fn search_matches_a_title_regardless_of_case() {
        let store = store_with(&[("Dentist", None, None), ("Standup", None, None)]);
        let found = store.search_events("dent", 20).expect("a search");
        assert_eq!(titles(&found), vec!["Dentist"]);
    }

    #[test]
    fn search_also_looks_in_location_and_notes() {
        let store = store_with(&[
            ("Lunch", Some("Blue Bottle"), None),
            ("Review", None, Some("bring the blue folder")),
            ("Standup", None, None),
        ]);
        let found = store.search_events("blue", 20).expect("a search");
        assert_eq!(titles(&found), vec!["Lunch", "Review"]);
    }

    #[test]
    fn a_literal_percent_does_not_match_every_event() {
        // Unescaped, this is the LIKE wildcard for "anything", so the search
        // would silently return the whole database.
        let store = store_with(&[("Raise 5% budget", None, None), ("Standup", None, None)]);
        let found = store.search_events("%", 20).expect("a search");
        assert_eq!(titles(&found), vec!["Raise 5% budget"]);
    }

    #[test]
    fn a_literal_underscore_does_not_match_any_character() {
        let store = store_with(&[("snake_case rename", None, None), ("Standup", None, None)]);
        let found = store.search_events("_", 20).expect("a search");
        assert_eq!(titles(&found), vec!["snake_case rename"]);
    }

    #[test]
    fn a_literal_backslash_is_found_rather_than_escaping_what_follows() {
        let store = store_with(&[("path\\to\\file", None, None), ("Standup", None, None)]);
        let found = store.search_events("\\", 20).expect("a search");
        assert_eq!(titles(&found), vec!["path\\to\\file"]);
    }

    #[test]
    fn an_empty_search_finds_nothing_rather_than_everything() {
        // "%%" matches every row, so an empty box must be answered before it
        // ever reaches SQL.
        let store = store_with(&[("Dentist", None, None), ("Standup", None, None)]);
        for query in ["", "   ", "\t\n"] {
            assert!(
                store.search_events(query, 20).expect("a search").is_empty(),
                "{query:?} should find nothing"
            );
        }
    }

    #[test]
    fn search_returns_matches_oldest_first_and_honors_the_limit() {
        let store = store_with(&[
            ("Sync one", None, None),
            ("Sync two", None, None),
            ("Sync three", None, None),
        ]);
        let found = store.search_events("sync", 2).expect("a search");
        assert_eq!(titles(&found), vec!["Sync one", "Sync two"]);
    }

    #[test]
    fn search_skips_calendars_hidden_in_the_sidebar() {
        let store = store_with(&[("Dentist", None, None)]);
        store
            .set_calendar_visible(1, false)
            .expect("visibility to update");
        assert!(
            store
                .search_events("dent", 20)
                .expect("a search")
                .is_empty(),
            "a hidden calendar's events must stay hidden in search"
        );
    }

    fn test_account(display_name: &str, provider_account_id: &str) -> Account {
        Account {
            id: 1,
            provider: "google".to_string(),
            provider_account_id: provider_account_id.to_string(),
            display_name: display_name.to_string(),
            token_key: "token:test".to_string(),
            server_url: None,
            last_sync_at: None,
            last_sync_error: None,
        }
    }

    #[test]
    fn an_accounts_label_drops_the_provider_id_when_it_repeats_the_name() {
        // Google names an account after its primary calendar, whose summary is
        // usually the address that is also its id — "a@b.com (a@b.com)" tells
        // the user nothing twice.
        let account = test_account("ian@example.com", "ian@example.com");
        assert_eq!(account.label(), "ian@example.com");
    }

    #[test]
    fn an_accounts_label_keeps_a_provider_id_that_adds_information() {
        let account = test_account("Work", "work-calendar@group.calendar.google.com");
        assert_eq!(
            account.label(),
            "Work (work-calendar@group.calendar.google.com)"
        );
    }

    #[test]
    fn an_accounts_label_ignores_case_and_padding_when_comparing() {
        let account = test_account(" Ian@Example.com ", "ian@example.com");
        assert_eq!(account.label(), "Ian@Example.com");
    }

    #[test]
    fn an_accounts_label_does_not_repeat_a_provider_id_the_name_already_contains() {
        // CalDAV accounts are named "<username> (<host>)", and the username is
        // the provider id — appending it again would read "ian (example.com) (ian)".
        let mut account = test_account("ian (example.com)", "ian");
        account.provider = "caldav".to_string();
        assert_eq!(account.label(), "ian (example.com)");
    }

    #[test]
    fn an_accounts_label_falls_back_to_the_provider_id_when_unnamed() {
        let account = test_account("", "ian@example.com");
        assert_eq!(account.label(), "ian@example.com");
    }

    #[test]
    fn deleting_an_account_removes_only_its_own_calendars_and_events() {
        let store = Store::open_in_memory().unwrap();
        let start = Local::now();
        let end = start + Duration::hours(1);

        let doomed = store
            .upsert_google_account("a@example.com", "a@example.com", "token:a")
            .unwrap();
        let doomed_calendar = store
            .upsert_google_calendar(doomed, "cal-a", "A", "#ff0000", true)
            .unwrap();
        store
            .upsert_google_event(doomed_calendar, "evt-a", &draft("A", start, end), &[])
            .unwrap();

        let keeper = store
            .upsert_google_account("b@example.com", "b@example.com", "token:b")
            .unwrap();
        let keeper_calendar = store
            .upsert_google_calendar(keeper, "cal-b", "B", "#00ff00", true)
            .unwrap();
        store
            .upsert_google_event(keeper_calendar, "evt-b", &draft("B", start, end), &[])
            .unwrap();

        // A purely local event lives on the default calendar, which has no
        // account at all; disconnecting must not reach it.
        store
            .create_event(store.default_calendar_id(), &draft("Local", start, end))
            .unwrap();

        store.delete_account(doomed).unwrap();

        let remaining: Vec<String> = store
            .events_between(start - Duration::minutes(1), end + Duration::minutes(1))
            .unwrap()
            .into_iter()
            .map(|event| event.title)
            .collect();
        assert!(!remaining.contains(&"A".to_string()), "{remaining:?}");
        assert!(remaining.contains(&"B".to_string()), "{remaining:?}");
        assert!(remaining.contains(&"Local".to_string()), "{remaining:?}");

        let accounts = store.all_accounts().unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].id, keeper);
        assert_eq!(accounts[0].provider, "google");
    }

    #[test]
    fn deleting_an_account_twice_is_harmless() {
        let store = Store::open_in_memory().unwrap();
        let id = store
            .upsert_google_account("a@example.com", "a@example.com", "token:a")
            .unwrap();
        store.delete_account(id).unwrap();
        // Disconnecting an already-removed account must not error — the UI can
        // race itself if the dialog is reopened.
        store.delete_account(id).unwrap();
        assert!(store.all_accounts().unwrap().is_empty());
    }

    #[test]
    fn upserting_a_synced_event_reports_the_row_it_wrote() {
        let store = Store::open_in_memory().unwrap();
        let account_id = store
            .upsert_google_account("me@example.com", "me@example.com", "token-key")
            .unwrap();
        let calendar_id = store
            .upsert_google_calendar(account_id, "cal-1", "Work", "#ff0000", true)
            .unwrap();
        let start = Local
            .with_ymd_and_hms(2026, 8, 24, 9, 0, 0)
            .single()
            .expect("an unambiguous local time");
        let written = draft("Standup", start, start + Duration::hours(1));

        // Undo needs the row id of an event created on a provider, or taking
        // back that create would reach for some older change instead.
        let id = store
            .upsert_google_event(calendar_id, "evt-1", &written, &[])
            .expect("the event to store");
        assert_eq!(
            store.event_by_id(id).unwrap().map(|event| event.title),
            Some("Standup".to_string())
        );

        let again = store
            .upsert_google_event(calendar_id, "evt-1", &written, &[])
            .expect("the upsert to run again");
        assert_eq!(again, id, "the second upsert updates the same row");
    }

    #[test]
    fn a_draft_taken_back_off_a_stored_event_matches_what_was_written() {
        let store = Store::open_in_memory().unwrap();
        let start = Local
            .with_ymd_and_hms(2026, 8, 24, 9, 0, 0)
            .single()
            .expect("an unambiguous local time");
        let mut written = draft("Standup", start, start + Duration::hours(1));
        written.location = Some("Room 2".to_string());
        written.notes = Some("bring the laptop".to_string());
        written.reminder_minutes = Some(15);
        written.recurrence = Some(Frequency::Weekly);
        // Attendees are deliberately left out: they are written separately by
        // `update_event_attendees` so that editing an event can't wipe what a
        // provider synced. See the attendee round-trip test below.

        let id = store.create_event(1, &written).expect("the event to store");
        let stored = store
            .event_by_id(id)
            .expect("the query to run")
            .expect("the row just written");

        // Undo decides whether a row still holds what it wrote by comparing
        // the fields an edit owns, so a round trip that changed any of them
        // would make every undo refuse itself as stale.
        assert_eq!(stored.draft(), written);
    }

    #[test]
    fn synced_attendees_round_trip_and_survive_a_local_edit() {
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
        let invitees = vec![
            Attendee {
                email: "ada@example.com".to_string(),
                name: Some("Ada Lovelace".to_string()),
                status: Some("accepted".to_string()),
                is_self: false,
            },
            Attendee {
                email: "bob@example.com".to_string(),
                name: None,
                status: None,
                is_self: false,
            },
        ];

        store
            .upsert_google_event(calendar_id, "evt-1", &draft("Sync", start, end), &invitees)
            .unwrap();

        let window = (start - Duration::minutes(1), end + Duration::minutes(1));
        let events = store.events_between(window.0, window.1).unwrap();
        assert_eq!(events[0].attendees, invitees);

        // A local edit goes through `update_event`, which never touches the
        // attendees column — the provider's invitee list must survive it.
        store
            .update_event(events[0].id, &draft("Sync (renamed)", start, end))
            .unwrap();
        let events = store.events_between(window.0, window.1).unwrap();
        assert_eq!(events[0].title, "Sync (renamed)");
        assert_eq!(events[0].attendees, invitees);
    }

    #[test]
    fn events_without_attendees_read_back_as_an_empty_list() {
        let store = Store::open_in_memory().unwrap();
        let start = Local::now();
        let end = start + Duration::hours(1);
        store
            .create_event(store.default_calendar_id(), &draft("Solo", start, end))
            .unwrap();

        let events = store
            .events_between(start - Duration::minutes(1), end + Duration::minutes(1))
            .unwrap();
        assert!(events[0].attendees.is_empty());
    }

    #[test]
    fn create_event_persists_and_reads_back_a_recurrence() {
        let store = Store::open_in_memory().unwrap();
        let start = Local
            .with_ymd_and_hms(2026, 7, 6, 9, 0, 0)
            .single()
            .unwrap();
        let mut weekly = draft("Standup", start, start + Duration::hours(1));
        weekly.recurrence = Some(Frequency::Weekly);
        store
            .create_event(store.default_calendar_id(), &weekly)
            .unwrap();

        let events = store
            .events_between(start - Duration::minutes(1), start + Duration::hours(2))
            .unwrap();
        assert_eq!(events[0].recurrence, Some(Frequency::Weekly));
    }

    #[test]
    fn a_weekly_local_event_expands_into_weeks_beyond_the_masters_own() {
        let store = Store::open_in_memory().unwrap();
        // 2026-07-02 is a Thursday.
        let base = Local
            .with_ymd_and_hms(2026, 7, 2, 9, 0, 0)
            .single()
            .unwrap();
        let mut weekly = draft("Standup", base, base + Duration::hours(1));
        weekly.recurrence = Some(Frequency::Weekly);
        store
            .create_event(store.default_calendar_id(), &weekly)
            .unwrap();

        // A one-day window two weeks past the master's start still shows it.
        let range_start = Local
            .with_ymd_and_hms(2026, 7, 16, 0, 0, 0)
            .single()
            .unwrap();
        let range_end = Local
            .with_ymd_and_hms(2026, 7, 17, 0, 0, 0)
            .single()
            .unwrap();
        let events = store.events_between(range_start, range_end).unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].start.date_naive(),
            NaiveDate::from_ymd_opt(2026, 7, 16).unwrap()
        );
        assert_eq!(events[0].recurrence, Some(Frequency::Weekly));
    }

    #[test]
    fn a_one_off_event_reads_back_with_no_recurrence() {
        let store = Store::open_in_memory().unwrap();
        let start = Local
            .with_ymd_and_hms(2026, 7, 6, 9, 0, 0)
            .single()
            .unwrap();
        store
            .create_event(
                store.default_calendar_id(),
                &draft("Once", start, start + Duration::hours(1)),
            )
            .unwrap();

        let events = store
            .events_between(start - Duration::minutes(1), start + Duration::hours(2))
            .unwrap();
        assert_eq!(events[0].recurrence, None);
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
            .upsert_google_event(calendar_id, "evt-1", &draft("Standup", start, end), &[])
            .unwrap();
        store
            .upsert_google_event(
                calendar_id,
                "evt-1",
                &draft("Standup (moved)", start, end),
                &[],
            )
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
                &[],
            )
            .unwrap();
        store
            .upsert_caldav_event(
                calendar_id,
                "/calendars/work/evt-1.ics",
                &draft("Lunch (moved)", start, end),
                &[],
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
    fn a_row_with_an_unreadable_timestamp_is_skipped_rather_than_fatal() {
        let store = Store::open_in_memory().unwrap();
        let start = Local
            .with_ymd_and_hms(2026, 7, 20, 10, 0, 0)
            .single()
            .unwrap();
        let end = start + Duration::hours(1);
        store
            .create_event(store.default_calendar_id(), &draft("Readable", start, end))
            .unwrap();
        // Nothing in this app writes a timestamp like that; a hand-edited row, a
        // half-applied migration or a partial restore can still leave one.
        store
            .conn
            .execute(
                "INSERT INTO events (calendar_id, title, start_at, end_at, all_day)
                 VALUES (?1, 'Corrupt', 'not a timestamp', ?2, 0)",
                params![store.default_calendar_id(), stored_timestamp(&end)],
            )
            .unwrap();

        let events = store
            .events_between(start - Duration::hours(1), end)
            .expect("one bad row must not fail the whole query");
        assert_eq!(
            events.iter().map(|e| e.title.as_str()).collect::<Vec<_>>(),
            ["Readable"]
        );
        // "r" is in both titles, so search would return the corrupt row too if
        // the skip only covered the range query.
        let found = store.search_events("r", 20).unwrap();
        assert_eq!(
            found.iter().map(|e| e.title.as_str()).collect::<Vec<_>>(),
            ["Readable"]
        );
    }

    #[test]
    fn reminder_minutes_round_trip_through_create_and_read() {
        let store = Store::open_in_memory().unwrap();
        let start = Local
            .with_ymd_and_hms(2026, 7, 20, 10, 0, 0)
            .single()
            .unwrap();
        let mut with_alert = draft("Dentist", start, start + Duration::hours(1));
        with_alert.reminder_minutes = Some(30);
        store
            .create_event(store.default_calendar_id(), &with_alert)
            .unwrap();

        let events = store
            .events_between(start - Duration::hours(1), start + Duration::hours(2))
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].reminder_minutes, Some(30));
    }

    #[test]
    fn sync_upsert_preserves_a_locally_set_reminder() {
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
        let start = Local
            .with_ymd_and_hms(2026, 7, 20, 12, 0, 0)
            .single()
            .unwrap();
        let end = start + Duration::hours(1);
        store
            .upsert_caldav_event(
                calendar_id,
                "/calendars/work/evt-1.ics",
                &draft("Lunch", start, end),
                &[],
            )
            .unwrap();

        // The user picks an alert on the synced event locally…
        let events = store
            .events_between(start - end.signed_duration_since(start), end)
            .unwrap();
        let mut with_alert = draft("Lunch", start, end);
        with_alert.reminder_minutes = Some(10);
        store.update_event(events[0].id, &with_alert).unwrap();

        // …then the next sync rewrites the row from the remote copy.
        store
            .upsert_caldav_event(
                calendar_id,
                "/calendars/work/evt-1.ics",
                &draft("Lunch (moved)", start, end),
                &[],
            )
            .unwrap();

        let events = store
            .events_between(start - Duration::hours(1), end)
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].title, "Lunch (moved)");
        assert_eq!(events[0].reminder_minutes, Some(10));
    }

    #[test]
    fn creating_a_google_event_stores_the_alert_the_user_chose() {
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
        let start = Local
            .with_ymd_and_hms(2026, 7, 20, 14, 0, 0)
            .single()
            .unwrap();
        let end = start + Duration::hours(1);
        let mut with_alert = draft("Review", start, end);
        with_alert.reminder_minutes = Some(15);

        // The row Calix caches right after creating the event on Google is the
        // only copy of the alert — nothing sends it to the server, and the sync
        // that follows deliberately leaves the column alone.
        store
            .upsert_google_event(calendar_id, "evt-new", &with_alert, &[])
            .unwrap();

        let events = store
            .events_between(start - Duration::hours(1), end)
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].reminder_minutes, Some(15));
    }

    #[test]
    fn creating_a_caldav_event_stores_the_alert_the_user_chose() {
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
        let start = Local
            .with_ymd_and_hms(2026, 7, 20, 14, 0, 0)
            .single()
            .unwrap();
        let end = start + Duration::hours(1);
        let mut with_alert = draft("Review", start, end);
        with_alert.reminder_minutes = Some(15);

        store
            .upsert_caldav_event(calendar_id, "/calendars/work/new.ics", &with_alert, &[])
            .unwrap();

        let events = store
            .events_between(start - Duration::hours(1), end)
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].reminder_minutes, Some(15));
    }

    #[test]
    fn expanded_recurring_occurrences_carry_the_masters_reminder() {
        let store = Store::open_in_memory().unwrap();
        let start = Local
            .with_ymd_and_hms(2026, 7, 6, 9, 0, 0)
            .single()
            .unwrap();
        let mut daily = draft("Standup", start, start + Duration::hours(1));
        daily.recurrence = Some(Frequency::Daily);
        daily.reminder_minutes = Some(5);
        store
            .create_event(store.default_calendar_id(), &daily)
            .unwrap();

        let window_start = start + Duration::days(10);
        let events = store
            .events_between(window_start, window_start + Duration::days(1))
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].reminder_minutes, Some(5));
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
                &[],
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
                &[],
            )
            .unwrap();
        store
            .upsert_google_event(
                home_calendar_id,
                "shared-id",
                &draft("Home event", start, end),
                &[],
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
            .upsert_google_event(calendar_id, "keep", &draft("Keep", start, end), &[])
            .unwrap();
        store
            .upsert_google_event(calendar_id, "gone", &draft("Gone", start, end), &[])
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
            .upsert_google_event(calendar_id, "gone-1", &draft("Gone 1", start, end), &[])
            .unwrap();
        store
            .upsert_google_event(calendar_id, "gone-2", &draft("Gone 2", start, end), &[])
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
    fn pruning_spares_every_instance_under_a_resource_that_could_not_be_read() {
        // The sync couldn't read one instance of series.ics, so it can't say
        // which of the cached instances the server dropped — all of them stay.
        let store = Store::open_in_memory().unwrap();
        let account_id = store
            .upsert_icloud_account("person@example.com", "person@example.com", "token")
            .unwrap();
        let calendar_id = store
            .upsert_caldav_calendar(account_id, "/calendars/work/", "Work", "#ff9500", true)
            .unwrap();
        let start = Local::now();
        let end = start + Duration::hours(1);
        for id in [
            "/calendars/work/series.ics#20260709T183000Z",
            "/calendars/work/series.ics#20260716T183000Z",
            "/calendars/work/gone.ics",
        ] {
            store
                .upsert_caldav_event(calendar_id, id, &draft("Series", start, end), &[])
                .unwrap();
        }

        store
            .prune_caldav_events(
                calendar_id,
                &[],
                &["/calendars/work/series.ics".to_string()],
                start - Duration::minutes(1),
                end + Duration::minutes(1),
            )
            .unwrap();

        let mut ids = store
            .events_between(start - Duration::minutes(1), end + Duration::minutes(1))
            .unwrap()
            .into_iter()
            .filter_map(|event| event.icloud_event_id)
            .collect::<Vec<_>>();
        ids.sort();
        assert_eq!(
            ids,
            vec![
                "/calendars/work/series.ics#20260709T183000Z".to_string(),
                "/calendars/work/series.ics#20260716T183000Z".to_string(),
            ],
            "the protected resource's instances stay; the absent one goes"
        );
    }

    #[test]
    fn pruning_still_removes_an_event_the_server_no_longer_lists() {
        let store = Store::open_in_memory().unwrap();
        let account_id = store
            .upsert_icloud_account("person@example.com", "person@example.com", "token")
            .unwrap();
        let calendar_id = store
            .upsert_caldav_calendar(account_id, "/calendars/work/", "Work", "#ff9500", true)
            .unwrap();
        let start = Local::now();
        let end = start + Duration::hours(1);
        store
            .upsert_caldav_event(
                calendar_id,
                "/calendars/work/kept.ics",
                &draft("Kept", start, end),
                &[],
            )
            .unwrap();
        store
            .upsert_caldav_event(
                calendar_id,
                "/calendars/work/gone.ics",
                &draft("Gone", start, end),
                &[],
            )
            .unwrap();

        store
            .prune_caldav_events(
                calendar_id,
                &["/calendars/work/kept.ics".to_string()],
                &[],
                start - Duration::minutes(1),
                end + Duration::minutes(1),
            )
            .unwrap();

        let events = store
            .events_between(start - Duration::minutes(1), end + Duration::minutes(1))
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].title, "Kept");
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
                &[],
            )
            .unwrap();
        store
            .upsert_google_event(
                calendar_id,
                "stale-current-event",
                &draft("Stale", now, now + Duration::hours(1)),
                &[],
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
