{
  description = "gogo — GOG.com offline-installer downloader (Rust CLI) development environment";

  inputs = {
    # Pinned by full commit hash: nixos-unstable branch, fetched 2026-10-09.
    # flake.lock additionally freezes the resolved revision — commit both.
    # All nixpkgs tools in the dev shell (nix/devshell.nix: rust-analyzer,
    # cargo-nextest, cargo-deny, git, just, pkg-config, stdenv.cc) inherit their
    # exact versions from this single pin.
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

      # Shell content lives in ./nix/devshell.nix; this is only the per-system
      # pkgs assembly — overlay wiring is a flake-level concern.
      mkDevShell = system:
        import ./nix/devshell.nix {
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ rust-overlay.overlays.default ];
          };
        };
    in
    {
      devShells = forAllSystems (system: { default = mkDevShell system; });
    };
}
