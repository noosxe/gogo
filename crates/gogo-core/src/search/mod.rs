//! `gogo search` (`docs/design/17-search.md`): classify everything the account owns
//! into games, DLC, packs and goodies, then match it with one deterministic
//! normalizer (§4). Presentation (tables, JSON) lives in the frontend.
//!
//! - [`gather`] fetches the inputs (library, owned ids, product cache/API).
//! - [`classify`] turns them into [`Item`]s (§2, §2.1).
//! - [`search`] matches and orders them (§4, §7); [`suggest`] is "did you mean".

pub mod gather;

use std::collections::{HashMap, HashSet};

use serde::Serialize;
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

use crate::gog::catalog::{InstallerEntry, Os, Product};
use crate::gog::library::LibraryProduct;

/// Normalize text for matching (§4 step 1): strip `™ ® ©`, NFKD, drop combining
/// marks, lowercase, and collapse every run of non-alphanumerics into one space.
pub fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut gap = false;
    // ™ goes before NFKD, which would otherwise turn it into "TM".
    for c in text
        .chars()
        .filter(|c| !matches!(c, '™' | '®' | '©'))
        .nfkd()
    {
        if is_combining_mark(c) {
            continue;
        }
        if c.is_alphanumeric() {
            if gap && !out.is_empty() {
                out.push(' ');
            }
            gap = false;
            out.extend(c.to_lowercase());
        } else {
            gap = true;
        }
    }
    out
}

/// A parsed query: normalized words (§4 step 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    words: Vec<String>,
    joined: String,
}

impl Query {
    /// `None` when nothing is left after normalization (usage error, §8).
    pub fn parse(raw: &str) -> Option<Self> {
        let joined = normalize(raw);
        if joined.is_empty() {
            return None;
        }
        let words = joined.split(' ').map(str::to_owned).collect();
        Some(Self { words, joined })
    }

    /// Every query word is a substring of `normalized` (§4 step 3).
    pub fn matches_text(&self, normalized: &str) -> bool {
        self.words.iter().all(|w| normalized.contains(w.as_str()))
    }

    /// A single all-digit word also matches ids exactly (§4 step 6).
    fn id(&self) -> Option<&str> {
        match self.words.as_slice() {
            [w] if w.chars().all(|c| c.is_ascii_digit()) => Some(w),
            _ => None,
        }
    }
}

/// Content kind (§2); the declaration order is the output group order (§7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Game,
    Dlc,
    Pack,
    Goodies,
    Other,
}

/// Why a row matched (§4 step 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum MatchedBy {
    #[serde(rename = "self")]
    Itself,
    #[serde(rename = "parent")]
    Parent,
}

/// The game (or DLC, for goodie entries) a row belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parent {
    pub id: u64,
    pub kind: Kind,
    pub title: String,
}

/// One searchable row: a product, or one goodie entry of a product.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub kind: Kind,
    /// Product id, or a goodie entry's own id as GOG sends it.
    pub id: String,
    pub title: String,
    pub slug: Option<String>,
    /// Goodies: `bonus_content.type` (`mixed` for a product whose entries differ).
    pub subtype: Option<String>,
    /// Goodies: a product of its own (rule b) rather than an entry (rule a).
    pub product: bool,
    pub parent: Option<Parent>,
    /// Platforms with an installer (games/DLC).
    pub os: Vec<Os>,
    pub size: u64,
    pub installed_version: Option<String>,
    /// `false` only for DLC of owned games the account lacks (§2.1).
    pub owned: bool,
    /// Raw `game_type` (shown for Other rows).
    pub game_type: Option<String>,
    text: String,
    parent_text: String,
}

/// Everything [`classify`] needs; [`gather::gather`] builds it from the API.
#[derive(Debug, Clone, Default)]
pub struct Library {
    /// Library games with their detail (`None`: the fetch failed).
    pub games: Vec<(LibraryProduct, Option<Product>)>,
    /// Owned ids that are neither a library game nor in an owned game's
    /// `expanded_dlcs` (batch-fetched; `None`: no catalog entry).
    pub others: Vec<(u64, Option<Product>)>,
    pub owned: HashSet<u64>,
}

