//! Which build this is. See `build.rs` for where these values come from.

/// `git describe` output at build time, or empty when built outside a checkout.
const GIT_DESCRIBE: &str = env!("CALIX_GIT_DESCRIBE");
/// Commit date (`YYYY-MM-DD`) at build time, or empty.
const GIT_DATE: &str = env!("CALIX_GIT_DATE");
const CARGO_VERSION: &str = env!("CARGO_PKG_VERSION");

/// One line identifying this build, for `--version` and the startup log.
pub fn stamp() -> String {
    format_stamp(CARGO_VERSION, GIT_DESCRIBE, GIT_DATE)
}

fn format_stamp(version: &str, describe: &str, date: &str) -> String {
    // Without a commit there's nothing to match against `git log`, so a date on
    // its own is dropped rather than reported as if it identified the build.
    if describe.is_empty() {
        return version.to_string();
    }
    if date.is_empty() {
        return format!("{version} ({describe})");
    }
    format!("{version} ({describe} {date})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stamp_names_the_commit_and_its_date() {
        assert_eq!(
            format_stamp("0.4.0", "a2ab381", "2026-07-30"),
            "0.4.0 (a2ab381 2026-07-30)"
        );
    }

    #[test]
    fn a_dirty_build_says_so() {
        // `git describe --dirty` carries the marker itself; don't re-derive it.
        assert_eq!(
            format_stamp("0.4.0", "a2ab381-dirty", "2026-07-30"),
            "0.4.0 (a2ab381-dirty 2026-07-30)"
        );
    }

    #[test]
    fn a_build_outside_a_checkout_reports_just_the_version() {
        // Tarball and Flatpak builds have no .git; they must still build and
        // still report something truthful rather than a fabricated commit.
        assert_eq!(format_stamp("0.4.0", "", ""), "0.4.0");
    }

    #[test]
    fn a_commit_without_a_date_still_names_the_commit() {
        assert_eq!(format_stamp("0.4.0", "a2ab381", ""), "0.4.0 (a2ab381)");
    }

    #[test]
    fn a_date_without_a_commit_is_not_worth_reporting() {
        // A date alone can't be matched against `git log`, which is the whole
        // point of the stamp.
        assert_eq!(format_stamp("0.4.0", "", "2026-07-30"), "0.4.0");
    }
}
