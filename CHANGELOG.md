# Changelog

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
