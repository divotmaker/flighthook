# Flighthook Web API

## Quick-Start: Real-Time Shot Stream

The fastest way to get shot data is the WebSocket at `/api/ws`. After a brief
init handshake, you'll receive a JSON message for every shot as it happens -- no
polling needed. This is ideal for video timestamp tagging, overlay triggers, stat
trackers, or any integration that needs to react to shots in real time.

Every `shot_result` event contains the full `ShotData` object (ball flight, club
data if available, spin data if available). Velocities and distances are
unit-tagged strings (e.g. `"67.2mps"`, `"180.5m"`). To convert to a standard
unit system without parsing suffixes yourself, POST the `ShotData` to
`/api/shots/convert?units=imperial` (or `metric`). You can also use `?units=` on
`GET /api/shots` for historical data. Angles are degrees, spin is RPM. The
`estimated` flag is `true` when the shot was synthesized from partial radar data.

Other useful events on the same connection:

- `actor_status` -- device/integration lifecycle + state (battery, tilt, club, etc.)
- `game_state_command` / `set_mode` -- global detection mode changed (full/chipping/putting)

### Terminal

```bash
# Stream shots with websocat + jq (install: cargo install websocat)
# Send the init handshake, keep stdin open to hold the connection, filter for shot results
(echo '{"type":"start","name":"cli"}'; cat) | \
  websocat ws://localhost:3030/api/ws | \
  jq 'select(.event.kind == "launch_monitor" and .event.event.type == "shot_result") | .event.event.shot'
```

```bash
# Convert a ShotData JSON blob to imperial units (yards, mph, feet, inches)
curl -s -X POST 'http://localhost:3030/api/shots/convert?units=imperial' \
  -H 'Content-Type: application/json' \
  -d @shot.json | jq
```

### Python

```python
import json, websockets, asyncio

async def main():
    async with websockets.connect("ws://localhost:3030/api/ws") as ws:
        # Init handshake
        await ws.send(json.dumps({"type": "start", "name": "my-dashboard"}))
        init = json.loads(await ws.recv())
        print(f"Connected: source_id={init['source_id']}")

        # Stream events
        async for msg in ws:
            event = json.loads(msg)
            fh_event = event["event"]
            if fh_event["kind"] == "launch_monitor":
                inner = fh_event["event"]
                if inner["type"] == "shot_result":
                    shot = inner["shot"]
                    # Convert to imperial via /api/shots/convert (no suffix parsing needed)
                    import requests
                    converted = requests.post(
                        "http://localhost:3030/api/shots/convert?units=imperial",
                        json=shot,
                    ).json()
                    ball = converted["ball"]
                    print(f"Shot #{converted['shot_number']}: "
                          f"speed={ball['launch_speed']}, carry={ball['carry_distance']}")

asyncio.run(main())
```

### JavaScript

