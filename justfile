build_debug:
    cargo build

build_release:
    cargo build --release

build: build_release

test_debug test_name='' $RUST_LOG="debug":
    cargo nextest run --workspace --no-capture {{ test_name }}

test_release test_name='':
    cargo nextest run --workspace --release {{ test_name }}

test: test_release

run:
    cargo run --release

[windows]
set shell := ["pwsh", "-NoLogo", "-NoProfile", "-Command"]

# mise must be installed outside devenv; MISE_BIN can select an absolute executable.
mise_bin := env("MISE_BIN", "mise")
macos_path := env("HOME", env("USERPROFILE", "")) + "/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"

# Install the pinned native packaging tools without inheriting devenv's environment.
setup-macos:
    /usr/bin/env -i HOME={{quote(env("HOME"))}} PATH={{quote(macos_path)}} MISE_DEFAULT_CONFIG_FILENAME=dist/mise.toml {{quote(mise_bin)}} install

# Build with mise-managed Rust and Apple's tools, using separate Cargo artifacts.
dmg:
    /usr/bin/env -i HOME={{quote(env("HOME"))}} PATH={{quote(macos_path)}} MISE_DEFAULT_CONFIG_FILENAME=dist/mise.toml {{quote(mise_bin)}} exec -- cargo build --release --locked --bin topq

    /usr/bin/env -i HOME={{quote(env("HOME"))}} PATH={{quote(macos_path)}} MISE_DEFAULT_CONFIG_FILENAME=dist/mise.toml {{quote(mise_bin)}} exec -- cargo bundle --release --format osx --binary-path dist/build/release/topq
    /usr/bin/codesign --force --sign - dist/build/release/bundle/osx/TopQ.app
    /usr/bin/codesign --verify --deep --strict --verbose=2 dist/build/release/bundle/osx/TopQ.app
    /bin/ln -sfn /Applications dist/build/release/bundle/osx/Applications
    /bin/mkdir -p dist/build/release/bundle/dmg
    /usr/bin/hdiutil create -ov -volname TopQ -srcfolder dist/build/release/bundle/osx -format UDZO dist/build/release/bundle/dmg/TopQ.dmg
    /usr/bin/hdiutil verify dist/build/release/bundle/dmg/TopQ.dmg

# Windows: select dist/mise.windows.toml via MISE_DEFAULT_CONFIG_FILENAME.
msi:
    mise exec -- cargo build --release --locked --bin topq
    mise exec -- cargo bundle --release --format wxsmsi --binary-path dist/build/release/topq.exe

# Linux: select dist/mise.linux.toml via MISE_DEFAULT_CONFIG_FILENAME.
# cargo-bundle 0.11 locates the binary via CARGO_TARGET_DIR (no --binary-path flag).
appimage:
    mise exec -- cargo build --release --locked --bin topq
    CARGO_BUNDLE_SKIP_BUILD=1 mise exec -- cargo bundle --release --format appimage

# Prepare locked dependencies for Flatpak's offline build sandbox.
[linux]
flatpak-vendor:
    cargo vendor --locked --versioned-dirs dist/flatpak/vendor > dist/flatpak/cargo-config.toml

# Check host tools before the potentially expensive dependency preparation.
[linux]
[private]
flatpak-check:
    @command -v flatpak >/dev/null 2>&1 || { echo "Missing flatpak. Install Flatpak or re-enter the Linux devenv shell; see dist/README.md." >&2; exit 1; }
    @command -v flatpak-builder >/dev/null 2>&1 || { echo "Missing flatpak-builder. Install it or re-enter the Linux devenv shell; see dist/README.md." >&2; exit 1; }
    @command -v appstreamcli >/dev/null 2>&1 || { echo "Missing appstreamcli. Install AppStream or re-enter the Linux devenv shell; see dist/README.md." >&2; exit 1; }
    @appstreamcli compose --help >/dev/null 2>&1 || { echo "Missing AppStream compose support. Install appstream-compose or re-enter the Linux devenv shell; see dist/README.md." >&2; exit 1; }

# Install the user-scoped runtime and SDK for the host architecture.
[linux]
setup-flatpak: flatpak-check
    flatpak remote-add --user --if-not-exists flathub https://flathub.org/repo/flathub.flatpakrepo
    flatpak install --user --noninteractive flathub org.freedesktop.Platform//25.08 org.freedesktop.Sdk//25.08 org.freedesktop.Sdk.Extension.rust-stable//25.08 org.freedesktop.Sdk.Extension.llvm20//25.08

# Build and export a local Flatpak bundle; run setup-flatpak first.
[linux]
flatpak: flatpak-check flatpak-vendor
    flatpak-builder --user --force-clean --state-dir=dist/build/flatpak/state --repo=dist/build/flatpak/repo dist/build/flatpak/app dist/flatpak/io.github.dirkvdb.topq.json
    flatpak build-bundle dist/build/flatpak/repo dist/build/flatpak/TopQ.flatpak io.github.dirkvdb.topq --runtime-repo=https://flathub.org/repo/flathub.flatpakrepo

# Refresh bundled themes from the source revision of gpui-kit in Cargo.lock.
sync-themes:
    python3 scripts/sync_gpui.py themes

# Replace the copied GPUI agent skills with those from the locked release.
sync-skills:
    python3 scripts/sync_gpui.py skills

# Refresh both themes and agent skills from a single locked source download.
sync-gpui:
    python3 scripts/sync_gpui.py all
