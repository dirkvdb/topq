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

Create the files below before starting TopQ. On Linux, the configuration directory is `$XDG_CONFIG_HOME/mqtt-ui`, or `~/.config/mqtt-ui` when unset. The directory name is `mqtt-ui`, not `topq`.

TopQ reads these files at startup and writes changes made in the application back to them. Declaratively managed, read-only files can be loaded, but saving changes to them will fail.

### Connections

`connections.json`:

```json
{
  "connections": [
    {
      "name": "Home broker",
      "host": "broker.example.com",
      "port": 8883,
      "client_id": "topq-workstation",
      "topics": [
        { "topic": "home/#", "qos": 1 },
        { "topic": "$SYS/#", "qos": 0 }
      ],
      "username": "",
      "tls": true,
      "validate_certificate": true,
      "websocket": false,
      "password_in_keyring": false
    }
  ],
  "selected": 0
}
```

- `selected` is the zero-based index of the connection to connect to at startup. Use `null` or omit it to start without connecting. An out-of-range index falls back to the first connection.
- `host` is a hostname or IP address, without a URL scheme, port, or path.
- `client_id` identifies the MQTT client. Use a different ID for each concurrently connected client on the same broker to avoid disconnecting each other. If omitted, TopQ generates an ID on load.
- `topics` must contain at least one unique MQTT filter. `+` matches one topic level; a final `#` matches any number of levels. `#` does not include topics starting with `$`; subscribe to `$SYS/#` separately for broker statistics. `qos` is the requested maximum delivery QoS: `0` (at most once), `1` (at least once), or `2` (exactly once). If `topics` is omitted, it defaults to `[{"topic":"#","qos":0}]`.
- `tls` and `websocket` select the transport. Set `port` explicitly; omitting it always uses `1883`, even with TLS or WebSocket enabled.

| Transport | `tls` | `websocket` | Connection editor's default port |
| --- | --- | --- | --- |
| MQTT (`mqtt://`) | `false` | `false` | `1883` |
| MQTT over TLS (`mqtts://`) | `true` | `false` | `8883` |
| MQTT over WebSocket (`ws://`) | `false` | `true` | `9001` |
| MQTT over secure WebSocket (`wss://`) | `true` | `true` | `9002` |

WebSocket connections use the fixed path `/mqtt`; custom paths are not configurable.

`validate_certificate` defaults to `true`: TLS uses the system's trusted certificates and verifies the broker hostname. Setting it to `false` disables certificate and hostname verification; encryption remains, but the broker is not authenticated. It has no effect without TLS.

Passwords are stored only in the system credential store. A JSON `password` field is ignored. Leave `password_in_keyring` omitted or `false` for a connection without a stored password; enter and save passwords through Connection settings. Set it to `true` only when the matching credential already exists. Saving or restoring passwords requires an available, unlocked credential store; there is no plaintext fallback.

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
