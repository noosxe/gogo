# The desktop GUI packages, Linux only (design 24 §5, §7; #552):
#   nix run .#gogo-gui          the application
#   nix run .#gogo-gui-fixture  the same on fictional data, for host passes
# Separate from ./app.nix, so the CLI package stays free of GUI code.
{
  pkgs,
  # Build with the `fixture` feature. Off in the real package (#613), which
  # carries no fictional data.
  fixture ? false,
}:

let
  inherit (pkgs) lib;
  common = import ./rust-common.nix { inherit pkgs; };
  inherit (common) rustPlatform;

  # The fixture package's command always passes `--fixture`: without it the
  # window would read the real account. It has its own name, so both packages
  # can be installed together.
  command = if fixture then "gogo-gui-fixture" else "gogo-gui";
  binary = if fixture then "libexec/gogo-gui" else "bin/gogo-gui";

  # What winit, softbuffer and wgpu dlopen. Each dlopen comes from the
  # executable itself (the crates are linked into it), so its RUNPATH finds
  # them, and nothing the GUI starts inherits a LD_LIBRARY_PATH. GPU drivers
  # come from /run/opengl-driver, which the loader and libglvnd already read.
  # Fonts are bundled.
  runtimeLibraries = [
    pkgs.wayland         # libwayland-client, libwayland-egl
    pkgs.libxkbcommon    # libxkbcommon, libxkbcommon-x11
    pkgs.vulkan-loader   # libvulkan
    pkgs.libglvnd        # libEGL, wgpu's GL fallback
    pkgs.libx11          # libX11, libX11-xcb
    pkgs.libxcb
    pkgs.libxcursor
    pkgs.libxi
  ];
in
rustPlatform.buildRustPackage (common.args // {
  pname = command;

  buildFeatures = lib.optional fixture "fixture";
  cargoBuildFlags = [ "-p" "gogo-gui" ];
  # `--render-png` against the committed goldens, `--check-skin` and the exit
  # codes. The tests turn `fixture` on for their own build (the crate's
  # dev-dependency on itself); the installed binary is the build phase's. The
  # rest of the GUI crates' tests run in CI's `gui` job.
  cargoTestFlags = [ "-p" "gogo-gui" "--test" "cli" ];

  # gog-warp: openssl-sys, and bindgen over xdelta3 (see ./app.nix).
  nativeBuildInputs = [
    pkgs.pkg-config
    rustPlatform.bindgenHook
  ] ++ lib.optional fixture pkgs.makeBinaryWrapper;
  buildInputs = [ pkgs.openssl ];

  postInstall = lib.optionalString fixture ''
    mkdir -p $out/libexec
    mv $out/bin/gogo-gui $out/${binary}
    makeBinaryWrapper $out/${binary} $out/bin/${command} --add-flags --fixture
  '';

  # After fixup's RPATH shrinking, which would drop what is only dlopened.
  postFixup = ''
    patchelf --add-rpath ${lib.makeLibraryPath runtimeLibraries} $out/${binary}
  '';

  # What the installed files do, beyond what the tests' own build shows.
  doInstallCheck = true;
  nativeInstallCheckInputs = lib.optional fixture pkgs.python3;
  installCheckPhase = ''
    runHook preInstallCheck

    gui=$out/bin/${command}
    "$gui" --version
    "$gui" --check-skin crates/gogo-skins/skins/default

    rpath=$(patchelf --print-rpath $out/${binary})
    for dir in ${lib.concatMapStringsSep " " (p: "${lib.getLib p}/lib") runtimeLibraries}; do
      case ":$rpath:" in
        *":$dir:"*) ;;
        *) echo "RUNPATH lacks $dir"; exit 1 ;;
      esac
    done
  '' + (if fixture then ''
    # The installed binary draws what the golden holds.
    "$gui" --render-png "$TMPDIR/shell.png"
    python3 -I ${./gui-golden-check.py} ${../scripts/linux-e2e-helper.py} \
      crates/gogo-gui-testkit/tests/goldens/default-shell@1x.png "$TMPDIR/shell.png"
  '' else ''
    # No fictional data in the real package: a usage error.
    status=0
    "$gui" --render-png "$TMPDIR/shell.png" || status=$?
    if [ "$status" != 2 ]; then
      echo "--render-png exited $status, not 2: is the fixture feature on?"
      exit 1
    fi
  '') + ''
    runHook postInstallCheck
  '';

  meta = {
    description =
      if fixture
      then "gogo's desktop GUI on fictional data (fixture build, for testing the window)"
      else "gogo's desktop GUI";
    homepage = "https://github.com/noosxe/gogo";
    # ofl: the embedded Noto Sans and JetBrains Mono.
    license = with lib.licenses; [
      mit
      asl20
      ofl
    ];
    platforms = lib.platforms.linux;
    mainProgram = command;
  };
})
