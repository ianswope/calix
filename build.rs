//! Bakes build provenance into the binary so a running Calix can say which
//! commit it came from. `~/.local/bin/calix` is a copy, not a symlink, so a
//! committed fix can sit uninstalled indefinitely with nothing to show for it;
//! `calix --version` plus `scripts/check-installed.sh` make that visible.

use std::process::Command;

fn main() {
    // Re-run when the checked-out commit changes, or when the index does (which
    // is what `git add` touches, so staging refreshes the -dirty marker).
    // A build straight from a tarball has no .git at all; the missing-path
    // directives are harmless.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");
    println!("cargo:rerun-if-changed=.git/refs");

    // `git describe` carries the nearest tag when there is one and falls back
    // to a bare short hash, so this reads well whether or not a release is
    // tagged. Anything unavailable becomes an empty string, which
    // `build_info::format_stamp` renders as "version only".
    let describe = git(&["describe", "--always", "--tags", "--dirty"]);
    let date = git(&["log", "-1", "--format=%cs"]);

    println!("cargo:rustc-env=CALIX_GIT_DESCRIBE={describe}");
    println!("cargo:rustc-env=CALIX_GIT_DATE={date}");
}

/// Runs a git command, returning its trimmed stdout, or an empty string if git
/// is missing, this isn't a repository, or the command fails. A build must
/// never fail just because provenance can't be determined.
fn git(args: &[&str]) -> String {
    Command::new("git")
        .args(args)
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|text| text.trim().to_string())
        .unwrap_or_default()
}
