# The gogo development shell (AGENTS.md: all work happens inside this shell —
# never use compilers, tools, or binaries from the host system).
#
# Content only: the nixpkgs pin and the rust-overlay wiring live in flake.nix,
# which assembles `pkgs` per system and imports this file.
{
  pkgs,
}:

let
  # Pinned toolchain — single source of truth in ./rust-toolchain.nix.
  rustToolchain = import ./rust-toolchain.nix { inherit pkgs; };
in
pkgs.mkShell {
  packages = [
    rustToolchain           # rustc, cargo, rustfmt, clippy, rust-src
    pkgs.rust-analyzer      # LSP
    pkgs.cargo-nextest      # test runner      (design 08 §6)
    pkgs.cargo-deny         # license/advisory (design 08 §6)
    pkgs.git
    pkgs.just               # task runner (smoke scripts, CI shorthands)
    pkgs.pkg-config
    pkgs.stdenv.cc          # C compiler for ring/asm builds — from nixpkgs, never system
  ] ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isDarwin [
    pkgs.libiconv           # some crates still want it on macOS
  ];

  env = {
    RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
  };

  shellHook = ''
    echo "[gogo devshell] $(rustc --version)"
  '';
}
