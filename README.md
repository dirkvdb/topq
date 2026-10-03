# MQTT UI

A read-only MQTT 3.1.1 topic explorer built with Rust and [GPUI Kit](https://gpui-kit.com).

## Features

- Multiple saved TCP/TLS broker connections with automatic reconnect.
- Live topic tree and latest per-topic payload, shown as formatted JSON, text, or hex, with message metadata.

The app subscribes to `#` at QoS 2, which excludes `$`-prefixed topics such as `$SYS`. There is no publishing, message history, WebSocket support, or custom TLS certificate support.

## Configuration

Connections and preferences are stored in the platform's `mqtt-ui` configuration directory (`connections.json`, `appearance.json`, and `layout.json`). Existing `connection.json` files are not migrated.

Passwords are stored in the system credential store, never in the JSON files. Saving or restoring a password requires an available, unlocked credential store; there is no plaintext fallback.

## Development
Activate the devenv virtual environment, then run:

```bash
just run
```
