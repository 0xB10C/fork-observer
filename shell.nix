{ pkgs ? import <nixpkgs> {} }:

pkgs.mkShell {
    nativeBuildInputs = [
      pkgs.cargo
      pkgs.pkg-config
      pkgs.openssl
    ];
}
