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
///
/// The cloud-writes gate on `saves.push`/`saves.sync` is app-only too. Today it is the
/// `GOGO_CLOUD_WRITES=1` environment variable, not a config key; the GUI's key for it is
/// added to [`APP_ONLY`] when the GUI's settings route lands.
pub const APP_ONLY_PREFIXES: &[&str] = &["auth."];

/// Exact keys that are app-only. None in contract 1.0 beyond [`APP_ONLY_PREFIXES`].
pub const APP_ONLY: &[&str] = &[];

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
    fn no_app_only_key_is_skin_settable() {
        assert!(is_app_only("auth.token_store"));
        for key in SKIN_SETTABLE {
            assert!(!is_app_only(key), "{key}");
        }
    }
}
