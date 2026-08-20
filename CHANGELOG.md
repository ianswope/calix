# Changelog

## Unreleased

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
