{ pkgs, inputs, lib, ... }:
let
  hotpath = pkgs.rustPlatform.buildRustPackage rec {
    pname = "hotpath";
    version = "0.28.4";

    src = pkgs.fetchCrate {
      inherit pname version;
      hash = "sha256-7ccHWqwtuq+r2pf/TIyy2X1ujHd8Hce9g8cpZCcCnLY=";
    };
    cargoHash = "sha256-wrv4GU2ZN5zGbA4o9y8xOJyfzc1FAOX1+U3PnSafJ04=";

    buildFeatures = [ "tui" ];
    buildFlags = [ "--bin" "hotpath" "--bin" "hotpath-samply" ];
    doCheck = false;

    meta = {
      description = "Rust profiler CLI with a terminal dashboard and samply wrapper";
      homepage = "https://github.com/pawurb/hotpath-rs";
      license = lib.licenses.mit;
      mainProgram = "hotpath";
    };
  };
in
{
  tasks."ci:checks" = {
    exec = "just build && just test";
  };

  enterShell = ''
    skill_dir="$DEVENV_ROOT/.agents/skills/rust-best-practices"

    ${pkgs.coreutils}/bin/mkdir -p "$DEVENV_ROOT/.agents/skills"
    ${pkgs.coreutils}/bin/rm -rf -- "$skill_dir"
    ${pkgs.coreutils}/bin/cp -rL --no-preserve=mode -- \
      "${inputs.s-stack}/skills/rust-best-practices" \
      "$skill_dir"
  '';

  languages.rust = {
    enable = true;
    channel = "stable";
    version = "1.95.0";
    components = [
      "rustc"
      "cargo"
      "rust-src"
      "rustfmt"
      "clippy"
    ];
  };

  packages = with pkgs; [
    cargo-nextest
    hotpath
    samply
    just
    sccache
    pkg-config
    fontconfig
    fontconfig.dev
    vulkan-headers
    libxkbcommon
    xorg.libxcb
  ] ++ lib.optionals pkgs.stdenv.isLinux [
    (lib.getBin util-linux)
  ] ++ lib.optionals pkgs.stdenv.isDarwin [
    apple-sdk_15
  ];

  env = {
    RUSTC_WRAPPER = "${pkgs.sccache}/bin/sccache";
  } // lib.optionalAttrs pkgs.stdenv.isLinux {
    LD_LIBRARY_PATH = lib.makeLibraryPath [
      pkgs.wayland
      pkgs.libxkbcommon
      pkgs.xorg.libxcb
      pkgs.vulkan-loader
    ];
  };
}
