{
  description = "gogo — GOG.com offline-installer downloader (Rust CLI) development environment";

  inputs = {
    # Pinned by full commit hash: nixos-unstable branch, fetched 2026-10-09.
    # flake.lock additionally freezes the resolved revision — commit both.
    # All nixpkgs tools below (rust-analyzer, cargo-nextest, cargo-deny, git, just,
    # pkg-config, stdenv.cc) inherit their exact versions from this single pin.
    nixpkgs.url = "github:NixOS/nixpkgs/e7439b6b14ad3cc35d05608ebca9bce01a25f5f8";

    # Rust toolchains with exact, text-visible versions (see rustToolchain below).
    rust-overlay = {
      url = "github:oxalica/rust-overlay/4ca2963243427cb86bd6b4d9ca6010db0162a619";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, rust-overlay }:
    let
      # Supported platforms per docs/design/README.md: Linux + macOS only, no Windows.
      systems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" "x86_64-darwin" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;

      mkDevShell = system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ rust-overlay.overlays.default ];
          };

          # RULE (docs/design): compilers and tools come from this flake — never from
          # the system. The exact toolchain version is written here on purpose.
          # Current stable at pin time: 1.99.0 (2026-09-28).
          rustToolchain = pkgs.rust-bin.stable."1.99.0".default.override {
            extensions = [ "rust-src" "rustfmt" "clippy" ];
          };
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
        };
    in
    {
      devShells = forAllSystems (system: { default = mkDevShell system; });
    };
}
