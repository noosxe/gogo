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
    pkgs.jq                 # scripts/vm-test.sh JSON (tart list, run summary)
    pkgs.shellcheck         # lint for scripts/*.sh
    pkgs.innoextract        # unpack GOG offline installers for PE-scan research (docs/22 §7), never run
  ] ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isDarwin [
    pkgs.libiconv           # some crates still want it on macOS
  ] ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [
    pkgs.openssl            # gog-warp's reqwest keeps default native-tls → openssl-sys on Linux
    pkgs.rustPlatform.bindgenHook # libclang for gog-warp's xdelta3 bindgen (macOS finds Xcode's)
  ];

  env = {
    RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
  };

  shellHook = ''
    echo "[gogo devshell] $(rustc --version)"
  '';
}
