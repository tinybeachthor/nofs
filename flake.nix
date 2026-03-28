{
  inputs = {
	nixpkgs.url = "github:nixos/nixpkgs?ref=nixos-unstable";
	fenix.url = "github:nix-community/fenix/monthly";
  };

  outputs = { self, nixpkgs, fenix }:
    let
      pkgs = nixpkgs.legacyPackages.x86_64-linux;
      toolchain = fenix.packages.x86_64-linux.default.toolchain;
      rustPlatform = pkgs.makeRustPlatform {
        cargo = toolchain;
        rustc = toolchain;
      };
    in {
      packages.x86_64-linux.default = rustPlatform.buildRustPackage {
        pname = "nofs";
        version = "0.1.0";
        src = ./.;
        cargoLock.lockFile = ./Cargo.lock;
        nativeBuildInputs = [ pkgs.pkg-config ];
        buildInputs = [
          pkgs.fuse3
          pkgs.wayland
          pkgs.libxkbcommon
          pkgs.xorg.libxcb
          pkgs.fontconfig
          pkgs.libGL
        ];
      };

      devShells.x86_64-linux.default = pkgs.mkShell {
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
