# MQTT UI

A small, read-only MQTT explorer written in Rust with [GPUI Kit](https://gpui-kit.com).

## Run

With the included Nix development environment:

```sh
devenv shell
cargo run
```

Or run directly with `devenv shell -- cargo run`.

With a standalone Rust toolchain, use `cargo run` (Rust 1.95 or newer). On Linux,
GPUI also needs Fontconfig, FreeType, XKB, XCB/Wayland, and a Vulkan-capable
graphics driver. The development environment supplies the build libraries.

## Usage

1. On the first launch, enter a broker host and port, and optionally credentials.
   Enable TLS for a broker using a certificate trusted by the system (usually port 8883).
2. Click **Connect**, or press Enter. After the broker accepts the subscription,
   the connection is saved and automatically used on subsequent launches.
3. Expand the topic tree on the left. Updated topics and their parent branches
   briefly fade their names to the highlight color and back. Click a topic to see
   its latest payload and metadata on the right. Use arrow keys and Home/End to
   navigate the tree.
4. Drag the divider to resize the panes; the width is remembered between launches.
   Payload text can be selected and copied;
   the copy buttons copy the topic name or original text payload.
5. Use **Connection…** to edit settings, or **Disconnect** / **Reconnect** to control
   the connection. Dropped connections retry automatically every two seconds.

The interface defaults to Ayu and follows the system's light/dark appearance.
Use **Theme** (also available before connecting) to choose a fixed light or dark
theme. Changes apply immediately and are remembered between launches; choose
**Follow system (Ayu)** to resume automatic switching. The menu includes Ayu,
Tokyo Night, all four Catppuccin variants, Gruvbox, Solarized, Everforest, Matrix,
and GPUI Kit's original Default Light/Dark themes.

Text highlights and spinners honor
GPUI's reduced-motion policy. Use Tab/Shift+Tab to move between controls; Escape
cancels connection edits and restores focus.

### Keyboard shortcuts

| Command | Shortcut |
| --- | --- |
| Connection settings | Ctrl+, / Cmd+, |
| Focus topics | Ctrl+1 / Cmd+1 |
| Expand / collapse a branch | Right / Left, or Enter |
| First / last visible topic | Home / End |
| Narrow / widen the topic pane | Ctrl+Alt+Left / Ctrl+Alt+Right |

JSON payloads are pretty-printed with theme-aware syntax highlighting; other UTF-8 payloads are shown as text and binary
payloads as hexadecimal. Details show receive time, delivered QoS, retained flag,
payload size, and a per-topic message count. Only the latest message is kept for
each topic. Manually connecting or reconnecting clears the topic data; automatic
retries preserve the current session's data.

The app uses MQTT 3.1.1 over TCP or TLS and subscribes to `#` at QoS 2, allowing
messages to arrive at their original QoS. As specified by MQTT, `#` does not
include `$`-prefixed topics such as `$SYS`. No publishing, graphs, message history,
WebSockets, or custom TLS certificates are included in this initial version.

### Saved connection

Settings are stored in the platform's application configuration directory:

- Linux: `${XDG_CONFIG_HOME:-~/.config}/mqtt-ui/connection.json`
- macOS: `~/Library/Application Support/mqtt-ui/connection.json`
- Windows: `%APPDATA%\mqtt-ui\config\connection.json`

The JSON contains the broker settings and username, but never the MQTT password.
Passwords are saved through `keyring-rs` in the system credential store:

- Linux: Secret Service (for example GNOME Keyring or KDE Wallet), via the session D-Bus.
- macOS: Keychain Services.
- Windows: Credential Manager.

The credential store must be available and unlocked when saving or restoring a
password. If it cannot be accessed, the app reports an error and does not fall
back to plaintext storage. Connections without a password do not require the
credential store. Clearing a previously saved password removes its credential.
Passwords are scoped to the broker host, port, TLS setting, and username under
the `mqtt-ui` service name. Existing plaintext passwords are not imported;
re-enter the password in **Connection…** and connect to save it securely.

Configuration files retain owner-only permissions on Unix. Remove
`connection.json` to return to first-launch configuration.
The pane width is stored separately in `layout.json` in the same directory.
Theme selection is stored in `appearance.json`.

### Additional themes

Place GPUI Kit theme-set `.json` files in a `themes/` subdirectory alongside
`appearance.json`, then restart the app. They appear in the **Theme** menu,
grouped by light/dark appearance. The files use GPUI Kit's native format with a
`themes` array; examples are available in the [GPUI Kit theme collection](https://github.com/longbridge/gpui-kit/tree/main/themes).
Invalid files are skipped with a diagnostic on stderr; if a saved theme is
removed, the app falls back to system-following Ayu.

## Development

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features --locked -- -D warnings
```

Tests cover topic hierarchy (including empty path levels), counters, update flash
expiry, payload formatting, connection validation, and the MQTT wire flow against
a local test broker, including retained messages, reconnect, subscription refusal,
and cancellation.
UI integration tests exercise keyboard navigation, stable topic selection,
connection validation, duplicate-submission prevention, focus restoration,
clipboard copying, and pane alignment in both themes at larger font sizes.
Theme coverage includes pointer/keyboard menu selection, dismissal and focus,
saved preferences, system-following versus fixed themes, custom theme loading,
and switching editor colors and geometry across all bundled themes.
