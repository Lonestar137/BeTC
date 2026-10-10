{ pkgs ? import <nixpkgs> { } }:

let
  runtimeLibraries = with pkgs; [
    alsa-lib
    fontconfig
    libx11
    libxcursor
    libxi
    libxinerama
    libxrandr
    libxkbcommon
    udev
    vulkan-loader
    wayland
  ];
in
pkgs.mkShell {
  nativeBuildInputs = with pkgs; [
    pkg-config
    rustup
    stdenv.cc
  ];

  buildInputs = runtimeLibraries;

  LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibraries;

  shellHook = ''
    if [ -z "''${CARGO_TARGET_DIR:-}" ] && [ -d /mnt/ventoy/tmp/cache ]; then
      export CARGO_TARGET_DIR=/mnt/ventoy/tmp/cache
    fi
    # Player state must not ride $CARGO_TARGET_DIR (which may be a scratch disk): pin benilla-config
    # to the tree so characters/settings persist independently of where the build lands.
    export BENILLA_HOME="$PWD/benilla-config"
    export WOW_DATA=./WoW
    printf 'Cargo target dir: %s\n' "''${CARGO_TARGET_DIR:-workspace target/}"
  '';
}
