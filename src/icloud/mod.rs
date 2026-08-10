pub mod credentials;

/// iCloud's CalDAV entry point. iCloud is just a CalDAV provider with a fixed
/// server URL and app-specific-password auth, so its sync/edit paths build a
/// [`crate::caldav::Credentials`] pointed here and reuse the generic engine.
pub const ICLOUD_CALDAV_ROOT: &str = "https://caldav.icloud.com/";

/// Deep link to the Apple Account section that generates app-specific
/// passwords. Signed-out visitors land on the sign-in page and arrive here
/// afterwards, which is the best available — Apple has no signed-out anchor for
/// the section itself.
pub const APP_PASSWORD_URL: &str = "https://account.apple.com/account/manage/section/security";

/// The shape Apple issues: sixteen letters, shown in four groups.
const APP_PASSWORD_LEN: usize = 16;

/// Canonicalizes an app-specific password, or returns `None` if `input` isn't
/// one at all.
///
/// Apple displays the password grouped (`abcd-efgh-ijkl-mnop`) and users reach
/// it by every route that mangles it: pasting with a trailing newline, typing
/// it without the hyphens, copying the groups separated by spaces, or letting a
/// phone keyboard capitalize the first letter. All of those are the same
/// secret, and all of them fail as an indistinguishable 401 today.
///
/// `None` is deliberately *not* treated as "reject this input" by the caller —
/// it drives a hint, not a block. Apple could change the format, and a client
/// that refuses to send anything unfamiliar would lock users out of their own
/// accounts. Being wrong here should cost a hint, never a connection.
pub fn normalize_app_password(input: &str) -> Option<String> {
    let stripped: String = input
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect();
    if stripped.len() != APP_PASSWORD_LEN || !stripped.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let lowered = stripped.to_ascii_lowercase();
    Some(
        lowered
            .as_bytes()
            .chunks(4)
            .map(|chunk| std::str::from_utf8(chunk).expect("ascii"))
            .collect::<Vec<_>>()
            .join("-"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const CANONICAL: &str = "abcd-efgh-ijkl-mnop";

    #[test]
    fn a_password_already_in_apples_format_is_left_alone() {
        assert_eq!(
            normalize_app_password(CANONICAL).as_deref(),
            Some(CANONICAL)
        );
    }

    #[test]
    fn a_password_typed_without_hyphens_is_grouped() {
        assert_eq!(
            normalize_app_password("abcdefghijklmnop").as_deref(),
            Some(CANONICAL)
        );
    }

    #[test]
    fn a_password_capitalized_by_a_phone_keyboard_is_lowered() {
        assert_eq!(
            normalize_app_password("Abcd-efgh-ijkl-mnop").as_deref(),
            Some(CANONICAL)
        );
        assert_eq!(
            normalize_app_password("ABCD-EFGH-IJKL-MNOP").as_deref(),
            Some(CANONICAL)
        );
    }

    #[test]
    fn a_password_pasted_with_stray_whitespace_survives() {
        for spelling in [
            "  abcd-efgh-ijkl-mnop\n",
            "abcd efgh ijkl mnop",
            "abcd - efgh - ijkl - mnop",
        ] {
            assert_eq!(
                normalize_app_password(spelling).as_deref(),
                Some(CANONICAL),
                "{spelling:?} is the same secret"
            );
        }
    }

    #[test]
    fn an_apple_account_password_is_not_mistaken_for_an_app_specific_one() {
        // The mistake this exists to catch: a real account password reaching
        // the field. Nothing about it should normalize into Apple's format.
        for not_one in ["hunter2", "correct horse battery staple", ""] {
            assert!(
                normalize_app_password(not_one).is_none(),
                "{not_one:?} should not read as an app-specific password"
            );
        }
    }

    #[test]
    fn a_wrong_length_or_non_letter_secret_is_rejected() {
        for not_one in [
            "abcd-efgh-ijkl-mno",   // fifteen
            "abcd-efgh-ijkl-mnopq", // seventeen
            "abcd-efgh-ijkl-mn0p",  // a digit, so not Apple's alphabet
        ] {
            assert!(
                normalize_app_password(not_one).is_none(),
                "{not_one:?} should not read as an app-specific password"
            );
        }
    }
}
