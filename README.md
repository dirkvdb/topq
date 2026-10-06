# TopQ

An MQTT 5 topic explorer built with Rust and [GPUI Kit](https://gpui-kit.com).

![Screenshot](./data/screenshot-1.png)

## Features

- Connection management to quickly connect to your favorite brokers, with optional TLS and WebSocket support.
- Live topic tree
- Payload pane to inspect the message payloads
- Publish messages.
- Deletion of retained messages for a selected topic and its known subtopics.
- Live line and area charts for monitored numeric JSON fields

## Configuration

Settings are stored in `config.json` in the application configuration directory. On Linux, this is `$XDG_CONFIG_HOME/topq`, or `~/.config/topq` when unset.


`config.json`:

```json
{
  "mode": "system",
  "light_theme": "Ayu Light",
  "dark_theme": "Ayu Dark",
  "reduce_motion": "system",
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
- Older connection entries may use a separate `username` field; it is read and moved into the URI when saved.
- `client_id` identifies the MQTT client. Use a different ID for each concurrently connected client on the same broker to avoid disconnecting each other. If omitted, an ID is generated when the connection is loaded.
- `topics` must contain at least one unique MQTT filter. `+` matches one topic level; a final `#` matches any number of levels. `#` does not include topics starting with `$`; subscribe to `$SYS/#` separately for broker statistics. `qos` is the requested maximum delivery QoS: `0` (at most once), `1` (at least once), or `2` (exactly once). If `topics` is omitted, it defaults to `[{"topic":"#","qos":0}]`.
- Connection entries using separate `host`, `port`, `tls`, and `websocket` fields are also accepted. When saved, the broker address is written as a `connection` URI instead.

| Transport | URI scheme | Connection editor's default port |
| --- | --- | --- |
| MQTT | `mqtt://` | `1883` |
| MQTT over TLS | `mqtts://` | `8883` |
| MQTT over WebSocket | `ws://` | `9001` |
| MQTT over secure WebSocket | `wss://` | `9002` |

WebSocket connections use the fixed path `/mqtt`; custom paths are not configurable.

`validate_certificate` defaults to `true`: TLS uses the system's trusted certificates and verifies the broker hostname. Setting it to `false` disables certificate and hostname verification; encryption remains, but the broker is not authenticated. It has no effect without TLS.

Passwords are stored only in the system credential store and must not be included in the URI. A JSON `password` field is ignored. For connections with a username, the password is retrieved from the credential store. Anonymous connections (without a username) do not need a password. Saving or restoring passwords requires an available, unlocked credential store; there is no plaintext fallback.

Appearance settings live alongside connection settings in `config.json`:

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
