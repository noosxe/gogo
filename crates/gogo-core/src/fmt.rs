//! Pure formatting helpers shared by all frontends (docs/design/01 §1) — no colors,
//! no tables, no terminal assumptions, so CLI and TUI render sizes identically.

/// "724.2 MB" style human sizes (`docs/design/03` §1 list table).
pub fn human_size(bytes: u64) -> String {
    bytesize::ByteSize::b(bytes).to_string()
}

/// Lowercase hex of an md5 digest (no `hex` dep; a `{:02x}` fold).
pub fn md5_hex(hasher: md5::Md5) -> String {
    use md5::Digest;
    use std::fmt::Write as _;
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::human_size;

    /// Live-verified size from docs/04 (setup_gog_… 724,243,232 bytes).
    #[test]
    fn formats_gigabyte_range() {
        let s = human_size(724_243_232);
        assert!(s.ends_with("MB") || s.ends_with("MiB"), "got: {s}");
    }
}
