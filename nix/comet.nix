# Comet (imLinguin/comet, Apache-2.0), the Galaxy service stand-in gogo runs
# beside a game (design 23 decision 4), built from source at the pinned tag.
# The release binary links glibc and has no loader on NixOS, so
# `gogo-linux-app` points GOGO_COMET at this build. The version must match
# the catalog pin (a unit test checks it).
{
  pkgs,
}:

let
  # The dev shell's pinned toolchain: Comet is edition 2024.
  rustToolchain = import ./rust-toolchain.nix { inherit pkgs; };
  rustPlatform = pkgs.makeRustPlatform {
    cargo = rustToolchain;
    rustc = rustToolchain;
  };
in
rustPlatform.buildRustPackage (finalAttrs: {
  pname = "comet";
  version = "0.4.1";

  src = pkgs.fetchFromGitHub {
    owner = "imLinguin";
    repo = "comet";
    tag = "v${finalAttrs.version}";
    # proto/ is a submodule (Yepoleb/gog_protocols); build.rs compiles it.
    fetchSubmodules = true;
    hash = "sha256-NK3td9huujLM2sSU/gMPV0Bak+i+6BDwre/z/JNlR1M=";
  };

  # Comet's own Cargo.lock, no git dependencies.
  cargoHash = "sha256-iOG52ZT1oRjqUVd81UhRdXUgDkIky0qrboZfVJjuoSk=";

  # build.rs runs protoc: without PROTOC it uses a prebuilt binary from the
  # protoc-bin-vendored crate, which can't run in the sandbox. aws-lc-sys
  # (rustls) and libsqlite3-sys link nixpkgs' aws-lc and sqlite, which
  # nixpkgs' cargo hook demands (AWS_LC_SYS_USE_SYSTEM=1, system sqlite),
  # found through pkg-config. TLS is rustls: no openssl.
  nativeBuildInputs = [
    pkgs.protobuf
    pkgs.pkg-config
  ];
  buildInputs = [
    pkgs.aws-lc
    pkgs.sqlite
  ];
  env.PROTOC = "${pkgs.protobuf}/bin/protoc";

  # Not gogo's tests to run; the upstream suite needs network access.
  doCheck = false;

  meta = {
    description = "Open stand-in for the GOG Galaxy service (Galaxy SDK sign-in), built for gogo";
    homepage = "https://github.com/imLinguin/comet";
    license = pkgs.lib.licenses.asl20;
    platforms = [ "x86_64-linux" ];
    mainProgram = "comet";
  };
})
