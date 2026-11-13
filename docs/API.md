# Flighthook Web API

flighthook serves four things on its web server port (default `5880`):

- **`/frp`**: the [Flight Relay Protocol](https://github.com/flightrelay/spec) (FRP) 0.3.0 over WebSocket. Any program
  can listen for shots, send shots or take part in the session as an FRP actor. See [WebSocket: `/frp`](#websocket-frp).
- **`/api/ws`**: the dashboard's own socket (private to the dashboard; see [`/api/ws`](#websocket-apiws)).
- **REST endpoints** under `/api`, for status, shot history, the export, settings and commands.
- The dashboard itself.

## Quick-Start: Real-Time Shot Stream

The fastest way to get shot data is the WebSocket at `/frp`. There is no handshake: connect and the messages start
arriving. flighthook first sends a greeting (the `bus.actor` and state of each actor it hosts, then a `bus.sync`),
then every message on its bus as it happens, with no polling. This is ideal for video timestamp tagging, overlay
triggers, stat trackers, or any integration that reacts to shots in real time.

Every message is one JSON object in one text frame, with `kind` (the message kind) and `from` (the sending actor's
id). A shot is a sequence of messages that share a `key` (`shot_id` and `shot_number`):

1. `shot.trigger`: ball strike detected (no data yet)
2. `shot.launch`: the measured launch (speed, angles, spin)
3. `shot.flight`: a flight result (carry, apex, flight time and so on) with the `basis` it was obtained on. The launch
   monitor sends one when it has flight values; flighthook's flight model sends its own, for the values the monitor
   did not report (see [Simulated values](ARCHITECTURE.md#simulated-values))
4. `shot.club`: club data (speed, path, attack angle, face angle, loft)
5. `shot.impact`: face impact location
6. `shot.finished`: shot complete, accumulators should finalize

The messages between a shot's trigger and its finish may arrive in any order. flighthook's flight model sends its
`shot.flight` under its own `from`, after the launch monitor's `shot.finished`, and any other actor's `shot.flight` can
arrive after it too. The `ShotAggregator` in the
[FRP Rust SDK](https://github.com/flightrelay/sdk-rust) assembles them; other languages collect by `key`. All
`launch`, `flight` and `club` fields are optional, except a flight's `basis`.

Velocities and distances are unit-tagged strings (e.g. `"67.2mps"`, `"180.5m"`). To convert a shot to a standard unit
system without parsing suffixes yourself, POST a `ShotData` (the `GET /api/shots` shape; it carries a `shot_number`) to
`/api/shots/convert?units=imperial` (or `metric`), or read `GET /api/shots?units=imperial`, which also serves
historical data. Angles are degrees, spin is RPM.

Other messages on the same connection:

- `bus.actor`: who an actor is, its roles, blockers (`blockers` empty: ready), device info (battery, tilt) and
  telemetry; also its heartbeat
- `golfer.turn`: the golfer up, club, hand and mode
- `golfer.profile`: a golfer known to an actor (flighthook's saved golfers, a game's players)
- `practice.block`: the open [sets](#get-apiblocks)
- `game.round`, `game.scores`, `game.ball`, `game.stroke`: a game's round (the Pureplay Accessory card, beta)
- `bus.reply`, `bus.alert`: answers to commands, and warnings

The kinds and fields are defined by the [spec](https://github.com/flightrelay/spec); [`/frp`](#websocket-frp) below
says what flighthook does with them.

### Terminal

```bash
# Stream shot messages with websocat + jq (install: cargo install websocat)
websocat ws://localhost:5880/frp | jq 'select(.kind == "shot.finished")'
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
        shots = {}  # (from, shot_id) -> {launch, flights, club}
        async for raw in ws:
            msg = json.loads(raw)
            kind = msg["kind"]
            if "key" not in msg:
                continue
            key = (msg["from"], msg["key"]["shot_id"])
            if kind == "shot.trigger":
                shots[key] = {"flights": []}
            elif key not in shots:
                continue
            elif kind == "shot.launch":
                shots[key]["launch"] = msg["launch"]
            elif kind == "shot.flight":
                shots[key]["flights"].append(msg["flight"])
            elif kind == "shot.club":
                shots[key]["club"] = msg["club"]
            elif kind == "shot.finished":
                shot = shots.pop(key)
                carry = next((f.get("carry_distance") for f in shot["flights"]), None)
                print(f"Shot #{msg['key']['shot_number']}: "
                      f"speed={shot.get('launch', {}).get('launch_speed')} carry={carry}")

asyncio.run(main())
```

### JavaScript

```javascript
const ws = new WebSocket("ws://localhost:5880/frp");
const shots = new Map();

ws.onmessage = (e) => {
  const msg = JSON.parse(e.data);
  if (!msg.key) return;
  const id = `${msg.from}:${msg.key.shot_id}`;
  switch (msg.kind) {
    case "shot.trigger":
      shots.set(id, { flights: [] });
      break;
    case "shot.launch":
      if (shots.has(id)) shots.get(id).launch = msg.launch;
      break;
    case "shot.flight":
      if (shots.has(id)) shots.get(id).flights.push(msg.flight);
      break;
    case "shot.club":
      if (shots.has(id)) shots.get(id).club = msg.club;
      break;
    case "shot.finished": {
      const shot = shots.get(id);
      shots.delete(id);
      if (shot) console.log(`Shot #${msg.key.shot_number}`, shot.launch, shot.flights);
      break;
    }
  }
};
```

These examples collect the launch monitor's own messages only. To include the flight model's flight, key the shots by
`key.shot_id` alone, keep a shot after its `shot.finished`, and read the flight when its `shot.flight` arrives, or read
`GET /api/shots`.

---

## REST Endpoints

The API has no authentication; it is meant for a trusted LAN. Any client that
reaches the port can change settings or clear the shot history, download
swing clips, camera stills and live camera frames, and the bodiless `POST`s
(Clear history, a clip's body tracking, the body tracking model download) are
CORS simple requests, so a web page open in a browser on the LAN can send them
too. The same goes for the `/frp` and `/api/ws` sockets.

### GET /api/status

Comprehensive system state: each local card's status, every FRP actor on the bus, and the golfer turn.

**Response** `200 OK`:

```json
{
  "cards": {
    "mevo.0": {
      "name": "Mevo WiFi",
      "status": "connected",
      "telemetry": {
        "ready": "true",
        "battery_pct": "85",
        "tilt": "0.5",
        "roll": "-0.2",
        "phase": "measuring",
        "shot_count": "12",
        "device_info": "FlightScope Mevo+"
      },
      "state": "connected",
      "red_for_ms": 0
    },
    "openconnect_client.0": {
      "name": "GSPro",
      "status": "connected",
      "telemetry": { "club": "DR", "handed": "rh" },
      "state": "connected",
      "red_for_ms": 0
    }
  },
  "actors": {
    "5b80e2f7d14c9a63": {
      "name": "Mevo WiFi",
      "roles": ["launch_monitor"],
      "blockers": [],
      "device": { "manufacturer": "FlightScope", "model": "Mevo+", "source": "measured" },
      "card": "mevo.0"
    },
    "c19e04b7a2d5f318": {
      "name": "flighthook",
      "roles": ["session", "practice", "collector"],
      "blockers": []
    }
  },
  "turn": { "golfer": "flighthook.0f8c2d1a-77b4-4e1e-9a50-3c6d2b8e4f10", "club": "7I", "handedness": "rh", "mode": "full" },
  "cameras_supported": true,
  "clip_dir": "C:\\Users\\me\\AppData\\Local\\flighthook\\clips",
  "config_error": {
    "path": "C:\\Users\\me\\AppData\\Roaming\\flighthook\\config.toml",
    "error": "line 4, column 8: unknown variant `liv`, expected one of `off`, `historical`, `live`"
  },
  "round": { "active": true, "holder": "Pureplay", "held": true }
}
```

- `cards`: each local card's status, by card key (`mevo.0`, `openconnect_client.0`, ...; see below).
- `actors`: every FRP actor on the bus, local and remote, by actor id. `name`, `roles` and `blockers` are from its
  latest `bus.actor` (`blockers` empty means ready); `device` is the `bus.actor` device object (manufacturer, model,
  firmware, battery, attitude, `source`); `card` names the local card key when the actor is a flighthook card. Other
  FRP programs connected to `/frp` appear here with no `card`.
- `turn`: the current `golfer.turn` (flighthook's own): `golfer` (a golfer id or `null`), `club` (a
  [club code](#club) or `null`), `handedness` and `mode`. Omitted before the session has published one.
- `cameras_supported`: `true` when this server can record from cameras
  (Windows and Linux); omitted where it can't (macOS). The
  [test picture](#get-apicameras) works everywhere
- `trackball_supported`: `true` when this server can read a trackball (Linux),
  so the dashboard offers the Trackball card (alpha); omitted where it
  can't. A `trackball` section on another platform shows the error on its
  card
- `clip_dir`: the folder [camera clips](#get-apishotsshot_idvideo) are
  written to: `[replay] clip_dir`, or `flighthook/clips` in the user's local
  data folder (`%LOCALAPPDATA%` on Windows, `~/.local/share` on Linux)
- `config_error`: present only while the config file at startup could not be
  used (it failed to parse, or could not be read) and the app runs on the
  defaults. `path` is the file; `error` is the reason, for a parse error the
  line and column, then the message. The dashboard shows it as an error frame
  over every tab. The file is left as it is until the first settings save:
  that save copies it to `config.pre-<version>.toml` (see
  [Loading and migration](ARCHITECTURE.md#loading-and-migration)) and then
  replaces it, after which the field is gone. A save is refused with `500`
  and `{"error": "..."}` when the copy can't be made; the file and the field
  stay. The dashboard reads it with the rest of the status at startup and
  again after every settings change.
- `round`: present only while a tournament round is active. A round is active while a simulator holds one (`held`:
  an output such as Pureplay in strict mode, or the Pureplay Accessory's match), while tournament mode is switched on
  (`manual`, see [`POST /api/tournament`](#post-apitournament)), or both. `holder` is the display name of the first
  simulator holding it, else `"Manual"`. `manual` and `held` are omitted when false. The same object arrives on
  [`/api/ws`](#websocket-apiws) as `round_state`.
- Each entry of `cards` has: `name`, the card's configured name, as the config holds it at the request (a rename
  shows here at once; it restarts nothing; an Open Connect server with a blank name reads `Open Connect`, and Open
  Connect servers that share a name are numbered in index order, `Open Connect 1`, `Open Connect 2`); `status`:
  `"starting"` | `"disconnected"` | `"connected"` | `"reconnecting"`; `telemetry` (below); `state` and
  `red_for_ms`:
  - `state` is the [connection state](#connectionstate) the dashboard shows, derived by the server from `status` and
    the card's history since it last started:
    - `"connecting"`: not `connected` since it started, or since it was restarted by a config change;
    - `"connected"`: `status` is `connected`;
    - `"reconnecting"`: `connected` since it started, and has since lost the connection (any other status);
    - `"disabled"`: the card is turned off (its config section has `enabled = false`, see
      [Turning a card off](#turning-a-card-off)). No actor runs: `status` is `disconnected`, `telemetry` is empty and
      `red_for_ms` is `0`. Not an error state.
  - `red_for_ms`: milliseconds the `reconnecting` state stays an alert (red), counted from when the reply was built:
    what is left of the 10 minutes (600000 ms) after the most recent change from `connected` to any other status,
    else `0`. Another loss while already `reconnecting` does not start the 10 minutes again; a new `connected` then
    a loss does. A client counts it down from the moment it receives the reply, so its clock need not match the
    server's.

The server keeps each card's history for as long as it runs, whoever is connected: a dashboard opened five minutes
after a loss still gets `reconnecting` with about five minutes of `red_for_ms`. A `starting` status, a restart from a
config change, removing the card (then adding it again) and turning it off then on again each begin a new history. A
client of [`/api/ws`](#websocket-apiws) applies the same rules to the `card_status` events to keep `state` and the
red time current.

A card's `telemetry` holds its rows as key/value strings. A key is
reported by one source only:
- **Device-reported** (reported by the device): `ready`, `battery_pct`, `tilt`,
  `roll`, `temp_c`, `external_power` (a Mevo+), `zero_spin_discards` (a Square Golf), and a device driver's own state keys
  (the mock's `radar_mode`, `tracking_mode`; the Square Golf's
  `device_state`; the Trackball's `synthetic`
  (`true`), `input` (the name the device reports), `cpi`, `profile` (the
  club in play) and `handed` (`rh` | `lh`))
- **Every card**: `device_info`, `shot_count`,
  `phase`; `error` on a card on a device driver whose connects keep failing:
  the latest connect error, removed when a session connects
- **Software cards**: `club`, `handed`, `name`, `error`
- **Open Connect clients** (GSPro, Pureplay, Muni Golf, generic):
  `address` (the simulator's, `127.0.0.1:921`), `last_shot` (the
  `ShotNumber` the simulator got last and the local time it was sent,
  `#12 at 16:41:07`), and for the current connection only `shots_sent`
  (`3`), `monitor_ready` (the readiness last sent to the simulator, `true` |
  `false`) and `response` (the simulator's last response code and message,
  `201 GSPro Player Information`). `club` and `handed` are the simulator's
  player information. When the connection ends, `shots_sent`,
  `monitor_ready`, `response`, `club` and `handed` are removed; `address`
  and `last_shot` stay
- **Pureplay Connect outputs** (alpha): `address` (Pureplay's,
  `127.0.0.1:9760`), `last_shot` (the ShotId Pureplay got last and the local
  time it was sent, `#4 at 16:41:07`), and for the current link only
  `shots_sent` (`4`), `ready_state` (the ready state last sent, `ready` |
  `not ready` | `processing`) and `strict_mode` (`on` | `off`; `on` while a
  tournament round is in progress, when Auto-chip does not fire). `club` and
  `handed` are the club and hand Pureplay set. `error` says why the handshake
  failed (no reply to Hello, an unsupported protocol version); a refused or
  closed connection sets none. When the link ends, `shots_sent`,
  `ready_state`, `strict_mode`, `club` and `handed` are removed
- **Pureplay Accessory cards** (beta): while connected, `name`,
  `club` and `handed` (the active local player: the player who is up when
  that player sits at the Pureplay computer), `hole` (`7, par 4`),
  `blockers` (what keeps Pureplay's shot from being played, while it can't
  be: `shot in progress`), `club_out` (whether a club picked in flighthook
  goes to Pureplay: `on` when the pairing grants the `read` and `control`
  scopes (the player chose **Aim and Club** at Pureplay's prompt); `read
  only` when it grants `read` alone (**Read Only**) or no scopes at all;
  `off in Pureplay` when Pureplay's "Let Apps Change Aim and Club" is off;
  `not supported` when Pureplay offers no club command, or the player who
  is up has no bag), `shots` (the Shots setting: `auto`, `auto: last
  from Pureplay`, `auto: last from flighthook`, `from Pureplay` or `from
  flighthook`), `missed_events` (gaps in Pureplay's event numbers, only
  above 0; each one asks Pureplay for its state again) and `pureplay` (the
  game's version); each only while it has a value. `last_shot` (the shot
  the card took last and the local time, `#12 at 16:41:07`) stays after
  the connection. Pureplay's readiness is the card's
  `ready` (`true` | `false`), as
  for any launch monitor, with manufacturer `Pureplay`, model `Accessory
  API` and firmware the game's version. Not connected,
  `pairing` says where pairing stands: `Waiting for Pureplay`, `Approve
  flighthook in Pureplay (42 s)` (counting down), `Another app on this
  computer is waiting for approval in Pureplay`, `Pureplay has too many
  apps connected`, `Turn on Ask about new apps in Pureplay's Settings >
  Accessories`, `Pureplay turned the request down`, `The request timed out
  in Pureplay`, `Pureplay asks to wait (1:42)` (counting down), `Removed in
  Pureplay. Press Reconnect to pair again.` or `Press Reconnect to pair
  again.`, or, after six `bad_request` answers in a row, Pureplay's own
  message. `error` says Pureplay speaks another protocol or version, offers
  none of the events the card follows, or that the pairing could not be
  saved. See [Pureplay Accessory](devices/pureplay.md)
- **Cameras**: `camera` (the camera's own name),
  `position`, `resolution` (`1280x720`), `mode_fps` (the frame rate asked
  for), `fps` (achieved over the last second), `dropped` (frames missing
  since it connected), `error`. `fps`, `dropped` and `position` are sent at
  most every 2 s, and only when they change

---
---

### GET /api/shots

Shot history (most recent N shots, FIFO, max 1000 stored). Shots the device
produced no data for (no launch, flight, club or impact) are kept for the
[export](#get-apishotsexport) but not listed here.

**Query params**:

- `limit` (optional, default `50`): max shots to return (the dashboard asks
  for `1000`, the whole history). `0` returns `[]`; a value above the number
  stored returns them all; a non-numeric value is `400 Bad Request`
- `units` (optional): `"imperial"` or `"metric"`. Converts all distance and
  velocity fields to the specified unit system. Imperial: yards/feet/inches/mph.
  Metric: meters/m/s. Default (omitted): returns values in native units (as
  stored by the launch monitor accumulator, typically metric).

**Response** `200 OK` -- `ShotData[]`, oldest first:

```json
[
  {
    "shot_number": 42,
    "shot_id": "550e8400-e29b-41d4-a716-446655440000",
    "from": "5b80e2f7d14c9a63",
    "card": "mevo.0",
    "source": "measured",
    "block": "6f1c2a3b-4d5e-4f60-8a71-92b3c4d5e6f7",
    "turn": { "golfer": "flighthook.0f8c2d1a-77b4-4e1e-9a50-3c6d2b8e4f10", "club": "7I", "handedness": "rh", "mode": "full" },
    "club_selected": "7I",
    "launch": {
      "launch_speed": "67.2mps",
      "launch_azimuth": -1.3,
      "launch_elevation": 14.2,
      "backspin_rpm": 3200,
      "sidespin_rpm": -450
    },
    "flights": [
      {
        "from": "5b80e2f7d14c9a63",
        "flight": { "basis": "modeled", "carry_distance": "180.5m", "carry_offline": "-7.6m",
                    "max_height": "28.3m", "flight_time": 6.4, "descent_angle": 48.2 }
      },
      {
        "from": "e4a7136b9f02cd58",
        "flight": { "basis": "modeled", "model": "flighthook", "roll_distance": "11.2m",
                    "total_distance": "191.7m", "total_offline": "-8.5m" }
      }
    ],
    "path": {
      "flight": [[0, 0, 0, 0], [0.464, 25.37, 6.46, -0.63], ...],
      "ground": [[6.4, 180.34, 0, -7.6], ...]
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
    "metrics": {
      "total_spin_rpm": 3231,
      "spin_axis": -8.0,
      "face_to_path": 3.3,
      "spin_loft": 21.9,
      "curve": "-3.5m"
    }
  }
]
```

With `?units=imperial`, the velocities become `"150.3mph"`, the distances `"197.4yd"`, heights `"92.8ft"` and club
offset and height inches; angles, spin and `path` (always meters and seconds) are unchanged.

- `shot_number`, `shot_id`: the shot's `key` from its lifecycle messages.
- `launch`: `Launch` or absent. The launch (speed, angles, spin), exactly as the launch monitor measured it. All
  fields are optional. Velocity fields are unit-tagged strings (`"67.2mps"`, `"150.3mph"`).
- `flights`: every `shot.flight` of the shot with its sender (`from`, an actor id), in arrival order. The launch
  monitor's own flight (`from` equal to the shot's `from`) is the device's; a flight from another actor (flighthook's
  flight model, or any FRP program) is simulated or modeled. A flight carries `basis` (`tracked` or
  `modeled`), an optional `model` name, the result values (`carry_distance`, `carry_offline`, `total_distance`,
  `total_offline`, `roll_distance`, `max_height`, `flight_time`, `descent_angle`) and, when the sender assumed launch
  values the monitor did not measure, `assumed` (the same fields as `launch`, for example the backspin a launch-only
  chip was given). Absent or empty when no actor sent one. Distance fields are unit-tagged strings (`"180.5m"`,
  `"197.4yd"`). A flight from the flight model can arrive after the shot's `shot.finished`; the stored shot gets it when it does
  (`/api/ws` sends the shot again).
- `path`: `{flight, ground}`, the flight model's ball path as `[t, x, y, z]` points in seconds and meters
  (origin at the tee, `x` downrange, `y` up, `z` right of the target line). Absent when the flight model sent none.
  Never converted by `?units=`.
- `club`: `ClubData` or absent. Club head data. All fields are optional.
- `impact`: `FaceImpact` or absent. Face impact location.
- `metrics`: `ShotMetrics` or absent. Values computed from the measured `launch`, the device's flight and `club`
  when the shot finished (see [Shot metrics](#shot-metrics)). `curve` is converted by `?units=` like a flight.
- `block`: id of the practice block (set) the shot belongs to (see [`GET /api/blocks`](#get-apiblocks)); absent for
  a shot in no block.
- `turn`: the `golfer.turn` when the shot was triggered: `golfer` (a golfer id or `null`), `club`, `handedness` and
  `mode`. A shot records a hand: `rh` until one is chosen. Face impact is the same for either hand; a face-on
  drawing of it puts a right-hander's toe on the left and a left-hander's on the right.
- `club_selected`: the club in play at the trigger: the putter for a launch monitor that arms only for putts
  (ExPutt), else the turn's club; absent when none was selected.
- `from`: the launch monitor's actor id. `card`: its local card key (`mevo.0`) when the monitor is a flighthook
  card; absent for a remote launch monitor. `from_name` (beta): a remote launch monitor's name from its `bus.actor` when
  the shot was stored, so the shot stays named after the program leaves; absent for a card. Never in the export.
- `golfer_name` (beta): the name a game's `golfer.profile` gave the golfer up (`turn.golfer`) when the shot was stored, so
  the shot stays named after the round is over; absent for a flighthook golfer or when no profile was held. Never
  in the export.
- `source`: where the shot comes from, from the launch monitor's `bus.actor` `device.source` (see
  [Source](#source)): `"measured"` (a physical launch monitor measured it: one flighthook reads, one whose app sends
  its shots to the Open Connect input, or a remote launch monitor that declares it) or `"virtual"` (the Trackball, the
  mock launch monitor, Auto-chip, a Pureplay Accessory card). Absent on a shot stored without it; a tournament round
  counts such a shot as not measured.
- `stroke` (beta): a game's view of the stroke, the `game.stroke` joined to the shot by `shot_id`; it arrives
  seconds after the shot, from a Pureplay Accessory card or any FRP game. The fields are the spec's: `stroke_id`,
  `golfer`, `hole`, `stroke_number`, `outcome` (`in_play`, `green`, `holed`, `out_of_bounds`, `water`, `lost`,
  `unplayable`, `penalty_only`, `gimme` or `auto_putt`), `putt`, `penalty_strokes`, `flown` (the launch the game
  flew), `flight` (the game's flight, `basis` `modeled`), `lie`, positions and distances to the hole. Distances
  follow `units`:

  ```json
  "stroke": { "stroke_id": "r-20261110-1/7/2", "golfer": "pureplay.thill", "hole": 7, "stroke_number": 2,
    "outcome": "green", "putt": false, "penalty_strokes": 0,
    "flight": { "basis": "modeled", "model": "Pureplay", "carry_distance": "138.4m", "total_distance": "145.1m", "max_height": "24.6m" },
    "lie": { "surface": "green" }, "start_distance_to_hole": "152m", "end_distance_to_hole": "7.4m" }
  ```

- `video` (beta): the cameras' clips of the shot, one per camera, omitted
  when there are none. Each is a `ShotVideo`, with `camera`, the camera's actor ID:

  ```json
  "video": [
    { "camera": "camera.0", "position": "golfer_front", "duration_ms": 5500, "fps": 119.6,
      "trigger_offset_ms": 4000, "impact_offset_ms": 4000, "complete": true, "dropped": 0,
      "pose": true, "swing": true, "rotation": 90, "analyzed_with": "0.0.4" }
  ]
  ```

  `position` is the camera's [`CameraPosition`](#cameraposition); `trigger_offset_ms` and `impact_offset_ms` place
  the trigger and the estimated impact in the clip; `complete` is false for a clip that was cut short and `dropped`
  counts frames the camera missed. `pose` (body tracking, alpha; omitted while false) says the golfer's
  keypoints on every frame of the clip are ready at
  [`{camera}.pose.json`](#get-apishotsshot_idvideocameraposejson). In Historical mode a clip is tracked
  only once asked for
  ([`POST .../analyze`](#post-apishotsshot_idvideocameraanalyze)). `swing` (alpha; omitted while false)
  says the clip's swing analysis is ready at
  [`{camera}.swing.json`](#get-apishotsshot_idvideocameraswingjson); it stays false for a camera at
  `golfer_back` or `overhead`. `rotation` (omitted at 0) is the camera's
  rotation, clockwise degrees, the clip was tracked with: its keypoints are
  in the picture turned by it. `analyzed_with` (omitted until tracked) is
  the version of the body tracking library that tracked it.

  A clip that two launch monitors' triggers share (one swing) is on both
  shots' rows. It is added when the camera finishes the clip, which can be
  before or after the shot finishes, and removed when the clip is deleted.
  Clips are files on disk and
  outlive a restart; the shot history doesn't, so after a restart a clip is
  reachable by its shot ID only. Never in the [export](#get-apishotsexport).

`POST /api/shots/convert` passes `shot_id`, `block`, `turn` and `club_selected` through unchanged, and drops `video`.

#### Shot metrics

`metrics` holds values flighthook computes from the shot's measured `launch`,
the device's flight and `club` when the shot finishes, so consumers don't
each compute them. The
formulas and signs are the Flight Relay Protocol's spin relations and
"Derived Values":

| Field | Type | Formula | Present when |
| --- | --- | --- | --- |
| `total_spin_rpm` | integer | `√(backspin_rpm² + sidespin_rpm²)`, rounded | the device sent both components and neither `total_spin_rpm` nor `spin_axis` |
| `spin_axis` | number (deg) | `atan2(sidespin_rpm, backspin_rpm)`; + = tilted right, curves the ball right | as `total_spin_rpm`, and the total is not 0 |
| `face_to_path` | number (deg) | `face_angle − path`; + = face right of the club path | the device sent both |
| `spin_loft` | number (deg) | `dynamic_loft − attack_angle`, an approximation of the measured 3D spin loft | the device sent both and no `spin_loft` of its own |
| `curve` | Distance | `carry_offline − carry_distance × tan(launch_azimuth)`, in a common unit; + = curved right | the device sent all three (carry and its offline in its own flight) |

Each field is absent when an input is missing (or not a finite number), and
`metrics` is absent when no field is present. Every input is a measured
value: the flight model's result is never used, so a shot without a measured
`carry_offline` has no `curve`. When the device reports total spin and axis
itself (R10, Square Golf, Open Connect), they are under `launch`, not here;
the same goes for a measured `spin_loft` under `club`. One exception: a launch-only shot whose launch monitor's own
flight carries `assumed` spin gets `total_spin_rpm` and `spin_axis` from that estimate; `launch` has no spin then, and
the flight's `assumed` marks it. To show total spin and
spin axis for any device, read `launch.total_spin_rpm` and `launch.spin_axis`
first, else `metrics.total_spin_rpm` and `metrics.spin_axis`; likewise
`club.spin_loft`, else `metrics.spin_loft`. Lateral values are
absolute: + is right of the target line for every golfer. `metrics` is on
`/api/shots`, on [`/api/ws`](#websocket-apiws) and in the [export](#get-apishotsexport). `/frp` carries
the launch monitor's messages, which hold no `metrics`: the SDK's `ShotAggregator` computes them when it assembles a
shot, and other `/frp` consumers can use the formulas above or read the shot from `/api/shots`.

---

### GET /api/shots/{shot_id}/video

The shot's camera clips (beta): a `ShotVideo[]` (see
[`video`](#get-apishots)), one per camera whose clip is finished on disk,
`[]` when there are none. `shot_id` is the shot's `key.shot_id`; any of the
shots a clip belongs to finds it.

### GET /api/shots/{shot_id}/video/{camera}.json and .avi

One camera's clip of the shot (beta). `{camera}` is the camera's actor ID,
dot included: `/api/shots/550e8400-.../video/camera.0.avi`. Only finished
clips are served; anything else is `404` with a text body.

- `.avi`: the clip, MJPEG frames exactly as the camera sent them in an AVI
  1.0 file (`Content-Type: video/x-msvideo`). VLC, Kinovea and Windows Media
  Player play it; it is also the download.
- `.json`: its sidecar, the frame index:

  ```json
  {
    "version": 1, "fourcc": "MJPG", "width": 1280, "height": 720,
    "fps": { "nominal": 120.0, "measured": 119.62 },
    "start_ns": 81234567890123,
    "frames": [ { "t_ns": 0, "offset": 232, "len": 41022 }, ... ],
    "trigger_index": 480, "impact_index": 480,
    "trigger_ns": 4012000000, "impact_ns": 4012000000,
    "complete": true, "dropped": 0, "truncated": false
  }
  ```

  `start_ns` is the first frame's time on the server's monotonic clock, the
  same clock for every camera, so `start_ns + frames[i].t_ns` lines cameras
  up. `offset`/`len` locate each frame's JPEG inside the AVI. `trigger_ns`
  and `impact_ns` are relative to the first frame (`impact_ns` is negative
  when the estimate precedes the clip); `trigger_index` may equal the frame
  count when frames stopped before the trigger. `truncated` is true when the
  same launch monitor's next shot cut the clip short.

Both support HTTP `Range` (`206 Partial Content`), so a client fetches the
sidecar, then just the frames it needs. Both are sent with
`Cache-Control: private, no-cache`: a finished file doesn't change, but its
clip can be deleted, so a cached copy is checked with the server first.

```
GET /api/shots/550e8400-.../video/camera.0.avi
Range: bytes=41262-82137
```

Many webcams leave the Huffman tables out of their JPEG frames; strict
decoders need them inserted first (the stance crate's
`jpeg::ensure_huffman` does that).

### GET /api/shots/{shot_id}/video/{camera}.pose.json

The golfer's keypoints on every frame of one camera's clip (body tracking,
alpha), once they are written: after the camera's `video` entry has
`pose: true` ([`shot_pose`](#local)). Served with the same rules as the
`.avi` and `.json`: `404` with a text body before then and once the clip is
deleted, HTTP `Range`, `Cache-Control: private, no-cache`.

```json
{
  "version": 1, "skeleton": "halpe26",
  "model": "rtmpose-m-halpe26", "detector": "yolox-tiny",
  "rotation": 90, "width": 1280, "height": 720,
  "frames": [
    { "t_ns": 0, "score": 0.81, "bbox": { "x1": 512.0, "y1": 96.0, "x2": 790.0, "y2": 700.0 },
      "keypoints": [[651.2, 160.4, 0.93], ...] },
    { "t_ns": 8358000 },
    ...
  ]
}
```

- `rotation` (omitted at 0): the clockwise turn applied to the clip's frames before tracking. `width`, `height` and
  every keypoint are in the turned picture; the `.avi` frames are as the camera sent them, so turn a frame by
  `rotation` before drawing on it.
- One entry per clip frame, in order; `t_ns` is the sidecar's `t_ns` of that
  frame, so frame `i` of the clip is the entry with the same `t_ns`.
- `keypoints`: 26 `[x, y, score]` in pixels of the turned picture, in the
  [Halpe-26 order](#halpe-26-keypoints), as the model found them (not
  smoothed); absent, with `score` and `bbox`, where nobody was found.
- `score`: the mean keypoint score; `bbox`: the box the pose model looked in.

About 1 KB per frame: a 240-frame clip's file is about 220 KB.

### GET /api/shots/{shot_id}/video/{camera}.swing.json

The swing analysis of one camera's clip (alpha): swing events, tempo and the
camera view's body metrics, measured on the clip's keypoints. Written right
after the [`.pose.json`](#get-apishotsshot_idvideocameraposejson) for a
camera at `golfer_front` (face-on) or `target_line` (down the line); a
camera at `golfer_back` or `overhead` gets none. Served once the camera's
`video` entry has `swing: true` ([`shot_swing`](#local)), with the same
rules as the `.avi` and `.json`: `404` with a text body before then and once
the clip is deleted, HTTP `Range`, `Cache-Control: private, no-cache`.

```json
{
  "version": 1,
  "view": "face_on",
  "handed": "right",
  "view_guess": { "value": "face_on", "confidence": 0.8 },
  "handed_guess": { "value": "right", "confidence": 1.0 },
  "height_cm": 180.0,
  "scale": { "torso_px": 200.0, "cm_per_px": 0.25 },
  "smoothing": { "min_score": 0.3, "max_gap_ns": 150000000, "cutoff_hz": 8.0, "order": 2 },
  "events": {
    "address": { "t_ns": 1000000000, "frame": 120, "confidence": 0.8 },
    "takeaway": { "t_ns": 1008333333, "frame": 121, "confidence": 0.8 },
    "top": { "t_ns": 1850000000, "frame": 222, "confidence": 0.9 },
    "impact": { "t_ns": 2150000000, "frame": 258, "confidence": 0.9 },
    "impact_source": "refined",
    "finish": { "t_ns": 2800000000, "frame": 336, "confidence": 0.7 }
  },
  "tempo": { "backswing_ns": 841666667, "downswing_ns": 300000000, "ratio": 2.81 },
  "metrics": [
    { "id": "hip_sway", "view": "face_on", "unit": "torso",
      "values": { "top": -0.05, "impact": 0.12 }, "cm": { "top": -2.5, "impact": 6.0 },
      "confidence": 0.8 },
    { "id": "lead_arm_angle", "view": "face_on", "unit": "degrees",
      "values": { "address": 175.0, "top": 150.0, "impact": 176.0 }, "confidence": 0.8 },
    { "id": "hip_turn", "view": "face_on", "unit": "degrees",
      "values": { "top": 41.4, "impact": 38.8 }, "confidence": 0.7 }
  ],
  "series": {
    "t_ns": [0, 8333333, 16666667],
    "metrics": [
      { "id": "hip_sway", "unit": "torso", "values": [null, 0.0, -0.0021] },
      { "id": "hip_turn", "unit": "degrees", "values": [null, 0.0, 6.4638] }
    ]
  }
}
```

- `view` (from the camera's position) and `handed`, the hand the shot was
  hit with (flighthook's hand at the shot, right-handed when unknown).
  `view_guess` and `handed_guess` are what the swing itself suggests.
  `mirrored` (`true`, else absent) says the picture was mirrored and
  analyzed as the golfer's true side; `smoothing` is the keypoint cleaning
  used.
- `events`: times on the clip's time base (`t_ns` as in the sidecar), each
  with a 0 to 1 confidence; an event the clip doesn't show is absent. Besides
  `address`, `takeaway`, `top`, `impact` and `finish`: `arm_parallel_back`
  and `arm_parallel_down`, the lead arm parallel to the ground in the
  backswing and the downswing.
  flighthook gives the analysis the clip's estimated impact (the sidecar's
  `impact_ns`, when it falls inside the clip). `impact_source` says what
  became of it:
  - `refined`: moved to where the hands bottom out within 0.3 s of it;
  - `given`: no swing found near it. The file holds the impact alone (no
    other event, no `tempo`, no `handed_guess`), and the Cameras pane shows
    "No swing found in this clip" for the camera instead of the numbers;
  - `estimated`: there was no estimate, so impact was found from the hands
    alone.
- `tempo`: takeaway to top against top to impact.
- `metrics`: the view's metrics at `address`, `arm_parallel_back` (the
  lead arm parallel to the ground in the backswing), `top`,
  `arm_parallel_down` (the same in the downswing), `impact` and `finish`
  (each absent where not measured). `unit` is `degrees`, `torso` (a share of
  the torso length, with `cm` when the golfer's height is set) or
  `ratio`. Every value is a 2D measure in this camera's picture. Sideways
  shifts face-on (`hip_sway`, `head_lateral`) are positive toward the
  target and negative away from it, for either hand. The metrics are
  described in
  [Cameras](devices/cameras.md#swing-analysis).
- `series`: every metric at every frame of the clip, for following a
  replay frame by frame: `t_ns` (the clip's frame times, as in the
  sidecar) and per metric its `unit` and one value per frame (`null` where
  not measured), from the same formulas; shifts and turns are from address.
  At an event's frame a series holds the metric's value at that position.
  Values have 4 decimals. `torso` values convert to centimeters as
  `value × scale.torso_px × scale.cm_per_px`. Absent when no metric was
  measured; a file without it is read as before. About 100 bytes per frame
  face-on: a 560-frame clip's file is about 55 KB.

#### Halpe-26 keypoints

`0` nose, `1`/`2` left/right eye, `3`/`4` ears, `5`/`6` shoulders, `7`/`8`
elbows, `9`/`10` wrists, `11`/`12` hips, `13`/`14` knees, `15`/`16`
ankles, `17` head (top), `18` neck, `19` hip (center), `20`/`21` left/right
big toe, `22`/`23` small toes, `24`/`25` heels. Left and right are the
golfer's own sides.

### POST /api/shots/{shot_id}/video/{camera}/analyze

Track one camera's clip now (body tracking, alpha): find the golfer's
keypoints on it and, for a `golfer_front` or `target_line` camera, analyze
the swing. This is how a clip is tracked in Historical mode, where nothing
runs until a clip is viewed; the UI sends it, once per clip,
when it shows a clip whose `video` entry has no `pose`, in Historical and
Live mode (in Live mode, for a clip recorded before Live was on: a clip
recorded in Live mode is tracked by itself). No body.

The clip goes to the front of body tracking's queue (clips run one at a
time; the clip just asked for is the one on screen) and starts at once,
without the 3 s wait a clip recorded in Live mode has. The models are
loaded by the first such request, so that clip waits for them; they are
never downloaded by it: while their files are missing (see
[`GET /api/body_tracking/models`](#get-apibody_trackingmodels)) the clip is
left untracked, and a later request tries again. When it is done the files are written beside the clip as for
any tracked clip, and [`shot_pose`](#local), then
[`shot_swing`](#local), go out on [`/api/ws`](#websocket-apiws). A clip already
tracked, waiting or being tracked is not tracked again.

The clip is tracked with its camera's `rotation_deg` at the time it runs.
A clip keeps the tracking it was given; nothing tracks it again by itself.

**Query parameters**:

- `force=1` (optional, also `force=true`): delete the clip's
  `{camera}.pose.json` and `{camera}.swing.json`, clear `pose`, `swing`,
  `rotation` and `analyzed_with` on its `video` entry (in `shot.json` and on
  the shot rows), and track it again: `202`. The UI sends it once per clip
  and session when it shows a clip tracked with a `rotation` other than its
  camera's `rotation_deg` now. While body tracking is off it is `409` and
  nothing is deleted.

**Response**: `{"state": "queued"}` or `{"state": "done"}`; errors carry
`{"error": "<reason>"}`.

| Status | When |
| --- | --- |
| `202 Accepted` | queued, or already waiting or being tracked (`"queued"`) |
| `200 OK` | its keypoints are there already (`"done"`): fetch the files |
| `404 Not Found` | the shot has no finished clip from `{camera}` |
| `409 Conflict` | body tracking is off (`"body tracking is off"`) |

```
POST /api/shots/550e8400-e29b-41d4-a716-446655440000/video/camera.0/analyze
→ 202 {"state":"queued"}
… /api/ws: shot_pose, then shot_swing
POST /api/shots/550e8400-e29b-41d4-a716-446655440000/video/camera.0/analyze
→ 200 {"state":"done"}
POST /api/shots/550e8400-e29b-41d4-a716-446655440000/video/camera.0/analyze?force=1
→ 202 {"state":"queued"}
… /api/ws: shot_pose, then shot_swing
```

### GET /api/shots/{shot_id}/grades/{camera}

One camera's swing numbers graded against bands from published tour
averages (alpha), for the tour and the amateur reference: what colors the
Cameras pane's tiles and tables (see
[Bands and colors](devices/cameras.md#bands-and-colors)). Computed on each
request from the camera's
[`{camera}.swing.json`](#get-apishotsshot_idvideocameraswingjson), the
shot's `club_selected` and hand, and the sex and height of the shot's golfer as the saved golfers
hold them; never stored, so a change to a golfer shows in the next
request. `404` (`{"error": "<reason>"}`) until the camera's `video` entry has
`swing: true`, and once the clip is deleted.

```json
{
  "shot_id": "550e8400-e29b-41d4-a716-446655440000",
  "camera": "camera.0",
  "club_class": "driver",
  "sex": "male",
  "hand": "rh",
  "references": [
    {
      "reference": "tour",
      "grades": [
        { "id": "hip_turn", "position": "impact", "value": 44.0, "unit": "degrees",
          "low": 33.0, "high": 43.0, "fade": [36.0, 46.0], "draw": [30.0, 40.0],
          "z": 1.2, "z_fade": 0.6, "z_draw": 1.8, "fit": "fade", "side": "high",
          "direction": "open", "word": "more open: fade side",
          "proxy_sd": true, "confidence": 0.9 },
        { "id": "hip_sway", "position": "impact", "value": 6.9, "unit": "inches",
          "low": 2.0, "high": 6.0, "fade": [1.25, 5.25], "draw": [2.75, 6.75],
          "z": 1.45, "z_fade": 1.83, "z_draw": 1.08, "fit": "near", "side": "high",
          "direction": "toward", "word": "further toward the target than tour: slide, blocks",
          "confidence": 0.8 }
      ],
      "tempo": [
        { "id": "tempo_ratio", "position": "swing", "value": 2.8, "unit": "ratio",
          "low": 2.5, "high": 3.5, "z": -0.4, "fit": "both", "side": "on",
          "proxy_sd": true, "confidence": 1.0 }
      ]
    },
    { "reference": "amateur", "grades": [], "tempo": [] }
  ]
}
```

- `club_class`: `driver` (driver, mini driver), `mid_iron` (every wood,
  hybrid and iron) or `wedge` (PW, GW, SW, LW). A putter or a shot without a
  club has none, and `references` is empty: nothing is graded.
- `sex`: the shot's golfer's (`male` for a shot with no golfer), which picks the men's or women's bands. `hand`:
  the shot's [`Handedness`](#handedness) (`"rh"` when unknown). The metrics are measured relative to the
  target, so both hands grade alike.
- `references`: `tour`, then `amateur`. `grades` holds every metric at every
  key `position` (as in the swing file) that has a band; `tempo` the
  `tempo_ratio` and the `downswing_time` (`position` `swing`). A value
  without a band, or a band that is not graded, is absent.
- `value`: in the band's `unit` (`degrees`, `inches`, `ratio`, `seconds`),
  signed as the band: turns positive open (toward the target) and negative
  closed, shifts positive toward the target, the ball or up. A distance is
  put into inches with the golfer's height, or an assumed height for the sex
  without one (`assumed_scale: true`).
- `low`, `high`: the band, center ± one SD. `fade`, `draw`: for hip turn,
  shoulder turn, shoulder tilt and hip sway at impact, the fade band and the
  draw band (the center shifted by a small offset each way, same width).
  `z`, `z_fade`, `z_draw`: the value's distance from each center in SDs.
- `fit`: `both` (inside the band, or inside both the fade and the draw band),
  `fade` (inside the fade band only), `draw` (inside the draw band only),
  `near` (outside, within 2 SD of the nearer band) or `far`. `side`: `low`,
  `high` or `on` (inside). `direction`: the side's word (`open`, `closed`,
  `toward`, `away`, `up`, `down`, `toward_ball`, `back`, `more`, `less`),
  absent on the center. `word`: what the Cameras pane writes after the
  metric's name for a value off its band; absent for `both`.
- `proxy_sd` (the SD is borrowed from the nearest published spread),
  `assumed_scale`, `club_fallback` (a wedge graded against the mid-iron
  band), `sex_fallback` (a woman graded against the men's band): present
  when `true`. `confidence`: the metric's keypoint confidence.
- A turn from one camera is graded at impact only; elsewhere it is absent.

### GET /api/body_tracking/models

Where body tracking's model files are (alpha): the files the configured
`body_tracking.model` needs (the pose model, then the YOLOX-Tiny person
detector), whether each is in the models folder and matches its SHA-256,
and a download's progress. flighthook downloads them only through
[`POST /api/body_tracking/models/download`](#post-apibody_trackingmodelsdownload);
the UI asks for that only after the user confirms its download dialog, and
polls this route about 4 times a second while it shows the download.
Checking a file reads and hashes it the first time (a few hundred ms);
later calls only look at its size and modification time.

**Response**: `200`

```json
{
  "state": "downloading",
  "model": "rtmpose-m",
  "dir": "/home/golfer/.local/share/flighthook/models",
  "files": [
    {"name": "rtmpose-m-halpe26.onnx", "label": "RTMPose-m pose model",
     "host": "download.openmmlab.com", "size": 55685444, "present": false},
    {"name": "yolox-tiny.onnx", "label": "YOLOX-Tiny person detector",
     "host": "github.com", "size": 20219662, "present": true}
  ],
  "download": {
    "files": [
      {"name": "rtmpose-m-halpe26.onnx", "label": "RTMPose-m pose model",
       "host": "download.openmmlab.com", "done": 20971520, "total": 52089522}
    ],
    "current": 0,
    "done": 20971520,
    "total": 52089522
  }
}
```

- `state`: `missing` (a file is not there, or does not match, and no
  download runs), `downloading` (a download runs: `download` is present),
  `ready` (every file is there) or `failed` (the last download for this
  model failed: `error` says why, as stance words it: the URL and the
  network error, or a hash mismatch).
  A failure stays until the next download or a cancel; a canceled download
  leaves `missing`, with no error.
- `model`, `dir`: the configured pose model and the models folder.
- `files`: `name` (in `dir`), `label`, `host` (where it is downloaded from;
  GitHub hands its file over from `release-assets.githubusercontent.com`),
  `size` (bytes of the model file), `present`.
- `download` (only while one runs, whichever model it fetches): `files`,
  the files it fetches in order, each with `done` and `total` bytes (`total`
  is the response's length once it has started, which for a zip source is
  the archive's, and the model's size until then); `current`, the index of
  the one being fetched; `done` and `total` over all of them.

### POST /api/body_tracking/models/download

Download the files the configured pose model lacks (alpha), from their
official sources over HTTPS, one after the other, each checked against its
SHA-256 before it is kept. This is the only way flighthook downloads a
model: no mode, camera, clip or startup does. One download runs at a time;
asking again while one runs starts nothing. No body.

**Response**: the [state](#get-apibody_trackingmodels).

| Status | When |
| --- | --- |
| `202 Accepted` | a download started, or one was already running (`"downloading"`) |
| `200 OK` | every file is there already (`"ready"`): nothing to download |

Once it is done the state is `ready` and the models load the next time
body tracking needs them; a model wanted while its files were missing loads
then.

### DELETE /api/body_tracking/models/download

Stop the download running, if any (alpha). The state is `missing` at once;
the download stops between two reads (at most one read's timeout on a
stalled connection) and its partial file is deleted. The files already
completed by it stay. The UI sends it from the download overlay's Cancel and
then saves `body_tracking.mode` as `"off"`; this route does not change the
mode. Saving `"off"` also stops a download.

**Response**: `200` with the [state](#get-apibody_trackingmodels), whether
or not a download was running.

### GET /api/cameras

The cameras this computer has (beta), for picking one on a camera card.
Enumerating can take a second; the result is reused for 3 s. A camera a
camera actor records from is never opened again to list it (its entry is
the one read when it was opened).

**Response** `200 OK` -- `CameraListResponse` (`500` with a text body when
the enumeration fails unexpectedly):

```json
{
  "supported": true,
  "cameras": [
    {
      "id": "\\\\?\\usb#vid_046d&pid_085e&mi_00#...",
      "name": "HD Pro Webcam C920",
      "usb": "046d:085e",
      "modes": ["1280x720@60", "1920x1080@30", "640x480@30"],
      "in_use_by": "camera.0"
    },
    { "id": "test-pattern", "name": "Test pattern", "modes": ["1280x720@60", "640x360@30"] }
  ]
}
```

- `supported`: as `cameras_supported` in [`GET /api/status`](#get-apistatus).
- `id`: the stable id a camera section's `camera` holds: on Windows the
  device's symbolic link, on Linux its `/dev/v4l/by-id` path, or
  `<bus>/<name>` for identical cameras without serial numbers. It survives
  replugging into the same USB port and reboots.
- `name`: the camera's own name.
- `usb`: its USB vendor and product id, when known.
- `modes`: its MJPEG modes, fastest first.
- `in_use_by`: the camera actor recording from it, if one is.
- `error`: why its modes could not be read (in use by another application).
- The last entry is always the test picture, `test-pattern`, with the modes
  it suggests (it renders any `WIDTHxHEIGHT@FPS`; `auto` is its fastest
  listed mode).

### GET /api/cameras/{actor}/still

The camera actor's newest frame (beta): a JPEG as the camera sent it, at
most half a second old while it records (`Content-Type: image/jpeg`,
`Cache-Control: no-store`). `404` while the actor has no frame. The frame
comes from the actor's own capture; the camera is never opened a second
time.

### GET /api/cameras/{actor}/live

The camera actor's live frame (beta), for showing the camera live: a JPEG
as the camera sent it. The request waits until the camera has a frame newer
than the one the client has, up to 1 s.

**Query**: `after` — the `X-Frame-Seq` of the frame the client has; `0` (or
omitted) for none. Sequence numbers start again at 1 when flighthook
restarts: an `after` above every number given out since counts as `0`, so a
client should take a frame numbered below its last one as the newest.

**Response** `200 OK` (`Content-Type: image/jpeg`, `Cache-Control:
no-store`) with:

- `X-Frame-Seq`: the frame's sequence number, to send as the next
  request's `after`. Numbers count up over every camera's live frames and
  never repeat while the server runs.
- `X-Frame-Time-Ns`: when the frame reached the computer, less the
  camera's latency, in nanoseconds on the server's monotonic clock: the
  clock of the clip sidecars' `start_ns`.
- `X-Pose` (body tracking, alpha): the golfer's keypoints on this frame,
  present when body tracking ran on it. Compact JSON:

  ```json
  {"skeleton":"halpe26","keypoints":[[605.0,251.9,0.88],[602.5,237.0,0.85],...],"bbox":[529.8,184.0,791.6,691.3],"ms":35.6}
  ```

  `keypoints`: 26 `[x, y, score]` in the pixels of the picture turned by the camera's `rotation_deg` (the JPEG body is
  not turned) to 0.1 px, scores to 0.01, in the [Halpe-26 order](#halpe-26-keypoints), smoothed with the
  `body_tracking.smoothing` filter; `[]` when nobody was found, then without
  `bbox`. `bbox`: `[x1, y1, x2, y2]`, the box the pose model looked in.
  `ms`: how long body tracking took on the frame. The pose is for this exact
  frame: a header on the frame's own response needs no matching on the
  client and leaves the JPEG body as the camera sent it; it is about 1 KB.

  For a camera at `golfer_front` or `target_line` that found the golfer,
  `posture` adds the golfer's posture on the frame for that view, measured
  for the hand in effect and smoothed with the same `body_tracking.smoothing`
  filter: 2D angles in degrees to 0.1, each left out when its keypoints were
  not found.

  ```json
  "posture":{"view":"face_on","spine_angle":11.2,"shoulder_tilt":19.4,"lead_arm_angle":172.0}
  "posture":{"view":"down_the_line","spine_angle":27.3,"lead_knee_flex":18.1,"trail_knee_flex":17.0,"trail_elbow_angle":160.2}
  ```

  `view` `face_on` carries `spine_angle` (the spine's sideways tilt, positive
  with the shoulders away from the target), `shoulder_tilt` (positive with
  the lead shoulder higher) and `lead_arm_angle` (180 straight);
  `down_the_line` carries `spine_angle` (the forward bend, positive toward
  the ball), `lead_knee_flex`, `trail_knee_flex` (0 straight) and
  `trail_elbow_angle` (180 straight).
- `X-Pose-State` (body tracking): `off` (`body_tracking.mode` is not
  `live`: no live tracking),
  `loading` (the model is not ready yet), `missing` (its files are not
  downloaded: see
  [`GET /api/body_tracking/models`](#get-apibody_trackingmodels)) or
  `downloading` (a model download is running); in these frames go out
  without `X-Pose`. Then `ready`, or `failed`, with `X-Pose-Error` saying why
  (printable ASCII, at most 300 characters): the model could not be loaded,
  or this camera's live body tracking stopped after repeated errors (frames
  then go out without `X-Pose` until no request has come for 3 s). Also on
  the `204`.

| Status | When |
| --- | --- |
| `204 No Content` | no newer frame within 1 s (the camera is not capturing, or its frames are slower) |
| `404 Not Found` | `{actor}` is not a configured camera |
| `503 Service Unavailable` | 8 requests are already waiting on this camera |

Each request keeps the camera preparing live frames for the next 3 s, at up
to 15 frames a second (slower cameras send every frame); frames in between
are skipped. With body tracking in Live mode, frames come at the rate body tracking
keeps up with, at most 15 a second, each with its pose; frames that arrive
while it works on one are skipped. Send the next request when the last one is answered. A camera
that nobody asked for in the last 3 s prepares none, and the first request
after that waits for a fresh frame. The camera is never opened a second
time, and recording is unaffected.

```
GET /api/cameras/camera.0/live?after=1841
→ 200, X-Frame-Seq: 1843, X-Frame-Time-Ns: 5123456789012, X-Pose: {...}, X-Pose-State: ready, <JPEG>
```

---

### GET /api/blocks

The practice blocks of this run, open and closed: the **sets** the dashboard shows. Blocks are published by
flighthook's practice actor as `practice.block` and recorded for every shot, whatever `shot_grouping` says; that field
only picks the Shots tab's default view (see [POST /api/shots/grouping](#post-apishotsgrouping)).

- There is one open block per golfer (a golfer with no linked profile counts as one golfer; shots with no golfer
  selected have a block of their own). A shot belongs to the open block of the golfer up at its `shot.trigger`.
- A golfer's block rolls when the golfer up changes club and the block has shots: it closes and a new one opens with the
  new club. A block with no shots yet is republished in place (same `id` and `number`) with the new club, so no empty
  blocks or gaps in the numbers appear. Changing club and coming back to the old one without a shot in between starts a
  new block.
- A golfer who comes back on the club they left with continues their open block; each golfer has their own.
- **New set** ([POST /api/blocks](#post-apiblocks)) closes the golfer's open block and opens a fresh one; it is a no-op
  on a block with no shots. **New session** (the same endpoint with `new_session`) closes every open block and counts
  the session up.
- A shot that never finishes (the monitor triggers another shot first, drops out, or 45 s pass) leaves its block
  uncounted.

**Response** `200 OK` -- `BlocksResponse`:

```json
{
  "session": 1,
  "blocks": [
    { "id": "6f1c2a3b-4d5e-4f60-8a71-92b3c4d5e6f7", "golfer": "flighthook.0f8c2d1a-77b4-4e1e-9a50-3c6d2b8e4f10",
      "club": "7I", "number": 3, "session": 1, "started_at": 1794063845123,
      "open": false, "shots": 10, "first_shot_at": 1794063850001 },
    { "id": "0b7e9c1d-2f3a-4b5c-9d6e-7f8091a2b3c4", "golfer": "flighthook.0f8c2d1a-77b4-4e1e-9a50-3c6d2b8e4f10",
      "club": "DR", "number": 4, "session": 1, "started_at": 1794063960004,
      "open": true, "shots": 0 }
  ]
}
```

- `session`: the current practice session, counting from 1 each run and up with every New session or
  [Clear history](#post-apishotsclear).
- `blocks`: every block seen this run, in the order first seen. Each is the `practice.block` as last published:
  - `id`: UUID v4.
  - `golfer`: the golfer id of the turn it was opened or last republished under, or `null` for shots with no golfer.
  - `club`: a [club code](#club) or `null`.
  - `number`: the set number, counting from 1 per golfer within a session. The Shots tab shows it (`SET 3` in By set
    headers).
  - `session`: the practice session it belongs to.
  - `started_at`: when it opened, milliseconds since the Unix epoch (host clock).
  - `open`: not closed by the practice actor yet. A closed block never reopens.
  - `shots`: stored shots with data that belong to it (the ones `/api/shots` lists).
  - `first_shot_at`: when its first stored shot was triggered, milliseconds since the Unix epoch; absent before its
    first shot.

---

### POST /api/blocks

Send a `practice.new_block` command as flighthook and wait for the practice actor's `bus.reply`. The Shots tab's
**New set** and **New session** controls send it.

**Request**: a JSON object, both fields optional:

```json
{ "golfer": "flighthook.0f8c2d1a-77b4-4e1e-9a50-3c6d2b8e4f10" }
```

```json
{ "new_session": true }
```

- `golfer`: the golfer whose block to roll (a golfer id); absent for the golfer up. A golfer who is not up gets a block
  with their own last club. Ignored with `new_session`.
- `new_session`: `true` closes every open block, counts the session up and opens a block for the golfer up. Default
  `false`.

**Response**: `200 OK` once the practice actor answered with `ok`; `400 Bad Request` with `{"error": "<reason>"}` when
the body is no request (`"bad_request"`) or the actor refused it (its `bus.reply` `error`); `504 Gateway Timeout`
with `{"error": "no reply"}` when nothing answered within 5 s. The changed `practice.block` messages are published
before the reply.

---

### POST /api/select

Select the golfer, club, hand and mode: a `golfer.select` command from flighthook to the session, answered by a
`bus.reply`. The dashboard's Golfer, Club and Shot Type controls send the same command on [`/api/ws`](#websocket-apiws).

**Request**: a JSON object with any subset of the fields; an absent field stays as it is:

```json
{ "club": "7I", "handedness": "lh" }
```

- `golfer` (beta): a golfer id (`flighthook.<uuid>`, or a game's id such as `pureplay.thill`), or `null` to clear.
- `club`: a [club code](#club) exactly as listed (`DR`, `2W`, ... `PT`), or `null` for no club. Any other spelling
  is `400`.
- `handedness`: `"rh"` or `"lh"`.
- `mode`: `"full"`, `"putting"` or `"chipping"`.

The session refuses nothing and publishes a `golfer.turn` that shows the outcome. Mode changes only when a select carries `mode`; a club
alone does not change it. Choosing a golfer sets the club and hand to that golfer's own last ones. While a simulator is connected, its card
(Open Connect client, Pureplay Connect, Pureplay Accessory) sends the club, hand and mode it holds straight back, so
the turn follows the simulator again. A Pureplay Accessory card
takes the club to Pureplay when its `club_out` row is `on` (see its card telemetry under
[GET /api/status](#get-apistatus)).

**Response**: `200 OK` once the session answered with `ok`; `400 Bad Request` with `{"error": "<reason>"}` for a body
that is no select or has a value it does not know (`"bad_request"`); `504 Gateway Timeout` with `{"error": "no reply"}` when nothing answered within 5 s.

---

### POST /api/shots/clear

No body. Returns `202 Accepted`; the Shots tab's **Clear history** (trash icon in the Shots tab's navigation, after a
confirmation) sends this or its `/api/ws` twin. flighthook then:

- deletes its shot history: `/api/shots` and the
  [export](#get-apishotsexport) are empty, raw frames included;
- drops every block, open or closed, and counts the practice session up, as
  New session does; the next shot starts set 1 in the new session, with the club in play unchanged;
- forgets the shots in flight: a device that finishes a shot it triggered
  before the clear stores nothing. The next trigger is a new shot.

The system actor deletes every camera clip on disk too (beta): the clips
belong to the shots. A clip a camera is still writing is dropped when it
finishes, and a folder that can't be deleted at once (a file still open) is
retried later.

`/api/ws` clients get a `clear_shot_history` event and clear their shots and sets. Nothing is undone; export
first to keep the shots (the export never holds the clips).

---

### POST /api/shots/grouping

Set and persist the default view of the Shots tab (the same `shot_grouping`
field a Settings save writes). Sets are assigned the same way in every view;
this changes no set.

**Request**:

```json
{ "mode": "by_club" }
```

- `mode`: [`ShotGrouping`](#shotgrouping): `"by_set"` | `"by_club"` | `"none"`

**Response** `200 OK` once the system actor has applied it, echoing the body
(`{"mode":"by_club"}`). `504 Gateway Timeout` when it has not within 22 s
(see [config timeout](#config-timeout)); the command is already on the bus then, so it may still apply later. A
`mode` outside the three is
`422 Unprocessable Entity`, a malformed body `400 Bad Request` and a missing
`Content-Type: application/json` `415 Unsupported Media Type`.

---

### POST /api/auto-chip

Send an Auto-chip (beta): a short, straight chip of a chosen total distance,
for chips the launch monitor can't read.
Off by default; turn it on in Settings → General → Simulation (useful with radar launch
monitors that miss short chips). The dashboard's Auto-Chip button is enabled only while an Open Connect client or a
Pureplay Connect card is connected.

**Request**:

```json
{ "yards": 3 }
```

- `yards`: the chip's total distance (carry + roll) in yards: `1`, `3` or
  `5`.

**Response**: `200 OK` with the shot that was sent:

```json
{
  "actor": "autochip",
  "shot_id": "0b8f1c52-7a0e-4d61-9a43-5f2f2b7c9e10",
  "shot_number": 1,
  "ball_speed_mph": 7.809,
  "launch_elevation": 28.0,
  "backspin_rpm": 781
}
```

`400 Bad Request` with a JSON `{"error": "yards must be one of 1, 3, 5"}`
body for any other distance. `409 Conflict` with a JSON `{"error"}` body when
Auto-chip is turned off (`[simulation]` `auto_chip` is `false`, the default; the error is
`"Auto-Chip is disabled in settings"`, and no shot is sent or numbered), while
a tournament round is active, whoever holds it (nothing sent or numbered either; see
`round` under [`GET /api/status`](#get-apistatus)), or
when the `[simulation]` altitude or temperature is out of range. A body that is not
`{"yards": <non-negative integer>}` is `422 Unprocessable Entity`, and a
missing `Content-Type: application/json` `415 Unsupported Media Type`.

The chip is launched with `launch_azimuth` 0, `launch_elevation` 45°,
`backspin_rpm` = round(300 × ball speed in mph) and `sidespin_rpm` 0, and no
club data. Its ball speed is the one at which flighthook's simulation, with
the `[simulation]` altitude, temperature and surface, comes to rest at
`yards` (within 0.1 yd), whatever the simulation `mode`. On the default
settings that is about 4.4 mph for 1 yd, 7.8 mph for 3 yd and 10.4 mph for
5 yd. The simulator runs its own physics on the launch, so the distance it
shows depends on its own ground (e.g. its green speed).

The shot goes on the bus as a launch monitor of its own, actor id `[frp] auto_chip_actor_id` and card key `autochip`
(`device.source` `virtual`), numbered from 1 in this run of flighthook (a request that fails takes no number), with the
shot messages a launch monitor sends: `shot.trigger`, `shot.launch`, a `shot.flight` simulated as for a shot in
chipping mode (also with `[simulation] mode = "off"`: the simulated carry and total are the chip's only results) and
`shot.finished`. So it appears in
[`GET /api/shots`](#get-apishots), the shot sets and the
[export](#get-apishotsexport) (device kind `autochip`, `settings.detection_mode`
`"chipping"`), and every
Open Connect client sends it to its simulator, and every Pureplay output to
Pureplay: Auto-chip shots are taken whatever monitor `full_monitor`,
`chipping_monitor` and `putting_monitor` route to.

An Auto-chip joins the set in play like any shot, and counts in that set's
`shots`. The dashboard lists it under that set but leaves it out of the
set's shot count and of every set or club statistic (range bars, dispersion,
the flight chart's set), and it is not a device of the device filter.

---

### POST /api/tournament

Switch tournament mode on or off. While it is on, Auto-chip is off and the
mock launch monitor, the trackball and every remote launch monitor that
declares itself virtual on `/frp` (e.g. a trackball served over FRP) hold
their shots (a swing then is dropped and takes no shot number); no output's payload changes. It is never
refused: a virtual launch monitor feeding an output just holds its shots. A
simulator's round is untouched either way, and while one holds the round the
setting stays underneath it: when the round ends, tournament mode is on again
if it was on, and while one holds the round `{"on": false}` clears the setting but the round stays active (`held`) until the
simulator ends it; tournament mode is on afterward only if the setting is still on. The dashboard disables its
toggle while `held`. The setting is saved as
`[simulation] tournament` (absent while
off) and read at start-up. This route is its only writer: a
[`POST /api/settings`](#post-apisettings) save, scoped to `global` or of the
whole config, keeps the live value whatever its body carries.

```json
{ "on": true }
```

Answers `200` with the round as it is then, the same object as `round` in
[`GET /api/status`](#get-apistatus):

```json
{ "active": true, "holder": "Manual", "manual": true }
```

`504 Gateway Timeout` when the change is not applied in time. A body that is
not `{"on": <boolean>}` is `422 Unprocessable Entity`.

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
device produced no data for are included: their `data` has no `launch`,
`flights`, `club` or `impact`, and `raw` shows what the device sent.

Camera clips are never exported: `data` carries no `video`, and nothing
names a camera. Swing video shows the player; it stays on the computer
running flighthook.

**JSONL**: one `ShotExport` object per line (pretty-printed here):

```json
{
  "schema": 4,
  "flighthook_version": "0.2.1",
  "exported_at": "2026-10-25T21:04:11.123Z",
  "time": "2026-10-25T20:58:03.412Z",
  "actor": "mevo.0",
  "shot_id": "550e8400-e29b-41d4-a716-446655440000",
  "shot_number": 7,
  "device": { "kind": "flightscope", "manufacturer": "FlightScope", "model": "Xi", "firmware": "1.23 (Feb 3 2024)" },
  "golfer_id": "flighthook.0f8c2d1a-77b4-4e1e-9a50-3c6d2b8e4f10",
  "club_selected": "8I",
  "block": "6f1c2a3b-4d5e-4f60-8a71-92b3c4d5e6f7",
  "settings": {
    "device": { "ball_type": 1, "track_pct": 80.0, "environment": "limited_flight" },
    "simulation": { "altitude": "500ft", "temperature_c": 30.0 },
    "detection_mode": "full",
    "handed": "rh"
  },
  "data": {
    "shot_number": 7,
    "shot_id": "550e8400-e29b-41d4-a716-446655440000",
    "from": "mevo.0",
    "card": "mevo.0",
    "block": "6f1c2a3b-4d5e-4f60-8a71-92b3c4d5e6f7",
    "turn": { "golfer": "flighthook.0f8c2d1a-77b4-4e1e-9a50-3c6d2b8e4f10", "club": "8I", "handedness": "rh", "mode": "full" },
    "club_selected": "8I",
    "source": "measured",
    "launch": { "launch_speed": "37.9mps", "launch_azimuth": -3.1, "launch_elevation": 23.9, "backspin_rpm": 6897, "sidespin_rpm": 0, "total_spin_rpm": 6897, "spin_axis": 0.0 },
    "flights": [ { "from": "simulation", "flight": { "basis": "modeled", "carry_distance": "95.2m" } } ],
    "club": { "club_speed": "31.2mps", "path": 1.1, "attack_angle": -9.8, "face_angle": 0.4 }
  },
  "raw": {
    "transport": "flightscope-tcp",
    "frames": [
      { "t_ms": 0, "dir": "rx", "label": "E5 AVR>APP", "hex": "0c42414c4c2054524947474552000000" },
      { "t_ms": 612, "dir": "rx", "label": "E8 AVR>APP", "hex": "5d0000040000..." },
      { "t_ms": 640, "dir": "rx", "label": "D4 AVR>APP", "hex": "9c0000040000..." },
      { "t_ms": 703, "dir": "tx", "label": "69 APP>AVR", "hex": "" },
      { "t_ms": 760, "dir": "tx", "label": "B0 APP>AVR", "hex": "0101" }
    ]
  }
}
```

- `schema`: export format version (`4`). Bumped when a field changes meaning. Schema 4: `data` is the stored shot
  as `/api/shots` serves it: the measured `launch`, every flight with its sender in `flights`, the turn at the trigger
  in `turn`, the practice block in `block` (read from `group` in an older line), `club_selected`, and the game's
  `stroke` when there is one. `golfer_id` and `block` are new at the top level. Schema 3: the raw records of a device
  on a device driver (the FlightScope, the R10, the Square Golf, the ExPutt) carry a `label` per message instead of
  `type`, `src`, `dst` and `channel`. Schema 2 has the launch and the device's flight values in
  `data.launch` and `data.flight`; a schema 1 line has both in one `data.ball` block. The `flighthook` crate's
  `ShotExport` reads schema 1 into the schema 2 shape and schema 2 and 3 lines as they are.
- `time`: when `shot.trigger` reached the web layer (host clock, RFC 3339 UTC).
- `device`: `kind` (`flightscope`, `square_golf`, `garmin_r10`, `exputt`,
  `trackball`, `openconnect`, `pureplay`, `mock`, `autochip`, and `frp` for a remote launch monitor (beta), whose `actor` is its FRP actor id) plus manufacturer,
  model and firmware as the device reported them.
  FlightScope: `model` is the product (`Mevo+`, `Mevo Gen2`, `Mevo` when the
  generation is unknown, or `Xi`) and `firmware` the device's version and
  build date, e.g. `"1.23 (Feb 3 2024)"`.
- `golfer_id`: the golfer of the turn the shot was hit in (a golfer id, by the lookup rule: a game's id that a saved
  golfer lists reads as that golfer's own `flighthook.*` id); absent when no golfer was up.
- `club_selected`: the club in play when the shot was triggered (`PT` for an
  ExPutt shot); absent when unknown.
- `settings.detection_mode`, `settings.handed`: the turn's mode and hand when the shot was
  triggered; absent when unknown.
- `block`: id of the practice [block](#get-apiblocks) the shot joined (a UUID); absent when the shot has no data.
- `settings.device`: the device's config section with `name`, `address`,
  `bind`, unset fields and device driver settings at their default removed. `settings.simulation`: the `[simulation]`
  section, with the same shape as in [GET /api/settings](#get-apisettings)
  (`mode` present unless `"on"`). A launch whose spin flighthook estimated for a launch-only chip shows it in
  the `assumed` of the launch monitor's own flight (`data.flights`).
- `data`: the `/api/shots` element for this shot (`ShotData`), with every flighthook actor (`data.from`, each
  `data.flights[].from`) named by its card key instead of its actor id (a remote launch monitor keeps its actor id),
  including `metrics`, `source`, `turn`, `flights`, `path` and `stroke`. CSV has no stroke
  columns. `data.shot_id` and `data.block`, when set, equal the
  top-level fields, `data.turn.handedness` equals `settings.handed`, and `data.golfer_name`, `data.from_name` and
  `data.video` are never present.
- `raw`: `ShotRaw` (omitted when absent or with `raw=false`), the device frames captured for the shot (see
  [ShotRaw](#shotraw)).

#### ShotRaw

`raw` is the device frames captured for this shot (omitted when none):

- `transport`: `flightscope-tcp`, `square-ble`, `r10-ble`, `exputt-wifi`
  (`text` records: the session's messages as JSON text),
  `openconnect-json`, or `none` (mock).
- `frames`: `RawRecord[]`, in arrival order:
  - `t_ms`: milliseconds relative to the shot trigger (negative = before it)
  - `dir`: `rx` (device → flighthook) or `tx` (flighthook → device)
  - `label`: the device driver's own name for the frame, stored as it gave it, one record per protocol message. A
    schema 2 export carries `type`, `src`, `dst` and `channel` instead.
  - `hex`: payload as lowercase hex (binary protocols)
  - `text`: payload text (JSON protocols)
- `dropped`: frames over the per-shot cap that were not kept (omitted when 0)
- `unavailable`: why `frames` is empty or incomplete, e.g. `"mock device
  has no wire protocol"`, `"evicted: session raw budget"`

Raw frames are not sent on `/frp` (`shot.raw` is never relayed) or on `/api/ws`; the export is where they are.


**CSV**: one header row, one row per shot, RFC 4180 quoting, empty cell for a
missing value. Units are fixed per column and named in the header (imperial:
mph, yd, ft for heights, in for club offset/height/low point; metric: mps,
m). Face impact is always mm. `environment` is the FlightScope environment in
effect (configured value, else the default). `block` is the set id (empty when
the shot has no data). Unprefixed columns are the device's values (`launch`,
the monitor's own flight, `club`, `impact`); `sim_*` columns are the flight model's result and
`metrics_*` columns the [shot metrics](#shot-metrics). With `[simulation]
mode = "force"`, a shot can have both a device column (`carry_*`) and its
`sim_*` column filled; the CSV does not say which one takes precedence. `spin_estimated` is `true`
when `backspin_rpm` and `sidespin_rpm` are flighthook's estimate (the launch monitor's flight `assumed` them), else
empty. When any exported shot has a golfer, a last column `golfer_id` holds it. Read columns by header name:
columns are added over time.

```
time,actor,device_kind,manufacturer,model,firmware,shot_number,shot_id,
club_selected,block,handed,detection_mode,environment,
ball_speed_mph,launch_elevation_deg,launch_azimuth_deg,backspin_rpm,sidespin_rpm,
total_spin_rpm,spin_axis_deg,
carry_yd,carry_offline_yd,total_yd,total_offline_yd,roll_yd,max_height_ft,
flight_time_s,descent_angle_deg,
sim_carry_yd,sim_carry_offline_yd,sim_max_height_ft,sim_flight_time_s,
sim_descent_angle_deg,sim_roll_yd,sim_roll_offline_yd,sim_total_yd,sim_total_offline_yd,
club_speed_mph,club_speed_post_mph,path_deg,attack_angle_deg,face_angle_deg,
dynamic_loft_deg,smash_factor,swing_plane_horizontal_deg,swing_plane_vertical_deg,
club_offset_in,club_height_in,spin_loft_deg,low_point_distance_in,
impact_lateral_mm,impact_vertical_mm,
metrics_total_spin_rpm,metrics_spin_axis_deg,metrics_face_to_path_deg,
metrics_spin_loft_deg,metrics_curve_yd,spin_estimated[,golfer_id]
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

Stateless unit conversion utility. Accepts a `ShotData`
body (the shape `GET /api/shots` returns) and returns it with all
distance and velocity fields converted to the requested unit system.

This saves external integrations (Python, C#, JS, etc.) from having to parse
unit-tagged strings and re-implement conversion math.

**Query params**:

- `units` (required): `"imperial"` or `"metric"`

**Request** -- `ShotData` (as `/api/shots` serves it):

```json
{
  "shot_number": 42,
  "from": "5b80e2f7d14c9a63",
  "launch": {
    "launch_speed": "67.2mps",
    "launch_azimuth": -1.3,
    "launch_elevation": 14.2,
    "backspin_rpm": 3200,
    "sidespin_rpm": -450
  },
  "flights": [
    { "from": "5b80e2f7d14c9a63",
      "flight": { "basis": "modeled", "carry_distance": "180.5m", "max_height": "28.3m", "total_distance": "195.0m" } }
  ],
  "club": {
    "club_speed": "42.1mps",
    "path": -2.1,
    "attack_angle": -3.5,
    "face_angle": 1.2,
    "dynamic_loft": 18.4,
    "smash_factor": 1.42
  }
}
```

**Response** `200 OK` -- `ShotData` with converted units:

```json
{
  "shot_number": 42,
  "from": "5b80e2f7d14c9a63",
  "launch": {
    "launch_speed": "150.3mph",
    "launch_azimuth": -1.3,
    "launch_elevation": 14.2,
    "backspin_rpm": 3200,
    "sidespin_rpm": -450
  },
  "flights": [
    { "from": "5b80e2f7d14c9a63",
      "flight": { "basis": "modeled", "carry_distance": "197.4yd", "max_height": "92.8ft", "total_distance": "213.3yd" } }
  ],
  "club": {
    "club_speed": "94.2mph",
    "path": -2.1,
    "attack_angle": -3.5,
    "face_angle": 1.2,
    "dynamic_loft": 18.4,
    "smash_factor": 1.42
  }
}
```

Angles (degrees) and spin (RPM) pass through unchanged, and so does `path` (always meters and seconds). Every flight
in `flights` is converted the same way (distances and offlines to yards or meters, `max_height` to feet or meters;
`flight_time` and `descent_angle` unchanged). `video` is dropped.

**Errors**:

- `400 Bad Request`: `units` param missing or not `"imperial"`/`"metric"`

---

### POST /api/find-mat

Have an ExPutt locate its mat again, e.g. after it locked onto the wrong spot
because someone stood in its view when it connected. The ExPutt card's Find
mat button sends it.

**Request**:

```json
{ "actor": "exputt.0" }
```

- `actor`: the ExPutt's actor ID.

**Response**: `202 Accepted` (no body). `404 Not Found` with a text body
(`no ExPutt with ID 'exputt.1'`) when no ExPutt has that ID.

A connected ExPutt then reports `ready` false and `device_state`
`finding mat`, `mat found` and `ready` as the device works. An ExPutt acts on
it once connected (one sent while it connects runs once the session is up);
one sent while it waits to reconnect is dropped.

---

### POST /api/actors/{id}/reconnect

Stop a launch monitor or Software card, wait for its device or connection to close,
and start it again from the saved settings: what a flighthook restart does for
that one device. Every other actor, the shot history, the sets and the game
state are left alone. The Reconnect icon on each launch monitor's and
Software card's Settings page sends it.

`id` is the actor ID (`mevo.0`, `square.0`, `openconnect_server.0`,
`openconnect_client.1`, ...); `gspro.N` names `openconnect_client.N`. No
request body.

**Response** `200 OK` once the new instance has started:

```json
{ "restarted": ["mevo.0"] }
```

The card then shows the usual statuses as it connects (`starting`, then
`connected`).

`202 Accepted` with an empty body `{}` when the actor starts again by
itself later:

- its old connection has not closed in time, or is still closing from an
  earlier request (see below); the actor's `error` telemetry says
  `"The previous connection has not closed yet. It reconnects when it does."`;
- another save or reconnect is restarting it already;
- SystemActor has not finished within 22 s (see
  [config timeout](#config-timeout)). Until it has, the actor counts as in
  flight, so another request for it is `409`.

It has started again when the card reports `starting`, or a
`config_outcome` event on [`/api/ws`](#websocket-apiws) lists it in `restarted` (the one without `request_id`
included). The dashboard keeps the card's Reconnect disabled until then, for
up to a minute.

**Errors**: a JSON body `{"error": "<reason>"}`.

| Status | When |
|---|---|
| `400 Bad Request` | `id` is a camera, the web server or another actor that is not a launch monitor or output |
| `404 Not Found` | no launch monitor or output in the saved settings has that ID |
| `409 Conflict` | a reconnect of the same actor is already in flight (`"a reconnect of 'mevo.0' is already in progress"`) |
| `409 Conflict` | the actor is turned off (`"'mevo.0' is off"`) |
| `409 Conflict` | the actor was removed, or its saved settings cannot start it, after the request was checked |
| `504 Gateway Timeout` | flighthook is shutting down: the command was not processed |

The reply is decided when the command's outcome arrives, from the actor's state
at that moment. A request that finds another save or reconnect restarting the
actor is `202`; if that other command finishes in the instant between the
refusal and the reply (under 50 ms, and only for an actor that closes at once),
the reply is `409` with `"'mevo.0' was not reconnected"` although the actor has
restarted. A `409` of that kind is settled by the actor's status. The
dashboard cannot hit it: it disables Reconnect while a save is out.

How long the old connection is given to close depends on the device: up to
3 s, 6 s for a Garmin R10 or a Square Golf, 18 s for an ExPutt (a session start in progress
cannot be cut short), 1 s for a Trackball. A Mevo/Xi, an Open Connect client,
a mock or a Trackball still
closing after that is replaced anyway (it holds nothing the new one needs and
closes itself); so is an Open Connect server once its port is free. A Garmin
R10, a Square Golf or an ExPutt is not: the request answers `202`, the
actor's `error` telemetry and an Error alert say the previous connection has
not closed yet, and the actor starts again by itself once it has. Its `error` is
cleared then, and a `config_outcome` without `request_id` lists it in
`restarted`. Pressing Reconnect again meanwhile starts nothing.

An Open Connect client that is reconnected starts its `ShotNumber` count
over, as after a flighthook restart.

**Side effects**: the system actor stops and starts the card, then announces a `config_outcome` (see
[`/api/ws`](#websocket-apiws)) listing it in `restarted`, or with `restarted` empty when it did not start it.

---

### POST /api/actors/{id}/forget-pairing

Delete a Pureplay Accessory card's saved pairing, then reconnect the card as
[`POST /api/actors/{id}/reconnect`](#post-apiactorsidreconnect) does, so it
asks Pureplay to pair again: Pureplay shows its approval prompt. The Forget
pairing icon on a Pureplay Accessory card's Settings page sends it.

`id` is a Pureplay Accessory card's actor ID (`pureplay_accessory.0`). No request
body. The pairing deleted is the one saved for the card's address.

**Response**: as for a reconnect, once the pairing is deleted: `200 OK` with
`{ "restarted": ["pureplay_accessory.0"] }`, `202 Accepted`, or a reconnect's
`409` (a card that is turned off loses its pairing and is not started).

**Errors**: a JSON body `{"error": "<reason>"}`.

| Status | When |
|---|---|
| `404 Not Found` | no Pureplay Accessory card in the saved settings has that ID (any other card type included) |
| `409 Conflict` | a reconnect of the card is already in progress (`"a reconnect of 'pureplay_accessory.0' is already in progress"`); the pairing is kept |
| `500 Internal Server Error` | the pairing file could not be written (`"couldn't delete the Pureplay pairing: ..."`) |

Pureplay keeps its own entry for the old pairing until the player removes it
in Pureplay's Settings > Accessories.

---

### GET /api/settings

Full persisted config (mirrors `config.toml`).

**Response** `200 OK` -- `FlighthookConfig`:

```json
{
  "default_units": "imperial",
  "frp": { "actor_id": "c19e04b7a2d5f318", "auto_chip_actor_id": "e4a7136b9f02cd58" },
  "selected_golfer": "flighthook.0f8c2d1a-77b4-4e1e-9a50-3c6d2b8e4f10",
  "shot_grouping": "by_club",
  "simulation": {
    "actor_id": "7d3b90e1c4a2f685",
    "mode": "force",
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
      "actor_id": "5b80e2f7d14c9a63",
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
      "address": "SquareGolf(CD01)",
      "club": "PW",
      "dot_bottom_to_face_center_mm": {
        "DR": 15.0,
        "7I": 18.5
      }
    }
  },
  "exputt": {
    "0": {
      "name": "ExPutt"
    }
  },
  "trackball": {
    "0": {
      "name": "Trackball",
      "device": "usb-Example_Trackball-event-mouse",
      "tuning": {
        "full_power_speed": 40.0
      }
    }
  },
  "openconnect_server": {
    "0": {
      "name": "Open Connect Server",
      "bind": "0.0.0.0:921"
    }
  },
  "openconnect_client": {
    "0": {
      "actor_id": "a2f9c6d03e7b1485",
      "name": "GSPro",
      "address": "127.0.0.1:921",
      "full_monitor": "5b80e2f7d14c9a63",
      "chipping_monitor": null,
      "putting_monitor": null,
      "chipping_clubs": ["GW", "SW", "LW"],
      "putting_clubs": ["PT"]
    }
  },
  "replay": {
    "keep_shots": 50,
    "pre_roll_s": 4.0,
    "post_roll_s": 1.5,
    "start_before_impact_s": 2.0
  },
  "body_tracking": {
    "model": "rtmpose-s",
    "detect_every": 5
  },
  "golfers": [
    {
      "identifiers": ["flighthook.0f8c2d1a-77b4-4e1e-9a50-3c6d2b8e4f10", "pureplay.thill"],
      "name": "Sam",
      "handed": "lh",
      "bag": ["DR", "3W", "5H", "6I", "7I", "8I", "9I", "PW", "SW", "PT"],
      "sex": "female",
      "height": 168.0
    }
  ],
  "camera": {
    "0": {
      "name": "Golfer Front",
      "camera": "/dev/v4l/by-id/usb-046d_HD_Pro_Webcam_C920_8A2B3C4D-video-index0",
      "camera_name": "HD Pro Webcam C920",
      "position": "golfer_front",
      "mode": "auto",
      "rotation_deg": 90,
      "yaw_deg": -4.0
    }
  }
}
```

- `frp` holds flighthook's own FRP actor ids (`actor_id` for the flighthook actor, `auto_chip_actor_id` for
  Auto-chip), generated once and kept so the actors stay the same across restarts. Every launch monitor, output and
  software section has an `actor_id` of its own the same way. They identify actors, never the machine or the
  person. Omitted while empty
- `selected_golfer` is the `flighthook.*` id of the golfer last picked in flighthook, the golfer up at start. Omitted
  while unset
- `golfers` (beta) lists the golfers in display order. Each has `identifiers` (every golfer id that is this golfer:
  its own `flighthook.<uuid>`, written when the golfer is created, then any game ids linked to it, such as
  `pureplay.thill`; an id belongs to one golfer only), `name`, and optional `handed` (`"rh"` | `"lh"`), `bag` (club
  codes in display order), `sex` (`"male"` | `"female"`: whose tour averages the swing analysis is compared with) and
  `height` (centimeters, 100 to 250). A save with a golfer that has no name, an identifier that is not a golfer id, an
  id in two golfers or a height out of range is `400`. Saving with `?scope=golfers` returns the golfers as saved
  (see below). Omitted while empty
- Keys are type-prefixed global IDs: `mevo.0`, `r10.0`, `square.0`, `exputt.0`, `trackball.0`, `mock_monitor.0`,
  `openconnect_server.0`, `openconnect_client.0`, `pureplay.0`, `pureplay_accessory.0`, `random_club.0`, `webserver.0`,
  `camera.0`
- `shot_grouping`: the Shots tab's default view, [`ShotGrouping`](#shotgrouping),
  `"by_set"` by default. It does not change how
  [sets](#get-apiblocks) are assigned. Omitted while at the default
- `simulation` (beta) holds the settings for simulated values: `mode`
  ([`SimMode`](#simmode), default `"on"`, omitted while `"on"`), `altitude`
  (unit-tagged distance, default `"0ft"`, valid 0 to 5000 m), `temperature_c`
  (default `25.0`, valid -30 to 50) and `surface` ([`SimSurface`](#simsurface),
  default `"fairway"`) and `auto_chip` (boolean, default `false`, omitted
  while `false`): whether the dashboard offers [Auto-chip](#post-apiauto-chip)
  and `POST /api/auto-chip` accepts a chip. Off by default; turn it on in
  Settings → General → Simulation (useful with radar launch monitors that miss short chips).
  `tournament` (boolean, default `false`, omitted while `false`): whether
  tournament mode is on (see [`POST /api/tournament`](#post-apitournament)); read
  at start-up and written only by that route (a settings save keeps the live
  value). With `"mode": "force"`, every simulated shot's results
  (carry, apex, flight time, descent, offline, roll, total) come from the
  simulation for every device, carried in the flight model's flight; the device's own results stay in the launch
  monitor's own flight.
  Launch values are never replaced. `simulation` carries the flight model's `actor_id`; its other keys are
  **omitted while at their defaults**.
  Every key is optional when posting. An out-of-range altitude or temperature
  turns simulation off until it is fixed. Changes apply from the next shot
  without restarting any actor
- All launch monitor config fields are optional (omitted = use defaults)
- R10 sections have `name`, an optional `aim_offset_deg` (beta; degrees, -10
  to 10, added to the launch direction of every shot and to the face angle
  and path of a tracked club; + = right; omitted = 0; `400`
  `"r10.0: aim_offset_deg: must be -10 to 10 degrees"` outside the range;
  applied from the next shot, no restart) and an optional `range`: the distance from the
  device to the ball (unit-tagged, e.g. `"7ft"`), sent to the R10 as its tee
  distance once it wakes up; when absent the device keeps the distance it
  has. No address (BLE auto-discovery). `range` is the R10 driver's setting:
  it must be greater than 0 (a save with `"0ft"` or a negative distance is
  `400` `"r10.0: range: must be a distance greater than 0"`; a file holding
  one loads, and that card does not start and shows the error). A changed
  range restarts the card, a changed aim offset does not
- Square sections have `name` and the Square Golf driver's settings, each
  left out of responses and saved files while at its default: `address`
  (the device's advertised name, e.g. `"SquareGolf(CD01)"`, its BLE address
  or on macOS its peripheral UUID; absent or blank finds the first Square
  Golf device), `club` (the club armed on connect until the game names one,
  default `"7I"`), `advanced_spin` (default `true`),
  `discard_non_putting_zero_spin` (drop a shot that reads no spin at all
  unless the putter is in play, default `true`) and
  `dot_bottom_to_face_center_mm` (below). The driver refuses no value it can
  read: an unknown club, an unknown calibration key or a negative
  calibration is skipped with a warning in the log when the device
  connects; a value of the wrong type is an invalid body (`4xx`), and in the
  config file a load error naming the key's line. Any change of these
  settings restarts the card
- Mevo (FlightScope) sections: `name`, `enabled`, `impact_lead_s` and
  `launch_only_spin` are the card's; `address` (`ip:port`, default
  `192.168.2.1:5100`), `ball_type` (0 standard, 1 RCT, default 1),
  `tee_height` (default `1.5in`), `range` (default `8ft`), `surface_height`
  (default `0in`), `track_pct` (0 to 100, default 80) and the keys below are
  the FlightScope driver's settings; each is omitted while unset (the
  driver's default applies), never `null`. The driver refuses a `track_pct`
  outside 0 to 100, a negative distance, a `ball_type` other than 0 or 1 and
  an `address` that is not an IP address and port: a save that changes one
  into such a value is `400` (`"mevo.0: track_pct: must be 0 to 100"`), and
  a config file holding one loads, names it and keeps that card from
  starting. `spin_source` and `launch_only_spin` apply live; any other change
  of the driver's settings restarts the card
- `use_estimated` on Mevo sections controls whether estimated ball flights
  are emitted when no full result arrives (defaults to `false`). With it
  off, a shot the device only estimated (e.g. a flight too short to read in
  Outdoor mode, or a very short chip) is skipped
- `camera_mode` on Mevo sections is `standard` (default), `fusion`, or
  `raw_fusion`. The Fusion modes request the device's club-data processing and
  are omitted from the response when unset
- `environment` on Mevo sections is `limited_flight` (default; for nets and
  simulator screens) or `outdoor` (full flight). It sets how the device detects
  a full swing (Mevo+ and Xi). Omitted from the
  response when unset
- `spin_source` on Mevo sections is `d4` (default), `e8`, or `auto`, and
  applies to a FlightScope Xi only. `d4` reports the Xi's final spin, which is
  what FS Golf displays. `e8` reports the Xi's second spin value, split by
  the spin axis from `d4` when the shot has club data, and as the total
  alone (no axis, no back/side components) otherwise, and falls back to
  `d4` when the shot has no E8 spin; on chips and pitches `e8` isn't
  measured, so choose it knowingly. `auto` is `e8` when
  flighthook is in Full mode, and `d4` otherwise -- a chip hit while flighthook
  is still in Full mode gets `e8`'s speed-scaled value. See
  [Spin on the Xi](devices/flightscope.md#spin-on-the-xi). Omitted while at the
  default. Changes apply from the next shot without restarting the actor
- `launch_only_spin` on Mevo sections is the backspin, in rpm per mph of ball
  speed, given to a full result with launch data but no spin and no flight
  (very short chips on the Xi), with sidespin 0. Default `100`, from `0` to
  `1000`; `0` turns it off, and such a shot is then sent as the device
  reported it (spin 0, carry, apex and flight time 0, rolled as a ground
  ball), as it also is in Putting mode. A save with a value below 0, above
  1000 or not finite is rejected with `400`; one outside that range in the
  config file is treated as `0`. See
  [Launch-only chips](devices/flightscope.md#launch-only-chips-beta). Omitted
  while at the default. Changes apply from the next shot without restarting
  the actor
- ExPutt sections have `name` and an optional `address`, the ExPutt
  driver's setting: the device's IP address (no port). When absent or blank
  the device is found on the local network. Omitted while unset. A save that
  changes it into anything else (a host name, an address with a port) is
  rejected with `400` (`"exputt.0: address: \"exputt.local\" is not an IP
  address; leave it blank to find the device on the network"`); a config
  file holding one loads, and that card does not start and shows the error.
  A change restarts the card. The device must be in its mobile mode on the same network. Its shots are putts: club
  `PT` and Putting mode whatever club is in play, vertical launch 0, no spin
- `trackball` sections (alpha) have `name` and the trackball driver's
  settings, each omitted while at its default: `device` (the input to read:
  a `/dev/input/by-id` name, a `/dev/input` path or the name the device
  reports; absent or blank uses the only readable trackball or mouse),
  `club` (the club in play until the game names one, an FRP club code,
  default `"7I"`), `cpi` (counts per inch of ball travel, default `400.0`),
  `report_limit` (the largest movement one report carries, default `127`), a
  `thresholds` table (`speed_window_ms`, `gap_ms`, `v_start`, `v_stop`,
  `v_min_swing`, `dwell_min_ms`, `back_min`, `full_cock`, `fwd_min`,
  `back_max_s`, `coil_timeout_s`, `fwd_window_ms`, `idle_settle_ms`,
  `cooldown_max_s`, `pull_cone_deg`: how swings are found) and a `tuning`
  table (`full_power_speed`, `max_club_speed`, `max_putt_speed`,
  `angle_gain`, `putt_angle_gain`, `curve_gain`, `curve_sense` (`"arcade"` |
  `"physical"`), `backswing_power`, `power_gamma`, `mirror_for_left_hand`,
  `face_max`, `face_to_path_max`: how a swing becomes a shot); see
  [Trackball](devices/trackball.md). A save that changes one into a value
  the driver refuses is rejected with `400` naming the key
  (`"trackball.0: cpi: must be greater than 0"`, `"trackball.0:
  thresholds.v_stop: must be below thresholds.v_start (0.067)"`,
  `"trackball.0: tuning.backswing_power: must be 0 to 1"`); a config file
  holding one loads, and that card does not start and shows the error. A
  change of `device`, `cpi` or `report_limit` restarts the card; the rest
  applies to the next swing. The card never grabs the trackball, so the
  desktop pointer keeps following the ball: a `grab` or `hand` key is read without an
  error, ignored, and left out of the next save (the hand is the golfer up's, right-handed until one is chosen). No `impact_lead_s` (a value written there
  is ignored): the cameras cut no clip for a Trackball shot
- `mock_monitor` sections have `name` and the mock launch monitor's
  settings, each omitted while at its default: `shot_interval_s` (30, the
  seconds from the ball being ready to the shot), `arm_delay_s` (2, from
  connecting or a shot's end to armed), `ball_ready_delay_s` (1, from armed
  to the ball being ready), `post_shot_delay_s` (1, from a shot to
  re-arming), each 0 to 86400, and `serial` (the serial the mock reports,
  `0001` when absent). A save with a delay out of range or not finite is
  rejected with `400`; a config file holding one loads, and that card does
  not start and shows the error. A change restarts the card
- Every launch monitor, camera and output section (`mevo`, `r10`, `square`,
  `exputt`, `trackball`, `mock_monitor`, `openconnect_server`,
  `pureplay_accessory`, `camera`, `openconnect_client`, `pureplay`,
  `random_club`) takes `enabled`: `true`
  (the default) runs its actor, `false` turns the card off, keeping its
  section and settings. Omitted while `true`, so a config written before the
  field existed is unchanged; only `enabled = false` is written. Web server
  sections have no `enabled`. See [Turning a card off](#turning-a-card-off)
- Every launch monitor section (`mevo`, `r10`, `square`, `exputt`,
  `mock_monitor`, `openconnect_server`, `pureplay_accessory`) takes an
  optional `impact_lead_s`:
  seconds, 0 to 5, from the ball's impact to the monitor's `shot.trigger`,
  used to place the cameras' clips (beta). Omitted when unset, which uses the
  device's default (FlightScope, ExPutt and mock 0, the Pureplay Accessory
  card 0.9, Square 1, R10 2, Open Connect 2.5, any other 1.5). The ExPutt dates its trigger at the strike,
  so its timing counts from the strike, not from the putt's report. A save with a value below 0, above 5 or not
  finite is rejected with `400`; one outside that range in the config file is
  ignored. Changes apply from the next shot without restarting anything
- `camera` sections take an optional `latency_s`: seconds, 0 to 10, a frame is
  old when it reaches flighthook, for a camera that delivers late such as a
  buffered network stream (beta). Every frame is dated that much earlier than it
  arrived, which lines the clips up with the shot and with the other cameras.
  Omitted when unset (0). A save with a value below 0, above 10 or not finite
  is rejected with `400`; one outside that range in the config file is
  ignored. Changes apply at once without reopening the camera
- `camera` sections take an optional `rotation_deg`: 0, 90, 180 or 270, the
  clockwise quarter turns applied to the camera's frames before body
  tracking and display (clips are stored as the camera sent them). Omitted
  at 0. A save with any other value is rejected with `400`; one in the config
  file is not used (0, with a warning in the log). Changes apply at once
  without reopening the camera: the live view and the clips tracked from
  then on use the new rotation
- `camera` sections take optional `yaw_deg` and `pitch_deg`: degrees, -30 to
  30. `yaw_deg` is how far the camera points off square, positive to the
  right of the target line for a right-handed golfer; `pitch_deg` is its
  tilt down from level. Omitted when unset (0). A save with a value outside
  that range or not finite is rejected with `400`; one in the config file is
  not used. Changes apply at once without reopening the camera
- `dot_bottom_to_face_center_mm` on Square sections overrides allsquare's built-in
  per-club face impact calibration (distance from the bottom edge of the
  sticker's dot down to face center, in mm). Keyed the same way `club` is. Omitted when no
  overrides are set. The spelling `dot_bottom_to_face_centre_mm` is also read (in requests and
  config files); responses and saved files use `dot_bottom_to_face_center_mm`
- `openconnect_server` sections are **launch monitors**, not integrations: they
  listen (`bind`, default `0.0.0.0:921`) and accept inbound shots from
  launch monitor software that sends GSPro Open Connect V1 as a client. This
  is the inverse of `openconnect_client`, which dials a
  simulator. GSPro listens on 921 too, but moving GSPConnect to 922 (set
  `<OpenAPIUseAltPort>true</OpenAPIUseAltPort>` in
  `C:\GSPro\GSPC\GSPconnect.exe.config`) frees 921 so both can share one host
- `openconnect_server` sections answer each message with a `200` and tell the
  monitor the player's hand and club as GSPro's player information (`201`,
  `Handed` `RH`/`LH`, `Club` GSPro's code such as `W3`, `I7`, `PT`) once the
  club is known: after the monitor's first message and on every change. `Handed` is always sent (`RH` until a
  hand is chosen), and the mini driver goes out as `DR`
- `openconnect_client` sections are **outputs**: each dials `address` (default
  `127.0.0.1:921`), a simulator that accepts GSPro Open Connect V1 — GSPro
  and Pureplay on 921, Muni Golf on 9210 — and sends it every shot. `full_monitor`,
  `chipping_monitor` and `putting_monitor` take the `actor_id` of a launch monitor's section (the value in that
  card's section of this response, not its key `mevo.0`) and accept shots in that mode from it alone; absent accepts
  any monitor. A card key is read only from the config file, where flighthook replaces it with the card's `actor_id`
  at the next start; a key posted here routes nothing until then. `chipping_clubs` and `putting_clubs`
  (club codes, default `["GW","SW","LW"]` and `["PT"]`) are the clubs the simulator holds for a chip and a putt:
  with either list not empty, the card sends a mode with the club it sends (a putting club: putting, a chipping club:
  chipping, any other: full) and sends it again when the turn's mode differs; both empty: the card sends no mode.
  [Auto-chip](#post-apiauto-chip) shots are accepted whatever they name.
  A Pureplay Accessory card (`pureplay_accessory.N`) never feeds an output, under
  "any" or by ID: its shots and readiness are the simulator's own. A save
  that names one is rejected with `400` (`"An output can't take shots from
  Pureplay"`); a config file that names one loads with a warning, and that
  route passes nothing.
  Each shot's `ShotNumber` is its `shot_number`, raised when needed to one
  more than the last one the client sent, so the numbers a simulator gets
  always increase, across devices, Auto-chip shots and device reconnects.
  The count starts over when the client itself restarts: a
  [Reconnect](#post-apiactorsidreconnect) of it, a save that restarts it, or
  a flighthook restart
- `pureplay` sections (alpha) are **outputs**: each dials `address` (default
  `127.0.0.1:9760`), Pureplay, and sends it every shot over Pureplay
  Connect, numbered from 1 on each connection. `full_monitor`,
  `chipping_monitor` and `putting_monitor` route as for `openconnect_client`
  (a Pureplay Accessory card never feeds it),
  and also pick the launch monitor whose ready state, connection and battery
  Pureplay is told. Pureplay's club and hand commands set flighthook's club
  and hand (a `golfer.select` from the card), and the mode follows the club and the shot distance Pureplay gives
  (putter: putting; under 30 yards: chipping; else full). See [Pureplay](outputs/pureplay.md)
- `pureplay_accessory` sections (beta) are **Pureplay Accessory cards**, under
  Software: each dials `address` (default `127.0.0.1:9770`), Pureplay's
  Accessory API, pairs with it, and follows the round: the card is a `game`
  actor that publishes the round, its players as golfers, the scores and each stroke
  (`game.round`, `golfer.profile`, `game.scores`, `game.stroke`). Pureplay's club, hand
  and player for the player who is up at that computer set
  flighthook's turn (a `golfer.select` from the card), a club
  picked in flighthook sets Pureplay's when the pairing allows it (the
  card's `club_out` row), a shot
  of a player at that computer becomes a flighthook shot, and Pureplay's
  result for a shot arrives as the shot's `stroke`. `shots`
  (omitted at the default) picks which shots the card takes: `auto` (the
  default: every one that is not the return of a shot an output sent to a
  simulator within the last 5 s), `from_pureplay` (every one) or
  `from_flighthook` (none; the results still reach the shots an output
  sent). `impact_lead_s` as for every launch monitor (default 0.9). The
  card's settings take effect without restarting it; an `address` change
  restarts it and pairs again. The pairing token is in
  `pureplay-tokens.json` in flighthook's data folder, never in the config.
  See [Pureplay Accessory](devices/pureplay.md)
- `replay` (beta) holds the camera clip settings: `keep_shots` (default
  `20`, 1 to 1000: clip folders kept on disk, newest first; a swing two
  launch monitors saw is one clip), `pre_roll_s` (default `4.0`) and
  `post_roll_s` (default `1.5`), each 0 to 10 seconds, `start_before_impact_s`
  (default `2.0`, 0 to 10 seconds: where a replay opens and restarts, that
  long before the estimated impact), `shot_cards` (default `true`; `false`:
  the UI's Cards view shows no camera cards, clips are still recorded; written
  only when `false`), and an optional
  `clip_dir` (the folder clips go to; see `clip_dir` in
  [`GET /api/status`](#get-apistatus)). Omitted while at its defaults. The
  rolls apply from the next shot without restarting any camera, the start
  from the next replay opened; a lower
  `keep_shots` deletes the oldest clips when the next clip finishes. A save
  with a value out of range is `400`; a file holding one loads with a
  warning in the log, and the value is held to its range. A new `clip_dir`
  applies to the next clip: the clips in the old folder stay on disk as
  they are, and only the new folder's clips are listed (a
  [`shot_video_removed`](#local) names the shots that lost
  theirs)
- `body_tracking` (alpha) holds the body tracking settings, each omitted
  while at its default, and the section with them: `mode` (`"off"`, the
  default: no body tracking, clips only; `"historical"`: a clip is tracked
  the first time it is viewed, see
  [`POST .../analyze`](#post-apishotsshot_idvideocameraanalyze); `"live"`:
  the live views while watched and every clip as it is recorded), `model`
  (`"rtmpose-m"` default, or `"rtmpose-s"`), `min_score`
  (default `0.3`, 0 to 1: keypoints scoring below it are not drawn),
  `smoothing` (default `1.0`: the live One Euro filter's cutoff at rest in
  Hz, 0.1 to 10, lower is smoother; `0` is off) and `detect_every` (default
  `10`, 1 to 60: the live view looks for the golfer afresh at least every
  that many frames) and `show_skeleton` (default `true`; `false`: the UI
  draws no skeleton over the live views, the clips or the replay). Every
  value
  applies at once, without restarting a
  camera. A save with a value out of range is `400`; a file holding one
  loads with a warning in the log, and the default is used. An unknown
  `mode` is an invalid body, like an unknown `model`. A section with
  `enabled` instead of `mode` (a config from before the modes) reads
  `false` as `"off"` and `true` as `"historical"`, and one with neither as
  `"off"`; responses carry `mode` only. Saving a mode other than `"off"`
  downloads nothing: the models are downloaded only through
  [`POST /api/body_tracking/models/download`](#post-apibody_trackingmodelsdownload).
- `camera` sections (beta) are swing cameras: `name`; `camera`, the stable id
  from [`GET /api/cameras`](#get-apicameras) (`test-pattern` for the test
  picture); `camera_name`, the camera's own name when it was picked, used
  when the id is not connected (a camera moved to another USB port);
  `position` ([`CameraPosition`](#cameraposition), default `golfer_front`); and
  `mode`, `auto` (the highest frame rate, then the largest size) or
  `WIDTHxHEIGHT@FPS` (`1280x720@120`). Changing the camera or the mode
  restarts the actor; a name or position change applies at once
- `gspro` sections are also accepted when posting and read as
  `openconnect_client` sections with the same index; when both name an index,
  the `openconnect_client` one is used. Responses carry only
  `openconnect_client`

---

### POST /api/settings

Config update. Sends the change to SystemActor, which saves the config and reconciles the actors, and
returns the response once it has applied it.

**Request**: complete `FlighthookConfig` JSON (same shape as GET response)

**Query parameters**:

- `scope` (optional): a global actor ID (`mevo.0`, `openconnect_client.1`,
  `pureplay.0`, `pureplay_accessory.0`, `webserver.0`), `global`, `body_tracking` or `golfers`. `gspro.N`
  is accepted for `openconnect_client.N`.
  - An actor ID narrows the save to that one actor — only its section is
    written to the config, and only that actor is reconciled. Every other
    section of the live config is left as-is, whatever the request body
    carries for it. A `scope` naming a section the body omits removes that
    actor.
  - `global` writes only the global settings (`default_units`,
    `shot_grouping`, `simulation`, `replay`, `body_tracking`) from the body; every actor section of the live config is kept, whatever the body
    carries for it. Every actor is reconciled.
  - `body_tracking` writes only the body's `body_tracking` section; every
    other setting of the live config is kept, whatever the body carries for
    it. No actor is reconciled (body tracking is read live). The UI saves
    this way from the model download prompt: when body tracking is turned on
    from `off`, and when the prompt's Cancel or Turn off sets it back to `off`.
  - `golfers` writes only the body's `golfers`; every other setting of the live config is kept. No actor is
    reconciled. The response lists the golfers as saved, in the order posted, each with its own `flighthook.*` id (a
    new golfer's id is given by the save).
  - Any save that changes `body_tracking.mode` applies it at once: leaving
    `live` stops live tracking and the recorded clips still waiting to be
    tracked (a clip being tracked finishes); `off` also drops every clip
    waiting, releases the models and stops a model download.
  - An unparseable ID or unknown type prefix is rejected with `400`: nothing
    is saved.
  - flighthook runs one web server. A save that removes a `webserver.N`
    section (a `webserver.N` scope with the section left out, or a
    whole-config body without one the config holds) is rejected with `400`,
    as is one that adds a `webserver.N` while the config already holds one.
    A config that already holds several keeps them, each editable.
  - Without `scope`, the body replaces the whole config and every actor is
    reconciled.

**Response** `200 OK`:

```json
{
  "restarted": ["mevo.0"],
  "stopped": ["openconnect_client.1"]
}
```

- `restarted`: actors that were stopped and recreated (e.g. address changed).
  Omitted when empty.
- `stopped`: actors that were removed from the config or turned off.
  Omitted when empty.
- `golfers`: only for `?scope=golfers`: the golfers as saved.

#### Turning a card off

A section with `enabled = false` runs no actor; its card stays, with its
settings, and its [state](#connectionstate) is `disabled`. A save that turns
a running actor off stops it by the same rules as a removal (see
[config timeout](#config-timeout)) and lists it in `stopped`; one that turns
it on starts it like a new actor and lists it in `started` of the
`config_outcome`. Changing other settings of a card that is off starts
nothing. The dashboard's on/off switch in each card header saves this way:
a `scope` of that actor, with only its `enabled` changed.

One card per address may be on. Two cards of a type conflict when their
addresses collide: the same host and port (`localhost` is `127.0.0.1`, a blank
address is the type's default), or, for an Open Connect server, the same port
with overlapping bind addresses (`0.0.0.0` overlaps all). A Trackball's
address is the `device` it names, compared as text; a blank one is the only
trackball or mouse, so two blank sections conflict. A section that is
off holds no address, so duplicates can exist while at most one is on. A save
that turns a section on, or adds one that is on, while an enabled section of
its type has its address is refused with `409` (see below) and saves
nothing. A save that edits a section that is already on onto a held address
is applied and writes that section `enabled = false`, so it is listed in
`stopped`. A config file that holds two enabled sections on one address loads
the first (by numeric index) as it is and the later ones off, with a warning
in the log; `enabled = false` is written by the next save.

An actor that is off is not a shot source: it sends no shots, and is
neither counted nor refused as a feed by a tournament round. It has no
battery in the header and adds nothing to readiness: an output routed to a
monitor that is off is not ready, as with a monitor that is not configured.

**Errors**: a JSON body `{"error": "<reason>"}`; the save is not reported
as applied.

| Status | When |
|---|---|
| `400 Bad Request` | `scope` is not `global`, `body_tracking`, `golfers` or a known actor ID |
| `400 Bad Request` | the save removes the web server (`"the web server can't be removed"`) or adds a second one (`"flighthook runs one web server; edit the existing one"`) |
| `400 Bad Request` | a Mevo section it writes has a `launch_only_spin` below 0, above 1000 or not finite (`"mevo.0: launch_only_spin must be 0 to 1000 rpm per mph, not -5"`) |
| `400 Bad Request` | a launch monitor section it writes has an `impact_lead_s` below 0, above 5 or not finite (`"square.0: impact_lead_s 6 is outside 0 to 5 s"`) |
| `400 Bad Request` | a camera section it writes has a `latency_s` below 0, above 10 or not finite (`"camera.0: latency_s 11 is outside 0 to 10 s"`) |
| `400 Bad Request` | a section it writes changes its device driver settings into ones the driver refuses, named by card and field (`"mock_monitor.0: shot_interval_s: must be 0 to 86400"`, `"r10.0: range: must be a distance greater than 0"`, `"mevo.0: track_pct: must be 0 to 100"`); a section whose saved driver settings are refused still saves when it keeps them as they are (its name, `enabled`, `impact_lead_s`, ...) |
| `400 Bad Request` | a camera section it writes has a `rotation_deg` other than 0, 90, 180 or 270 (`"camera.0: rotation_deg 45 is not 0, 90, 180 or 270"`), or a `yaw_deg` or `pitch_deg` outside -30 to 30 or not finite (`"camera.0: yaw_deg 40 is outside -30 to 30"`) |
| `400 Bad Request` | a camera section it writes has no `camera` (`"camera.0: no camera picked"`) or a `mode` that is neither `auto` nor `WIDTHxHEIGHT@FPS` |
| `400 Bad Request` | a whole-config or `global` save has a `replay` value out of range (`"replay: keep_shots 0 is outside 1 to 1000"`) |
| `400 Bad Request` | a whole-config, `global` or `body_tracking` save has a `body_tracking` value out of range (`"body_tracking: min_score 2 is outside 0 to 1"`) |
| `400 Bad Request` | a whole-config, `global` or `golfers` save has a golfer with no name, an identifier that is not a golfer id, an id in two golfers (naming both) or a `height` outside 100 to 250 cm or not finite |
| `400 Bad Request` | a whole-config or output-scoped save routes an Open Connect or Pureplay Connect output to a Pureplay Accessory card (`"An output can't take shots from Pureplay"`) |
| `409 Conflict` | the save turns on a section, or adds one that is on, at an address an enabled section of the same type has (`"An Open Connect card already uses 127.0.0.1:921"`); nothing is saved |
| `500 Internal Server Error` | the config file is unusable and cannot be backed up before the first save |
| `504 Gateway Timeout` | SystemActor did not answer within 22 s (see below) |

A body that is not a valid `FlighthookConfig` is rejected by the JSON
extractor with a `4xx` and a plain-text reason.

#### Config timeout

A save that restarts or removes actors stops them together and waits for
each to let go of its device or connection, as a
[reconnect](#post-apiactorsidreconnect) does: up to 3 s, 6 s for a Garmin
R10 or a Square Golf, 18 s for an ExPutt, 1 s for a Trackball. One still closing after that is handled the same way
as for a reconnect: replaced beside it when it holds nothing the new one
needs, otherwise its card says the previous connection has not closed yet,
it is listed in neither `restarted` nor `started`, and it starts by itself
once the old connection has closed. SystemActor waits for the stops off its
own thread, so a save sent while a reconnect or another save waits is
handled at once. A save of an actor another command is still waiting for
follows the same rule as one whose old connection is still closing: it is
not started by the save (removed, it is listed in `stopped`), and it starts
from the saved settings when that wait is over. The server waits up to 22 s
for SystemActor (the longest stop wait, 18 s, plus 4 s), so a save is
answered with its outcome, not a `504`.

A save that changes the Web Server's own `bind` restarts the server that is
answering, so the reply can be lost even though the config was saved: read
`GET /api/settings` to see what was applied.

**Side effects**: SystemActor persists the new config to disk and reconciles actors:

- Address, routing and settings changes restart the actor (shutdown, then recreate)
- Removed config sections stop the corresponding actor
- New config sections start new actors
- It announces a `config_outcome` (see [`/api/ws`](#websocket-apiws)) when it is done

---

## WebSocket: `/frp`

`ws://<host>:5880/frp` is a plain [FRP 0.3.0](https://github.com/flightrelay/spec) hub. flighthook hosts several
actors on it and relays everything else, so one connection is enough to watch shots, to send them as a launch monitor,
or to drive the session.

**No handshake.** One message per WebSocket text frame, JSON, each with `kind` and `from` (the sending actor's id).
Unknown kinds and fields are ignored.

### Connecting

On connect flighthook sends its greeting: for each actor it hosts, that actor's `bus.actor` followed by the rest of its
state (`golfer.turn`, `practice.block`, `golfer.profile`, `game.*`, ...), then a `bus.sync` from the flighthook
actor. The `bus.sync` asks every actor on the other side to republish its state, so a peer that is itself an FRP
actor announces itself the same way: its `bus.actor`, its state, then a `bus.sync`. Nothing is replayed: shots hit
before the connection are not sent again (read [`GET /api/shots`](#get-apishots) for those).

A peer that falls too far behind gets the greeting again and carries on from there.

### Actors flighthook hosts

| Actor | Roles | Publishes |
|---|---|---|
| flighthook (its id is `[frp] actor_id`) | `session`, `practice`, `collector` | `golfer.turn`, `golfer.profile` for each saved golfer (namespace `flighthook`), `practice.block` |
| each launch monitor card (Mevo, R10, Square Golf, ExPutt, Trackball, mock, Open Connect server input) | `launch_monitor` | `shot.*`; its `bus.actor` carries `device` (maker, model, battery, attitude, `source`) and `blockers` |
| Auto-chip | `launch_monitor` (`source` `virtual`) | `shot.*` for each chip |
| flight model (the Simulation card) | `flight_model` | `shot.flight` with `basis` `modeled`, after the monitor's own |
| Pureplay Accessory card (beta) | `game`, `launch_monitor` | `game.round`, `game.scores`, `game.ball`, `game.stroke`, `golfer.profile` for its players (namespace `pureplay`), `shot.*` |

Every actor republishes its `bus.actor` every 2 seconds. flighthook's own messages carry the actor's id in `from`; a
card's id is the `actor_id` in its settings section, generated once and kept.

### What flighthook sends and takes

- **Out:** every FRP message on flighthook's bus goes to every peer, as it is, except the messages that peer sent
  itself and `shot.raw` (raw device frames never leave through `/frp`; use the [export](#get-apishotsexport)). Other
  peers' messages are relayed to every other peer, so two programs connected to flighthook see each other.
- **In:** a peer's messages go onto flighthook's bus and are treated like any actor's:
  - `shot.*` from a launch monitor: stored, shown, sent to outputs routed to it, and counted for the golfer up and the
    open [block](#get-apiblocks). A peer whose `bus.actor` `device.source` is `virtual` has its shots held
    while a tournament round is active, as the mock and Trackball cards do.
  - `golfer.select`: answered with a `bus.reply` by flighthook's session, so the golfer, club, hand and mode can be
    driven from another program. While a simulator card is connected it sends its own values back (the turn follows
    the simulator).
  - `practice.new_block`: answered by flighthook's practice actor.
  - `golfer.profile`, `game.*`: shown (the Golfer card lists a game's players; a game's strokes attach to the stored
    shots by `shot_id`).
  - `game.aim`: flighthook's Pureplay Accessory card answers `unsupported`.
- **Other FRP programs show up as cards** (remote launch monitors, beta) in Settings, read-only, with their device info, readiness (`blockers`) and
  battery. A peer is gone after three heartbeats without a `bus.actor` (about 6 s at the 2 s default), or when it sends
  its `bus.actor` with `removed`; flighthook then forgets its state and drops its unfinished shots.
- **Own-id guard.** A message whose `from` is one of flighthook's own actors is dropped, and the Web Server card shows
  an error: the peer is relaying flighthook's messages back (a relay loop, or settings copied to a second PC along
  with its actor ids).
- **Ownership.** At most one actor holds each of `session`, `practice` and `game`. When another program claims a role
  flighthook holds, the cards involved show a conflict; flighthook acts only on its own session's `golfer.turn` and
  `practice.block`.

### Example: watching and driving

```jsonc
// from flighthook
{"kind":"bus.actor","from":"5b80e2f7d14c9a63","name":"Mevo WiFi","frp":"0.3.0","heartbeat":2.0,"roles":["launch_monitor"],"blockers":[],"device":{"manufacturer":"FlightScope","model":"Mevo+","source":"measured"}}
{"kind":"golfer.turn","from":"c19e04b7a2d5f318","golfer":"flighthook.0f8c2d1a-77b4-4e1e-9a50-3c6d2b8e4f10","club":"7I","handedness":"rh","mode":"full"}
{"kind":"bus.sync","from":"c19e04b7a2d5f318"}
// to flighthook
{"kind":"golfer.select","from":"9aa1f40c2b7d3e65","ref":"sel-1","club":"DR"}
// from flighthook
{"kind":"golfer.turn","from":"c19e04b7a2d5f318","golfer":"flighthook.0f8c2d1a-77b4-4e1e-9a50-3c6d2b8e4f10","club":"DR","handedness":"rh","mode":"full"}
{"kind":"bus.reply","from":"c19e04b7a2d5f318","to":"9aa1f40c2b7d3e65","command":"golfer.select","ref":"sel-1","ok":true}
```

### Messages

The message kinds, their fields, units and signs are defined by the [spec](https://github.com/flightrelay/spec); the
[Rust SDK](https://github.com/flightrelay/sdk-rust) implements them (`ShotAggregator` collects a shot's messages by
`key`). flighthook adds no kinds and no fields to what it sends on `/frp`. The kinds on a flighthook bus:

| Kind | Class | Sent by |
|---|---|---|
| `bus.actor` | state | every actor (heartbeat) |
| `bus.sync` | occurrence | any actor joining; flighthook on connect |
| `bus.reply` | occurrence | the owner of a command |
| `bus.alert` | occurrence | any actor |
| `shot.trigger`, `shot.launch`, `shot.club`, `shot.impact`, `shot.flight`, `shot.finished` | occurrence | launch monitors (`shot.flight` also the flight model) |
| `golfer.profile` | state | the actor that issues the id's namespace |
| `golfer.turn` | state | the session |
| `golfer.select` | command | any actor, to the session |
| `practice.block` | state | the practice actor |
| `practice.new_block` | command | any actor, to the practice actor |
| `game.round`, `game.scores`, `game.ball` | state | a game |
| `game.stroke` | occurrence | a game |
| `game.aim` | command | any actor, to a game |

Golfer ids are `<namespace>.<local id>`: `flighthook.<uuid>` for the golfers saved in Settings, `pureplay.thill` for a
Pureplay player (the player's name; `pureplay.player-<id>` for a player with no name).

---

## WebSocket: `/api/ws`

The dashboard's own socket. It is private to the dashboard: its messages carry flighthook-specific events that are
not FRP, and they can change with the dashboard. Use [`/frp`](#websocket-frp) for integrations.

Each message is one JSON text frame, tagged by its variant: `{"snapshot": {...}}`, `{"frp": {...}}`,
`{"local": {...}}` or `{"shot": {...}}`.

### Server -> client

On connect the server sends one `snapshot`, then every bus message and every local event as it happens.

#### snapshot

Sent once, first.

```json
{
  "snapshot": {
    "actor_id": "c19e04b7a2d5f318",
    "card_keys": { "5b80e2f7d14c9a63": "mevo.0" },
    "states": [ { "kind": "golfer.turn", "from": "c19e04b7a2d5f318", "golfer": null, "club": "7I", "handedness": "rh", "mode": "full" } ],
    "cards": { "mevo.0": { "name": "Mevo WiFi", "status": "connected", "telemetry": {}, "state": "connected", "red_for_ms": 0 } },
    "round": { "active": false },
    "config_generation": 3
  }
}
```

- `actor_id`: flighthook's own actor id; its `golfer.turn` and `practice.block` are the ones that count.
- `card_keys`: the local card key of each flighthook actor on the bus, by actor id.
- `states`: every FRP state on the bus (local and remote actors), as FRP messages.
- `cards`: each local card's status, as in [`GET /api/status`](#get-apistatus).
- `round`: the tournament round (the `round` object of `GET /api/status`; `{"active": false}` when none).
- `config_generation`: counts up with every applied configuration change.

#### frp

`{"frp": {...}}`: a message on flighthook's bus, as an FRP message (`kind`, `from`, ...). It carries everything `/frp`
carries, including the replies to the dashboard's own commands (`bus.reply` with the dashboard's `ref`, on that
connection only), and the shots' `shot.*` messages.

#### shot

`{"shot": {...}}`: a shot the store finished or updated, as [`GET /api/shots`](#get-apishots) lists it (`ShotData`).
Sent again when a later message (a flight model result, a game's `stroke`, a camera clip) changes the stored shot.

#### local

`{"local": {"card": "mevo.0", "event": {"kind": "...", ...}}}`: an event of a card (`mevo.0`, or `system` / `web`)
that is not an FRP message. `kind` is one of:

- `card_status`: `status` and `telemetry` (the card's rows) of the card.
- `alert`: `severity` (`warn`, `error`, `critical`) and `message`, from a card that is no FRP actor.
- `round_state`: `round`, the tournament round started or ended (the same object as `round` in
  [`GET /api/status`](#get-apistatus)).
- `config_outcome`: a configuration change was applied: `request_id` (omitted for a change the dashboard did not
  make), `restarted`, `stopped`, `started` (card keys) and `config_generation`.
- `card_keys`: `card_keys`, the card key of every local actor by actor id, sent after every configuration change.
- `clear_shot_history`: the shot history was deleted ([`POST /api/shots/clear`](#post-apishotsclear)); clients clear
  their shots and sets.
- `shot_video` (beta): a camera finished a clip of the shots in `keys` (`shot_id`/`shot_number`): `position`
  ([`CameraPosition`](#cameraposition)), `duration_ms`, `fps`, `trigger_offset_ms`, `impact_offset_ms`, `complete` and
  `dropped`. The clip is then in the shots' `video`.
- `shot_video_removed` (beta): the clips of the shots in `keys` were deleted.
- `shot_pose` (alpha): body tracking of a camera's clip is done for the shots in `keys`; `rotation` is the rotation it
  was tracked with. The clip's `video` entry turns `pose: true`.
- `shot_swing` (alpha): the swing analysis of a camera's clip is done for the shots in `keys`; the clip's `video` entry
  turns `swing: true`.

### Client -> server

Each message is one JSON text frame, tagged `frp` or `local`.

- `{"frp": {"kind": "golfer.select", "from": "ui", "ref": "ui-1", "club": "7I"}}`: an FRP command (`golfer.select` or
  `practice.new_block`). The server stamps `from` with flighthook's actor id, so the dashboard's `from` is not read.
  The matching `bus.reply` (with the dashboard's `ref`) comes back on this connection only.
- `{"local": {"cmd": "clear_history"}}`: delete the shot history, as [`POST /api/shots/clear`](#post-apishotsclear).
- `{"local": {"cmd": "grouping", "mode": "by_club"}}`: set the default shot view, as
  [`POST /api/shots/grouping`](#post-apishotsgrouping).

Invalid JSON and unknown commands are ignored; failed commands report in their `bus.reply`.

---

## Error Handling

REST errors answer with a status code and, where the endpoint says so, a JSON `{"error": "<reason>"}` body. On
`/frp` and `/api/ws`, invalid JSON, unknown kinds and unknown commands are ignored; a command to an actor is answered
with a `bus.reply` (`ok`, or `error`: `bad_request`, `not_allowed`, `not_ready` or `unsupported`), and a command with
no owner gets no answer.

---

## Enums Reference

### ActorStatus

`"starting"` | `"disconnected"` | `"connected"` | `"reconnecting"`

### ConnectionState

`"connecting"` | `"connected"` | `"reconnecting"` | `"disabled"` (see
[`GET /api/status`](#get-apistatus) for how it is derived)

### Mode

`"full"` | `"putting"` | `"chipping"`: the stroke the golfer is about to play, on `golfer.turn`. A launch monitor
tunes its detection to it. It changes only when a `golfer.select` carries it.

### Handedness

`"rh"` | `"lh"`

### Source

`"measured"` | `"virtual"`: where a launch monitor's shots come from, from its `bus.actor` `device.source`. `measured`:
a physical launch monitor measured the shot, either one flighthook reads directly (FlightScope, Garmin R10, Square
Golf, ExPutt) or one whose own app sends its shots to the Open Connect input. `virtual`: no physical measurement (the
Trackball, the mock launch monitor, Auto-chip, and a Pureplay Accessory card, whose shots are the values the game flew
the ball with). A remote launch monitor declares its own; one that declares nothing counts as not measured.

### CameraPosition

Where a camera stands: `"target_line"` (behind the golfer's hands, looking
down the target line) | `"golfer_front"` (facing the golfer's chest) |
`"golfer_back"` (looking at the golfer's back) | `"overhead"` (above, looking
down)

### Severity

`"warn"` | `"error"` | `"critical"`

### UnitSystem

`"imperial"` | `"metric"`

### ShotGrouping

The Shots tab's view: `"by_set"` (default, a section per set) | `"by_club"`
(a section per club, combining a golfer's sets of that club within a session) |
`"none"` (the last shot only). It does not change how sets are assigned.

### Club

In bag order:

| Kind | Codes |
|---|---|
| Woods | `"DR"` `"MD"` `"2W"` `"3W"` `"4W"` `"5W"` `"6W"` `"7W"` `"8W"` `"9W"` |
| Hybrids | `"1H"` `"2H"` `"3H"` `"4H"` `"5H"` `"6H"` `"7H"` `"8H"` `"9H"` |
| Irons | `"1I"` `"2I"` `"3I"` `"4I"` `"5I"` `"6I"` `"7I"` `"8I"` `"9I"` |
| Wedges | `"PW"` `"GW"` `"SW"` `"LW"` |
| Putter | `"PT"` |

Clubs serialize digit-first as above, and a golfer's `bag` and an Open Connect
client's `chipping_clubs`/`putting_clubs` take these exact codes. A club given as a free-form string
(a Square section's `club` and calibration keys,
GSPro player messages) is case-insensitive, ignores surrounding whitespace
and also accepts: the letter-first form GSPro sends (`"W2"`, `"H6"`, `"I1"`)
plus `"nHY"` for every hybrid (`"1HY"` to `"9HY"`); `"D"`/`"DRIVER"`/`"1W"`/`"W1"` for the driver; `"MINI"`/
`"MINIDRIVER"`/`"MINI DRIVER"` for the mini driver; wedge lofts (`"46"`/`"48"`
PW, `"50"`/`"52"` GW, `"54"`/`"56"` SW, `"58"`/`"60"`/`"62"`/`"64"` LW), each
also with a trailing `"W"` (e.g. `"52W"`); `"AW"`/`"UW"`/`"A"` for the gap
wedge, `"S"` for the sand wedge, `"L"`/`"ULW"` for the lob wedge; and `"P"`/
`"PUTTER"` for the putter.

### SimMode

`"off"` | `"on"` | `"force"` -- what is simulated (beta). `"off"`: nothing.
`"on"` (default): the result values the device does not report. `"force"`:
every result value, replacing the device's.

### SimSurface

`"fairway"` | `"green"` | `"firm_green"` -- flat ground for simulated landing
and roll (beta). A ground ball taken in Putting mode rolls on `"green"` when
the setting is `"fairway"`.

### Velocity (unit-tagged string)

Format: `{number}{suffix}`. Suffixes: `mph` (miles/hour), `mps` (meters/second),
`kph` (km/hour), `fps` (feet/second).

### Distance (unit-tagged string)

Format: `{number}{suffix}`. Suffixes: `yd` (yards), `ft` (feet), `in` (inches),
`m` (meters), `cm` (centimeters), `mm` (millimeters; face impact, never converted).
