{
  description = "settings-dialog: network, Bluetooth, sound and screen settings built on fastframe";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { nixpkgs, rust-overlay, ... }:
    let
      forAll = f: nixpkgs.lib.genAttrs [ "x86_64-linux" "aarch64-linux" ] (system:
        f (import nixpkgs { inherit system; overlays = [ rust-overlay.overlays.default ]; }));
      runtimeLibs = pkgs: with pkgs; [
        libGL
        libxkbcommon
        wayland
        libx11
        libxcursor
        libxi
        libxrandr
      ];
    in
    {
      packages = forAll (pkgs:
        let
          toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          rustPlatform = pkgs.makeRustPlatform { cargo = toolchain; rustc = toolchain; };
        in
        {
          default = rustPlatform.buildRustPackage {
            pname = "settings-dialog";
            version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;
            src = pkgs.lib.cleanSource ./.;
            cargoLock = {
              lockFile = ./Cargo.lock;
              allowBuiltinFetchGit = true;
            };
            nativeBuildInputs = [ pkgs.pkg-config ];
            # winit and glutin load these with dlopen.
            postFixup = ''
              patchelf --add-rpath ${pkgs.lib.makeLibraryPath (runtimeLibs pkgs)} $out/bin/settings-dialog
            '';
            meta.mainProgram = "settings-dialog";
          };
        });

      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          packages = [
            (pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml)
            pkgs.pkg-config
            pkgs.watchexec
            pkgs.iw
            (pkgs.writeShellScriptBin "watch" ''
              exec watchexec --restart --watch src --watch themes --watch Cargo.toml --exts rs,toml,json -- cargo run -- "$@"
            '')
          ];
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (runtimeLibs pkgs);
        };
      });
    };
}
