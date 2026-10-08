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
}
