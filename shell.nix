{ pkgs ? import <nixpkgs> {} }:

pkgs.mkShell {
  nativeBuildInputs = with pkgs; [
    cargo
    pkg-config
    rustc
    clang_14 # linker
  ];

  buildInputs = with pkgs; [
    openssl
  ];
}
