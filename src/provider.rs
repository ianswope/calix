//! The three sync providers, and how each is named to the user.
//!
//! Google, iCloud and CalDAV each had their own copy of the add/sync UI, which
//! meant three copies of every label, tooltip and error string — and they had
//! already drifted. Naming lives here instead, so the shared account plumbing
//! can talk about a provider without a `match` per sentence.
//!
//! Deliberately GTK-free: `key` is the value stored in the `accounts.provider`
//! column, so this is as much storage vocabulary as it is presentation.

/// A sync provider: its stored key and the words used for it in the UI.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Provider {
    /// The `accounts.provider` value in SQLite.
    pub key: &'static str,
    /// How the provider is named in buttons, toasts and the account list.
    pub label: &'static str,
    /// The noun counted in the post-sync summary toast. Google's is bare
    /// because its toast predates the others and reads as the default.
    pub calendar_noun: &'static str,
}

pub const GOOGLE: Provider = Provider {
    key: "google",
    label: "Google",
    calendar_noun: "calendar",
};

pub const ICLOUD: Provider = Provider {
    key: "icloud",
    label: "iCloud",
    calendar_noun: "iCloud calendar",
};

pub const CALDAV: Provider = Provider {
    key: "caldav",
    label: "CalDAV",
    calendar_noun: "CalDAV calendar",
};

/// Every provider, in the order their controls appear in the sidebar.
pub const ALL: [Provider; 3] = [GOOGLE, ICLOUD, CALDAV];

impl Provider {
    /// The provider for a stored `accounts.provider` value.
    pub fn from_key(key: &str) -> Option<Provider> {
        ALL.into_iter().find(|provider| provider.key == key)
    }

    /// How to name a stored provider value in the account list, falling back to
    /// the raw value so a row written by a newer version still reads sensibly.
    pub fn label_for_key(key: &str) -> &str {
        match Provider::from_key(key) {
            Some(provider) => provider.label,
            None => key,
        }
    }

    /// Shown when a sync is asked for with nothing connected. There is one
    /// account entry point now, so it is named rather than a per-provider Add.
    pub fn none_connected(&self) -> String {
        format!(
            "No {} accounts connected. Use Connect an account first.",
            self.label
        )
    }

    pub fn sync_failed(&self, error: &str) -> String {
        format!("{} sync failed: {}", self.label, error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stored_provider_value_maps_to_its_provider() {
        assert_eq!(Provider::from_key("google"), Some(GOOGLE));
        assert_eq!(Provider::from_key("icloud"), Some(ICLOUD));
        assert_eq!(Provider::from_key("caldav"), Some(CALDAV));
        assert_eq!(Provider::from_key("fastmail"), None);
    }

    #[test]
    fn an_unknown_provider_value_is_shown_as_itself() {
        // A row from a newer version shouldn't render as blank in the list.
        assert_eq!(Provider::label_for_key("google"), "Google");
        assert_eq!(Provider::label_for_key("fastmail"), "fastmail");
    }

    #[test]
    fn the_nothing_connected_message_points_at_the_one_connect_action() {
        // The per-provider Add buttons these used to name are gone; a message
        // telling someone to press a button that isn't there is worse than none.
        assert_eq!(
            ICLOUD.none_connected(),
            "No iCloud accounts connected. Use Connect an account first."
        );
        assert_eq!(
            CALDAV.none_connected(),
            "No CalDAV accounts connected. Use Connect an account first."
        );
    }

    #[test]
    fn a_sync_failure_is_prefixed_with_the_provider() {
        assert_eq!(
            CALDAV.sync_failed("401 Unauthorized"),
            "CalDAV sync failed: 401 Unauthorized"
        );
    }
}
