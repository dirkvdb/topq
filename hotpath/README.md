# Performance profiling

Run the commands below from the project root with the devenv shell active.

The devenv shell includes [hotpath-rs](https://github.com/pawurb/hotpath-rs) 0.28.4
(with the TUI and `hotpath-samply` wrapper), `samply`, and Linux's `setsid`.
Re-enter the shell after changing `devenv.nix`.

```bash
just profile             # Timing, allocation, and thread metrics for 30 seconds
just profile 60000       # Profile for 60 seconds; duration is milliseconds
just profile 0           # Run until the application window is closed
just profile-console     # Live dashboard in a second terminal
just profile-cpu         # Also collect sampled CPU stacks
```

Timed runs automatically exit and print a report; avoid starting destructive
operations near the end of the profiling window. Use `0` for an interactive
session with normal shutdown. To save machine-readable results:

```bash
HOTPATH_OUTPUT_FORMAT=json HOTPATH_OUTPUT_PATH=target/hotpath.json just profile
```

The `profiling` Cargo profile uses release optimizations with debug symbols.
Profiling is opt-in: ordinary builds leave hotpath instrumentation disabled.
The initial coverage measures view rendering, tree rows and synchronization,
animation updates, message application, payload formatting, and JSON diffs.
`TopicRow::render` measures actual cached-row rebuilds: only flashing or invalidated
rows rebuild, while unchanged rows reuse their layout and paint. The explorer and
virtual tree can still compose each frame without rebuilding every row.
Timing totals include nested calls, so do not add them together as exclusive CPU
time. The CPU recipe also captures stacks inside GPUI and other dependencies;
use the `samply load` command printed by hotpath to explore them.

CPU sampling requires host permissions that a devenv shell cannot grant. On
Linux, restrictive `perf_event_paranoid` or `ptrace_scope` settings may block it;
on macOS, run `samply setup`. See the upstream
[CPU profiling guide](https://hotpath.rs/cpu_profiling) for platform setup and
security implications. No kernel settings are changed by these recipes. Timing
and allocation profiling do not need these permissions.

Metrics and reports remain local; no hotpath Cloud integration is enabled.
[`policy.toml`](policy.toml) contains the upstream default policy for optional future
regression checks.
