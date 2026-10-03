build_debug:
    cargo build

build_release:
    cargo build --release

build: build_release

test_debug test_name='' $RUST_LOG="debug":
    cargo nextest run --workspace --no-capture {{ test_name }}

test_release test_name='':
    python3 scripts/generate-test-data.py
    cargo nextest run --workspace --release {{ test_name }}

test: test_release

run:
    cargo run --release
