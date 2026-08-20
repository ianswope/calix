# Onboarding UX audit — 2026-08-20

Scope: a clean-profile, keyboard-driven audit of first launch, provider setup,
and startup recovery on the current Omarchy/Wayland desktop.

## Flow evidence

1. **First launch — healthy.** `01-first-launch.png` shows a single account
   entry point, recognizable services, clear privacy reassurance, and a
   distinct local-only path.
2. **Google setup — healthy with an external dependency.**
   `02-google-setup.png` shows the complete advanced setup, in-app credential
   storage, Testing-mode warning, and help link without scrolling. A real
   Google Cloud OAuth client is still required.
3. **Google validation focus — needs follow-up accessibility testing.**
   `03-google-keyboard-focus.png` confirms keyboard focus reaches password-row
   controls. Native entry-row icons add stops before the primary action.
4. **Apple iCloud setup — healthy.** `04-icloud-setup.png` clearly distinguishes
   an app-specific password from the normal Apple Account password and links
   directly to Apple's settings.
5. **Fastmail setup — healthy.** `05-fastmail-setup.png` asks only for username
   and app password; the known server address and insecure HTTP control are no
   longer exposed.
6. **Database startup recovery — healthy.** `06-startup-recovery.png` preserves
   the user's sense of safety and provides retry, data-location, and diagnostic
   actions instead of terminating silently.

## Findings addressed during the audit

- Moved the welcome flow's local-only choice out of the header and made it an
  explicit secondary action.
- Hid protocol-level Fastmail fields that Calix already knows.
- Shortened the Apple iCloud title so it does not truncate.
- Added a non-unique developer launch mode for reproducible clean-profile QA.
- Made startup recovery open the closest existing data location when the
  intended database directory could not be created.

## Evidence limits

- No real Google, Apple, Fastmail, or Nextcloud credentials were entered, so
  provider consent pages and successful remote sync were not captured.
- The audit session's AT-SPI accessibility bus was unavailable. Keyboard
  traversal was exercised, but screen-reader names, roles, announcements,
  contrast ratios, and full WCAG conformance were not verified.
- Window translucency comes from the active desktop compositor configuration;
  screenshots show the resulting readability risk but do not establish the
  experience on opaque or differently themed desktops.