/// Classify the library into rows (§2, §2.1). `installed(product, dlc)` gives the
/// downloaded version, as `list` shows it.
pub fn classify(
    lib: &Library,
    installed: &dyn Fn(u64, Option<u64>) -> Option<String>,
) -> Vec<Item> {
    let mut items = Vec::new();
    let mut seen: HashSet<u64> = lib.games.iter().map(|(g, _)| g.id).collect();

    for (game, detail) in &lib.games {
        let slug = game
            .slug
            .clone()
            .or_else(|| detail.as_ref().and_then(|d| d.slug.clone()));
        let text = normalize(&format!(
            "{} {}",
            game.title,
            slug.as_deref().unwrap_or_default()
        ));
        items.push(Item {
            kind: Kind::Game,
            id: game.id.to_string(),
            title: game.title.clone(),
            slug,
            subtype: None,
            product: false,
            parent: None,
            os: detail.as_ref().map(installer_os).unwrap_or_default(),
            size: detail.as_ref().map(product_size).unwrap_or_default(),
            installed_version: installed(game.id, None),
            owned: true,
            game_type: Some("game".into()),
            text: text.clone(),
            parent_text: String::new(),
        });
        let Some(detail) = detail else { continue };
        let parent = Parent {
            id: game.id,
            kind: Kind::Game,
            title: game.title.clone(),
        };
        push_entries(&mut items, detail, &parent, &text);

        for dlc in &detail.expanded_dlcs {
            if !seen.insert(dlc.id) {
                continue;
            }
            let owned = lib.owned.contains(&dlc.id);
            let Some(mut item) = product_item(dlc, owned, Some(parent.clone()), &text) else {
                continue;
            };
            if owned && item.kind == Kind::Dlc {
                item.installed_version = installed(game.id, Some(dlc.id));
                let dlc_parent = Parent {
                    id: dlc.id,
                    kind: Kind::Dlc,
                    title: item.title.clone(),
                };
                let dlc_text = format!("{} {}", item.text, text);
                items.push(item);
                push_entries(&mut items, dlc, &dlc_parent, &dlc_text);
            } else {
                items.push(item);
            }
        }
    }

    for (id, product) in &lib.others {
        if !seen.insert(*id) {
            continue;
        }
        let item = match product {
            Some(p) => product_item(p, true, None, ""),
            None => None,
        };
        let item = item.unwrap_or_else(|| other_item(*id, product.as_ref()));
        let is_dlc = item.kind == Kind::Dlc;
        let (dlc_parent, dlc_text) = (
            Parent {
                id: *id,
                kind: Kind::Dlc,
                title: item.title.clone(),
            },
            item.text.clone(),
        );
        items.push(item);
        if is_dlc && let Some(p) = product {
            push_entries(&mut items, p, &dlc_parent, &dlc_text);
        }
    }
    items
}

