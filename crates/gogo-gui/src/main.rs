//! gogo-gui: the desktop GUI binary (docs/design/24-gui-architecture.md §2, design 25).
//!
//! It wires `gogo-core` into the skin engine through `CoreBackend`, owns the tokio runtime
//! and picks a renderer. The only GUI crate that depends on `gogo-core` (design 24 §2.1
//! rule 1). Nothing runs yet: the binary lands with PR E4 (#534) on fixture data and
//! PR F1 (#535) on real data.
#![deny(unsafe_code)]

fn main() {
    eprintln!("gogo-gui is not built yet: see https://github.com/noosxe/gogo/issues/520");
    std::process::exit(2);
}
