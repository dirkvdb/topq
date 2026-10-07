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

# mise must be installed outside devenv; MISE_BIN can select an absolute executable.
mise_bin := env("MISE_BIN", "mise")
macos_path := env("HOME") + "/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"

# Install the pinned native packaging tools without inheriting devenv's environment.
setup-macos:
    /usr/bin/env -i HOME={{quote(env("HOME"))}} PATH={{quote(macos_path)}} MISE_DEFAULT_CONFIG_FILENAME=dist/mise.toml {{quote(mise_bin)}} install

# Build with mise-managed Rust and Apple's tools, using separate Cargo artifacts.
dmg:
    /usr/bin/env -i HOME={{quote(env("HOME"))}} PATH={{quote(macos_path)}} MISE_DEFAULT_CONFIG_FILENAME=dist/mise.toml {{quote(mise_bin)}} exec -- cargo build --release --locked --bin topq
    /usr/bin/sips -z 512 512 data/icon.png --out dist/build/icon_512x512.png
    /usr/bin/sips -z 1024 1024 data/icon.png --out dist/build/icon_512x512@2x.png
    /usr/bin/env -i HOME={{quote(env("HOME"))}} PATH={{quote(macos_path)}} MISE_DEFAULT_CONFIG_FILENAME=dist/mise.toml {{quote(mise_bin)}} exec -- cargo bundle --release --format dmg --binary-path dist/build/release/topq
    /usr/bin/hdiutil verify dist/build/release/bundle/dmg/TopQ.dmg

# Refresh bundled themes from the source revision of gpui-kit in Cargo.lock.
sync-themes:
    python3 scripts/sync_themes.py
