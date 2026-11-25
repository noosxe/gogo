//! Which settings a skin may change (design 26 §4.9, S21).
//!
//! `settings.set` from a skin takes only keys on [`SKIN_SETTABLE`]: keys that change looks
//! or comfort, never safety. Every other key is changed only through the app's own settings
//! route. Among those, a key is *app-only* when it gates an action or weakens a
//! confirmation; auth settings are also left out of the `settings` source (§3.7).

/// The keys `settings.set` accepts from a skin in contract 1.0. The list only grows in
/// minor versions (§8).
pub const SKIN_SETTABLE: &[&str] = &[
    "gui.skin",
    "gui.mode",
    "gui.reduced_motion",
    "gui.big_screen",
    "download.limit",
];

/// Key prefixes that are app-only whatever the key: auth settings.
pub const APP_ONLY_PREFIXES: &[&str] = &["auth."];

/// Exact keys that are app-only.
///
/// `cloud_saves.writes` is the cloud-writes gate on `saves.push`/`saves.sync`. Today the
/// gate is only the `GOGO_CLOUD_WRITES=1` environment variable; the key is reserved here
/// so S21 covers it as soon as a settings route can change it.
pub const APP_ONLY: &[&str] = &["cloud_saves.writes"];

/// Whether a skin may bind `settings.set` to `key` (S21). The validator rejects a route
/// that binds any other key.
pub fn skin_may_set(key: &str) -> bool {
    SKIN_SETTABLE.contains(&key)
}

/// Whether `key` is app-only: it gates an action or weakens a confirmation.
pub fn is_app_only(key: &str) -> bool {
    APP_ONLY.contains(&key) || APP_ONLY_PREFIXES.iter().any(|p| key.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_list_is_exact() {
        assert!(skin_may_set("gui.mode"));
        assert!(skin_may_set("download.limit"));
        for key in [
            "download.workers",
            "auth.token_store",
            "gui",
            "gui.mode.x",
            "GUI.MODE",
            "",
        ] {
            assert!(!skin_may_set(key), "{key}");
        }
    }

    #[test]
    fn nothing_about_cloud_writes_is_skin_settable() {
        // S21: the cloud-writes gate must never become skin-settable.
        for key in SKIN_SETTABLE {
            assert!(!key.contains("cloud"), "{key}");
            assert!(!APP_ONLY.contains(key), "{key}");
        }
    }

    #[test]
    fn no_app_only_key_is_skin_settable() {
        assert!(is_app_only("auth.token_store"));
        assert!(is_app_only("cloud_saves.writes"));
        assert!(!skin_may_set("cloud_saves.writes"));
        for key in SKIN_SETTABLE {
            assert!(!is_app_only(key), "{key}");
        }
    }
}
