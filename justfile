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

# Refresh bundled themes from the source revision of gpui-kit in Cargo.lock.
sync-themes:
    python3 scripts/sync_themes.py
