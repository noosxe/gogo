//! The languages a depot build offers, and `--language` / `--lang` checked
//! against them (design 14 §11, #377).
//!
//! Both generations carry them in the build manifest gogo already fetches: a v2
//! depot has a `languages` array, a v1 depot its `languages` names (gog-warp turns
//! both into IETF-style codes, `English` → `en-US`). `*` (v1 `Neutral`) is a depot
//! every language takes. Nothing here touches the network or the disk.

use std::collections::BTreeSet;

use gog_warp::content_system::languages::get_language;
use gog_warp::content_system::types::{Manifest, v1};
use serde::Serialize;

/// The language a build gets when none is asked for.
pub const DEFAULT: &str = "en-US";

/// What one product (the base game or a DLC) offers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProductLanguages {
    pub id: String,
    /// The codes its language-specific depots name.
    pub codes: BTreeSet<String>,
    /// It has a depot for every language.
    pub neutral: bool,
}

/// A DLC that doesn't offer every language the base game does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DlcShort {
    pub id: String,
    /// What it offers (empty: only some of the build's languages, none by name).
    pub languages: Vec<String>,
    /// The base game's languages it lacks.
    pub missing: Vec<String>,
}

/// The languages of one build manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildLanguages {
    pub generation: u32,
    pub base: ProductLanguages,
    pub dlcs: Vec<ProductLanguages>,
}

/// Why a language code was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LanguageError {
    #[error("this build has no language `{requested}`; it offers: {}", list(.valid))]
    Unknown {
        requested: String,
        valid: Vec<String>,
    },
    #[error("`{requested}` matches more than one language of this build: {}; use the full code", list(.matches))]
    Ambiguous {
        requested: String,
        matches: Vec<String>,
    },
}

fn list(codes: &[String]) -> String {
    codes.join(", ")
}

/// The English name of a GOG language code, when gogo knows it.
pub fn name(code: &str) -> Option<&'static str> {
    get_language(code).map(|l| l.name)
}

fn add(out: &mut Vec<ProductLanguages>, id: &str, languages: &[String]) {
    let p = match out.iter().position(|p| p.id == id) {
        Some(i) => &mut out[i],
        None => {
            out.push(ProductLanguages {
                id: id.to_string(),
                ..Default::default()
            });
            out.last_mut().expect("just pushed")
        }
    };
    for l in languages {
        if l == "*" {
            p.neutral = true;
        } else {
            p.codes.insert(l.clone());
        }
    }
}

/// Read the languages out of `manifest`: per product, from its depots (design 19
/// §3.3's rule on v1: a depot naming the root game is the base's).
pub fn of_manifest(manifest: &Manifest) -> BuildLanguages {
    let mut products = Vec::new();
    let (base, generation) = match manifest {
        Manifest::V2(m) => {
            for d in m.depots() {
                add(&mut products, d.product_id(), d.languages());
            }
            (m.base_product_id().clone(), 2)
        }
        Manifest::V1(m) => {
            let p = m.product();
            let root = p.root_game_id();
            for d in p.depots() {
                if let v1::ManifestDepot::Files {
                    languages,
                    game_ids,
                    ..
                } = d
                {
                    if game_ids.contains(root) {
                        add(&mut products, root, languages);
                    } else {
                        for g in game_ids {
                            add(&mut products, g, languages);
                        }
                    }
                }
            }
            (root.clone(), 1)
        }
    };
    let mut base_langs = products
        .iter()
        .position(|p| p.id == base)
        .map(|i| products.remove(i))
        .unwrap_or_else(|| ProductLanguages {
            id: base.clone(),
            ..Default::default()
        });
    base_langs.id = base;
    BuildLanguages {
        generation,
        base: base_langs,
        dlcs: products,
    }
}

impl BuildLanguages {
    /// The codes a build can be asked for: the base game's, else (a base with only
    /// language-neutral depots) every DLC's. Empty when no depot names one: the
    /// build has a single set of files and `--language` changes nothing.
    pub fn codes(&self) -> Vec<String> {
        let mut set = self.base.codes.clone();
        if set.is_empty() {
            for d in &self.dlcs {
                set.extend(d.codes.iter().cloned());
            }
        }
        set.into_iter().collect()
    }

