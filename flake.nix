{
  description = "Topq MQTT explorer";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    { nixpkgs, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
        linuxRuntimeDeps = with pkgs; [
          fontconfig
          libxcb
          libxkbcommon
          wayland
          vulkan-loader
        ];
      in
      {
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "topq";
          version = "0.1.0";
          src = ./.;

          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = [
            pkgs.pkg-config
          ]
          ++ pkgs.lib.optionals pkgs.stdenv.isLinux [
            pkgs.makeWrapper
            pkgs.imagemagick
          ];
          buildInputs =
            with pkgs;
            [
              fontconfig.dev
              vulkan-headers
            ]
            ++ pkgs.lib.optionals pkgs.stdenv.isLinux linuxRuntimeDeps
            ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [ pkgs.apple-sdk_15 ];

          postInstall = pkgs.lib.optionalString pkgs.stdenv.isLinux ''
            install -Dm644 topq.desktop $out/share/applications/topq.desktop
            install -d $out/share/icons/hicolor/512x512/apps
            magick data/logo.png -resize 512x512 $out/share/icons/hicolor/512x512/apps/topq.png
          '';

          postFixup = pkgs.lib.optionalString pkgs.stdenv.isLinux ''
            wrapProgram $out/bin/topq \
              --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath linuxRuntimeDeps}
          '';
        };
      }
    );
}
