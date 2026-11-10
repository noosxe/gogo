# The tools of scripts/linux-e2e.sh (docs/linux-e2e.md): the whole PATH of a
# run, all from the flake's nixpkgs pin, so every run has the same sway,
# Xwayland and grim. A package rather than part of the dev shell: the macOS
# shell can't have it, and nobody but this script needs a compositor.
{
  pkgs,
}:

let
  # wlroots 0.20.2 can leave an X11 window unmapped for good: the
  # WL_SURFACE_SERIAL message on the X socket and Xwayland's first buffer
  # commit on the Wayland socket aren't ordered, and when the buffer comes
  # first, the surface is associated with a buffer already there and no map
  # follows. A client that draws once and idles (the self-test's xterm, a
  # launcher dialog) never shows a window (#309). Fixed upstream in
  # wlroots 08a9f9f1, not in a release or on the 0.20 branch yet: drop the
  # patch once the pin's wlroots has it.
  wlroots = pkgs.wlroots_0_20.overrideAttrs (old: {
    patches = (old.patches or [ ]) ++ [ ./wlroots-xwm-association-map-race.patch ];
  });
  sway = pkgs.sway-unwrapped.override { wlroots_0_20 = wlroots; };
in
pkgs.buildEnv {
  name = "gogo-linux-e2e-tools";
  paths = [
    sway                   # headless compositor, swaymsg; the wrapper wants a D-Bus config in /etc
    pkgs.xwayland
    pkgs.grim              # screenshots
    pkgs.libxkbcommon      # xkbcli: the virtual keyboard's pc105/us keymap
    pkgs.xwininfo          # proves the display is the headless one
    pkgs.xterm             # the self-test's client
    pkgs.pulseaudio        # the sandbox's null sink: pulseaudio, pactl, pacat
    pkgs.bubblewrap
    pkgs.passt             # pasta: the build sandbox's own netns, NATed out to the internet
    pkgs.iproute2          # ip: lo up in the build's netns
    pkgs.jq
    pkgs.python3           # scripts/linux-e2e-helper.py, run with -I
    pkgs.mesa-demos        # glxinfo: which GL driver --gpu gives
    pkgs.vulkan-tools      # vulkaninfo: which Vulkan device --gpu gives
    pkgs.bash
    pkgs.coreutils
    pkgs.findutils
    pkgs.gnugrep
    pkgs.gnused
    pkgs.gawk
    pkgs.procps
    pkgs.util-linux
  ];
  meta = {
    description = "Tools of the headless sway e2e harness (scripts/linux-e2e.sh)";
    platforms = pkgs.lib.platforms.linux;
  };
}
