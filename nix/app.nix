# The gogo package: builds the Rust workspace and installs its binaries.
# Consumed by flake.nix (`packages`, `apps`), enabling:
#   nix build .            /  nix build github:noosxe/gogo
#   nix run . -- --help    /  nix run github:noosxe/gogo
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
rustPlatform.buildRustPackage {
  pname = "gogo";
  # Keep in sync with [workspace.package] version in Cargo.toml.
  version = "0.1.0";

  src = ../.;
  # Lockfile mode vendors everything from the committed Cargo.lock. The git
  # rev-pinned gog-warp additionally needs a content hash (Nix cannot verify
  # rev-pinned content otherwise).
  cargoLock = {
    lockFile = ../Cargo.lock;
    outputHashes = {
      "gog-warp-0.1.0" = "sha256-skdvHnh6Y1BSLEbaP/ckVH1c4l49gLZk8DGXD8vJwkw=";
    };
  };

  meta = {
    description = "GOG.com offline-installer downloader (Rust CLI)";
    homepage = "https://github.com/noosxe/gogo";
    license = with pkgs.lib.licenses; [
      mit
      asl20
    ];
    mainProgram = "gogo";
  };
}
