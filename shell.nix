{ pkgs ? import <nixpkgs> {} }:

pkgs.mkShell {
    nativeBuildInputs = [
      pkgs.cargo
      pkgs.rustc
      pkgs.rustfmt
    ];

    # Used by corepc-node during integration tests: don't download a bitcoind
    # binary, use the one provided by Nix instead.
    BITCOIND_SKIP_DOWNLOAD = "1";
    BITCOIND_EXE = "${pkgs.bitcoind}/bin/bitcoind";
}
