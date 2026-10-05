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

# Optimized timing/allocation report; duration is milliseconds (0 waits for window close).
profile duration_ms='30000':
    HOTPATH_SHUTDOWN_MS="{{duration_ms}}" cargo run --locked --profile profiling --features hotpath,hotpath-alloc

# Includes sampled CPU stacks; requires host profiling permissions.
profile-cpu duration_ms='30000':
    HOTPATH_SHUTDOWN_MS="{{duration_ms}}" {{if os() == "linux" { "setsid -w " } else { "" }}}cargo run --locked --profile profiling --features hotpath,hotpath-alloc,hotpath-cpu

# Run in a second terminal while a profiling build is active.
profile-console:
    hotpath console
