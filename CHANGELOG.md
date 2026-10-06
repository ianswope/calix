# Changelog

## Unreleased

Scrolling in week and day view stays where it was put: a resize, a sync, a
save, a drag, or a step to the next week no longer moves it, a wheel detent
glides instead of jumping, and a quick double press of an arrow moves two
periods.

### Added

- Mouse-wheel scrolling of the week and day grid is eased over a few frames
  instead of jumping a step per detent. The distance per detent is GTK's own,
  so the wheel covers the same ground as before; touchpad scrolling, Shift+wheel
  paging, and a grid that already fits the window are untouched.

### Fixed

- Week and day view no longer snap back to the bottom of the day when the
  window is resized. From early afternoon on, the hour a page opens at — two
  hours before now — can't reach the top of the viewport, and the handler
  placing it kept watching for a range that would never come, re-placing and
  so re-clamping to the bottom on every later change to the grid's range. On a
  tiling desktop that is every window opened or closed beside Calix. The page
  now opens as far toward its hour as it can go and is then left alone. The
  opening position is also set before the grid is first laid out, so the first
  painted frame is already at the right hour rather than placed during it.
- A sync landing, a save, a drag, an undo, or a calendar toggled in the
  sidebar no longer throws the view back to "now". Every rebuild re-landed on
  the page's opening position, so an evening scrolled to was lost whenever the
  quarter-hourly sync finished, and an event dragged to 7 PM could scroll out
  of view the moment it was dropped. Rebuilds the user didn't navigate to keep
  the hours on screen; Today, a picked date, and a change of view still land
  where they always did.
- Swiping or stepping to the next week shows it at the same hours. The
  neighbouring pages opened at "now" or 8 AM regardless of where the visible
  page had been scrolled, so the hour axis jumped with every change of period.
  The three pages now scroll together, and a page built to replace one keeps
  the hours on screen.
- Pressing an arrow twice quickly moves two periods. A press during the step
  animation was swallowed, because the carousel was already heading for that
  page, and a press in the frames after it landed rebuilt all three pages under
  the user's eyes. Presses that arrive while a step is in flight are queued and
  applied once it lands; a run of them jumps in one rebuild.
- A backward swipe recycles its far page at once instead of after a 180 ms
  wait. libadwaita emits `page-changed` only once the scroll animation has
  finished, so there was no animation to wait out — only a window in which the
  carousel wasn't trusted and a second swipe or press was ignored.
- Opening an event from search lands the week or day grid an hour above the
  event instead of at "now".

## 0.7.0 — 2026-09-08

Events can be copied onto another day, changes taken back with Ctrl+Z, and
locations completed while they're typed. Seven CalDAV sync bugs are fixed, and
the calendar stops leaking the widgets it builds.

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

- The previous/next arrows and Ctrl+← / Ctrl+→ no longer flash the wrong
  period on the way. They rebuilt all three carousel pages, destroying and
  re-creating the very page being looked at, while a swipe of the same distance
  reused it. They now animate onto the neighbouring page the carousel is
  already holding, exactly as a swipe does, and fall back to the rebuild only
  when there is no trustworthy page to move onto.
- Week and day view no longer flick through midnight on the way to the hour
  they open at. The scroll position was applied from an idle callback, which
  runs at a lower priority than GTK's redraw, so the grid was painted at the
  top of the day and only then jumped. An idle that ran before the grid was
  measured did nothing at all, leaving that page at midnight for good.
- Closing and reopening the window while background alerts hold the process no
  longer starts a second set of alert, sync and logind loops on top of the ones
  already running.
- A failed alert query no longer advances the checkpoint past the events it
  never managed to read, which silently skipped their alerts.
- Answering a Google invitation tells the other guests, instead of changing the
  reply where only the organiser would ever see it.
- CalDAV events the server has cancelled are skipped rather than cached as
  ordinary appointments.
- A sync no longer overwrites a local edit that hasn't been pushed yet.
- A calendar's show/hide switch goes back to where it was when the store
  refuses the change, rather than showing a state that was never saved.
- An alert set on a recurring event is carried onto the occurrences a sync
  expands, so it survives the first sync after it was set.
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
- Memory no longer grows for as long as Calix is open. Every calendar page,
  event popover, search popover and event dialog held a reference to itself
  from one of its own handlers, so nothing built for the grid was ever freed —
  and the grid is rebuilt after every sync, edit and navigation. Pinch-zoom was
  the worst case, leaking a full day grid per frame. The window's own state
  went the same way, which in background mode left a closed window's sync,
  alert and clock timers running for the life of the process.
- A Google sign-in can no longer be left unfinishable by a browser that opens
  a connection to the sign-in listener and sends nothing.
- Connecting a Google account no longer overwrites a `config.toml` that Calix
  couldn't parse. It reports the problem instead, leaving whatever else the
  file holds intact.

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
