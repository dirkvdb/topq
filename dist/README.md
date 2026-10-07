# macOS packaging

Packaging uses mise-managed Rust 1.95.0 and cargo-bundle 0.12.0. Cargo-bundle
provides app and DMG generation; cargo-packager offers an updater but is not
needed for this workflow. Cargo-dist is aimed at broader release automation.

## Setup

Install [mise](https://mise.jdx.dev/getting-started.html) outside devenv. Select
Apple's Xcode or Command Line Tools using `xcode-select`, with the Metal toolchain
available for GPUI. From the repository root:

```sh
mise trust dist/mise.toml
just setup-macos
just dmg
```

`just dmg-native` is equivalent to `just dmg`. All commands are directly in the
justfile; there are no packaging helper scripts. Setup downloads Rust and
cargo-bundle and may compile cargo-bundle.

Both recipes start mise with a clean environment, without inherited devenv/Nix
compiler wrappers, SDK overrides, or linker flags. The packaging config is
selected explicitly using `MISE_DEFAULT_CONFIG_FILENAME=dist/mise.toml`; it does
not change the development environment. Apple's Clang is selected explicitly.
Cargo/rustup homes and compilation artifacts are isolated in `dist/build/`.
Cargo configs in the checkout or ancestor directories still apply; ensure they
do not add Nix library paths.

The clean PATH includes `~/.local/bin`, `/opt/homebrew/bin`, `/usr/local/bin`, and
Apple's system directories. For mise installed elsewhere, set `MISE_BIN` to its
absolute path when invoking either recipe.

## Output

- `dist/build/release/bundle/dmg/TopQ.app`
- `dist/build/release/bundle/dmg/TopQ.dmg`

The recipe verifies the DMG checksum. Open the DMG and drag the app into
Applications. Each build packages the host architecture, not a universal binary.
Inspect library dependencies with:

```sh
otool -L dist/build/release/bundle/dmg/TopQ.app/Contents/MacOS/topq
```

A build without external libraries should list only `/System/Library/` and
`/usr/lib/` dependencies, not `/nix/store/` paths.

Cargo-bundle requires `[package.metadata.bundle]` in the application's
`Cargo.toml`; that is the only packaging configuration kept outside this
directory, apart from the requested justfile recipes. Change the default bundle
identifier to your own stable reverse-DNS identifier before public distribution.
The recipe does not Developer ID-sign or notarize the package; these are separate
steps for public releases.