/// A product that is not a library row: pack, DLC or goodie product (§2 rules).
/// `None` for a not-owned product that is not DLC-shaped (stays out, §2.1).
fn product_item(
    p: &Product,
    owned: bool,
    parent: Option<Parent>,
    parent_text: &str,
) -> Option<Item> {
    // Rows from `expanded_dlcs` are DLC even if the entry omits `game_type`.
    let game_type = p
        .game_type
        .clone()
        .or_else(|| parent.as_ref().map(|_| "dlc".to_owned()));
    let installers = p
        .downloads
        .as_ref()
        .is_some_and(|d| !d.installers.is_empty());
    let bonus: &[InstallerEntry] = p
        .downloads
        .as_ref()
        .map(|d| d.bonus_content.as_slice())
        .unwrap_or_default();
    let (kind, subtype) = match game_type.as_deref() {
        Some("pack") => (Kind::Pack, None),
        Some("dlc") if !installers && !bonus.is_empty() => (Kind::Goodies, subtype_of(bonus)),
        Some("dlc") if installers || !owned => (Kind::Dlc, None),
        _ => (Kind::Other, None),
    };
    if !owned && !matches!(kind, Kind::Dlc | Kind::Goodies) {
        return None;
    }
    let title = p.title.clone().unwrap_or_else(|| p.id.to_string());
    let text = normalize(&format!(
        "{title} {} {}",
        p.slug.as_deref().unwrap_or_default(),
        subtype.as_deref().unwrap_or_default()
    ));
    Some(Item {
        kind,
        id: p.id.to_string(),
        title,
        slug: p.slug.clone(),
        product: kind == Kind::Goodies,
        subtype,
        parent,
        os: installer_os(p),
        size: product_size(p),
        installed_version: None,
        owned,
        game_type,
        text,
        parent_text: parent_text.to_owned(),
    })
}

/// An owned id that fits no rule, or whose catalog entry is missing (§2 Other).
fn other_item(id: u64, product: Option<&Product>) -> Item {
    let title = product
        .and_then(|p| p.title.clone())
        .unwrap_or_else(|| id.to_string());
    let slug = product.and_then(|p| p.slug.clone());
    Item {
        kind: Kind::Other,
        id: id.to_string(),
        text: normalize(&format!("{title} {}", slug.as_deref().unwrap_or_default())),
        title,
        slug,
        subtype: None,
        product: false,
        parent: None,
        os: Vec::new(),
        size: 0,
        installed_version: None,
        owned: true,
        game_type: product.and_then(|p| p.game_type.clone()),
        parent_text: String::new(),
    }
}

/// Goodie entries (rule a) of an owned game or owned DLC.
fn push_entries(items: &mut Vec<Item>, p: &Product, parent: &Parent, parent_text: &str) {
    let Some(downloads) = &p.downloads else {
        return;
    };
    for entry in &downloads.bonus_content {
        let title = entry.name.clone().unwrap_or_else(|| entry.id.clone());
        items.push(Item {
            kind: Kind::Goodies,
            id: entry.id.clone(),
            text: normalize(&format!(
                "{title} {}",
                entry.kind.as_deref().unwrap_or_default()
            )),
            title,
            slug: None,
            subtype: entry.kind.clone(),
            product: false,
            parent: Some(parent.clone()),
            os: Vec::new(),
            size: entry.total_size.unwrap_or_default(),
            installed_version: None,
            owned: true,
            game_type: None,
            parent_text: parent_text.to_owned(),
        });
    }
}

/// A goodie product's subtype: its entries' common type, or `mixed`.
fn subtype_of(entries: &[InstallerEntry]) -> Option<String> {
    let first = entries.first()?.kind.clone();
    if entries.iter().all(|e| e.kind == first) {
        first
    } else {
        Some("mixed".into())
    }
}

/// Platforms with an installer, in win → mac → linux order.
fn installer_os(p: &Product) -> Vec<Os> {
    let mut os: Vec<Os> = Vec::new();
    for entry in p.downloads.iter().flat_map(|d| &d.installers) {
        if let Some(o) = entry.os
            && !os.contains(&o)
        {
            os.push(o);
        }
    }
    os.sort_by_key(|o| match o {
        Os::Windows => 0,
        Os::Mac => 1,
        Os::Linux => 2,
    });
    os
}

/// Installers + bonus content, as `list` sums it.
fn product_size(p: &Product) -> u64 {
    p.downloads
        .iter()
        .flat_map(|d| d.installers.iter().chain(&d.bonus_content))
        .filter_map(|e| e.total_size)
        .sum()
}

/// Row filters (§3).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filters {
    /// Kinds to keep; empty keeps every kind (Other included).
    pub kinds: Vec<Kind>,
    pub goodies_type: Option<String>,
    pub os: Option<Os>,
    /// `--owned`: hide not-owned DLC.
    pub owned_only: bool,
}

