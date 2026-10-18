{
  description = "gogo — GOG.com offline-installer downloader (Rust CLI): dev environment + installable package";

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
      # Supported platforms: Linux (x86_64 + aarch64) and macOS (Apple silicon
      # only — x86_64-darwin dropped 2026-10-18: nixpkgs 26.11 drops the
      # platform and there are no Intel-mac users). No Windows (AGENTS.md).
      systems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;

      # Per-system pkgs assembly shared by the dev shell and the app — overlay
      # wiring is a flake-level concern; shell/package content lives in nix/.
      pkgsFor = system:
        import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };

      mkDevShell = system:
        import ./nix/devshell.nix { pkgs = pkgsFor system; };

      packageFor = system:
        import ./nix/app.nix { pkgs = pkgsFor system; };
    in
    {
      devShells = forAllSystems (system: { default = mkDevShell system; });

      packages = forAllSystems (system: rec {
        default = packageFor system;
        gogo = default;
      });

      # `nix run .` / `nix run github:noosxe/gogo`
      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${packageFor system}/bin/gogo";
        };
      });
    };
}
