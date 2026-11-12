//! What a game has set up on this machine, for `gogo list` (#378): a built app, a depot
//! cache, a downloaded offline installer. Read only: records already on disk, no network.

use std::path::Path;

use serde::Serialize;

use crate::gog::content::Lane;
use crate::macapp::apps::App;

/// A game built by `gogo linux-app` / `gogo mac-app`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Built {
    /// `linux` or `mac`.
    pub platform: &'static str,
    /// `depot` or `installer`; `None` when `content.toml` can't be read.
    pub lane: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub dlc_count: usize,
}

/// A game's depot cache (`.gogo-depot.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Cached {
    pub build_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

/// Everything a game has locally.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Local {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub built: Option<Built>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache: Option<Cached>,
    /// The downloaded offline installer's version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installer: Option<String>,
}

impl Built {
    pub fn from_app(app: &App) -> Built {
        let content = app.content.as_ref();
        Built {
            platform: if app.linux { "linux" } else { "mac" },
            lane: content.map(|c| match c.lane {
                Lane::Depot => "depot",
                Lane::Installer => "installer",
            }),
            version: content.and_then(|c| c.version.clone()),
            build_id: content.and_then(|c| c.build_id.clone()),
            language: content.and_then(|c| c.language.clone()),
            dlc_count: app.dlcs.len(),
        }
    }
}

/// The cache record in `root` (the game's cache dir), if readable.
pub fn read_cache(root: &Path) -> Option<Cached> {
    let r = crate::gog::depot::read_record(root)?;
    Some(Cached {
        build_id: r.build_id,
        language: r.language,
    })
}

impl Local {
    /// Whether anything is set up.
    pub fn any(&self) -> bool {
        self.built.is_some() || self.cache.is_some() || self.installer.is_some()
    }

    /// The compact cell of the `LOCAL` column; `None` when nothing is set up.
    ///
    /// The built app leads: `linux depot 2.31 en-US +2 DLC`: platform, lane, then the
    /// version (only a human version string, never a build id: depot builds have none on
    /// disk), the language (the build's, else the cache's when the cache is for the same
    /// build) and the DLC count; absent parts are left out. A cache for the built build
    /// is implied and not shown; one for another build follows as `+ cache (other build)`.
    /// An installer follows as `+ installer <version>`. Without an app: `cache <lang>`
    /// (`cache` alone with no language), and `✓ installer <version>` for an installer
    /// alone. Build ids stay in the JSON.
    pub fn label(&self) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        if let Some(b) = &self.built {
            let mut s = match b.lane {
                Some(l) => format!("{} {l}", b.platform),
                None => b.platform.to_string(),
            };
            // A cache that is for the built build says its language when the build
            // doesn't; with no build id on either side there is nothing to compare.
            let same = self
                .cache
                .as_ref()
                .filter(|c| b.build_id.as_deref().is_none_or(|id| id == c.build_id));
            let lang = b
                .language
                .clone()
                .or_else(|| same.and_then(|c| c.language.clone()));
            for e in [b.version.clone(), lang].into_iter().flatten() {
                s.push(' ');
                s.push_str(&e);
            }
            if b.dlc_count > 0 {
                s.push_str(&format!(" +{} DLC", b.dlc_count));
            }
            parts.push(s);
            if self.cache.is_some() && same.is_none() {
                parts.push("cache (other build)".into());
            }
        } else if let Some(c) = &self.cache {
            parts.push(match &c.language {
                Some(l) => format!("cache {l}"),
                None => "cache".into(),
            });
        }
        if let Some(v) = &self.installer {
            parts.push(format!("installer {v}"));
        }
        if parts.is_empty() {
            return None;
        }
        // Only an installer: keep the established `✓ installer <version>`.
        if self.built.is_none() && self.cache.is_none() {
            return Some(format!("✓ {}", parts[0]));
        }
        Some(parts.join(" + "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn built() -> Built {
        Built {
            platform: "linux",
            lane: Some("depot"),
            version: Some("2.31".into()),
            build_id: Some("55".into()),
            language: None,
            dlc_count: 2,
        }
    }

    fn cache(build: &str) -> Cached {
        Cached {
            build_id: build.into(),
            language: Some("en-US".into()),
        }
    }

    #[test]
    fn labels() {
        let mut l = Local::default();
        assert_eq!(l.label(), None);
        assert!(!l.any());
        l.installer = Some("3.00 (gog-5)".into());
        assert_eq!(l.label().unwrap(), "✓ installer 3.00 (gog-5)");
        // Cache only: no build id, language when known.
        l.installer = None;
        l.cache = Some(cache("55"));
        assert_eq!(l.label().unwrap(), "cache en-US");
        l.cache.as_mut().unwrap().language = None;
        assert_eq!(l.label().unwrap(), "cache");
        // Built, no version: never the build id.
        l.cache = None;
        l.built = Some(built());
        l.built.as_mut().unwrap().version = None;
        assert_eq!(l.label().unwrap(), "linux depot +2 DLC");
        l.built.as_mut().unwrap().dlc_count = 0;
        assert_eq!(l.label().unwrap(), "linux depot");
        l.installer = Some("1.0".into());
        assert_eq!(l.label().unwrap(), "linux depot + installer 1.0");
    }

    #[test]
    fn cache_for_the_built_build_is_implied() {
        let l = Local {
            built: Some(Built {
                version: None,
                ..built()
            }),
            cache: Some(cache("55")),
            installer: None,
        };
        // Same build: nothing about the cache, its language fills in.
        assert_eq!(l.label().unwrap(), "linux depot en-US +2 DLC");
        // The build's own language wins.
        let mut l2 = l.clone();
        l2.built.as_mut().unwrap().language = Some("de-DE".into());
        assert_eq!(l2.label().unwrap(), "linux depot de-DE +2 DLC");
        // With a version.
        assert_eq!(
            Local {
                built: Some(built()),
                ..l.clone()
            }
            .label()
            .unwrap(),
            "linux depot 2.31 en-US +2 DLC"
        );
    }

    #[test]
    fn cache_for_another_build_is_flagged() {
        let l = Local {
            built: Some(Built {
                version: None,
                ..built()
            }),
            cache: Some(cache("56")),
            installer: Some("1.0".into()),
        };
        assert_eq!(
            l.label().unwrap(),
            "linux depot +2 DLC + cache (other build) + installer 1.0"
        );
    }

    #[test]
    fn reads_a_cache_record() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(read_cache(t.path()), None);
        std::fs::write(
            t.path().join(crate::gog::depot::RECORD),
            r#"{"buildId":"77","language":"en-US","files":[]}"#,
        )
        .unwrap();
        let c = read_cache(t.path()).unwrap();
        assert_eq!(
            (c.build_id.as_str(), c.language.as_deref()),
            ("77", Some("en-US"))
        );
    }
}
