# TopQ

An MQTT 5 topic explorer built with Rust and [GPUI Kit](https://gpui-kit.com).

![Screenshot](./data/screenshot-1.png)

## Features

- Connection management to quickly connect to your favorite brokers, with optional TLS and WebSocket support.
- Live topic tree
- Payload pane to inspect the message payloads
- Live line and area charts for monitored numeric JSON fields, with independent per-chart type and smoothing controls and responsive rows. Clicking a field’s inline monitor icon always opens a new standalone chart, leaving existing charts unchanged and sharing the field’s history and color. Drag the icon onto a chart to compare fields on shared time/value axes, or use the chart’s **Add field…** menu with the keyboard. Adding a monitored field moves at most one standalone copy into the target; other copies and grouped charts remain unchanged. Closing a chart stops collecting a field only when no other chart uses it. Each field retains at most 300 samples.
- Deletion of retained messages for a selected topic and its known subtopics.
- Publish messages.

## Configuration

Create the files below before starting TopQ. On Linux, the configuration directory is `$XDG_CONFIG_HOME/topq`, or `~/.config/topq` when unset.

TopQ reads these files at startup and writes changes made in the application back to them. Declaratively managed, read-only files can be loaded, but saving changes to them will fail.

### Connections

`connections.json`:

```json
{
  "connections": [
    {
      "name": "Home broker",
      "connection": "mqtts://user@broker.example.com:8883",
      "client_id": "topq-workstation",
      "topics": [
        { "topic": "home/#", "qos": 1 },
        { "topic": "$SYS/#", "qos": 0 }
      ],
      "validate_certificate": true
    }
  ],
  "active_connection": "Home broker"
}
```

- `active_connection` is the name of the connection to connect to at startup. Use `null` or omit it to start without connecting. An unknown name falls back to the first connection. When multiple connections have the same non-empty name, only the first is loaded; give each connection a unique name.
- `connection` is the broker URI: use `mqtt://`, `mqtts://`, `ws://`, or `wss://` to select MQTT, MQTT over TLS, WebSocket, or secure WebSocket. Include the broker host and preferably an explicit port, for example `mqtts://mqtt-user@mqtt.lan:8883`. Put the username before `@`; omit it for an empty username. URL-encode reserved characters in the username. Do not put a password in the URI. If the URI omits the port, it defaults to `1883`, `8883`, `80`, or `443` for those schemes, respectively. IPv6 hosts must be bracketed, for example `mqtt://user@[::1]:1883`.
- Older settings files may still use a separate `username` field; TopQ reads it and moves it into the URI when saving.
- `client_id` identifies the MQTT client. Use a different ID for each concurrently connected client on the same broker to avoid disconnecting each other. If omitted, TopQ generates an ID on load.
- `topics` must contain at least one unique MQTT filter. `+` matches one topic level; a final `#` matches any number of levels. `#` does not include topics starting with `$`; subscribe to `$SYS/#` separately for broker statistics. `qos` is the requested maximum delivery QoS: `0` (at most once), `1` (at least once), or `2` (exactly once). If `topics` is omitted, it defaults to `[{"topic":"#","qos":0}]`.
- Existing settings files that use separate `host`, `port`, `tls`, and `websocket` fields are still accepted. When TopQ saves them, it writes the broker address as a `connection` URI instead.

| Transport | URI scheme | Connection editor's default port |
| --- | --- | --- |
| MQTT | `mqtt://` | `1883` |
| MQTT over TLS | `mqtts://` | `8883` |
| MQTT over WebSocket | `ws://` | `9001` |
| MQTT over secure WebSocket | `wss://` | `9002` |

WebSocket connections use the fixed path `/mqtt`; custom paths are not configurable.

`validate_certificate` defaults to `true`: TLS uses the system's trusted certificates and verifies the broker hostname. Setting it to `false` disables certificate and hostname verification; encryption remains, but the broker is not authenticated. It has no effect without TLS.

Passwords are stored only in the system credential store and must not be included in the URI. A JSON `password` field is ignored. If a connection has a username, TopQ looks up its password in the credential store. When none is found for the selected connection, TopQ shows a warning and does not attempt to connect; enter and save a password through Connection settings. Anonymous connections (without a username) do not need a password. Other credential-store failures still prevent loading connections. Saving or restoring passwords requires an available, unlocked credential store; there is no plaintext fallback.

Credentials are scoped by host, port, transport, and username. Saved connections must have unique combinations of those values, even if their names or client IDs differ.

### Appearance

`appearance.json`:

```json
{
  "mode": "system",
  "light_theme": "Ayu Light",
  "dark_theme": "Ayu Dark",
  "reduce_motion": "system"
}
```

- `mode`: `system`, `light`, or `dark`. `system` (default) follows the OS appearance.
- `light_theme` and `dark_theme`: exact theme names for each mode, not filenames. Defaults are `Ayu Light` and `Ayu Dark`. Missing themes or themes for the wrong mode fall back to the defaults.
- `reduce_motion`: `system` (default) follows the OS preference, `on` reduces motion, and `off` overrides the OS preference to allow motion.

Place custom theme JSON files in the configuration directory's `themes/` subdirectory. See the [GPUI Kit theme documentation](https://gpui-kit.com/component/theme) for the theme format.


## Development

Activate the devenv virtual environment, then run:

```bash
just run
```

## Credits
Big thanks to [MQTT Explorer](https://mqtt-explorer.com/) for the source of inspiration. I have been a long time user of MQTT Explorer and have enjoyed using it, but in a quest to reduce the number of Electron apps I use, I decided to build a native alternative.
