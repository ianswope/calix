# Contributing to Calix

Thanks for your interest! Calix is early and moving fast, and contributions are
very welcome — code, bug reports, packaging help, and design feedback alike.

## Getting started

You need a Rust toolchain and GTK4 (≥ 4.14) + libadwaita (≥ 1.5) development
headers (Arch: `gtk4`, `libadwaita`; Debian/Ubuntu: `libgtk-4-dev`,
`libadwaita-1-dev`).

```sh
cargo build
cargo test
cargo run
```

The [Architecture section of the README](README.md#architecture) is a
file-by-file map of the codebase — start there. Date math (`date_util.rs`) and
storage (`store.rs`) are plain Rust with unit tests and no GTK dependency, so
they're the easiest places to make a first change.

## Finding something to work on

Check the [issue tracker](https://github.com/ianswope/calix/issues) — issues
labeled `good first issue` are scoped for a first contribution, and `help
wanted` marks the features I'd most like help with. If you want to build
something that isn't filed yet, open an issue first so we can agree on the
approach before you invest time in it.

## Before you open a PR

CI runs these, so save yourself a round trip:

```sh
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

A few conventions:

- Keep pure logic (date math, sync bookkeeping, storage) out of the GTK layer
  and unit test it — see `date_util.rs` and `store.rs` for the pattern.
- Small, focused PRs are much easier to review than big ones.
- Match the style of the surrounding code.

## Testing sync without real accounts

Any CalDAV server works for testing the sync engine. The quickest local
option is [Radicale](https://radicale.org/) — a pip-installable CalDAV server
you can point a Calix CalDAV account at (`http://localhost:5232`).

Google sync requires your own OAuth client (see the README); iCloud requires
an Apple account with an app-specific password. Neither is needed for most
development — the CalDAV engine (`src/caldav.rs`) is shared, so Radicale
exercises the same code paths iCloud uses.

## Reporting bugs

Open an issue with your distro, how you installed Calix, what you expected,
and what happened. If it's a sync issue, say which provider (Google / iCloud /
other CalDAV) and whether the calendar was read-only or writable.
