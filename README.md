# Calix

A calendar app for Linux, built because [GNOME Calendar](https://apps.gnome.org/Calendar/) doesn't cut it and Apple Calendar isn't an option here. Native GTK4 + libadwaita, swipeable month/week views, and (eventually) direct sync with Apple/iCloud and Google calendars.

**Status: early days.** The swipeable month/week grid works; calendar sync is not built yet.

## Building

Requires a Rust toolchain and GTK4 + libadwaita development headers (on Arch: `gtk4`, `libadwaita`).

```sh
cargo build
cargo run
```

## Architecture

- `src/date_util.rs` — pure date-math helpers (month grids, week ranges, month/week shifting), unit tested independent of any GTK state.
- `src/views/month_view.rs`, `src/views/week_view.rs` — build a single month-grid or week-grid page for a given anchor date.
- `src/window.rs` — owns the `AdwCarousel` paging between prev/current/next pages, the header bar (Today / prev / next / Month-Week toggle), and the current view-mode + date state.
- `src/style.rs` — the app's small CSS (today badge, cell borders, the "now" line).

### A carousel gotcha worth knowing

Page navigation deliberately avoids `AdwCarousel::scroll_to()`. In the libadwaita version this was built against, `scroll_to()` is unreliable when the target widget was just appended in the same call — it silently leaves the carousel on the wrong page some fraction of the time rather than erroring. Instead, `Ui::reset()` in `window.rs` clears the carousel and repopulates it via `append` (making position 0 correct by construction, since it's the only child) followed by `prepend` for the previous page — no jump is ever requested, so there's nothing to fail.

## Roadmap

- [x] Swipeable month/week grid
- [ ] Local event storage (SQLite) + create/edit events
- [ ] Apple/iCloud calendars via CalDAV (app-specific password)
- [ ] Google Calendar via OAuth + REST
- [ ] Packaging (AUR, Flatpak)

## License

MIT
