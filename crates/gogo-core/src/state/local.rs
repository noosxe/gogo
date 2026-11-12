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
    /// The built app leads: `linux depot · 2.31 +2 DLC` (platform, lane, version or
    /// `b<build>`, DLC count). A cache and an installer follow as `+ cache <build> <lang>`
    /// and `+ installer <version>`. With no app: `cache <build> <lang>`, and
    /// `✓ installer <version>` for an installer alone.
    pub fn label(&self) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        if let Some(b) = &self.built {
            let mut s = match b.lane {
                Some(l) => format!("{} {l}", b.platform),
                None => b.platform.to_string(),
            };
            let ver = b
                .version
                .clone()
                .or_else(|| b.build_id.as_ref().map(|id| format!("b{id}")));
            if let Some(v) = ver {
                s.push_str(&format!(" · {v}"));
            }
            if b.dlc_count > 0 {
                s.push_str(&format!(" +{} DLC", b.dlc_count));
            }
            parts.push(s);
        }
        if let Some(c) = &self.cache {
            let mut s = format!("cache {}", c.build_id);
            if let Some(l) = &c.language {
                s.push_str(&format!(" {l}"));
            }
            parts.push(s);
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
            dlc_count: 2,
        }
    }

    #[test]
    fn labels() {
        let mut l = Local::default();
        assert_eq!(l.label(), None);
        assert!(!l.any());
        l.installer = Some("3.00".into());
        assert_eq!(l.label().unwrap(), "✓ installer 3.00");
        l.cache = Some(Cached {
            build_id: "55".into(),
            language: Some("en-US".into()),
        });
        assert_eq!(l.label().unwrap(), "cache 55 en-US + installer 3.00");
        l.installer = None;
        assert_eq!(l.label().unwrap(), "cache 55 en-US");
        l.built = Some(built());
        assert_eq!(
            l.label().unwrap(),
            "linux depot · 2.31 +2 DLC + cache 55 en-US"
        );
        l.cache = None;
        l.built.as_mut().unwrap().version = None;
        l.built.as_mut().unwrap().dlc_count = 0;
        assert_eq!(l.label().unwrap(), "linux depot · b55");
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
