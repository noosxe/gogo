# `gogo-umu-python`: Python 3 inside nixpkgs' Steam runtime FHS env, the
# interpreter gogo runs umu-launcher's release zipapp with on Nix (design 18
# §2.1, R1; verified in docs/21 §8 item 1); `gogo-linux-app` wraps gogo with
# it (./linux-app.nix). x86_64 Linux only: the env is
# multiArch, and its i686 half doesn't exist on aarch64.
#
# Mirrors nixpkgs' umu-launcher (pkgs/by-name/um/umu-launcher/package.nix),
# which is `steam.buildRuntimeEnv` around umu: here the program is Python
# and umu is gogo's pinned zipapp, so `gogo-umu-python <store>/umu-run …`
# is that package with the pin gogo chose. A minimal buildFHSEnv isn't
# enough: no i386 loader, no /run/opengl-driver mapping (docs/21 §8 item 1).
#
# Free software only: `buildRuntimeEnv` is a passthru of `steam`, but the env
# it builds holds none of Steam (steam-unwrapped is unfree; this needs no
# allowUnfree).
{
  pkgs,
}:

pkgs.steam.buildRuntimeEnv {
  pname = "gogo-umu-python";
  inherit (pkgs.python3) version;

  extraPkgs = p: [ p.python3 ];
  runScript = "python3";

  meta = {
    description = "Python 3 in the Steam runtime FHS env, for umu-launcher's zipapp (gogo)";
    homepage = "https://github.com/noosxe/gogo";
    mainProgram = "gogo-umu-python";
    platforms = [ "x86_64-linux" ];
  };
}
