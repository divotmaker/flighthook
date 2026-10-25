# Flighthook Web API

## Quick-Start: Real-Time Shot Stream

The fastest way to get shot data is the WebSocket at `/frp`. After a brief
init handshake, you'll receive JSON messages for every shot lifecycle event as
it happens -- no polling needed. This is ideal for video timestamp tagging,
overlay triggers, stat trackers, or any integration that needs to react to
shots in real time.

Shot data arrives as a sequence of correlated events sharing a `ShotKey`:

1. `shot_trigger` -- ball strike detected (no data yet)
2. `ball_flight` -- ball flight data (speed, angles, distances, spin), plus
   `derived` values flighthook simulated for fields the device didn't report
   (beta; see [ball_flight](#ball_flight))
3. `club_path` -- club data (speed, path, attack angle, face angle, loft)
4. `face_impact` -- face impact location
5. `shot_finished` -- shot complete, accumulators should finalize

Use the `ShotAccumulator` pattern (or wait for `shot_finished`) to collect the
full shot. All `BallFlight` and `ClubData` fields are `Option` (matching FRP spec).

Velocities and distances are unit-tagged strings (e.g. `"67.2mps"`,
`"180.5m"`). To convert to a standard unit system without parsing suffixes
yourself, POST the `ShotData` to `/api/shots/convert?units=imperial` (or
`metric`). You can also use `?units=` on `GET /api/shots` for historical data.
Angles are degrees, spin is RPM.

Other useful events on the same connection:

- `device_telemetry` -- device-reported state (ready, battery, tilt, temp, identity)
- `actor_status` -- actor/connection lifecycle (status enum, mode, radar_mode)
- `set_detection_mode` -- detection mode and/or handedness change (both optional, latched)

### Terminal

```bash
# Stream shots with websocat + jq (install: cargo install websocat)
# Send the init handshake, keep stdin open to hold the connection, filter for shot events
(echo '{"kind":"start","version":["0.1.0"],"name":"cli"}'; cat) | \
  websocat ws://localhost:5880/frp | \
  jq 'select(.event.kind == "shot_finished")'
```

```bash
# Convert a ShotData JSON blob to imperial units (yards, mph, feet, inches)
curl -s -X POST 'http://localhost:5880/api/shots/convert?units=imperial' \
  -H 'Content-Type: application/json' \
  -d @shot.json | jq
```

### Python

```python
import json, websockets, asyncio

async def main():
    async with websockets.connect("ws://localhost:5880/frp") as ws:
        # Init handshake
        await ws.send(json.dumps({"kind": "start", "version": ["0.1.0"], "name": "my-dashboard"}))
        init = json.loads(await ws.recv())
        print(f"Connected: actor_id={init['actor_id']}")

        # Accumulate shot data
        shots = {}  # key: (actor, shot_id) -> {ball, club}
        async for raw in ws:
            msg = json.loads(raw)
            ev = msg["event"]
            kind = ev["kind"]

            if kind == "ball_flight":
                key = (msg["actor"], ev["key"]["shot_id"])
                shots[key] = {"ball": ev["ball"]}
            elif kind == "club_path":
                key = (msg["actor"], ev["key"]["shot_id"])
                if key in shots:
                    shots[key]["club"] = ev["club"]
            elif kind == "shot_finished":
                key = (msg["actor"], ev["key"]["shot_id"])
                shot = shots.pop(key, None)
                if shot:
                    print(f"Shot #{ev['key']['shot_number']}: "
                          f"speed={shot['ball']['launch_speed']}")

asyncio.run(main())
```

### JavaScript

```javascript
const ws = new WebSocket("ws://localhost:5880/frp");
const shots = new Map();

ws.onopen = () => {
  ws.send(JSON.stringify({ kind: "start", version: ["0.1.0"], name: "my-dashboard" }));
};
ws.onmessage = (e) => {
  const msg = JSON.parse(e.data);
  if (msg.kind === "init") {
    console.log("Connected:", msg.actor_id);
    return;
  }
  const { event, actor } = msg;
  const key = event.key ? `${actor}:${event.key.shot_id}` : null;

  switch (event.kind) {
    case "ball_flight":
      shots.set(key, { ball: event.ball });
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
        "ready": "true",
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
        "club": "DR"
      }
    }
  }
}
```

- `mode`: global detection mode (`"full"` | `"putting"` | `"chipping"`), `null` if not yet set
- `status`: `"starting"` | `"disconnected"` | `"connected"` | `"reconnecting"`
- `telemetry`: actor-specific key/value pairs (all string values)

Telemetry keys are merged from two sources with no overlap:
- **Device-reported** (from `device_telemetry`): `ready`, `battery_pct`, `tilt`,
  `roll`, `temp_c`, `external_power`
- **Actor-framework** (from `actor_status`): `detection_mode`, `radar_mode`, `device_info`,
  `shot_count`, `tracking_mode`
- **Integration actors**: `club`, `handed`, `name`, `error`

---

### GET /api/shots

Shot history (most recent N shots, FIFO, max 1000 stored). Shots the device
produced no data for (no ball, club or impact) are kept for the
[export](#get-apishotsexport) but not listed here.

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
    "actor": "mevo.0",
    "shot_number": 42,
    "ball": {
      "launch_speed": "67.2mps",
      "launch_azimuth": -1.3,
      "launch_elevation": 14.2,
      "carry_distance": "180.5m",
      "max_height": "28.3m",
      "flight_time": 6.4,
      "backspin_rpm": 3200,
      "sidespin_rpm": -450
    },
    "derived": {
      "carry_offline": "-19.9m",
      "descent_angle": 48.2,
      "roll_distance": "20.3m",
      "roll_offline": "-3.7m",
      "total_distance": "200.8m",
      "total_offline": "-23.6m"
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
  }
]
```

With `?units=imperial`:

```json
{
  "ball": {
    "launch_speed": "150.3mph",
    "carry_distance": "197.4yd",
    "max_height": "92.8ft"
  },
  "derived": {
    "carry_offline": "-21.8yd",
    "roll_distance": "22.2yd",
    "roll_offline": "-4.1yd",
    "total_distance": "219.6yd",
    "total_offline": "-25.8yd"
  },
  "club": {
    "club_speed": "94.2mph",
    "club_offset": "0.2in",
    "club_height": "0.47in"
  }
}
```

- `ball`: `BallFlight` or `null`. Launch conditions and distances, exactly as
  the device measured them. All fields are `Option`.
  Velocity fields are unit-tagged strings (`"67.2mps"`, `"150.3mph"`).
  Distance fields are unit-tagged strings (`"180.5m"`, `"197.4yd"`).
- `derived`: `DerivedFlight` or absent. Values simulated for fields the device
  didn't report (beta); see [ball_flight](#ball_flight) for the fields.
  Converted by `?units=` like `ball` (distances to yards/meters, `max_height`
  to feet/meters).
- `club`: `ClubData` or `null`. Club head data. All fields are `Option`.
- `impact`: `FaceImpact` or `null`. Face impact location.

---

### GET /api/shots/export

Download the session's shots for sharing or analysis: JSONL with the raw
device frames behind each shot, or CSV without them. Shots live in memory for
the session (the same 1000-shot history as `/api/shots`); nothing is written to
disk.

**Query params**:

| Param    | Values                  | Default                                                   | Notes |
| -------- | ----------------------- | --------------------------------------------------------- | ----- |
| `format` | `jsonl` \| `csv`        | `jsonl`                                                   | anything else → `400` |
| `units`  | `imperial` \| `metric`  | JSONL: native units (as `/api/shots`); CSV: `default_units` | converts `data` / the CSV columns; anything else → `400` |
| `limit`  | 1..=1000                | all stored                                                | most recent N; out of range → `400` |
| `raw`    | `true` \| `false`       | `true`                                                    | JSONL only; `false` leaves out `raw` for a small file |

**Response** `200 OK`, streamed one shot per line:

- `Content-Type: application/x-ndjson` (JSONL) or `text/csv; charset=utf-8`
- `Content-Disposition: attachment; filename="flighthook-shots-YYYYMMDD-HHMMSS.jsonl"` (or `.csv`)
- `Cache-Control: no-store`

With no shots stored, the body is empty (CSV: the header row only). Shots the
device produced no data for are included: their `data` has only `actor` and
`shot_number`, and `raw` shows what the device sent.

**JSONL**: one `ShotExport` object per line (pretty-printed here):

```json
{
  "schema": 1,
  "flighthook_version": "0.1.13",
  "exported_at": "2026-10-25T21:04:11.123Z",
  "time": "2026-10-25T20:58:03.412Z",
  "actor": "mevo.0",
  "shot_id": "550e8400-e29b-41d4-a716-446655440000",
  "shot_number": 7,
  "device": { "kind": "flightscope", "manufacturer": "FlightScope", "model": "Xi", "firmware": "1.23 (Feb 3 2024)" },
  "club_selected": "8I",
  "settings": {
    "device": { "ball_type": 1, "track_pct": 80.0, "environment": "limited_flight" },
    "simulation": { "enabled": true, "altitude": "0ft", "temperature_c": 25.0, "surface": "fairway" },
    "detection_mode": "full",
    "handed": "rh"
  },
  "data": {
    "actor": "mevo.0",
    "shot_number": 7,
    "ball": { "launch_speed": "37.9mps", "launch_azimuth": -3.1, "launch_elevation": 23.9, "carry_distance": "95.2m", "backspin_rpm": 6897, "sidespin_rpm": 0 },
    "club": { "club_speed": "31.2mps", "path": 1.1, "attack_angle": -9.8, "face_angle": 0.4 }
  },
  "raw": {
    "transport": "flightscope-tcp",
    "frames": [
      { "t_ms": 0, "dir": "rx", "type": "0xE5", "src": "AVR", "dst": "APP", "hex": "0c42414c4c2054524947474552000000" },
      { "t_ms": 612, "dir": "rx", "type": "0xE8", "src": "DSP", "dst": "APP", "hex": "5d0000040000..." },
      { "t_ms": 640, "dir": "rx", "type": "0xD4", "src": "DSP", "dst": "APP", "hex": "9c0000040000..." },
      { "t_ms": 703, "dir": "tx", "type": "0x69", "src": "APP", "dst": "AVR", "hex": "" },
      { "t_ms": 760, "dir": "tx", "type": "0xB0", "src": "APP", "dst": "AVR", "hex": "0101" }
    ]
  }
}
```

- `schema`: export format version (`1`). Bumped when a field changes meaning.
- `time`: when `shot_trigger` reached the web layer (host clock, RFC 3339 UTC).
- `device`: `kind` (`flightscope`, `square_golf`, `garmin_r10`, `openconnect`,
  `mock`) plus manufacturer, model and firmware as the device reported them.
  FlightScope: `model` is the product (`Mevo+`, `Mevo Gen2`, `Mevo` when the
  generation is unknown, or `Xi`) and `firmware` the device's version and
  build date, e.g. `"1.23 (Feb 3 2024)"`.
- `club_selected`, `settings.detection_mode`, `settings.handed`: game state
  when the shot was triggered (e.g. the club GSPro had selected); absent when
  unknown.
- `settings.device`: the device's config section with `name`, `address`,
  `bind` and unset fields removed. `settings.simulation`: the `[simulation]`
  section.
- `data`: exactly the `/api/shots` element for this shot (`ShotData`).
- `raw`: `ShotRaw` (see [shot_finished](#shot_finished)).

**CSV**: one header row, one row per shot, RFC 4180 quoting, empty cell for a
missing value. Units are fixed per column and named in the header (imperial:
mph, yd, ft for heights, in for club offset/height; metric: mps, m). Face
impact is always mm. `environment` is the FlightScope environment in effect
(configured value, else the default). `sim_*` columns are the `derived` block.

```
time,actor,device_kind,manufacturer,model,firmware,shot_number,shot_id,
club_selected,handed,detection_mode,environment,
ball_speed_mph,launch_elevation_deg,launch_azimuth_deg,backspin_rpm,sidespin_rpm,
carry_yd,total_yd,roll_yd,max_height_ft,flight_time_s,
sim_carry_yd,sim_carry_offline_yd,sim_max_height_ft,sim_flight_time_s,
sim_descent_angle_deg,sim_roll_yd,sim_roll_offline_yd,sim_total_yd,sim_total_offline_yd,
club_speed_mph,club_speed_post_mph,path_deg,attack_angle_deg,face_angle_deg,
dynamic_loft_deg,smash_factor,swing_plane_horizontal_deg,swing_plane_vertical_deg,
club_offset_in,club_height_in,impact_lateral_mm,impact_vertical_mm
```

(One line in the file; wrapped here.)

**Privacy**: the export never contains the FRP `device` ID (network name, BLE
name or address), actor display names, telemetry, or configured addresses.
Device frames that carry identity or network details are never captured (see
the per-device notes under [docs/devices](devices/)), and identity strings
are replaced with `<redacted>` in text payloads and device labels. Open
Connect payloads are kept verbatim apart from that; check the file before
posting it publicly.

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
  "actor": "mevo.0",
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

Angles (degrees) and spin (RPM) pass through unchanged. The `actor` field
is preserved as-is. A `derived` block, when present, is converted the same way
(distances and offlines to yards or meters, `max_height` to feet or meters;
`flight_time` and `descent_angle` unchanged).

**Errors**:

- `400 Bad Request`: `units` param missing or not `"imperial"`/`"metric"`

---

### POST /api/mode

Change the global detection mode. Emits `SetDetectionMode` on the bus;
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
  "simulation": {
    "enabled": true,
    "altitude": "5280ft",
    "temperature_c": 30.0,
    "surface": "fairway"
  },
  "webserver": {
    "0": {
      "name": "Web Server",
      "bind": "0.0.0.0:5880"
    }
  },
  "mevo": {
    "0": {
      "name": "Mevo WiFi",
      "address": "192.168.2.1:5100",
      "ball_type": 1,
      "tee_height": "1.5in",
      "range": "9ft",
      "surface_height": "0in",
      "track_pct": 80.0,
      "camera_mode": "standard",
      "environment": "limited_flight"
    }
  },
  "r10": {
    "0": {
      "name": "Garmin R10"
    }
  },
  "square": {
    "0": {
      "name": "Square Golf Omni",
      "club": "7i",
      "advanced_spin": true,
      "discard_non_putting_zero_spin": true,
      "dot_bottom_to_face_centre_mm": {
        "DR": 15.0,
        "7I": 18.5
      }
    }
  },
  "mock_monitor": {},
  "openconnect_server": {
    "0": {
      "name": "OpenConnect Server",
      "bind": "0.0.0.0:921"
    }
  },
  "gspro": {
    "0": {
      "name": "Local GSPro",
      "address": "127.0.0.1:921"
    }
  },
  "random_club": {}
}
```

- Keys are type-prefixed global IDs: `mevo.0`, `r10.0`, `square.0`, `mock_monitor.0`,
  `openconnect_server.0`, `gspro.0`, `random_club.0`, `webserver.0`
- `simulation` (beta) holds the settings for simulated values: `enabled`
  (default `true`), `altitude` (unit-tagged distance, default `"0ft"`, valid
  0 to 5000 m), `temperature_c` (default `25.0`, valid -30 to 50) and `surface`
  ([`SimSurface`](#simsurface), default `"fairway"`). It is **omitted while every
  setting is at its default**; an absent `simulation` means the defaults. Every
  key is optional when posting. An out-of-range altitude or temperature turns
  simulation off until it is fixed. Changes apply from the next shot without
  restarting any actor
- All launch monitor config fields are optional (omitted = use defaults)
- R10 sections have only `name` (BLE auto-discovery; no address or radar settings)
- `use_estimated` on Mevo sections controls whether estimated (E8) ball flights
  are emitted when no full (D4) result arrives (defaults to `false`). With it
  off, a shot the device only estimated (e.g. a flight too short to read in
  Outdoor mode, or a very short chip) is skipped
- `camera_mode` on Mevo sections is `standard` (default), `fusion`, or
  `raw_fusion`. The Fusion modes request the device's club-data processing and
  are omitted from the response when unset
- `environment` on Mevo sections is `limited_flight` (default; detection mode 9,
  for nets and simulator screens) or `outdoor` (mode 8, full flight). It sets
  the full-swing detection mode, and the only mode on an Xi. Omitted from the
  response when unset
- `dot_bottom_to_face_centre_mm` on Square sections overrides allsquare's built-in
  per-club face impact calibration (distance from the bottom edge of the
  sticker's dot down to face centre, in mm). Keyed the same way `club` is. Omitted when no
  overrides are set
- `openconnect_server` sections are **launch monitors**, not integrations: they
  listen (`bind`, default `0.0.0.0:921`) and accept inbound shots from any
  monitor that speaks GSPro Open Connect V1 as a client — Uneekor, Foresight,
  SkyTrak, MLM2PRO. This is the inverse of `gspro`, which dials GSPro. GSPro
  listens on 921 too, but moving GSPConnect to 922 (set
  `<OpenAPIUseAltPort>true</OpenAPIUseAltPort>` in
  `C:\GSPro\GSPC\GSPconnect.exe.config`) frees 921 so both can share one host

---

### POST /api/settings

Config update via event-sourced bus pattern. Emits a `ConfigCommand`
on the bus, waits for `ConfigOutcome` from SystemActor, then returns the response.

**Request**: complete `FlighthookConfig` JSON (same shape as GET response)

**Query parameters**:

- `scope` (optional): a global actor ID (`mevo.0`, `gspro.1`, `webserver.0`).
  Narrows the save to that one actor — only its section is written to the
  config, and only that actor is reconciled. Every other section of the live
  config is left as-is, whatever the request body carries for it. A `scope`
  naming a section the body omits removes that actor. An unparseable ID or
  unknown type prefix is rejected: nothing is saved and the response lists no
  changes. Without `scope`, the body replaces the whole config and every actor
  is reconciled.

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

- Emits `ConfigCommand` on the bus, processed by SystemActor. Without `scope`
  the action is `ReplaceAll`; with it, the matching `Upsert*` variant for that
  section (or `Remove` when the body omits it)
- SystemActor persists the new config to disk and reconciles actors:
  - Address/routing/settings changes trigger actor restart (shutdown + recreate)
  - Removed config sections stop the corresponding actor
  - New config sections start new actors
- SystemActor emits `ConfigOutcome` on the bus (used for request-reply + actor
  name cache refresh in the web layer)

---

## WebSocket

### Connection

**Endpoint**: `GET /frp` (HTTP upgrade to WebSocket)

Text-frame JSON messages in both directions.

### FRP Version Negotiation

The server performs FRP version negotiation on the `start` handshake. The client
sends an array of supported versions; the server selects the highest mutually
supported version. If no compatible version exists, the server sends a `critical`
alert and closes the connection.

Currently supported versions: `0.1.0`.

### Init Handshake (FRP-compliant)

Before streaming begins, the client must complete a handshake:

1. **Client sends** a `start` message:

```json
{
  "kind": "start",
  "version": ["0.1.0"],
  "name": "My Dashboard"
}
```

- `kind` (required): must be `"start"`
- `version` (required): array of supported FRP versions
- `name` (optional): human-readable client identifier for server-side logging.
  Defaults to `"anonymous"` if empty or omitted.

2. **Server responds** with an `init` message:

```json
{
  "kind": "init",
  "version": "0.1.0",
  "actor_id": "ws.a1b2c3d4",
  "global_state": {
    "player_info": null,
    "club_info": {
      "club": "DR"
    }
  }
}
```

- `kind`: `"init"` — FRP handshake response
- `version`: the FRP version selected by the server
- `actor_id`: unique identifier for this WebSocket session (`ws.{8-hex-chars}`)
- `global_state`: current snapshot of shared state
  - `player_info`: `{ "name": "Player 1" }` or `null`
  - `club_info`: `{ "club": "DR" }` or `null`

3. **Server streams** `FlighthookMessage` events (described below).

Messages sent before the `start` handshake (except `close`) are ignored.

---

### Server -> Client: FlighthookMessage

After the init handshake, the server streams `FlighthookMessage` events. All
messages use the FRP envelope shape:

```json
{
  "actor": "mevo.0",
  "device": "FS-M2-XXXXXX",
  "raw_payload": "0a1b2c...",
  "event": { "kind": "...", ... }
}
```

- `actor`: global ID of the originator (e.g. `"mevo.0"`, `"gspro.0"`, `"system"`).
  Flighthook extension field (FRP consumers ignore unknown fields per spec).
- `device`: FRP device identifier (e.g. Mevo WiFi SSID). Present on shot
  lifecycle and device telemetry events; absent on system/config events.
- `raw_payload`: optional. Binary payloads are lowercase hex strings (no spaces).
  Text payloads (e.g. GSPro JSON) are included as-is. Omitted when not applicable.
- `event`: the typed event, tagged by `"kind"`. FRP-only consumers silently
  ignore unknown `kind` values per spec.

---

#### Event Kinds

Events are tagged by the `"kind"` field inside the `"event"` object. All fields
are directly on the event object (flat struct variants).

---

##### shot_trigger

Ball strike detected. Emitted immediately by the launch monitor -- no data yet.

```json
{
  "actor": "mevo.0",
  "device": "FS-M2-XXXXXX",
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
  "actor": "mevo.0",
  "device": "FS-M2-XXXXXX",
  "event": {
    "kind": "ball_flight",
    "key": { "shot_id": "550e8400-...", "shot_number": 42 },
    "ball": {
      "launch_speed": "67.2mps",
      "launch_azimuth": -1.3,
      "launch_elevation": 14.2,
      "carry_distance": "180.5m",
      "max_height": "28.3m",
      "flight_time": 6.4,
      "backspin_rpm": 3200,
      "sidespin_rpm": -450
    },
    "derived": {
      "carry_offline": "-19.9m",
      "descent_angle": 48.2,
      "roll_distance": "20.3m",
      "roll_offline": "-3.7m",
      "total_distance": "200.8m",
      "total_offline": "-23.6m"
    }
  }
}
```

All `BallFlight` fields are `Option`. Missing fields are omitted from the JSON.

**`derived` (beta)** is a flighthook extension: values simulated from the
measured launch with the [dimple](https://crates.io/crates/dimple) ball-flight
model, for fields the device did not report. Every field under `ball` is
measured; every field under `derived` is simulated; a field is never in both.
FRP consumers ignore it as an unknown field. It is absent when simulation is
off, in putting mode, or when the launch can't be simulated — including a
shot with no spin reading (back and side both 0) — see the README's
"Simulated values" section for the rules. It is never sent to GSPro.

A launch-monitor-only device such as the Square Golf Omni gets the full set:

```json
"ball": {
  "launch_speed": "53.6mps",
  "launch_azimuth": -1.2,
  "launch_elevation": 17.0,
  "backspin_rpm": 6500,
  "sidespin_rpm": -300
},
"derived": {
  "carry_distance": "155.4m",
  "carry_offline": "-6.5m",
  "max_height": "29.3m",
  "flight_time": 5.9,
  "descent_angle": 50.1,
  "roll_distance": "8.7m",
  "roll_offline": "-0.8m",
  "total_distance": "164.1m",
  "total_offline": "-7.3m"
}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `carry_distance` | Distance | Tee to landing, horizontal. Only when the device didn't report carry |
| `carry_offline` | Distance | Landing point right (+) or left (−) of the target line |
| `max_height` | Distance | Apex above the tee. Only when the device didn't report it |
| `flight_time` | number (s) | Launch to landing. Only when the device didn't report it |
| `descent_angle` | number (deg) | Angle below horizontal at landing |
| `roll_distance` | Distance | Landing to rest. Negative when the ball spins back. Only when the device didn't report roll |
| `roll_offline` | Distance | Sideways movement from landing to rest, + right |
| `total_distance` | Distance | Tee to rest. Only when the device didn't report total |
| `total_offline` | Distance | Rest position right (+) or left (−) of the target line |

Every field is optional. Offline values are absolute: + is right of the target
line for every golfer, as in `launch_azimuth`. Where the device measures carry
(or roll), `total_distance` is that measured carry plus the simulated roll, so
carry + roll = total holds for the values a consumer displays. Roll, roll
offline, total and total offline are omitted if the ground simulation does
not reach rest.

---

##### club_path

Club path data available. May arrive before or after `ball_flight`.

```json
{
  "actor": "mevo.0",
  "device": "FS-M2-XXXXXX",
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

##### face_impact

Face impact location, in millimetres from the centre of the club face.
Positive `lateral` is toward the toe, positive `vertical` is above centre.
Neither axis is flipped for a left-handed player — toe/heel and high/low are
already golfer-relative.

Emitted only by devices whose reading is referenced to the face centre. The
Square Golf Omni emits this event (beta): horizontal as the device measures
it, vertical with a per-club calibration (built-in defaults, overridable per
club in config or the Settings UI — see
[docs/devices/square-golf.md](devices/square-golf.md)). The putter has no
vertical calibration by default, so only horizontal impact is sent for putts.

```json
{
  "actor": "openconnect_server.0",
  "device": "Uneekor EYE XO",
  "event": {
    "kind": "face_impact",
    "key": { "shot_id": "550e8400-...", "shot_number": 42 },
    "impact": {
      "lateral": "-6.5mm",
      "vertical": "3.0mm"
    }
  }
}
```

---

##### shot_finished

Shot sequence complete. Accumulators should finalize and emit the composed shot.

```json
{
  "actor": "mevo.0",
  "device": "FS-M2-XXXXXX",
  "event": {
    "kind": "shot_finished",
    "key": { "shot_id": "550e8400-...", "shot_number": 42 },
    "raw": {
      "transport": "flightscope-tcp",
      "frames": [
        { "t_ms": 0, "dir": "rx", "type": "0xE5", "src": "AVR", "dst": "APP", "hex": "0c42414c4c..." }
      ]
    }
  }
}
```

- `raw` (flighthook extension, omitted when absent): `ShotRaw`, the device
  frames captured for this shot. FRP consumers ignore it.
  - `transport`: `flightscope-tcp`, `square-ble`, `r10-ble`,
    `openconnect-json`, or `none` (mock).
  - `frames`: `RawRecord[]`, in arrival order:
    - `t_ms`: milliseconds relative to the shot trigger (negative = before it)
    - `dir`: `rx` (device → flighthook) or `tx` (flighthook → device)
    - `type`: message type, e.g. `"0xD4"` (FlightScope) or the first byte of
      a BLE notification or command type; absent for R10 chunks and JSON
    - `src` / `dst`: FlightScope bus addresses (`APP`, `AVR`, `DSP`, `PI`)
    - `channel`: `notify`, `cmd`, `write`, `register` (BLE) or `tcp`
    - `hex`: payload as lowercase hex (binary protocols; FlightScope: the
      unstuffed payload without header or checksum)
    - `text`: payload text (JSON protocols)
  - `dropped`: frames over the per-shot cap that were not kept (omitted when 0)
  - `unavailable`: why `frames` is empty or incomplete, e.g. `"mock device
    has no wire protocol"`, `"evicted: session raw budget"`
- `shot_finished` can be large: up to a few hundred KiB for a FlightScope
  Fusion shot with its PRC data. Consumers that only need timing can use
  `shot_trigger`.

---

##### device_telemetry

Device-reported state. Emitted any time a device-reported value changes: after
handshake (with identity fields), on readiness transitions, and on periodic
telemetry updates (battery, tilt, temperature).

`ready` is the single readiness signal — all device conditions met for a shot
(including ball detection where applicable). There is no separate
`ball_detected` key.

**Ownership split**: `device_telemetry` carries what the device reports about
itself. Actor/connection lifecycle (status enum, mode, shooting) is conveyed
separately via `actor_status`. The two event types have no overlapping
telemetry keys.

```json
{
  "actor": "mevo.0",
  "device": "FS-M2-XXXXXX",
  "event": {
    "kind": "device_telemetry",
    "manufacturer": "FlightScope",
    "model": "XXXXXXXX, H/W: XXXX v1.0, F/W: 1.00",
    "telemetry": {
      "ready": "true"
    }
  }
}
```

- `manufacturer`, `model`, `firmware`: optional identity fields (present on first emission)
- `telemetry`: optional key/value map. Standard keys: `"ready"`, `"battery_pct"`,
  `"tilt"`, `"roll"`, `"temp_c"`, `"external_power"`

---

##### player_info

Player info update (name). Flighthook extension event.

```json
{
  "actor": "gspro.0",
  "event": {
    "kind": "player_info",
    "player_info": { "name": "Player 1" }
  }
}
```

- `player_info.name`: optional player name string

---

##### club_info

Club selection update.

```json
{
  "actor": "gspro.0",
  "event": {
    "kind": "club_info",
    "club_info": { "club": "7I" }
  }
}
```

---

##### set_detection_mode

FRP controller command. Sets the shot detection mode and/or player handedness.
Both fields are optional and latched independently — the most recent value for
each field is the active value. Omitting a field does not reset it.

```json
{
  "actor": "system",
  "event": {
    "kind": "set_detection_mode",
    "mode": "chipping",
    "handed": "rh"
  }
}
```

- `mode` (optional): `"full"` | `"putting"` | `"chipping"`
- `handed` (optional): `"rh"` | `"lh"`

---

##### actor_status

Actor/connection lifecycle update. Emitted by all actors (launch monitors
and integrations) when their connection status or actor-framework state changes.

**Ownership split**: `actor_status` carries what flighthook knows about the
actor (connection lifecycle, operational mode). Device-reported state (ready,
battery, tilt, temp) is conveyed separately via `device_telemetry`. The two
event types have no overlapping telemetry keys.

```json
{
  "actor": "mevo.0",
  "event": {
    "kind": "actor_status",
    "status": "connected",
    "telemetry": {
      "detection_mode": "full",
      "radar_mode": "armed",
      "device_info": "XXXXXXXX (Gen2)"
    }
  }
}
```

- `status`: `"starting"` | `"disconnected"` | `"connected"` | `"reconnecting"`
- `telemetry`: actor-framework key/value pairs (all string values)

Common keys for launch monitors: `detection_mode`, `radar_mode`, `device_info`.
Mock launch monitors add: `shot_count`, `tracking_mode`.
Integration actors: `club`, `handed`, `name`, `error`.

---

##### config_command

Config mutation request (emitted by POST handler, processed by SystemActor).
External consumers can observe these to track config changes in flight.

```json
{
  "actor": "web",
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
  "actor": "system",
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
  "actor": "mevo.0",
  "event": {
    "kind": "alert",
    "severity": "warn",
    "message": "Could not process message: Wire(ChecksumMismatch { ... })"
  }
}
```

- `severity`: `"warn"` | `"error"` | `"critical"`
- `message`: human-readable description of the condition

---

### Client -> Server

JSON text frames with a `"cmd"` field. These are processed independently of
the init handshake -- they emit events on the unified bus.

#### mode

Change the global detection mode. Emits `SetDetectionMode` on the bus.

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

### Handedness

`"rh"` | `"lh"`

### Severity

`"warn"` | `"error"` | `"critical"`

### UnitSystem

`"imperial"` | `"metric"`

### SimSurface

`"fairway"` | `"green"` | `"firm_green"` -- flat ground for simulated landing
and roll (beta).

### Velocity (unit-tagged string)

Format: `{number}{suffix}`. Suffixes: `mph` (miles/hour), `mps` (meters/second),
`kph` (km/hour), `fps` (feet/second).

### Distance (unit-tagged string)

Format: `{number}{suffix}`. Suffixes: `yd` (yards), `ft` (feet), `in` (inches),
`m` (meters), `cm` (centimeters).
