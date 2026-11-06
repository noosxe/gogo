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

      packages = forAllSystems (system:
        let
          pkgs = pkgsFor system;
          gogo = packageFor system;
        in
        {
          default = gogo;
          inherit gogo;
        }
        # x86_64 Linux: gogo for the Linux game lane, wrapped to run umu's
        # zipapp in nixpkgs' Steam runtime FHS env (design 18 §2.1), and that
        # env on its own, for a dev-shell run: `export GOGO_UMU_PYTHON=$(nix
        # build .#umu-python --print-out-paths)/bin/gogo-umu-python`. The env
        # is multiArch; nixpkgs has no i686 set on aarch64-linux.
        // nixpkgs.lib.optionalAttrs (system == "x86_64-linux") rec {
          gogo-linux-app = import ./nix/linux-app.nix { inherit pkgs gogo; };
          umu-python = gogo-linux-app.umuPython;
          # scripts/linux-e2e.sh's PATH (docs/linux-e2e.md).
          linux-e2e-tools = import ./nix/linux-e2e.nix { inherit pkgs; };
        });

      # `nixpkgs.overlays = [ gogo.overlays.default ]` (NixOS, home-manager):
      # `pkgs.gogo`, and `pkgs.gogo-linux-app` on x86_64 Linux. Re-exports
      # `packages` rather than rebuilding against the consumer's `pkgs`: the
      # same store paths as `nix profile install`, and nothing depends on
      # which channel they're on (`steam.buildRuntimeEnv` in umu-python.nix
      # is a moving target). `inputs.nixpkgs.follows` is how a consumer opts
      # into their own nixpkgs instead (README "Install"). Names are fixed,
      # not system-dependent, so the overlay never forces `prev` to list them.
      overlays.default = final: prev:
        let
          system = prev.stdenv.hostPlatform.system;
          pick = name:
            self.packages.${system}.${name}
              or (throw "gogo: no ${name} for ${system} (packages: ${nixpkgs.lib.concatStringsSep ", " systems}; gogo-linux-app: x86_64-linux)");
        in
        {
          gogo = pick "gogo";
          gogo-linux-app = pick "gogo-linux-app";
        };

      # `nix run .` / `nix run github:noosxe/gogo`
      # `nix run .#gogo-linux-app` (x86_64 Linux): the Linux game lane's gogo.
      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${packageFor system}/bin/gogo";
        };
      } // nixpkgs.lib.optionalAttrs (system == "x86_64-linux") {
        gogo-linux-app = {
          type = "app";
          program = "${self.packages.${system}.gogo-linux-app}/bin/gogo";
        };
      });
    };
}
