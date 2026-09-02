# Changelog

## Unreleased

### Added

- Copy, cut and paste events. **Ctrl+C** copies the selected event and
  **Ctrl+X** cuts it (**Copy** in the event popover does the same); **Ctrl+V**
  pastes onto the selected slot, and **Paste Event** in the right-click menu
  pastes where you clicked. Pasting onto an hour in week or day view puts the
  event at that hour; pasting onto a month cell keeps its own time of day. The
  copy goes back to the calendar it came from — pushed to Google/iCloud/CalDAV
  when that calendar is synced — and every part of it can be taken back with
  Ctrl+Z. A repeat rule and a guest list are left behind, so a paste is one
  ordinary event rather than a second series or an unsent invitation, and
  cutting a repeating event is refused rather than guessed at.
- A visible selection. Clicking an event rings it; clicking empty calendar
  space highlights that slot — the day in month view, the hour in week or day
  view — and that highlight is where Ctrl+V will paste. **Esc** clears both and
  leaves the clipboard alone. Both survive the redraws that follow a sync or an
  edit.
- Location suggestions while typing. The **Location** field offers places this
  calendar has already used, then addresses from the Photon geocoder
  (OpenStreetMap data, no API key). Arrow keys and Enter pick one. Only the
  typed prefix leaves the machine, and only after a pause in typing;
  `[places] enabled = false` in `config.toml` turns that half off and leaves the
  local suggestions working offline, while `[places] endpoint` points it at a
  self-hosted geocoder instead.
- Undo and redo. **Ctrl+Z** takes back creating, editing, moving, resizing or
  deleting an event; **Ctrl+Shift+Z** (or Ctrl+Y) puts it back. Changes on
  synced calendars are undone on the provider too, so the next sync doesn't
  quietly reverse them.
- An undo only applies while the event still holds what the change wrote. If
  something else has edited it since — including a sync, or an invitation
  response arriving — Calix says so and leaves the newer version alone.
- Two command lines for reading the calendar from something else:
  `calix --agenda [FROM [THROUGH]]` prints the appointments in a range as JSON,
  and `calix --calendars` prints the calendars currently shown. Both answer
  before the window is touched, so neither opens one or keeps the app alive,
  and both read the database read-only so a running Calix is undisturbed. Meant
  for a status-bar widget on a refresh timer; see the README for the row shape
  and the error codes.

### Changed

- A single click on empty calendar space now selects that slot instead of
  opening the new-event dialog; **double-click** creates. A calendar with no
  way to point at an empty day has nowhere for a paste to land, and every other
  way of creating an event — double-click, drag across the grid, the right-click
  menu, the **+** button, Ctrl+N — is unchanged.

### Fixed

- Replying to an iCloud or CalDAV invitation no longer corrupts the event on
  the server. The reply was being spliced into the guest's address instead of
  replacing their response, so Accept, Maybe and Decline sent back an invalid
  guest line.
- CalDAV servers that write their XML with an unexpected namespace prefix are
  read correctly. Their responses used to be missed entirely, which made a sync
  treat every cached event — and every calendar — as deleted on the server.
- Editing a synced event that was created with a duration rather than an end
  time now writes a valid event; it used to carry both.
- Editing a repeating CalDAV event that the server handed back unexpanded is
  refused with an explanation, instead of silently re-anchoring the whole
  series on the occurrence that was clicked and dropping its time zone.
- **All events** edits that move a series across a daylight-saving change land
  where they were dragged: an all-day series moves a whole day rather than
  none, and a timed series keeps its hour rather than shifting by one.
- A CalDAV event at a time the clocks skipped (2:30 AM on a spring-forward day)
  is placed an hour later, the way the iCalendar standard and Apple Calendar
  place it, instead of being left out of the sync.
- Calendar and event names carrying `&`, `<` or an apostrophe as an XML entity
  decode correctly, numeric entities included.

### Known gaps

- An event deleted from a synced calendar can't be restored yet; Calix says so
  rather than putting back a local row the next sync would delete again.
- Whole-series operations ("All events", and creating a repeating event) are not
  recorded, since reversing one means rewriting the provider's recurrence rule.

## 0.6.0 — 2026-08-21

Calix can now answer invitations, invite people to new Google events,
and keep syncing and alerting after its window closes.

### Added

- Invitations identified as belonging to the connected account can be accepted,
  declined, or marked tentative from the event popover. Responses are written
  back to Google Calendar and to CalDAV resources that expose a matching attendee.
- New Google events can invite email addresses and ask Google to send updates.
- Calix can start in the background at login so automatic sync and local event
  alerts continue after its window closes. The option lives in the account center.

## 0.5.1 — 2026-08-20

A fix release. In 0.5.0, Calix stopped syncing online accounts after the
first launch; anyone running 0.5.0 should update.

### Fixed

- Online accounts sync again. In 0.5.0 the sync at launch, the periodic
  background sync, the sync after waking from suspend, and the refresh after
  editing a repeating remote event all stopped running once an account was
  connected, and the account controls in the sidebar stopped responding.
- **Update sign-in** for Google now reports a mismatch instead of quietly
  connecting a second account when a different Google account signs in.
- Starting a Google sign-in while one is already open in the browser is
  refused rather than opening a second one.
- The account list no longer describes an unreadable last-sync time as a
  successful automatic sync.
- Messages that still pointed at the removed per-provider Add buttons now
  name the single **Connect an account** action.

### Security

- The database's write-ahead log and shared-memory files are created with
  owner-only permissions. In 0.5.0 only the database file itself was
  restricted, leaving cached event data readable by other local accounts.

## 0.5.0 — 2026-08-20

Calix 0.5.0 makes installation and account setup substantially easier for
people who do not want to manage developer tooling or calendar protocols.

### Added

- A first-run welcome flow with one **Connect an account** entry point.
- Friendly setup choices for Google Calendar, Apple iCloud, Fastmail,
  Nextcloud, and other calendar servers.
- In-app Google OAuth client setup with secure owner-only configuration files.
- An account center with persistent last-sync time, failure state, retry,
  credential updates, and disconnect actions.
- A graphical database recovery screen with retry, data-location, and
  diagnostic-copy actions.
- Automated release archive installation and removal checks in CI.
- A matching uninstaller inside release archives.

### Changed

- Automatic sync is emphasized instead of separate provider-specific sync
  controls.
- Known providers request only the information users need to supply; Fastmail
  no longer exposes its server URL or insecure HTTP settings.
- Account, provider, recovery, and privacy language is clearer throughout the
  application and documentation.
- Release archives use an absolute desktop-launch path and preserve user data
  explicitly during upgrades and removal.
- Homebrew, AUR, Flatpak, and runtime-prerequisite documentation is more
  accurate about current support and dependencies.

### Security and reliability

- Calix enforces owner-only permissions on its data directory, SQLite
  database, and saved Google configuration.
- Malformed configuration and database startup failures now produce
  actionable graphical diagnostics instead of silent fallback or termination.
- Packaging validation checks desktop metadata, AppStream metadata, installed
  assets, binary version output, and complete uninstall symmetry.
