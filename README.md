# Calix

A calendar app for Linux, built because [GNOME Calendar](https://apps.gnome.org/Calendar/) doesn't cut it and Apple Calendar isn't an option here. Native GTK4 + libadwaita, swipeable month/week views, and (eventually) direct sync with Apple/iCloud and Google calendars.

**Status: early days.** The swipeable month/week/day grid works, events are stored locally (SQLite) with create/edit/delete, and one-way Google/iCloud sync can pull calendars from multiple accounts into the grid. Connected calendars can be shown/hidden from the calendar sidebar. Synced remote events are currently read-only locally.

## Building

Requires a Rust toolchain and GTK4 + libadwaita development headers (on Arch: `gtk4`, `libadwaita`).

```sh
cargo build
cargo test
cargo run
```

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
6. Run Calix, open the calendar sidebar, and click **Add Google** in the Accounts section. It opens your browser for the Google consent screen; once approved, the refresh token is saved to your system keyring (via Secret Service — GNOME Keyring, KWallet, etc.), not to a file. Repeat this for each Google account you want to connect, then use **Sync Google** to refresh all connected accounts.

If you previously connected Google before Calix had multi-account storage, **Sync Google** will try to migrate that older saved token into the new account model.

## Connecting iCloud Calendar

iCloud uses CalDAV with an Apple app-specific password:

1. Sign in at [account.apple.com](https://account.apple.com).
2. Under **Sign-In and Security → App-Specific Passwords**, generate a password for Calix.
3. In Calix, open the calendar sidebar and click **Add iCloud** in the Accounts section.
4. Enter your Apple Account email and the app-specific password. The password is saved to your system keyring, not to a file.
5. Use **Sync iCloud** to refresh connected iCloud accounts.

Synced iCloud events are currently read-only in Calix, like synced Google events.

## Using Calendars

The left sidebar lists local calendars and synced Google/iCloud calendars. Use the switch next to each calendar to show or hide it in the month/week/day grid. Remote calendars are currently read-only in Calix, but their visibility choice is local and is preserved across later syncs.

The calendar button in the header toggles the sidebar. The sidebar's Accounts section contains **Add Google**, **Sync Google**, **Add iCloud**, and **Sync iCloud**.

This file lives outside the repo and is never read by anything that gets committed — each user (or contributor) needs their own.

## Architecture

- `src/date_util.rs` — pure date-math helpers (month grids, week ranges, month/week shifting), unit tested independent of any GTK state.
- `src/views/month_view.rs`, `src/views/week_view.rs` — build a single month-grid or week-grid page for a given anchor date.
- `src/window.rs` — owns the `AdwCarousel` paging between prev/current/next pages, the header bar (Today / prev / next / Month-Week-Day toggle / New Event / Calendars), sidebar account actions, and the current view-mode + date state.
- `src/style.rs` — the app's small CSS (today badge, cell borders, the "now" line).
- `src/store.rs` — SQLite-backed account/calendar/event storage (create/list/update/delete), with in-memory-DB unit tests independent of the GUI.
- `src/calendar_dialog.rs` — reusable account/calendar list for the sidebar, including per-calendar visibility toggles.
- `src/event_dialog.rs` — the create/edit event dialog (`adw::Dialog` + `EntryRow`/`SwitchRow` form).
- `src/config.rs` — reads `~/.config/calix/config.toml` for user-supplied API credentials (currently just the Google OAuth client).
- `src/google/oauth.rs` — the OAuth2 + PKCE sign-in flow (loopback redirect, no embedded browser) and per-account refresh-token storage via the system keyring.
- `src/google/calendar_api.rs` — thin REST client over the Calendar API v3.
- `src/google/sync.rs` — fetches Google calendars and event windows, then upserts/prunes synced rows in SQLite. Google’s selected/hidden state is used only for a calendar’s initial Calix visibility; later sidebar choices are preserved.
- `src/icloud/` — CalDAV discovery/sync for iCloud calendars using an Apple app-specific password stored in the system keyring.

### A carousel gotcha worth knowing

Page navigation deliberately avoids `AdwCarousel::scroll_to()`. In the libadwaita version this was built against, `scroll_to()` is unreliable when the target widget was just appended in the same call — it silently leaves the carousel on the wrong page some fraction of the time rather than erroring. Instead, `Ui::reset()` in `window.rs` clears the carousel and repopulates it via `append` (making position 0 correct by construction, since it's the only child) followed by `prepend` for the previous page — no jump is ever requested, so there's nothing to fail.

## Roadmap

- [x] Swipeable month/week grid
- [x] Local event storage (SQLite) + create/edit events
- [x] Google sign-in (OAuth + PKCE, verified by fetching the calendar list)
- [x] Pull Google events from multiple Google accounts into the month/week grid (one-way sync)
- [x] Show/hide connected calendars from a native sidebar
- [x] Pull iCloud events via CalDAV (one-way sync)
- [ ] Two-way Google sync / editing synced Google events
- [ ] Two-way iCloud CalDAV sync / editing synced iCloud events
- [ ] Packaging (AUR, Flatpak)

## License

MIT
