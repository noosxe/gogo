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
      "gog-warp-0.1.0" = "sha256-oBxFYqMpcQ2rGCMd3aMf7YdqUcQ+k2tuzbw3G0y034o=";
    };
  };

  # These drive real processes through `ps` (the launcher's watchdog and
  # liveness checks, doctor's leftover-process scan), which the macOS build
  # sandbox denies; outside it the launcher reads that as "parent gone" and
  # tears down. They run in the dev shell (`cargo nextest run`) and CI.
  checkFlags = map (t: "--skip=${t}") [
    "doctor_reports_and_stops_leftover_wine_processes"
    "v2_early_exit_dialog_tries_the_next_renderer"
    "v2_launcher_teardown_reaps_its_own_prefix_only"
    "v2_launcher_watchdog_spares_a_later_launch"
    "v2_launcher_watchdog_tears_down_after_sigkill"
  ];

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
