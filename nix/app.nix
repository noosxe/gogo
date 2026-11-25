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
  # The desktop GUI crates (design 24 §2), by the same name patterns as the
  # `gui` filter in .github/workflows/ci-jobs.yml.
  guiCrates = builtins.filter
    (name: builtins.match "gogo-(contract|skins?|ui-.*|render-.*|gui|gui-.*)" name != null)
    (builtins.attrNames (pkgs.lib.filterAttrs (_: type: type == "directory") (builtins.readDir ../crates)));
in
rustPlatform.buildRustPackage {
  pname = "gogo";
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

  # The CLI only (design 24 §7): the desktop GUI crates are workspace members
  # too, and without these flags they would be built and tested into this
  # package. Every other member is tested, so a new non-GUI crate needs no
  # edit here.
  cargoBuildFlags = [ "-p" "gogo-cli" ];
  cargoTestFlags = [ "--workspace" ] ++ map (name: "--exclude=${name}") guiCrates;

  # gog-warp's reqwest keeps its default native-tls feature, which links
  # openssl-sys on Linux (macOS uses Security.framework instead). Its build
  # script also runs bindgen over xdelta3, which needs libclang.
  nativeBuildInputs = [
    pkgs.pkg-config
    pkgs.installShellFiles
    rustPlatform.bindgenHook
  ];
  buildInputs = pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.openssl ];

  # Panic locations in std/core point into the toolchain's rust-src
  # (`<toolchain>/lib/rustlib/src/…`), and Nix's reference scan would make
  # the whole ~1.5 GiB toolchain a runtime dependency (#183). Remap it to
  # rustc's usual virtual prefix, and fail the build if it creeps back.
  RUSTFLAGS = "--remap-path-prefix=${rustToolchain}=/rustc";
  disallowedReferences = [ rustToolchain ];

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

  # bash/zsh/fish completions from the built binary (`gogo completions`
  # needs no config or network). Skipped when the build host can't run it.
  postInstall = pkgs.lib.optionalString
    (pkgs.stdenv.buildPlatform.canExecute pkgs.stdenv.hostPlatform) ''
      export HOME=$TMPDIR
      installShellCompletion --cmd gogo \
        --bash <($out/bin/gogo completions bash) \
        --zsh <($out/bin/gogo completions zsh) \
        --fish <($out/bin/gogo completions fish)
    '';

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
