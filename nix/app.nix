# The gogo package: builds the Rust workspace and installs its binaries.
# Consumed by flake.nix (`packages`, `apps`), enabling:
#   nix build .            /  nix build github:noosxe/gogo
#   nix run . -- --help    /  nix run github:noosxe/gogo
{
  pkgs,
}:

let
  common = import ./rust-common.nix { inherit pkgs; };
  inherit (common) rustPlatform;
  # The desktop GUI crates (design 24 §2), by the same name patterns as the
  # `gui` filter in .github/workflows/ci-jobs.yml.
  guiCrates = builtins.filter
    (name: builtins.match "gogo-(contract|skins?|ui-.*|render-.*|gui|gui-.*)" name != null)
    (builtins.attrNames (pkgs.lib.filterAttrs (_: type: type == "directory") (builtins.readDir ../crates)));
in
rustPlatform.buildRustPackage (common.args // {
  pname = "gogo";

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
})
