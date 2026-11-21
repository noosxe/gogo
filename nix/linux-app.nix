# `gogo-linux-app`: the lean gogo package (./app.nix), wrapped so it runs
# umu-launcher's zipapp in nixpkgs' Steam runtime FHS env (design 18 §2.1,
# R1). x86_64 Linux only. A wrapper around the built package, not a second
# Rust build: the binary is the same store path.
#
# `--set-default GOGO_UMU_PYTHON` and `GOGO_COMET` (Comet built from source,
# nix/comet.nix; design 23 decision 4): a user's own value still wins. gogo
# records the path in each game's launch.conf as UMU_PYTHON, so the
# launcher doesn't need this wrapper. The default package stays without
# the env (about 1.5 GiB), for everything but the Linux game lane.
{
  pkgs,
  gogo,
}:

let
  umuPython = import ./umu-python.nix { inherit pkgs; };
  # Comet built from source: the release binary doesn't start on NixOS.
  comet = import ./comet.nix { inherit pkgs; };
in
pkgs.symlinkJoin {
  pname = "gogo-linux-app";
  inherit (gogo) version;
  paths = [ gogo ];
  nativeBuildInputs = [ pkgs.makeWrapper ];
  postBuild = ''
    wrapProgram $out/bin/gogo \
      --set-default GOGO_UMU_PYTHON ${umuPython}/bin/${umuPython.meta.mainProgram} \
      --set-default GOGO_COMET ${comet}/bin/${comet.meta.mainProgram}
  '';

  passthru = {
    inherit umuPython comet;
    unwrapped = gogo;
  };

  meta = gogo.meta // {
    description = "gogo with the FHS env umu-launcher's zipapp runs in (Linux game lane)";
    platforms = [ "x86_64-linux" ];
    mainProgram = "gogo";
  };
}
