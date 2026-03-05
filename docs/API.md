# Flighthook Web API

## Quick-Start: Real-Time Shot Stream

The fastest way to get shot data is the WebSocket at `/api/ws`. After a brief
init handshake, you'll receive JSON messages for every shot lifecycle event as
it happens -- no polling needed. This is ideal for video timestamp tagging,
overlay triggers, stat trackers, or any integration that needs to react to
shots in real time.

Shot data arrives as a sequence of correlated events sharing a `ShotKey`:

1. `shot_trigger` -- ball strike detected (no data yet)
2. `ball_flight` -- ball flight data (speed, angles, distances, spin)
3. `club_path` -- club data (speed, path, attack angle, face angle, loft)
4. `shot_finished` -- shot complete, accumulators should finalize

Use the `ShotAccumulator` pattern (or wait for `shot_finished`) to collect the
full shot. The `estimated` flag on `ball_flight` is `true` when the shot was
synthesized from partial radar data.

Velocities and distances are unit-tagged strings (e.g. `"67.2mps"`,
`"180.5m"`). To convert to a standard unit system without parsing suffixes
yourself, POST the `ShotData` to `/api/shots/convert?units=imperial` (or
`metric`). You can also use `?units=` on `GET /api/shots` for historical data.
Angles are degrees, spin is RPM.

Other useful events on the same connection:

- `actor_status` -- device/integration lifecycle + state (battery, tilt, club, etc.)
- `shot_detection_mode` -- global detection mode changed (full/chipping/putting)
- `launch_monitor_state` -- device armed/ball-detected state changes

### Terminal

```bash
# Stream shots with websocat + jq (install: cargo install websocat)
# Send the init handshake, keep stdin open to hold the connection, filter for shot events
(echo '{"type":"start","name":"cli"}'; cat) | \
  websocat ws://localhost:3030/api/ws | \
  jq 'select(.event.kind == "shot_finished")'
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

        # Accumulate shot data
        shots = {}  # key: (source, shot_id) -> {ball, club}
        async for msg in ws:
            event = json.loads(msg)
            fh = event["event"]
            kind = fh["kind"]

            if kind == "ball_flight":
                key = (event["source"], fh["key"]["shot_id"])
                shots[key] = {"ball": fh["ball"], "estimated": fh["estimated"]}
            elif kind == "club_path":
                key = (event["source"], fh["key"]["shot_id"])
                if key in shots:
                    shots[key]["club"] = fh["club"]
            elif kind == "shot_finished":
                key = (event["source"], fh["key"]["shot_id"])
                shot = shots.pop(key, None)
                if shot:
                    print(f"Shot #{fh['key']['shot_number']}: "
                          f"speed={shot['ball']['launch_speed']}")

asyncio.run(main())
```

### JavaScript

```javascript
const ws = new WebSocket("ws://localhost:3030/api/ws");
const shots = new Map();

ws.onopen = () => {
  ws.send(JSON.stringify({ type: "start", name: "my-dashboard" }));
};
ws.onmessage = (e) => {
  const msg = JSON.parse(e.data);
  if (msg.type === "init") {
    console.log("Connected:", msg.source_id);
    return;
  }
  const { event, source } = msg;
  const key = event.key ? `${source}:${event.key.shot_id}` : null;

  switch (event.kind) {
    case "ball_flight":
      shots.set(key, { ball: event.ball, estimated: event.estimated });
      break;
    case "club_path":
      if (shots.has(key)) shots.get(key).club = event.club;
      break;
    case "shot_finished":
      const shot = shots.get(key);
      shots.delete(key);
      if (shot) console.log(`Shot #${event.key.shot_number}`, shot.ball);
      break;
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
- `estimated`: `true` if synthesized from partial radar data.

---

### POST /api/shots/convert

Stateless unit conversion utility for WebSocket consumers. Accepts a `ShotData`
body (the same JSON you receive on the WebSocket) and returns it with all
distance and velocity fields converted to the requested unit system.