    /// DLC that offer some, but not all, of the base game's languages.
    pub fn short_dlcs(&self) -> Vec<DlcShort> {
        self.dlcs
            .iter()
            .filter(|d| !d.neutral && !d.codes.is_empty())
            .filter_map(|d| {
                let missing: Vec<String> = self.base.codes.difference(&d.codes).cloned().collect();
                (!missing.is_empty()).then(|| DlcShort {
                    id: d.id.clone(),
                    languages: d.codes.iter().cloned().collect(),
                    missing,
                })
            })
            .collect()
    }

    /// Notes on a game's `language` for `dlc list` (#377): a language the build no
    /// longer offers, or DLC with no files in it. Listing the build's languages is
    /// `gogo depot <game> --list-languages`.
    pub fn notes_for(
        &self,
        language: &str,
        title_of: &dyn Fn(&str) -> Option<String>,
    ) -> Vec<String> {
        let codes = self.codes();
        let has = |mut set: std::slice::Iter<String>| set.any(|c| c.eq_ignore_ascii_case(language));
        if !codes.is_empty() && !has(codes.iter()) {
            return vec![format!(
                "this build has no {language}; it offers {}",
                codes.join(", ")
            )];
        }
        self.dlcs
            .iter()
            .filter(|d| !d.neutral && !d.codes.is_empty())
            .filter(|d| !d.codes.iter().any(|c| c.eq_ignore_ascii_case(language)))
            .map(|d| {
                format!(
                    "DLC {} has no {language} files (it offers {})",
                    title_of(&d.id).unwrap_or_else(|| d.id.clone()),
                    d.codes.iter().cloned().collect::<Vec<_>>().join(", ")
                )
            })
            .collect()
    }

    /// Check a requested code (design 14 §11): case-insensitive; a short form such as
    /// `de` only when exactly one code of the build starts with it. A build with no
    /// language-specific depot takes any code as it is.
    pub fn resolve(&self, requested: &str) -> Result<String, LanguageError> {
        let valid = self.codes();
        if valid.is_empty() {
            return Ok(requested.to_string());
        }
        let want = requested.trim().to_ascii_lowercase().replace('_', "-");
        if let Some(c) = valid.iter().find(|c| c.to_ascii_lowercase() == want) {
            return Ok(c.clone());
        }
        let primary = |c: &str| c.split('-').next().unwrap_or(c).to_ascii_lowercase();
        let mut hits: Vec<String> = valid
            .iter()
            .filter(|c| primary(c) == want)
            .cloned()
            .collect();
        if hits.is_empty() {
            // GOG's older spellings (`zh`, `cn`) of a code the build has.
            hits = get_language(requested.trim())
                .map(|l| l.code.to_string())
                .filter(|c| valid.contains(c))
                .into_iter()
                .collect();
        }
        match hits.len() {
            1 => Ok(hits.remove(0)),
            0 => Err(LanguageError::Unknown {
                requested: requested.to_string(),
                valid,
            }),
            _ => Err(LanguageError::Ambiguous {
                requested: requested.to_string(),
                matches: hits,
            }),
        }
    }
}

/// Whether `requested` names the language a game or cache `recorded`, without a
/// manifest: the same code in any case, or a short form (`de`) of it.
pub fn same_language(requested: &str, recorded: &str) -> bool {
    let want = requested.trim().to_ascii_lowercase().replace('_', "-");
    let have = recorded.to_ascii_lowercase();
    want == have || (!want.contains('-') && have.split('-').next() == Some(want.as_str()))
}

/// One row of the listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Row {
    pub code: String,
    pub name: String,
    /// The language a build gets when none is asked for.
    pub default: bool,
    /// The language the cache or the game holds now.
    pub current: bool,
}

/// The listing of a build (`gogo depot --list-languages`, `dlc list`'s `--json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Listing {
    pub generation: u32,
    pub languages: Vec<Row>,
    /// No depot names a language: the build is one set of files.
    pub language_neutral: bool,
    pub dlcs_with_fewer: Vec<DlcShort>,
}