impl Filters {
    /// `--goodies-type` implies `-c goodies` (§3).
    pub fn new(
        mut kinds: Vec<Kind>,
        goodies_type: Option<String>,
        os: Option<Os>,
        owned_only: bool,
    ) -> Self {
        if goodies_type.is_some() && !kinds.contains(&Kind::Goodies) {
            kinds.push(Kind::Goodies);
        }
        Self {
            kinds,
            goodies_type,
            os,
            owned_only,
        }
    }

    /// The user asked for this kind by name (turns folding off, §5).
    pub fn asked(&self, kind: Kind) -> bool {
        self.kinds.contains(&kind)
    }

    fn keep(&self, item: &Item) -> bool {
        if !self.kinds.is_empty() && !self.kinds.contains(&item.kind) {
            return false;
        }
        if self.owned_only && !item.owned {
            return false;
        }
        if let Some(want) = &self.goodies_type
            && item.kind == Kind::Goodies
            && item.subtype.as_ref() != Some(want)
        {
            return false;
        }
        // Rows with unknown os data survive the filter, as in `list`.
        if let Some(want) = self.os
            && matches!(item.kind, Kind::Game | Kind::Dlc)
            && !item.os.is_empty()
            && !item.os.contains(&want)
        {
            return false;
        }
        true
    }
}

/// One search result.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub item: Item,
    pub matched_by: MatchedBy,
}

/// How `item` matches `query`, if it does (§4 steps 3–6). A parent match takes the
/// row's own text together with its parent's, so `witcher hearts` finds the DLC
/// "Hearts of Stone" of "The Witcher 3".
pub fn matches(item: &Item, query: &Query) -> Option<MatchedBy> {
    if query.matches_text(&item.text) || query.id() == Some(item.id.as_str()) {
        return Some(MatchedBy::Itself);
    }
    if !item.parent_text.is_empty()
        && query.matches_text(&format!("{} {}", item.text, item.parent_text))
    {
        return Some(MatchedBy::Parent);
    }
    None
}

/// Rank inside a group (§4): exact title → title starts with the query → rest.
fn rank(item: &Item, query: &Query) -> u8 {
    let title = normalize(&item.title);
    if title == query.joined {
        0
    } else if title.starts_with(&query.joined) {
        1
    } else {
        2
    }
}

/// Matching rows, in output order (§7): grouped by kind, DLC and goodies grouped
/// under their parent (parentless first, then parents by their best row), ranked.
pub fn search(items: &[Item], query: &Query, filters: &Filters) -> Vec<Hit> {
    let mut hits: Vec<Hit> = items
        .iter()
        .filter(|i| filters.keep(i))
        .filter_map(|i| {
            matches(i, query).map(|matched_by| Hit {
                item: i.clone(),
                matched_by,
            })
        })
        .collect();
    hits.sort_by_cached_key(|h| {
        (
            h.item.kind,
            rank(&h.item, query),
            normalize(&h.item.title),
            h.item.id.clone(),
        )
    });

    let mut out = Vec::with_capacity(hits.len());
    let mut rest = hits.into_iter().peekable();
    while let Some(kind) = rest.peek().map(|h| h.item.kind) {
        let mut group = Vec::new();
        while let Some(h) = rest.next_if(|h| h.item.kind == kind) {
            group.push(h);
        }
        if matches!(kind, Kind::Dlc | Kind::Goodies) {
            let mut parents: Vec<Option<u64>> = Vec::new();
            for h in &group {
                let p = h.item.parent.as_ref().map(|p| p.id);
                if !parents.contains(&p) {
                    parents.push(p);
                }
            }
            // Parentless rows first; `None` sorts before every `Some`.
            parents.sort_by_key(|p| p.is_some());
            for p in parents {
                out.extend(
                    group
                        .iter()
                        .filter(|h| h.item.parent.as_ref().map(|x| x.id) == p)
                        .cloned(),
                );
            }
        } else {
            out.extend(group);
        }
    }
    out
}