```javascript
const ws = new WebSocket("ws://localhost:3030/api/ws");
ws.onopen = () => {
  ws.send(JSON.stringify({ type: "start", name: "my-dashboard" }));
};
ws.onmessage = async (e) => {
  const msg = JSON.parse(e.data);
  if (msg.type === "init") {
    console.log("Connected:", msg.source_id);
    return;
  }
  const { event } = msg;
  if (event.kind === "launch_monitor" && event.event.type === "shot_result") {
    const { shot } = event.event;
    // Convert to imperial via /api/shots/convert (no suffix parsing needed)
    const resp = await fetch(
      "http://localhost:3030/api/shots/convert?units=imperial",
      { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(shot) },
    );
    const converted = await resp.json();
    console.log(`Shot #${converted.shot_number}`, converted.ball);
  }
};
```

---

## REST Endpoints

### GET /api/status

Comprehensive system state (all actors -- launch monitors and integrations).

**Response** `200 OK`:

```json
{
  "mode": "full",
  "actors": {
    "mevo.0": {
      "name": "Mevo WiFi",
      "status": "connected",
      "telemetry": {
        "armed": "true",
        "battery_pct": "85",
        "tilt": "0.5",
        "roll": "-0.2",
        "temp_c": "28.5",
        "external_power": "false",
        "device_info": "XXXXXXXX, H/W: XXXX v1.0, F/W: 1.00"
      },
    },
    "gspro.0": {
      "name": "Local GSPro",
      "status": "connected",
      "telemetry": {
        "club": "Driver",
        "handed": "RH"
      }
    }
  }
}
```

- `mode`: global detection mode (`"full"` | `"putting"` | `"chipping"`), `null` if not yet set
- `status`: `"starting"` | `"disconnected"` | `"connected"` | `"reconnecting"`
- `telemetry`: actor-specific key/value pairs (all string values)

Common telemetry keys for launch monitors: `armed`, `shooting`, `battery_pct`,
`tilt`, `roll`, `temp_c`, `external_power`, `device_info`.

Common telemetry keys for integrations: `club`, `handed`, `error`.

---

### GET /api/shots

Shot history (most recent N shots, FIFO, max 1000 stored).

**Query params**:

- `limit` (optional, default `50`): max shots to return
- `units` (optional): `"imperial"` or `"metric"`. Converts all distance and
  velocity fields to the specified unit system. Imperial: yards/feet/inches/mph.
  Metric: meters/m/s. Default (omitted): returns values in native units (as
  stored by the launch monitor accumulator, typically metric).

**Response** `200 OK` -- `ShotData[]`:

```json
[
  {
    "source": "mevo.0",
    "shot_number": 42,
    "ball": {
      "launch_speed": "67.2mps",
      "launch_azimuth": -1.3,
      "launch_elevation": 14.2,
      "carry_distance": "180.5m",
      "max_height": "28.3m",
      "total_distance": "195.0m",
      "backspin_rpm": 3200,
      "sidespin_rpm": -450
    },
    "club": {
      "club_speed": "42.1mps",
      "path": -2.1,
      "attack_angle": -3.5,
      "face_angle": 1.2,
      "dynamic_loft": 18.4,
      "smash_factor": 1.42,
      "club_speed_post": "29.5mps",
      "club_offset": "0.005m",
      "club_height": "0.012m"
    },
    "spin": {
      "total_spin": 3230,
      "spin_axis": -8.0
    },
    "estimated": false
  }
]
```

With `?units=imperial`:

```json
{
  "ball": {
    "launch_speed": "150.3mph",
    "carry_distance": "197.4yd",
    "max_height": "92.8ft",
    "total_distance": "213.3yd"
  },
  "club": {
    "club_speed": "94.2mph",
    "club_offset": "0.2in",
    "club_height": "0.47in"
  }
}
```

- `ball`: `BallFlight` -- launch conditions and distances. Always present.
  Velocity fields are unit-tagged strings (`"67.2mps"`, `"150.3mph"`).
  Distance fields are unit-tagged strings (`"180.5m"`, `"197.4yd"`).
- `club`: `ClubData` or `null`. Club head data.
- `spin`: `SpinData` or `null`. Total spin and axis.
- `estimated`: `true` if synthesized from E8 fallback (no D4 received).

---

### POST /api/shots/convert

Stateless unit conversion utility for WebSocket consumers. Accepts a `ShotData`
body (the same JSON you receive on the WebSocket) and returns it with all
distance and velocity fields converted to the requested unit system.

This saves external integrations (Python, C#, JS, etc.) from having to parse
unit-tagged strings and re-implement conversion math.

**Query params**:

- `units` (required): `"imperial"` or `"metric"`

**Request** -- `ShotData` (as received from the WebSocket `shot_result` event):

```json
{
  "source": "mevo.0",
  "shot_number": 42,
  "ball": {
    "launch_speed": "67.2mps",
    "launch_azimuth": -1.3,
    "launch_elevation": 14.2,
    "carry_distance": "180.5m",
    "max_height": "28.3m",
    "total_distance": "195.0m",
    "backspin_rpm": 3200,
    "sidespin_rpm": -450
  },
  "club": {
    "club_speed": "42.1mps",
    "path": -2.1,
    "attack_angle": -3.5,
    "face_angle": 1.2,
    "dynamic_loft": 18.4,
    "smash_factor": 1.42
  },
  "spin": {
    "total_spin": 3230,
    "spin_axis": -8.0
  },
  "estimated": false
}
```

**Response** `200 OK` -- `ShotData` with converted units:

```json
{
  "ball": {
    "launch_speed": "150.3mph",
    "carry_distance": "197.4yd",
    "max_height": "92.8ft",
    "total_distance": "213.3yd"
  },
  "club": {
    "club_speed": "94.2mph"
  }
}
```

Angles (degrees) and spin (RPM) pass through unchanged. The `estimated` and
`source` fields are preserved as-is.

**Errors**:

- `400 Bad Request`: `units` param missing or not `"imperial"`/`"metric"`

---

### POST /api/mode

Change the global detection mode. Emits `GameStateCommand::SetMode` on the
bus; all launch monitor actors react to the mode change.

**Request**:

```json
{
  "mode": "putting"
}
```

- `mode`: `"full"` | `"putting"` | `"chipping"`

**Response**: `202 Accepted` (no body)

---

### GET /api/settings

Full persisted config (mirrors `config.toml`).

**Response** `200 OK` -- `FlighthookConfig`:

```json
{
  "webserver": {
    "0": {
      "name": "Web Server",
      "bind": "0.0.0.0:3030"
    }
  },
  "mevo": {
    "0": {
      "name": "Mevo WiFi",
      "address": "192.168.2.1:5100",
      "ball_type": 0,
      "tee_height": "1.5in",
      "range": "9ft",
      "surface_height": "0in",
      "track_pct": 80.0,
      "use_partial": "chipping_only"
    }
  },
  "mock_monitor": {},
  "gspro": {
    "0": {
      "name": "Local GSPro",
      "address": "127.0.0.1:921"
    }
  },
  "random_club": {}
}
```

- Keys are type-prefixed global IDs: `mevo.0`, `mock_monitor.0`, `gspro.0`, `random_club.0`, `webserver.0`
- All launch monitor config fields are optional (omitted = use defaults)

---

### POST /api/settings

Full config replacement via event-sourced bus pattern. Emits a `ConfigCommand`
on the bus, waits for `ConfigOutcome` from SystemActor, then returns the response.

**Request**: complete `FlighthookConfig` JSON (same shape as GET response)

**Response** `200 OK`:

```json
{
  "restart_required": false,
  "restarted": ["mevo.0"],
  "stopped": ["gspro.1"]
}
```

- `restart_required`: `true` only when a webserver bind address changed
  (cannot be reloaded at runtime). Actor-level restarts are handled automatically.
- `restarted`: actors that were stopped and recreated (e.g. address changed).
  Omitted when empty.
- `stopped`: actors that were removed from the config. Omitted when empty.

**Side effects**:

- Emits `ConfigCommand::ReplaceAll` on the bus (processed by SystemActor)
- SystemActor persists the new config to disk and reconciles actors:
  - Radar settings changes (ball type, tee height, etc.) are applied in-place
    via `reconfigure()` emitting `ConfigChanged` on the bus (no actor restart)
  - Address changes trigger actor restart (shutdown + recreate)
  - Removed config sections stop the corresponding actor
  - New config sections start new actors
- SystemActor emits `ConfigOutcome` on the bus (used for request-reply + actor
  name cache refresh in the web layer)

---

## WebSocket

### Connection

**Endpoint**: `GET /api/ws` (HTTP upgrade to WebSocket)

Text-frame JSON messages in both directions.

### Init Handshake

Before streaming begins, the client must complete a handshake:

1. **Client sends** a `start` message:

```json
{
  "type": "start",
  "name": "My Dashboard"
}
```

- `type` (required): must be `"start"`
- `name` (optional): human-readable client identifier for server-side logging.
  Defaults to `"anonymous"` if empty or omitted.

2. **Server responds** with an `init` message:

```json
{
  "type": "init",
  "source_id": "a1b2c3d4",
  "global_state": {
    "player_info": null,
    "club_info": {
      "club": "Driver"
    }
  }
}
```

- `source_id`: unique identifier for this WebSocket session (`ws.{8-hex-chars}`)
- `global_state`: current snapshot of shared state
  - `player_info`: `{ "handed": "RH" }` or `null`
  - `club_info`: `{ "club": "Driver" }` or `null`

3. **Server streams** `FlighthookMessage` events (described below).

Messages sent before the `start` handshake (except `close`) are ignored.

---

### Server -> Client: FlighthookMessage

After the init handshake, the server streams `FlighthookMessage` events. All
messages share this envelope:

```json
{
  "source": "mevo.0",
  "timestamp": "2026-02-27T12:34:56.789012345Z",
  "raw_payload": "0a1b2c...",
  "event": { "kind": "...", ... }
}
```

- `source`: global ID of the originator (e.g. `"mevo.0"`, `"gspro.0"`, `"ws.a1b2c3d4"`)
- `timestamp`: ISO 8601 UTC timestamp
- `raw_payload`: optional, present on wire-level messages. Binary payloads are
  lowercase hex strings (no spaces). Text payloads (e.g. GSPro JSON) are
  included as-is. Omitted when not applicable.
- `event`: a `FlighthookEvent` tagged by `"kind"` (see below)

---

#### Event Kinds

Events are tagged by the `"kind"` field on the `event` object. Sub-events within
each kind are tagged by `"type"`.

---

##### actor_status

Generic actor lifecycle and state update. Emitted by all actors (launch monitors
and integrations) when their status or state changes.

```json
{
  "source": "mevo.0",
  "timestamp": "...",
  "event": {
    "kind": "actor_status",
    "status": {
      "status": "connected",
      "telemetry": {
        "armed": "true",
        "battery_pct": "85",
        "tilt": "0.5",
        "roll": "-0.2",
        "temp_c": "28.5",
        "external_power": "false"
      },
    }
  }
}
```

- `status.status`: `"starting"` | `"disconnected"` | `"connected"` | `"reconnecting"`
- `status.telemetry`: actor-specific key/value pairs (all string values)

---

##### launch_monitor

Shot data from a launch monitor.

```json
{
  "source": "mevo.0",
  "timestamp": "...",
  "event": {
    "kind": "launch_monitor",
    "event": {
      "type": "shot_result",
      "shot": {
        "source": "mevo.0",
        "shot_number": 42,
        "ball": {
          "launch_speed": "67.2mps",
          "launch_azimuth": -1.3,
          "launch_elevation": 14.2,
          "carry_distance": "180.5m",
          "max_height": "28.3m",
          "total_distance": "195.0m",
          "backspin_rpm": 3200,
          "sidespin_rpm": -450
        },
        "club": null,
        "spin": null,
        "estimated": false
      }
    }
  }
}
```

---

##### config_changed

Configuration update notification. Emitted when an actor's settings change
via the settings panel / `reconfigure()`. Consumed by integrations (e.g.
the GSPro bridge uses `use_partial` to filter partial shots).

```json
{
  "source": "mevo.0",
  "timestamp": "...",
  "event": {
    "kind": "config_changed",
    "config": {
      "ball_type": 0,
      "tee_height": "1.5in",
      "range": "9ft",
      "surface_height": "0in",
      "track_pct": 80.0,
      "use_partial": "chipping_only"
    }
  }
}
```

---

##### global_state_command

Global state mutation from an integration or WebSocket client.

```json
{
  "source": "gspro.0",
  "timestamp": "...",
  "event": {
    "kind": "global_state_command",
    "event": {
      "type": "set_club_info",
      "club_info": {
        "club": "7 Iron"
      }
    }
  }
}
```

- The originating integration is identified by `source` on the outer `FlighthookMessage`

Sub-events (`"type"` values): `set_player_info`, `set_club_info`, `set_mode`.

---

##### global_state_snapshot

Full snapshot of the current global state. Emitted after a `global_state_command`
is applied.

```json
{
  "timestamp": "...",
  "event": {
    "kind": "global_state_snapshot",
    "player_info": { "handed": "RH" },
    "club_info": { "club": "7 Iron" }
  }
}
```

---

##### user_data

Opaque data from a third-party WebSocket client.

```json
{
  "timestamp": "...",
  "event": {
    "kind": "user_data",
    "integration_type": "custom",
    "source_id": "a1b2c3d4",
    "data": { ... }
  }
}
```

---

##### alert

User-visible warning or error notification. Info/debug/trace-level diagnostics
stay in the tracing backend and are not emitted on the bus.

```json
{
  "source": "mevo.0",
  "timestamp": "...",
  "event": {
    "kind": "alert",
    "level": "warn",
    "message": "Could not process message: Wire(ChecksumMismatch { ... })"
  }
}
```

- `level`: `"warn"` | `"error"`
- `message`: human-readable description of the condition

---

### Client -> Server

JSON text frames with a `"cmd"` field. These are processed independently of
the init handshake -- they emit events on the unified bus.

#### mode

Change the global detection mode. Emits `GameStateCommand::SetMode` on the bus.

```json
{
  "cmd": "mode",
  "mode": "putting"
}
```

- `cmd` (required): `"mode"`
- `mode` (required): `"full"` | `"putting"` | `"chipping"`

---

## Error Handling

- Invalid JSON on WS: silently ignored
- Unknown `cmd` value on WS: silently ignored
- Routing to non-existent launch monitor: silently ignored
- No error feedback to WS clients for failed commands

---

## Enums Reference

### ActorStatus

`"starting"` | `"disconnected"` | `"connected"` | `"reconnecting"`

### ShotDetectionMode

`"full"` | `"putting"` | `"chipping"`

### PartialMode

`"never"` | `"chipping_only"` | `"always"`

### UnitSystem

`"imperial"` | `"metric"`

### Velocity (unit-tagged string)

Format: `{number}{suffix}`. Suffixes: `mph` (miles/hour), `mps` (meters/second),
`kph` (km/hour), `fps` (feet/second).

### Distance (unit-tagged string)

Format: `{number}{suffix}`. Suffixes: `yd` (yards), `ft` (feet), `in` (inches),
`m` (meters), `cm` (centimeters).
