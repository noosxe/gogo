//! Pure formatting helpers shared by all frontends (docs/design/01 §1) — no colors,
//! no tables, no terminal assumptions, so CLI and TUI render sizes identically.

/// "724.2 MB" style human sizes (`docs/design/03` §1 list table).
pub fn human_size(bytes: u64) -> String {
    bytesize::ByteSize::b(bytes).to_string()
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
