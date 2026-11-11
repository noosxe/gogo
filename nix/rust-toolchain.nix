# The pinned Rust toolchain — single source of truth inside the flake.
# RULE (docs/design): compilers and tools come from this flake — never from
# the system. The exact toolchain version is written here on purpose.
# Current stable at pin time: 1.99.1 (2026-10-13).
# Bump together with rust-toolchain.toml (rustup environments, CI).
{
  pkgs,
}:

pkgs.rust-bin.stable."1.99.1".default.override {
  extensions = [ "rust-src" "rustfmt" "clippy" ];
}
