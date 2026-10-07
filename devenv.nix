{ pkgs, inputs, lib, ... }:
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
    just
    python3
    sccache
    pkg-config
    fontconfig
    fontconfig.dev
    vulkan-headers
    libxkbcommon
    xorg.libxcb
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
