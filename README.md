# Calix

A calendar app for Linux, built after moving to [Omarchy](https://omarchy.org/) and wanting the kind of native calendar experience I had on a Mac. [GNOME Calendar](https://apps.gnome.org/Calendar/) doesn't cut it, and Apple Calendar isn't an option here. Native GTK4 + libadwaita, swipeable month/week views, and direct sync with Apple/iCloud and Google calendars.

**Status: early days.** The swipeable month/week/day grid works, events are stored locally (SQLite) with create/edit/delete, and Google/iCloud sync can pull calendars from multiple accounts into the grid. Connected calendars can be shown/hidden from the calendar sidebar. Events can be created by clicking or right-clicking anywhere on the grid, on local, Google, or iCloud calendars; synced events can be edited or deleted. Events drag to another day in the month grid, and move or resize directly in the week/day grid with a snapped live preview — including synced events, which push the change back to Google/iCloud. Grid text steps down a size when the window is narrow.

## Building

Requires a Rust toolchain and GTK4 + libadwaita development headers (on Arch: `gtk4`, `libadwaita`).

```sh
cargo build
cargo test
cargo run
```

## Homebrew

Until the first tagged release is published, install the current development
build from this repository's tap:

```sh
brew tap ianswope/calix https://github.com/ianswope/calix
brew install --HEAD ianswope/calix/calix
```

This installs the `calix` binary and the desktop entry/icon. A tagged release
will replace the `--HEAD` formula with a checksum-pinned stable package.

## Flatpak and AUR

The Flatpak manifest is in `flatpak/com.ianswope.Calix.json`. Before building,
generate its dependency manifest with:

```sh
scripts/generate-flatpak-sources.sh
flatpak-builder --user --install --force-clean build-dir flatpak/com.ianswope.Calix.json
```

`packaging/aur/PKGBUILD` is the release package definition for Arch users. It
is pinned to the current release version when publishing to the AUR; replace
its temporary `SKIP` checksum with the SHA-256 for the tagged source archive.

## Installing Locally

For a user-local install from a checkout:

```sh
scripts/install-local.sh
```

This builds `target/release/calix` and installs:

- `~/.local/bin/calix`
- `~/.local/share/applications/com.ianswope.Calix.desktop`
- `~/.local/share/icons/hicolor/scalable/apps/com.ianswope.Calix.svg`

Uninstall with:

```sh
scripts/uninstall-local.sh
```

## Release Tarball

To build a Linux release archive:

```sh
scripts/build-release.sh
```

The archive is written to `target/dist/calix-<version>-linux-<arch>.tar.gz`. It contains the release binary, desktop entry, icon, docs, and an `install.sh` script that installs to `~/.local` by default. Users still need GTK4 + libadwaita runtime libraries available from their distribution.

## Connecting Google Calendar

Google requires every app to bring its own OAuth client — there's no shared one you can just use. Setup takes about 10 minutes:

1. Create a project at [console.cloud.google.com](https://console.cloud.google.com) and enable the **Google Calendar API** for it.
2. Under **Google Auth Platform → Audience**, set the app to External, and add your own Google account under **Test users** (the app stays unverified/"Testing," which is fine for personal use — publishing for public verification is a separate, much heavier process not needed here).
3. Under **Data Access**, add the `.../auth/calendar` scope.
4. Under **Clients**, create an OAuth client of type **Desktop app**. Copy the Client ID and Client Secret.
5. Create `~/.config/calix/config.toml`:
   ```toml
   [google]
   client_id = "your-client-id.apps.googleusercontent.com"
   client_secret = "your-client-secret"
   ```
   This file lives outside the repo and is never read by anything that gets committed — each user (or contributor) needs their own.
6. Run Calix, open the calendar sidebar, and click **Add Google** in the Accounts section. It opens your browser for the Google consent screen; once approved, the refresh token is saved to your system keyring (via Secret Service — GNOME Keyring, KWallet, etc.), not to a file. Repeat this for each Google account you want to connect, then use **Sync Google** to refresh all connected accounts.

If you previously connected Google before Calix had multi-account storage, **Sync Google** will try to migrate that older saved token into the new account model.

## Connecting iCloud Calendar

iCloud uses CalDAV with an Apple app-specific password:

1. Sign in at [account.apple.com](https://account.apple.com).
2. Under **Sign-In and Security → App-Specific Passwords**, generate a password for Calix.
3. In Calix, open the calendar sidebar and click **Add iCloud** in the Accounts section.
4. Enter your Apple Account email and the app-specific password. The password is saved to your system keyring, not to a file.
5. Use **Sync iCloud** to refresh connected iCloud accounts.

Synced iCloud events can be edited or deleted when they are simple `.ics` resources. Expanded recurring iCloud instances are still read-only until recurrence exceptions are implemented.

## Using Calendars

The left sidebar lists local calendars and synced Google/iCloud calendars. Use the switch next to each calendar to show or hide it in the month/week/day grid. Remote calendar visibility is local and is preserved across later syncs.

The calendar button in the header toggles the sidebar. The sidebar's Accounts section contains **Add Google**, **Sync Google**, **Add iCloud**, and **Sync iCloud**.

### Working with events

- **Create**: click an empty slot (day cell in month view, hour cell in week/day view), right-click any empty spot for a **New Event** menu at that exact quarter-hour, or use the **+** header button.
- **Pick a calendar**: the new-event dialog's calendar dropdown lists only the calendars currently visible in the sidebar; **Show all calendars…** at the bottom expands it to everything. Hiding noisy subscribed calendars once keeps the picker short.
- **Move and resize**: in week/day view, drag an event's body to move it, or its top/bottom edge to resize, with a live preview snapped to 15 minutes; dragging against the top or bottom of the grid auto-scrolls to off-screen hours. In month view, drag a chip to another day. Changes to synced events are pushed back to Google/iCloud, and roll back if the remote update fails.
- **Edit**: click any event to open it.

## Architecture

- `src/date_util.rs` — pure date-math helpers (month grids, week ranges, month/week shifting), unit tested independent of any GTK state.
- `src/views/month_view.rs`, `src/views/week_view.rs` — build a single month-grid or week-grid page for a given anchor date; `src/views/mod.rs` holds shared helpers like the right-click New Event menu.
- `src/views/event_widget.rs` — the event chip/block widgets shared by the views.
- `src/views/drag.rs` — direct-manipulation move/resize for timed blocks in the week/day grid: a `GestureDrag` controller with a snapped live preview and edge auto-scroll, committing only on release (month-view drags use GTK's regular drag-and-drop instead).
- `src/window.rs` — owns the `AdwCarousel` paging between prev/current/next pages, the header bar (Today / prev / next / Month-Week-Day toggle / New Event / Calendars), sidebar account actions, and the current view-mode + date state.
- `src/style.rs` — the app's small CSS (today badge, cell borders, the "now" line, drag preview, and the compact text sizes applied below the window-width breakpoint).
- `src/store.rs` — SQLite-backed account/calendar/event storage (create/list/update/delete), with in-memory-DB unit tests independent of the GUI.
- `src/calendar_dialog.rs` — reusable account/calendar list for the sidebar, including per-calendar visibility toggles.
- `src/event_dialog.rs` — the create/edit event dialog (`adw::Dialog` + `EntryRow`/`SwitchRow` form); its calendar picker defaults to sidebar-visible calendars with an expandable full list.
- `src/config.rs` — reads `~/.config/calix/config.toml` for user-supplied API credentials (currently just the Google OAuth client).
- `src/google/oauth.rs` — the OAuth2 + PKCE sign-in flow (loopback redirect, no embedded browser) and per-account refresh-token storage via the system keyring.
- `src/google/calendar_api.rs` — thin REST client over the Calendar API v3.
- `src/google/sync.rs` — fetches Google calendars and event windows, then upserts/prunes synced rows in SQLite. Google’s selected/hidden state is used only for a calendar’s initial Calix visibility; later sidebar choices are preserved.
- `src/icloud/` — CalDAV discovery/sync for iCloud calendars using an Apple app-specific password stored in the system keyring.

## Roadmap

- [x] Swipeable month/week grid
- [x] Local event storage (SQLite) + create/edit events
- [x] Google sign-in (OAuth + PKCE, verified by fetching the calendar list)
- [x] Pull Google events from multiple Google accounts into the month/week grid (one-way sync)
- [x] Show/hide connected calendars from a native sidebar
- [x] Pull iCloud events via CalDAV (one-way sync)
- [x] Basic two-way Google sync / editing synced Google events
- [x] Basic two-way iCloud CalDAV sync / editing simple synced iCloud events
- [x] Calendar picker for creating new events directly on Google/iCloud calendars
- [x] Drag to move/resize events in the week/day grid (snapped preview, edge auto-scroll)
- [x] Right-click to create an event at a specific spot
- [ ] Recurrence exception editing for expanded iCloud recurring events
- [ ] Recurring event creation
- [ ] Event alerts / desktop notifications
- [ ] Automatic background sync
- [ ] Event search
- [ ] Packaging (AUR, Flatpak)

## License

MIT