/// Build the listing; `current` is the recorded language, if any.
pub fn listing(b: &BuildLanguages, current: Option<&str>) -> Listing {
    let codes = b.codes();
    let default = if codes.iter().any(|c| c == DEFAULT) {
        Some(DEFAULT.to_string())
    } else {
        None
    };
    let languages = codes
        .iter()
        .map(|c| Row {
            name: name(c).unwrap_or("").to_string(),
            default: default.as_deref() == Some(c.as_str()),
            current: current.is_some_and(|l| l.eq_ignore_ascii_case(c)),
            code: c.clone(),
        })
        .collect();
    Listing {
        generation: b.generation,
        language_neutral: codes.is_empty(),
        languages,
        dlcs_with_fewer: b.short_dlcs(),
    }
}

/// The text of a listing. `title_of` names a DLC by id.
pub fn render(l: &Listing, title_of: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::new();
    if l.language_neutral {
        out.push_str("this build has no language-specific files: --language changes nothing\n");
    }
    for r in &l.languages {
        let mut marks = Vec::new();
        if r.default {
            marks.push("default");
        }
        if r.current {
            marks.push("current");
        }
        out.push_str(&format!(
            "{:<9} {}{}\n",
            r.code,
            r.name,
            if marks.is_empty() {
                String::new()
            } else {
                format!("  ({})", marks.join(", "))
            }
        ));
    }
    for d in &l.dlcs_with_fewer {
        let who = title_of(&d.id).unwrap_or_else(|| d.id.clone());
        out.push_str(&format!(
            "note: DLC {who} ({}) lacks: {}\n",
            d.id,
            d.missing.join(", ")
        ));
    }
    out
}

/// What a language switch is estimated to download (design 14 §11, #386): the
/// compressed bytes of the depots `want_language` (plus `want_dlcs`) takes that the
/// cache, downloaded as `have_language` with `have_dlcs`, doesn't already hold.
/// Depots are told apart by product and manifest hash, not by language tag: a depot
/// tagged with several languages can point at different manifests, and two depots
/// with one hash hold the same files. Each distinct depot counts once.
///
/// An upper bound of the real fetch: the downloader compares by file and chunk, so a
/// depot whose files the cache has in part costs less. `None` for a v1 build, whose
/// depots carry no per-depot compressed size.
pub fn switch_download(
    manifest: &Manifest,
    have_language: &str,
    have_dlcs: &[String],
    want_language: &str,
    want_dlcs: &[String],
) -> Option<u64> {
    switch_download_held(
        manifest,
        &[(have_language, have_dlcs)],
        want_language,
        want_dlcs,
    )
}