This saves external integrations (Python, C#, JS, etc.) from having to parse
unit-tagged strings and re-implement conversion math.

**Query params**:

- `units` (required): `"imperial"` or `"metric"`

**Request** -- `ShotData` (as accumulated from shot lifecycle events):

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

Change the global detection mode. Emits `ShotDetectionMode` on the bus;
all launch monitor actors react to the mode change.

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
  "default_units": "imperial",
  "chipping_clubs": ["GW", "SW", "LW"],
  "putting_clubs": ["PT"],
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
      "track_pct": 80.0
    }
  },
  "mock_monitor": {},
  "gspro": {
    "0": {
      "name": "Local GSPro",
      "address": "127.0.0.1:921",
      "use_estimated": "chipping_only"
    }
  },
  "random_club": {}
}
```

- Keys are type-prefixed global IDs: `mevo.0`, `mock_monitor.0`, `gspro.0`, `random_club.0`, `webserver.0`
- All launch monitor config fields are optional (omitted = use defaults)
- `use_estimated` on GSPro sections controls whether estimated ball flights are
  forwarded to the integration: `"never"`, `"chipping_only"` (default), `"always"`

---

### POST /api/settings

Full config replacement via event-sourced bus pattern. Emits a `ConfigCommand`
on the bus, waits for `ConfigOutcome` from SystemActor, then returns the response.

**Request**: complete `FlighthookConfig` JSON (same shape as GET response)

**Response** `200 OK`:

```json
{
  "restarted": ["mevo.0"],
  "stopped": ["gspro.1"]
}
```

- `restarted`: actors that were stopped and recreated (e.g. address changed).
  Omitted when empty.
- `stopped`: actors that were removed from the config. Omitted when empty.

**Side effects**:

- Emits `ConfigCommand` (action: `ReplaceAll`) on the bus (processed by SystemActor)
- SystemActor persists the new config to disk and reconciles actors:
  - Address/routing/settings changes trigger actor restart (shutdown + recreate)
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

Events are tagged by the `"kind"` field on the `event` object. All fields are
directly on the event object (flat struct variants).

---

##### shot_trigger

Ball strike detected. Emitted immediately by the launch monitor -- no data yet.

```json
{
  "source": "mevo.0",
  "timestamp": "...",
  "event": {
    "kind": "shot_trigger",
    "key": { "shot_id": "550e8400-e29b-41d4-a716-446655440000", "shot_number": 42 }
  }
}
```

- `key.shot_id`: UUID v4 string, globally unique across sessions
- `key.shot_number`: session-level monotonic counter from the launch monitor

---

##### ball_flight

Ball flight data available. May arrive before or after `club_path`.

```json
{
  "source": "mevo.0",
  "timestamp": "...",
  "event": {
    "kind": "ball_flight",
    "key": { "shot_id": "550e8400-...", "shot_number": 42 },
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
    "estimated": false
  }
}
```

- `estimated`: `true` when synthesized from partial radar data.
  Integrations decide whether to use estimated shots via `use_estimated` config.

---

##### club_path

Club path data available. May arrive before or after `ball_flight`.

```json
{
  "source": "mevo.0",
  "timestamp": "...",
  "event": {
    "kind": "club_path",
    "key": { "shot_id": "550e8400-...", "shot_number": 42 },
    "club": {
      "club_speed": "42.1mps",
      "path": -2.1,
      "attack_angle": -3.5,
      "face_angle": 1.2,
      "dynamic_loft": 18.4,
      "smash_factor": 1.42
    }
  }
}
```

---

##### shot_finished

Shot sequence complete. Accumulators should finalize and emit the composed shot.

```json
{
  "source": "mevo.0",
  "timestamp": "...",
  "event": {
    "kind": "shot_finished",
    "key": { "shot_id": "550e8400-...", "shot_number": 42 }
  }
}
```

---

##### launch_monitor_state

Armed/ready/ball state from a launch monitor.

```json
{
  "source": "mevo.0",
  "timestamp": "...",
  "event": {
    "kind": "launch_monitor_state",
    "armed": true,
    "ball_detected": true
  }
}
```

---

##### player_info

Player info update (handedness).

```json
{
  "source": "gspro.0",
  "timestamp": "...",
  "event": {
    "kind": "player_info",
    "player_info": { "handed": "RH" }
  }
}
```

---

##### club_info

Club selection update.

```json
{
  "source": "gspro.0",
  "timestamp": "...",
  "event": {
    "kind": "club_info",
    "club_info": { "club": "7I" }
  }
}
```

---

##### shot_detection_mode

Global detection mode changed.

```json
{
  "source": "system",
  "timestamp": "...",
  "event": {
    "kind": "shot_detection_mode",
    "mode": "chipping"
  }
}
```

- `mode`: `"full"` | `"putting"` | `"chipping"`

---

##### actor_status

Generic actor lifecycle and state update. Emitted by all actors (launch monitors
and integrations) when their status or telemetry changes.

```json
{
  "source": "mevo.0",
  "timestamp": "...",
  "event": {
    "kind": "actor_status",
    "status": "connected",
    "telemetry": {
      "armed": "true",
      "battery_pct": "85",
      "tilt": "0.5",
      "roll": "-0.2",
      "temp_c": "28.5",
      "external_power": "false"
    }
  }
}
```

- `status`: `"starting"` | `"disconnected"` | `"connected"` | `"reconnecting"`
- `telemetry`: actor-specific key/value pairs (all string values)

---

##### config_command

Config mutation request (emitted by POST handler, processed by SystemActor).
External consumers can observe these to track config changes in flight.

```json
{
  "source": "web",
  "timestamp": "...",
  "event": {
    "kind": "config_command",
    "request_id": "abc123",
    "action": { "type": "replace_all", "config": { ... } }
  }
}
```

---

##### config_outcome

Config mutation outcome (emitted by SystemActor after processing). Always
emitted after a `config_command`, even for fire-and-forget commands.

```json
{
  "source": "system",
  "timestamp": "...",
  "event": {
    "kind": "config_outcome",
    "request_id": "abc123",
    "restarted": ["mevo.0"],
    "stopped": [],
    "started": []
  }
}
```

- `request_id`: correlation ID from the originating `config_command`, or omitted
  for fire-and-forget mutations
- `restarted`, `stopped`, `started`: lists of affected actor IDs (omitted when empty)

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

Change the global detection mode. Emits `ShotDetectionMode` on the bus.

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

### EstimatedMode

`"never"` | `"chipping_only"` | `"always"`

### UnitSystem

`"imperial"` | `"metric"`

### Velocity (unit-tagged string)

Format: `{number}{suffix}`. Suffixes: `mph` (miles/hour), `mps` (meters/second),
`kph` (km/hour), `fps` (feet/second).

### Distance (unit-tagged string)

Format: `{number}{suffix}`. Suffixes: `yd` (yards), `ft` (feet), `in` (inches),
`m` (meters), `cm` (centimeters).
