//! The shape of the names a skin invents: skin, token, style, variable and id names.

/// `a-z0-9-`, not empty (design 27 §2). The folder name and the id.
pub(crate) fn is_skin_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A name an expression can use as a variable: `let`, `each` and component parameters.
/// Lowercase letters, digits and `_`, starting with a letter or `_`.
pub(crate) fn is_ident(s: &str) -> bool {
    let mut it = s.bytes();
    it.next()
        .is_some_and(|b| b.is_ascii_lowercase() || b == b'_')
        && it.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        && s.len() <= 64
}

/// A token name: dot-separated segments of lowercase letters, digits, `_` and `-`; the
/// first segment starts with a letter (`bg0`, `space.4`, `tone.setting_up`, `led-green`).
pub(crate) fn is_token_name(s: &str) -> bool {
    if s.is_empty() || s.len() > 64 {
        return false;
    }
    let mut segs = s.split('.');
    let first_ok = segs
        .next()
        .is_some_and(|f| f.bytes().next().is_some_and(|b| b.is_ascii_lowercase()) && seg_ok(f));
    first_ok && segs.all(seg_ok)
}

fn seg_ok(seg: &str) -> bool {
    !seg.is_empty()
        && seg
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// A style, component or ui-variable name: lowercase letters, digits, `-`, `_` and `.`,
/// starting with a letter or a digit.
pub(crate) fn is_style_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
        && s.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.')
        })
}

/// An `id=`: letters, digits and `_-.:`.
pub(crate) fn is_element_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':'))
}

/// The closest of `candidates` to `word`, if it is near enough to be a typo.
pub(crate) fn suggest<'a>(
    word: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> Option<&'a str> {
    candidates
        .into_iter()
        .map(|c| (distance(word, c), c))
        .filter(|(d, c)| *d <= 2.max(c.len() / 4))
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c)
}

/// Levenshtein distance, on bytes: names are ASCII.
fn distance(a: &str, b: &str) -> usize {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != cb)).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_shapes() {
        assert!(is_skin_name("default") && is_skin_name("my-skin-2"));
        assert!(!is_skin_name("") && !is_skin_name("My") && !is_skin_name("a_b"));
        assert!(is_token_name("space.4") && is_token_name("tone.setting_up"));
        assert!(is_token_name("led-green") && !is_token_name("4x") && !is_token_name("a..b"));
        assert!(is_ident("g") && is_ident("_x1") && !is_ident("a-b") && !is_ident("A"));
        assert!(is_style_name("chip.ready") && !is_style_name("-x"));
    }

    #[test]
    fn suggestions() {
        assert_eq!(suggest("titel", ["title", "label"]), Some("title"));
        assert_eq!(suggest("zzzzzz", ["title", "label"]), None);
    }
}