/// "Did you mean" (§4): correct each query word that is no substring of any
/// vocabulary word to its closest word (Damerau-Levenshtein ≤ 1 for words of up to
/// 4 characters, ≤ 2 above; ties → more candidates, then alphabetical). Returns the
/// corrected query only when it finds something.
pub fn suggest(items: &[Item], query: &Query, filters: &Filters) -> Option<String> {
    let mut vocab: HashMap<&str, usize> = HashMap::new();
    for item in items.iter().filter(|i| filters.keep(i)) {
        let words: HashSet<&str> = item
            .text
            .split(' ')
            .chain(item.parent_text.split(' '))
            .filter(|w| !w.is_empty())
            .collect();
        for w in words {
            *vocab.entry(w).or_default() += 1;
        }
    }

    let mut changed = false;
    let corrected: Vec<&str> = query
        .words
        .iter()
        .map(|w| {
            if vocab.keys().any(|v| v.contains(w.as_str())) {
                return w.as_str();
            }
            let cap = if w.chars().count() <= 4 { 1 } else { 2 };
            let best = vocab
                .iter()
                .map(|(v, n)| (strsim::damerau_levenshtein(w, v), std::cmp::Reverse(*n), *v))
                .filter(|(d, ..)| *d <= cap)
                .min();
            match best {
                Some((_, _, v)) => {
                    changed = true;
                    v
                }
                None => w.as_str(),
            }
        })
        .collect();
    if !changed {
        return None;
    }
    let joined = corrected.join(" ");
    let retry = Query::parse(&joined)?;
    (!search(items, &retry, filters).is_empty()).then_some(joined)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const FIX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/search");

    fn fixture<T: serde::de::DeserializeOwned>(name: &str) -> T {
        let raw = std::fs::read_to_string(format!("{FIX}/{name}")).unwrap();
        serde_json::from_str(&raw).unwrap()
    }

    /// The fixture library: three games, their details, and the batch products.
    pub(crate) fn library() -> Library {
        #[derive(serde::Deserialize)]
        struct Page {
            products: Vec<LibraryProduct>,
        }
        #[derive(serde::Deserialize)]
        struct Owned {
            owned: Vec<u64>,
        }
        let games = fixture::<Page>("library.json").products;
        let details: Vec<Product> = [
            "product-1979.json",
            "product-hot-brass.json",
            "product.json",
        ]
        .iter()
        .map(|f| fixture(f))
        .collect();
        let mut owned: HashSet<u64> = fixture::<Owned>("owned.json").owned.into_iter().collect();
        // An owned id GOG returns no catalog entry for.
        owned.insert(1000000304);
        let mut batch: Vec<Product> = fixture("products-batch.json");
        let others = [1000000301, 1000000302, 1000000303, 1000000304]
            .into_iter()
            .map(|id| {
                let p = batch
                    .iter()
                    .position(|p| p.id == id)
                    .map(|i| batch.remove(i));
                (id, p)
            })
            .collect();
        Library {
            games: games
                .into_iter()
                .zip(details.into_iter().map(Some))
                .collect(),
            others,
            owned,
        }
    }

    pub(crate) fn items() -> Vec<Item> {
        classify(&library(), &|p, d| {
            (p == 1000000001 && d.is_none()).then(|| "2.2.2".to_owned())
        })
    }

    fn find<'a>(items: &'a [Item], id: &str) -> &'a Item {
        items.iter().find(|i| i.id == id).unwrap()
    }

    fn ids(hits: &[Hit]) -> Vec<&str> {
        hits.iter().map(|h| h.item.id.as_str()).collect()
    }

    fn q(raw: &str) -> Query {
        Query::parse(raw).unwrap()
    }

    #[test]
    fn normalize_table() {
        for (raw, want) in [
            ("Pokémon", "pokemon"),
            ("Ærø", "ærø"),
            ("Witcher™ 3®: Wild Hunt©", "witcher 3 wild hunt"),
            ("  --Hot__Brass!!  ", "hot brass"),
            ("the_witcher_3_wild_hunt", "the witcher 3 wild hunt"),
            ("Baldur's Gate", "baldur s gate"),
            ("中文 游戏", "中文 游戏"),
            ("ＦＵＬＬ ｗｉｄｔｈ", "full width"),
            ("™ ®  !!", ""),
        ] {
            assert_eq!(normalize(raw), want, "{raw}");
        }
        assert!(Query::parse(":: --").is_none());
    }

    #[test]
    fn matches_any_order_all_words() {
        let items = items();
        let witcher = find(&items, "1000000002");
        assert_eq!(
            matches(witcher, &q("wild hunt witcher")),
            Some(MatchedBy::Itself)
        );
        assert_eq!(matches(witcher, &q("witcher 4")), None);
        assert_eq!(matches(witcher, &q("the_witcher")), Some(MatchedBy::Itself));
    }

    #[test]
    fn id_match_vs_digits_in_title() {
        let items = items();
        // "1979" is a substring of the title, not an id.
        let hits = search(&items, &q("1979"), &Filters::default());
        assert_eq!(ids(&hits), ["1000000003"]);
        // An exact id finds any kind, goodie entries included.
        let hits = search(&items, &q("1000000301"), &Filters::default());
        assert_eq!(ids(&hits), ["1000000301"]);
        let hits = search(&items, &q("48214"), &Filters::default());
        assert_eq!(ids(&hits), ["48214"]);
        // Digits next to other words are plain substrings.
        assert!(search(&items, &q("1000000301 witcher"), &Filters::default()).is_empty());
    }

    #[test]
    fn parent_match_reason() {
        let items = items();
        let hos = find(&items, "1000000101");
        assert_eq!(matches(hos, &q("hearts")), Some(MatchedBy::Itself));
        let manual = find(&items, "48214");
        assert_eq!(matches(manual, &q("witcher")), Some(MatchedBy::Parent));
        assert_eq!(matches(manual, &q("manual")), Some(MatchedBy::Itself));
        // Own words plus parent words together.
        let artbook = find(&items, "48300");
        assert_eq!(
            matches(artbook, &q("witcher artbook")),
            Some(MatchedBy::Parent)
        );
    }

    #[test]
    fn classify_rules() {
        let items = items();
        let k = |id| find(&items, id).kind;
        assert_eq!(k("1000000001"), Kind::Game);
        assert_eq!(k("1000000301"), Kind::Pack);
        assert_eq!(k("1000000101"), Kind::Dlc);
        assert_eq!(k("1000000303"), Kind::Other);
        assert_eq!(
            find(&items, "1000000303").game_type.as_deref(),
            Some("bundle")
        );
        // An owned id without a catalog entry degrades to Other.
        assert_eq!(k("1000000304"), Kind::Other);
        assert_eq!(find(&items, "1000000304").game_type, None);

        // Goodie product (Hot Brass soundtrack shape), parent from expanded_dlcs.
        let ost = find(&items, "1000000201");
        assert_eq!((ost.kind, ost.product), (Kind::Goodies, true));
        assert_eq!(ost.subtype.as_deref(), Some("audio"));
        assert_eq!(ost.parent.as_ref().unwrap().id, 1000000001);
        // Its entries are not listed again as goodies.
        assert!(!items.iter().any(|i| i.id == "100897"));
        // A batch goodie product has no parent.
        assert!(find(&items, "1000000302").parent.is_none());

        // Goodie entries of a game and of an owned DLC.
        let manual = find(&items, "48214");
        assert_eq!((manual.kind, manual.product), (Kind::Goodies, false));
        assert_eq!(manual.parent.as_ref().unwrap().kind, Kind::Game);
        let artbook = find(&items, "48300");
        assert_eq!(artbook.parent.as_ref().unwrap().kind, Kind::Dlc);
        assert_eq!(artbook.subtype.as_deref(), Some("artworks"));

        // Downloaded state as `list`.
        assert_eq!(
            find(&items, "1000000001").installed_version.as_deref(),
            Some("2.2.2")
        );
        assert_eq!(find(&items, "1000000001").os, [Os::Windows, Os::Mac]);
    }

    #[test]
    fn not_owned_dlc() {
        let items = items();
        let bw = find(&items, "1000000102");
        assert_eq!((bw.kind, bw.owned), (Kind::Dlc, false));
        let free = find(&items, "1000000103");
        assert_eq!((free.kind, free.owned), (Kind::Goodies, false));
        // Its bonus entries are never goodies.
        assert!(!items.iter().any(|i| i.id == "48400" || i.id == "48500"));

        let all = search(&items, &q("blood wine"), &Filters::default());
        assert_eq!(ids(&all), ["1000000102"]);
        let owned = Filters::new(vec![], None, None, true);
        assert!(search(&items, &q("blood wine"), &owned).is_empty());
    }

    #[test]
    fn filters_and_order() {
        let items = items();
        let f = Filters::new(vec![], Some("audio".into()), None, false);
        assert_eq!(f.kinds, [Kind::Goodies]);
        let hits = search(&items, &q("soundtrack"), &f);
        // Grouped by parent, parents ordered by their best row ("Soundtrack (MP3)"
        // starts with the query).
        assert_eq!(ids(&hits), ["48213", "1000000103", "1000000201"]);

        let mac = Filters::new(vec![Kind::Game], None, Some(Os::Mac), false);
        let hits = search(&items, &q("hot brass"), &mac);
        assert_eq!(ids(&hits), ["1000000001"]);
        let linux = Filters::new(vec![Kind::Game], None, Some(Os::Linux), false);
        assert!(search(&items, &q("hot brass"), &linux).is_empty());

        // Ranking: exact title before prefix before the rest.
        let hits = search(&items, &q("hot brass"), &Filters::default());
        assert_eq!(hits[0].item.id, "1000000001");
        assert_eq!(hits[0].item.kind, Kind::Game);
    }

    #[test]
    fn suggest_one_typo() {
        let items = items();
        let none = Filters::default();
        assert_eq!(
            suggest(&items, &q("witchr"), &none).as_deref(),
            Some("witcher")
        );
        // Multi-word query with one bad word.
        assert_eq!(
            suggest(&items, &q("hot brsas"), &none).as_deref(),
            Some("hot brass")
        );
        // Nothing close: no suggestion.
        assert_eq!(suggest(&items, &q("zzzzzzzz"), &none), None);
    }

    #[test]
    fn suggest_length_cap() {
        let items = items();
        let none = Filters::default();
        // "mod" (3 chars) may move 1 edit only: "hot" and "of" are 2 away.
        assert_eq!(suggest(&items, &q("mod"), &none), None);
        assert_eq!(suggest(&items, &q("hxt"), &none).as_deref(), Some("hot"));
    }

    #[test]
    fn suggest_tie_break() {
        let mut lib = Library::default();
        for (id, title) in [(1, "Abc"), (2, "Abd"), (3, "Abd Two")] {
            let game: LibraryProduct =
                serde_json::from_str(&format!(r#"{{"id":{id},"title":"{title}","isGame":true}}"#))
                    .unwrap();
            lib.games.push((game, None));
        }
        let items = classify(&lib, &|_, _| None);
        // "abx" is 1 edit from both; "abd" occurs in more candidates.
        assert_eq!(
            suggest(&items, &q("abx"), &Filters::default()).as_deref(),
            Some("abd")
        );
        // Equal counts: alphabetical.
        lib.games.pop();
        let items = classify(&lib, &|_, _| None);
        assert_eq!(
            suggest(&items, &q("abx"), &Filters::default()).as_deref(),
            Some("abc")
        );
    }
}
