{
  inputs = {
	nixpkgs.url = "github:nixos/nixpkgs?ref=nixos-unstable";
	fenix.url = "github:nix-community/fenix/monthly";
  };

  outputs = { self, nixpkgs, fenix }:
    let
      pkgs = nixpkgs.legacyPackages.x86_64-linux;
    in {
      devShells.x86_64-linux.default = let
        toolchain = fenix.packages.x86_64-linux.default.toolchain;
      in pkgs.mkShell {
        buildInputs = [
          toolchain
          pkgs.cargo-edit
          pkgs.git
        ];
        RUST_SRC_PATH = "${toolchain}/lib/rustlib/src/rust/library";
      };
    };
}
