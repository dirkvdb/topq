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

# Refresh bundled themes from the source revision of gpui-kit in Cargo.lock.
sync-themes:
    python3 scripts/sync_themes.py
