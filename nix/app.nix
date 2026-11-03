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

  inherit (pkgs.stdenv.hostPlatform) isLinux isx86_64;
  # x86_64 only, like umu's catalog entry: the env is multiArch (i686 half),
  # which nixpkgs can't build on aarch64-linux.
  withUmuPython = isLinux && isx86_64;

  # The interpreter gogo runs umu-launcher's zipapp with (design 18 §2.1, R1).
  # On a stock NixOS root the bare zipapp can't start the Steam Linux
  # Runtime; inside nixpkgs' own wrapper for umu-launcher
  # (pkgs/by-name/um/umu-launcher/package.nix) it can: verified in docs/21
  # §8 item 1. Same env, with Python as the program instead of umu, so the
  # pinned zipapp from gogo's store runs in it.
  umuPython = import ./umu-python.nix { inherit pkgs; };
in
rustPlatform.buildRustPackage ({
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

  # gog-warp's reqwest keeps its default native-tls feature, which links
  # openssl-sys on Linux (macOS uses Security.framework instead). Its build
  # script also runs bindgen over xdelta3, which needs libclang.
  nativeBuildInputs = [ pkgs.pkg-config rustPlatform.bindgenHook ]
    ++ pkgs.lib.optionals withUmuPython [ pkgs.makeWrapper ];
  buildInputs = pkgs.lib.optionals isLinux [ pkgs.openssl ];

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
} // pkgs.lib.optionalAttrs withUmuPython {
  # x86_64 Linux: point gogo at the FHS env (GOGO_UMU_PYTHON). A default, so a
  # user's own value still wins. gogo records the path in each game's
  # launch.conf as UMU_PYTHON; the launcher doesn't need this wrapper.
  # Attrs absent elsewhere, so the macOS derivation is unchanged.
  postFixup = ''
    wrapProgram $out/bin/gogo \
      --set-default GOGO_UMU_PYTHON ${umuPython}/bin/${umuPython.meta.mainProgram}
  '';
  passthru = { inherit umuPython; };
})