/// [`switch_download`] for a cache that holds several languages whole (#389), each
/// with its DLC: a depot any of them has counts as had.
pub fn switch_download_held(
    manifest: &Manifest,
    have: &[(&str, &[String])],
    want_language: &str,
    want_dlcs: &[String],
) -> Option<u64> {
    let Manifest::V2(m) = manifest else {
        return None;
    };
    let take = |language: &str, dlcs: &[String]| {
        let language = language.to_string();
        m.depots()
            .iter()
            .filter(|d| {
                (d.product_id() == m.base_product_id() || dlcs.contains(d.product_id()))
                    && (d.languages().iter().any(|l| l == "*") || d.languages().contains(&language))
            })
            .map(|d| ((d.product_id().clone(), d.manifest().clone()), d))
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let have: std::collections::BTreeMap<_, _> = have
        .iter()
        .flat_map(|(language, dlcs)| take(language, dlcs))
        .collect();
    let want = take(want_language, want_dlcs);
    Some(
        want.iter()
            .filter(|(k, _)| !have.contains_key(*k))
            .map(|(_, d)| u64::try_from(*d.compressed_size()).unwrap_or(0))
            .sum(),
    )
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;

    /// A v2 manifest: `(product, languages)` per depot.
    pub(crate) fn v2(depots: &[(&str, &[&str])]) -> Manifest {
        let ds: Vec<_> = depots
            .iter()
            .map(|(p, l)| {
                serde_json::json!({"size": 1, "compressedSize": 1, "languages": l,
                    "manifest": "m", "productId": p})
            })
            .collect();
        serde_json::from_value(serde_json::json!({
            "baseProductId": "1", "depots": ds, "installDirectory": "G",
            "platform": "windows", "products": []
        }))
        .unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::v2;
    use super::*;

    fn v1m(depots: &[(&[&str], &[&str])]) -> Manifest {
        let ds: Vec<_> = depots
            .iter()
            .map(|(l, g)| {
                serde_json::json!({"languages": l, "size": "1", "gameIDs": g,
                    "systems": ["Windows"], "manifest": "m"})
            })
            .collect();
        serde_json::from_value(serde_json::json!({"product": {
            "timestamp": 1, "depots": ds, "support_commands": [],
            "installDirectory": "G", "gameIDs": [], "rootGameID": "1"}}))
        .unwrap()
    }

    /// A v2 manifest: `(product, languages, manifest hash, compressed size)`.
    fn v2h(depots: &[(&str, &[&str], &str, u64)]) -> Manifest {
        let ds: Vec<_> = depots
            .iter()
            .map(|(p, l, h, c)| {
                serde_json::json!({"size": 1, "compressedSize": c, "languages": l,
                    "manifest": h, "productId": p})
            })
            .collect();
        serde_json::from_value(serde_json::json!({
            "baseProductId": "1", "depots": ds, "installDirectory": "G",
            "platform": "windows", "products": []
        }))
        .unwrap()
    }

    fn sw(m: &Manifest, have: &str, want: &str, dlcs: &[&str]) -> Option<u64> {
        let d: Vec<String> = dlcs.iter().map(|s| s.to_string()).collect();
        switch_download(m, have, &d, want, &d)
    }

    #[test]
    fn switch_counts_only_the_differing_depots() {
        let m = v2h(&[
            ("1", &["*"], "a", 100),
            ("1", &["en-US", "pt-BR"], "b", 50),
            ("1", &["en-US"], "c", 7),
            ("1", &["pt-BR"], "d", 11),
            ("1", &["pt-BR"], "e", 13),
        ]);
        assert_eq!(sw(&m, "en-US", "pt-BR", &[]), Some(24));
    }

    #[test]
    fn switch_counts_a_shared_manifest_once_or_not_at_all() {
        let m = v2h(&[
            ("1", &["en-US"], "same", 40),
            ("1", &["pt-BR"], "same", 40),
            ("1", &["pt-BR"], "new", 9),
            ("1", &["de-DE", "pt-BR"], "new", 9),
        ]);
        assert_eq!(sw(&m, "en-US", "pt-BR", &[]), Some(9));
    }

    #[test]
    fn switch_counts_a_depot_whose_manifest_differs_per_language() {
        // One depot tagged with both languages in two entries, different hashes.
        let m = v2h(&[("1", &["en-US"], "x1", 60), ("1", &["pt-BR"], "x2", 65)]);
        assert_eq!(sw(&m, "en-US", "pt-BR", &[]), Some(65));
    }

    #[test]
    fn switch_to_the_same_language_costs_nothing() {
        let m = v2h(&[("1", &["en-US"], "x1", 60), ("1", &["*"], "y", 5)]);
        assert_eq!(sw(&m, "en-US", "en-US", &[]), Some(0));
    }

    #[test]
    fn switch_counts_wanted_dlc_the_cache_lacks() {
        let m = v2h(&[
            ("1", &["*"], "a", 100),
            ("2", &["*"], "dlc", 30),
            ("3", &["*"], "other", 99),
            ("1", &["pt-BR"], "p", 4),
        ]);
        let d = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(
            switch_download(&m, "en-US", &d(&["2"]), "pt-BR", &d(&["2", "3"])),
            Some(4 + 99)
        );
        assert_eq!(
            switch_download(&m, "en-US", &d(&[]), "pt-BR", &d(&["2"])),
            Some(34)
        );
    }

    #[test]
    fn switch_has_no_estimate_for_v1() {
        let m = v1m(&[(&["English"], &["1"])]);
        assert_eq!(sw(&m, "en-US", "pt-BR", &[]), None);
    }

    fn cyber() -> BuildLanguages {
        of_manifest(&v2(&[
            ("1", &["*"]),
            ("1", &["en-US"]),
            ("1", &["de-DE"]),
            ("1", &["zh-Hans"]),
            ("1", &["pt-BR"]),
            ("2", &["en-US", "de-DE"]),
            ("3", &["*"]),
        ]))
    }

    #[test]
    fn v2_lists_codes_and_short_dlcs() {
        let b = cyber();
        assert_eq!(b.codes(), ["de-DE", "en-US", "pt-BR", "zh-Hans"]);
        let short = b.short_dlcs();
        assert_eq!(short.len(), 1);
        assert_eq!(short[0].id, "2");
        assert_eq!(short[0].missing, ["pt-BR", "zh-Hans"]);
    }

    #[test]
    fn listing_marks_default_and_current_and_names() {
        let l = listing(&cyber(), Some("de-de"));
        let de = l.languages.iter().find(|r| r.code == "de-DE").unwrap();
        assert_eq!(de.name, "German");
        assert!(de.current && !de.default);
        assert!(
            l.languages
                .iter()
                .find(|r| r.code == "en-US")
                .unwrap()
                .default
        );
        let text = render(&l, &|id| (id == "2").then(|| "Extra".to_string()));
        assert!(text.contains("zh-Hans   Chinese (Simplified)"), "{text}");
        assert!(text.contains("de-DE     German  (current)"), "{text}");
        assert!(
            text.contains("note: DLC Extra (2) lacks: pt-BR, zh-Hans"),
            "{text}"
        );
        let json = serde_json::to_value(&l).unwrap();
        assert_eq!(json["languages"][0]["code"], "de-DE");
        assert_eq!(json["dlcsWithFewer"][0]["id"], "2");
        assert_eq!(json["languageNeutral"], false);
    }

    #[test]
    fn resolve_is_case_insensitive_and_takes_unique_short_forms() {
        let b = cyber();
        assert_eq!(b.resolve("DE-de").unwrap(), "de-DE");
        assert_eq!(b.resolve("de").unwrap(), "de-DE");
        assert_eq!(b.resolve("pt").unwrap(), "pt-BR");
        assert_eq!(b.resolve("zh").unwrap(), "zh-Hans");
        assert_eq!(b.resolve("en_us").unwrap(), "en-US");
    }

    #[test]
    fn resolve_refuses_unknown_and_ambiguous_codes() {
        let b = cyber();
        let e = b.resolve("xx-XX").unwrap_err();
        assert!(matches!(e, LanguageError::Unknown { .. }));
        assert!(e.to_string().contains("de-DE, en-US, pt-BR, zh-Hans"));
        let two = of_manifest(&v2(&[("1", &["pt-BR"]), ("1", &["pt-PT"])]));
        let e = two.resolve("pt").unwrap_err();
        assert_eq!(
            e,
            LanguageError::Ambiguous {
                requested: "pt".into(),
                matches: vec!["pt-BR".into(), "pt-PT".into()]
            }
        );
        assert_eq!(two.resolve("pt-pt").unwrap(), "pt-PT");
    }

    #[test]
    fn dlc_list_notes_name_dlc_without_the_games_language() {
        let b = cyber();
        let title = |id: &str| (id == "2").then(|| "Extra".to_string());
        assert_eq!(b.notes_for("en-US", &title), Vec::<String>::new());
        assert_eq!(
            b.notes_for("zh-Hans", &title),
            ["DLC Extra has no zh-Hans files (it offers de-DE, en-US)"]
        );
        assert_eq!(
            b.notes_for("ja-JP", &title),
            ["this build has no ja-JP; it offers de-DE, en-US, pt-BR, zh-Hans"]
        );
    }

    #[test]
    fn same_language_matches_case_and_short_forms() {
        assert!(same_language("DE", "de-DE"));
        assert!(same_language("de-de", "de-DE"));
        assert!(!same_language("de-AT", "de-DE"));
        assert!(!same_language("fr", "de-DE"));
    }

    #[test]
    fn a_build_without_named_languages_takes_any_code() {
        let b = of_manifest(&v2(&[("1", &["*"])]));
        assert!(b.codes().is_empty());
        assert_eq!(b.resolve("xx-XX").unwrap(), "xx-XX");
        assert!(listing(&b, None).language_neutral);
    }

    #[test]
    fn v1_names_become_codes_and_dlcs_are_told_apart() {
        let b = of_manifest(&v1m(&[
            (&["English"], &["1"]),
            (&["German"], &["1"]),
            (&["Neutral"], &["1"]),
            (&["English"], &["7"]),
        ]));
        assert_eq!(b.generation, 1);
        assert_eq!(b.codes(), ["de-DE", "en-US"]);
        assert!(b.resolve("german").is_err());
        assert_eq!(b.resolve("de").unwrap(), "de-DE");
        let short = b.short_dlcs();
        assert_eq!(short[0].id, "7");
        assert_eq!(short[0].missing, ["de-DE"]);
    }
}
