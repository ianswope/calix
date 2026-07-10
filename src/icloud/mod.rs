pub mod credentials;

/// iCloud's CalDAV entry point. iCloud is just a CalDAV provider with a fixed
/// server URL and app-specific-password auth, so its sync/edit paths build a
/// [`crate::caldav::Credentials`] pointed here and reuse the generic engine.
pub const ICLOUD_CALDAV_ROOT: &str = "https://caldav.icloud.com/";
