# The tools of scripts/gui-headless.sh (design 24 §5): the whole PATH of a run.
# Stock sway from the flake's nixpkgs pin: the GUI is a Wayland client, so none
# of the Xwayland work behind nix/linux-e2e.nix's patched wlroots applies. A
# package rather than part of the dev shell, like linux-e2e-tools.
{
  pkgs,
}:

pkgs.buildEnv {
  name = "gogo-gui-headless-tools";
  paths = [
    pkgs.sway-unwrapped    # headless compositor, swaymsg; the wrapper wants a D-Bus config in /etc
    pkgs.grim              # screenshots
    pkgs.bubblewrap        # the sandbox
    pkgs.python3           # scripts/linux-e2e-helper.py (screenshot checks), run with -I
    pkgs.bash
    pkgs.coreutils
    pkgs.gnugrep
    pkgs.util-linux        # setpriv --pdeathsig
  ];
  meta = {
    description = "Tools of the headless GUI runner (scripts/gui-headless.sh)";
    platforms = pkgs.lib.platforms.linux;
  };
}
