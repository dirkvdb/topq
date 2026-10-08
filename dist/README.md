# Packaging

Packaging uses mise-managed just 1.56.0, Rust 1.95.0, cargo-binstall 1.25.2,
and cargo-bundle 0.12.0. Cargo-bundle
provides app and DMG generation; cargo-packager offers an updater but is not
needed for this workflow. Cargo-dist is aimed at broader release automation.

## macOS setup

Install [mise](https://mise.jdx.dev/getting-started.html) outside devenv. Select
Apple's Xcode or Command Line Tools using `xcode-select`, with the Metal toolchain
available for GPUI. From the repository root, bootstrap outside devenv:

```sh
mise trust dist/mise.toml
MISE_DEFAULT_CONFIG_FILENAME=dist/mise.toml mise install
MISE_DEFAULT_CONFIG_FILENAME=dist/mise.toml mise exec -- just dmg
```

Once installed, `mise exec -- just setup-macos` can refresh tools using the same
configuration selection. There is no separate just installation.

Build commands are directly in the justfile; there are no packaging helper
scripts. All three platform configs install cargo-binstall before Cargo-backed
tools. Mise uses it to prefer prebuilt cargo-bundle binaries from upstream
releases, with third-party [cargo-quickinstall](https://github.com/cargo-bins/cargo-quickinstall)
artifacts also enabled to increase binary availability. This trusts that service's
builds in addition to upstream releases; set `cargo.binstall_quickinstall = false`
in the selected config to use only upstream binaries. If no compatible binary is
available, mise falls back to `cargo install` with the lockfile. Other binstall
errors still fail setup. Just, Rust, .NET, and cargo-binstall itself are downloaded
rather than compiled. This speeds tool setup, not compilation of TopQ.

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

## macOS output

- `dist/build/release/bundle/osx/TopQ.app`
- `dist/build/release/bundle/dmg/TopQ.dmg`

The recipe creates the app with cargo-bundle, ad-hoc signs the complete bundle,
verifies its signature, and creates and verifies the DMG. Open the DMG and drag the app into
Applications. Each build packages the host architecture, not a universal binary.
Inspect library dependencies with:

```sh
otool -L dist/build/release/bundle/osx/TopQ.app/Contents/MacOS/topq
```

A build without external libraries should list only `/System/Library/` and
`/usr/lib/` dependencies, not `/nix/store/` paths.

Cargo-bundle requires `[package.metadata.bundle]` in the application's
`Cargo.toml`; that is the only packaging configuration kept outside this
directory, apart from the requested justfile recipes. Change the default bundle
identifier to your own stable reverse-DNS identifier before public distribution.
Ad-hoc signing seals the app bundle but does not establish a trusted developer
identity. Downloaded builds remain subject to Gatekeeper. The recipe does not
Developer ID-sign or notarize the package; these are separate steps for public
releases. For local testing only, after verifying that a download is your own
trusted build, you can remove quarantine from that specific installed app:

```sh
xattr -dr com.apple.quarantine /Applications/TopQ.app
```

Do not disable Gatekeeper globally or apply this workaround to untrusted apps.

## Windows MSI

The distribution workflow also runs on `windows-latest`, with an x64 MSVC
environment and mise-managed just, Rust, cargo-bundle, and .NET 8 from
`dist/mise.windows.toml`. The .NET SDK uses isolated mode so other installed SDKs
do not override the selected version. It runs
`mise exec -- just msi` and uploads the MSI generated under
`dist/build/release/bundle/wxsmsi/bin` (currently `bin/x64/Release/topq.msi`).

For a local Windows build, install mise and Visual Studio's C++ build tools.
Mise installs just, Rust, cargo-bundle, and the .NET 8 SDK for you. From a Developer
PowerShell session at the repository root, select `dist/mise.windows.toml` using
`MISE_DEFAULT_CONFIG_FILENAME`, trust that configuration, run `mise install`,
and then `mise exec -- just msi`.

The workflow uses mise for portable tools on all platforms. Apple's Metal SDK,
Windows' MSVC environment, and Linux's native build libraries remain
platform-specific setup steps.

Windows uses cargo-bundle's WiX-backed `wxsmsi` format rather than its experimental
built-in MSI database generator. Cargo-bundle downloads WiX 6.0.2 through NuGet
when building the installer. This backend adds shortcuts and upgrade handling,
and includes DLLs placed alongside the executable. The MSI is unsigned.

`dist/Directory.Build.targets` is automatically imported by the generated WiX
project beneath `dist/build/`. It selects the x64 installer platform and corrects
cargo-bundle 0.12.0's hardcoded 32-bit `ProgramFilesFolder` to
`ProgramFiles64Folder` before WiX compilation. This avoids ICE80 while keeping
MSI validation enabled and using the prebuilt cargo-bundle tool. The correction
runs after each regeneration of `installer.wxs`; repeated builds are also safe.
It updates the directory in the generated `<Fragment>`, verifies the resulting
parent directory, and fails explicitly if the expected structure changes. A
`TopQ MSI: x64 installation directory verified` message confirms the fix ran.
Cargo-bundle's separate ICE69 shortcut component-reference warnings may remain.

## Linux AppImage

The Linux job builds on Ubuntu 22.04 (x86_64), with mise-managed just, Rust and
cargo-bundle from `dist/mise.linux.toml`. It installs native compiler, Fontconfig,
X11/XCB, Wayland, Vulkan and OpenSSL development packages through apt, then runs
`mise exec -- just appimage`.

The output is `dist/build/release/bundle/appimage/topq_0.1.0_x86_64.AppImage`
(the version comes from `Cargo.toml`). For a local Linux build, select
`dist/mise.linux.toml` using `MISE_DEFAULT_CONFIG_FILENAME`, trust the config,
install the native dependencies listed in the workflow, run `mise install`, and
then `mise exec -- just appimage` outside devenv.

After downloading the workflow artifact, extract it and mark the AppImage
executable before launching:

```sh
chmod +x topq_0.1.0_x86_64.AppImage
./topq_0.1.0_x86_64.AppImage
```

Cargo-bundle downloads the official AppImage runtime and packages the executable,
icons and desktop entry. It does not automatically collect shared libraries;
compatible system libraries (including Fontconfig/XCB/XKB), a working Vulkan
GPU driver, and a desktop session are still required. Building on Ubuntu 22.04
does not guarantee compatibility with older distributions. Systems without FUSE
can use the runtime's `--appimage-extract-and-run` option. The AppImage is unsigned.

## Shared icons

All platforms use the prepared icons in `dist/`. To regenerate them from
`data/icon.png` on macOS:

```sh
sips -z 512 512 data/icon.png --out dist/icon_512x512.png
sips -z 1024 1024 data/icon.png --out dist/icon_512x512@2x.png
```
