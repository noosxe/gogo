# The gogo development shell (AGENTS.md: all work happens inside this shell —
# never use compilers, tools, or binaries from the host system).
#
# Content only: the nixpkgs pin and the rust-overlay wiring live in flake.nix,
# which assembles `pkgs` per system and imports this file.
{
  pkgs,
  # Linux: make Mesa's software Vulkan (lavapipe) the only Vulkan device, for
  # the GPU renderer's tests (design 24 §7).
  lavapipe ? false,
}:

let
  # Pinned toolchain — single source of truth in ./rust-toolchain.nix.
  rustToolchain = import ./rust-toolchain.nix { inherit pkgs; };
  guiLibraryPath = pkgs.lib.makeLibraryPath [
    pkgs.wayland
    pkgs.libxkbcommon
    pkgs.vulkan-loader
    pkgs.libglvnd
    pkgs.fontconfig
    # winit and softbuffer on X11.
    pkgs.libx11
    pkgs.libxcb
    pkgs.libxcursor
    pkgs.libxi
    pkgs.libxrandr
  ];
in
pkgs.mkShell {
  packages = [
    rustToolchain           # rustc, cargo, rustfmt, clippy, rust-src
    pkgs.rust-analyzer      # LSP
    pkgs.cargo-nextest      # test runner      (design 08 §6)
    pkgs.cargo-deny         # license/advisory (design 08 §6)
    pkgs.git
    pkgs.just               # task runner (smoke scripts, CI shorthands)
    pkgs.pkg-config
    pkgs.stdenv.cc          # C compiler for ring/asm builds — from nixpkgs, never system
    pkgs.jq                 # scripts/vm-test.sh JSON (tart list, run summary)
    pkgs.shellcheck         # lint for scripts/*.sh
    pkgs.actionlint         # lint for .github/workflows/*.yml
    pkgs.innoextract        # unpack GOG offline installers for PE-scan research (docs/22 §7), never run
  ] ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isDarwin [
    pkgs.libiconv           # some crates still want it on macOS
  ] ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [
    pkgs.btrfs-progs        # mkfs.btrfs for the snapshots-btrfs CI job's image (design 20 §10.3)
    pkgs.openssl            # gog-warp's reqwest keeps default native-tls → openssl-sys on Linux
    pkgs.rustPlatform.bindgenHook # libclang for gog-warp's xdelta3 bindgen (macOS finds Xcode's)
  ];

  env = {
    RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
  } // pkgs.lib.optionalAttrs lavapipe {
    VK_DRIVER_FILES = "${pkgs.mesa}/share/vulkan/icd.d/lvp_icd.${pkgs.stdenv.hostPlatform.parsed.cpu.name}.json";
    # Or wgpu could pick the host's GL instead.
    WGPU_BACKEND = "vulkan";
  };

  # The desktop GUI (design 24 §5, §7): winit dlopens wayland and libxkbcommon
  # (and the X11 libraries on X11), wgpu dlopens the Vulkan loader and libEGL
  # (libglvnd, for its GL fallback), fontique reads fontconfig. Prepended in the hook, so a library path set
  # outside the shell survives. WAYLAND_DISPLAY and DISPLAY stay set: this
  # shell is used interactively, and agents run the GUI only through its
  # headless tools (AGENTS.md).
  shellHook = pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
    export LD_LIBRARY_PATH="${guiLibraryPath}''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
  '' + ''
    echo "[gogo devshell] $(rustc --version)"
  '';
}
