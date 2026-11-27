# What the Rust packages (./app.nix, ./gui.nix) share: the toolchain, the source,
# the vendored dependencies and the settings that keep the toolchain out of the
# closure.
{
  pkgs,
}:

let
  # Same pinned toolchain as the dev shell (nix/rust-toolchain.nix) — one
  # version everywhere, per the rule that toolchain bumps are deliberate.
  rustToolchain = import ./rust-toolchain.nix { inherit pkgs; };
  rustPlatform = pkgs.makeRustPlatform {
    cargo = rustToolchain;
    rustc = rustToolchain;
  };
in
{
  inherit rustPlatform;

  # Merged into each buildRustPackage call.
  args = {
    # Keep in sync with [workspace.package] version in Cargo.toml.
    version = "0.1.0";

    # Only what cargo reads (#243): the workspace, gogo-core's build.rs input
    # (compat/) and the lockfile. Everything else (docs, scripts/, e2e/*.toml,
    # nix/, .cargo/ whose only runner is macOS dev signing) stays out, so
    # editing it doesn't change this derivation or rebuild gogo. Tracked files
    # only, as for any flake source; a new path the build reads must be added here.
    src = pkgs.lib.fileset.toSource {
      root = ../.;
      fileset = pkgs.lib.fileset.unions [
        ../Cargo.toml
        ../Cargo.lock
        ../crates
        ../compat
      ];
    };
    # Lockfile mode vendors everything from the committed Cargo.lock. The git
    # rev-pinned gog-warp additionally needs a content hash (Nix cannot verify
    # rev-pinned content otherwise).
    cargoLock = {
      lockFile = ../Cargo.lock;
      outputHashes = {
        "gog-warp-0.1.0" = "sha256-oBxFYqMpcQ2rGCMd3aMf7YdqUcQ+k2tuzbw3G0y034o=";
      };
    };

    # Panic locations in std/core point into the toolchain's rust-src
    # (`<toolchain>/lib/rustlib/src/…`), and Nix's reference scan would make
    # the whole ~1.5 GiB toolchain a runtime dependency (#183). Remap it to
    # rustc's usual virtual prefix, and fail the build if it creeps back.
    RUSTFLAGS = "--remap-path-prefix=${rustToolchain}=/rustc";
    disallowedReferences = [ rustToolchain ];
  };
}
