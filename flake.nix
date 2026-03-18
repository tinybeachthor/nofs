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
        nativeBuildInputs = [
          pkgs.pkg-config
        ];
        buildInputs = [
          toolchain
          pkgs.cargo-edit
          pkgs.git
          pkgs.fuse3
          pkgs.wayland
          pkgs.libxkbcommon
          pkgs.xorg.libxcb
          pkgs.fontconfig
          pkgs.libGL
        ];
        RUST_SRC_PATH = "${toolchain}/lib/rustlib/src/rust/library";
        LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [
          pkgs.wayland
          pkgs.libxkbcommon
          pkgs.libGL
        ];
      };
    };
}
