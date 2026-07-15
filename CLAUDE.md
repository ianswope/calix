# Calix

A GTK4 / libadwaita desktop calendar in Rust, with local storage (SQLite via
`rusqlite`) and optional Google Calendar and CalDAV (iCloud) sync.

## Test-driven development

Calix is developed test-first. For any change to logic — parsing, date math,
sync reconciliation, storage queries, layout arithmetic — follow red → green →
refactor:

1. **Red.** Write a failing test that names the behavior you want. Run it and
   watch it fail for the reason you expect (not a compile error or a typo).
2. **Green.** Write the least code that makes it pass.
3. **Refactor.** Clean up with the test as your safety net.

Do not add product logic without a test that would have failed before it.

### Running tests

```sh
cargo test               # whole suite — runs in well under a second
cargo test drag          # filter by substring (module or test name)
cargo test -- --nocapture   # show println! / dbg! output
```

The suite is fast enough to run on every save. If you have `cargo-watch`
installed, `cargo watch -x test` gives a live red/green loop; it's optional and
not a project dependency.

### Where tests live

Unit tests sit in a `#[cfg(test)] mod tests { use super::*; }` block at the
bottom of the module they cover — this is what lets them reach private
functions. Name tests as full sentences describing the behavior, matching the
existing suite:

```rust
#[test]
fn moved_all_day_draft_keeps_its_calendar_day_span() { ... }
```

Build fixtures with small local helpers (see `d(y, m, day)` in `date_util.rs`,
`test_event(...)` in `window.rs`) rather than repeating struct literals.

### Testing GTK code

GTK widgets need a display and can't be unit-tested in CI. The project's
convention — keep following it — is to **push logic out of widgets into pure
functions** and test those:

- `views/week_view.rs` — overlap/lane-splitting math, tested without widgets.
- `views/drag.rs` — snap-to-grid, minute→time, time formatting, drag-payload
  parsing.
- `window.rs` — draft move/resize arithmetic, including DST edge cases.
- `omarchy.rs` — hex parsing and color mixing for theme overrides.
- `store.rs` — open an in-memory DB with `Store::open_in_memory()` and assert
  on real queries; no filesystem or network needed.

If a change lives inside a widget callback and feels untestable, that's the
signal to extract the decision into a free function and test it there. Wiring
(signal connections, layout) stays thin and is verified by running the app.

## CI gates

CI (`.github/workflows/ci.yml`) runs, in order and all required:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

Run all three locally before committing. A local pre-commit hook also blocks
unformatted commits. Clippy warnings are hard errors here — fix them, don't
`#[allow]` them without a reason.
