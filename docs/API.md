# Flighthook Web API

## Quick-Start: Real-Time Shot Stream

The fastest way to get shot data is the WebSocket at `/frp`. After a brief
init handshake, you'll receive JSON messages for every shot lifecycle event as
it happens -- no polling needed. This is ideal for video timestamp tagging,
overlay triggers, stat trackers, or any integration that needs to react to
shots in real time.

Shot data arrives as a sequence of correlated events sharing a `ShotKey`:

1. `shot_trigger` -- ball strike detected (no data yet)
2. `launch` -- the measured launch (speed, angles, spin), plus `derived`
   values flighthook simulated for what the device didn't report, or for
   every result field under `[simulation] mode = "force"` (beta; see
   [launch](#launch))
3. `flight_result` -- the device's flight values (carry, apex, flight time
   and so on) with the `basis` they were obtained on; flighthook's own
   simulated result when the device sent none (see
   [flight_result](#flight_result))
4. `club_path` -- club data (speed, path, attack angle, face angle, loft)
5. `face_impact` -- face impact location
6. `shot_finished` -- shot complete, accumulators should finalize

The shot's events between its trigger and its finish may arrive in any order.
Use the `ShotAggregator` pattern (or wait for `shot_finished`) to collect the
full shot. All `Launch`, `FlightResult` and `ClubData` fields are `Option`
(matching the FRP spec), except a flight result's `basis`.

Velocities and distances are unit-tagged strings (e.g. `"67.2mps"`,
`"180.5m"`). To convert to a standard unit system without parsing suffixes
yourself, POST the `ShotData` to `/api/shots/convert?units=imperial` (or
`metric`). You can also use `?units=` on `GET /api/shots` for historical data.
Angles are degrees, spin is RPM.

Other useful events on the same connection:

- `device_telemetry` -- device-reported state (ready, battery, tilt, temp, identity)
- `actor_status` -- actor/connection lifecycle (status enum, card telemetry)
- `hand_info` -- the player's hand, set by the simulator, the UI or the API
- `round_state` -- a tournament round started or ended
- `set_detection_mode` -- detection mode, handedness and/or club change (all optional, latched)
- `shot_group` -- which [set](#get-apishotsgroups) a finished shot joined
  (and so its club); the shot's `/api/shots` element carries the same
  `group`
- `shot_video` / `shot_video_removed` -- a camera clip of a shot is ready to
  replay, or has been deleted (beta)
- `shot_pose` -- the golfer's keypoints on a camera clip are ready (body
  tracking, alpha)
- `shot_swing` -- a camera clip's swing analysis is ready (body tracking,
  alpha)

### Terminal

```bash
# Stream shots with websocat + jq (install: cargo install websocat)
# Send the init handshake, keep stdin open to hold the connection, filter for shot events
(echo '{"kind":"start","version":["0.2.0"],"name":"cli"}'; cat) | \
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
        await ws.send(json.dumps({"kind": "start", "version": ["0.2.0"], "name": "my-dashboard"}))
        init = json.loads(await ws.recv())
        print(f"Connected: actor_id={init['actor_id']}")

        # Accumulate shot data
        shots = {}  # key: (actor, shot_id) -> {launch, flight, club}
        async for raw in ws:
            msg = json.loads(raw)
            ev = msg["event"]
            kind = ev["kind"]
            if "key" not in ev:
                continue
            key = (msg["actor"], ev["key"]["shot_id"])

            if kind == "shot_trigger":
                shots[key] = {}
            elif kind == "launch" and key in shots:
                shots[key]["launch"] = ev["launch"]
            elif kind == "flight_result" and key in shots:
                shots[key]["flight"] = ev["flight"]
            elif kind == "club_path" and key in shots:
                shots[key]["club"] = ev["club"]
            elif kind == "shot_finished":
                shot = shots.pop(key, None)
                if shot:
                    print(f"Shot #{ev['key']['shot_number']}: "
                          f"speed={shot.get('launch', {}).get('launch_speed')} "
                          f"carry={shot.get('flight', {}).get('carry_distance')}")

asyncio.run(main())
```

### JavaScript

```javascript
const ws = new WebSocket("ws://localhost:5880/frp");
const shots = new Map();

ws.onopen = () => {
  ws.send(JSON.stringify({ kind: "start", version: ["0.2.0"], name: "my-dashboard" }));
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
    case "shot_trigger":
      shots.set(key, {});
      break;
    case "launch":
      if (shots.has(key)) shots.get(key).launch = event.launch;
      break;
    case "flight_result":
      if (shots.has(key)) shots.get(key).flight = event.flight;
      break;
    case "club_path":
      if (shots.has(key)) shots.get(key).club = event.club;
      break;
    case "shot_finished":
      const shot = shots.get(key);
      shots.delete(key);
      if (shot) console.log(`Shot #${event.key.shot_number}`, shot.launch, shot.flight);
      break;
  }
};
```

---

## REST Endpoints

The API has no authentication; it is meant for a trusted LAN. Any client that
reaches the port can change settings or clear the shot history, download
swing clips, camera stills and live camera frames, and the bodiless `POST`s
(New set, New session, Clear history, a clip's body tracking, the body
tracking model download) are CORS simple requests, so a web page open in a
browser on the LAN can send them too.

### GET /api/status

Comprehensive system state (all actors -- launch monitors and integrations).

**Response** `200 OK`:

```json
{
  "mode": "full",
  "club": "7I",
  "handed": "rh",
  "cameras_supported": true,
  "trackball_supported": true,
  "clip_dir": "C:\\Users\\me\\AppData\\Local\\flighthook\\clips",
  "config_error": {
    "path": "C:\\Users\\me\\AppData\\Roaming\\flighthook\\config.toml",
    "error": "line 4, column 8: unknown variant `liv`, expected one of `off`, `historical`, `live`"
  },
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
      "telemetry": {
        "club": "DR",
        "handed": "rh"
      },
      "state": "connected",
      "red_for_ms": 0
    }
  }
}
```

`name` is the actor's configured name, as the config holds it at the request
(a rename shows here at once; it restarts nothing). An Open Connect server
with a blank
name reads `Open Connect`, and Open Connect servers that share a name are
numbered in index order (`Open Connect 1`, `Open Connect 2`).

- `mode`: global detection mode (`"full"` | `"putting"` | `"chipping"`), `null` if not yet set
- `club`: club in play (see [`POST /api/club`](#post-apiclub)); absent until a
  club has been selected. Same source as `init.global_state.club_info`
- `handed`: the player's [hand](#handedness) (see
  [`POST /api/hand`](#post-apihand)); absent until a hand has been set. Same
  source as `init.global_state.handed`
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
- `round`: present only while a tournament round is active (an output such
  as Pureplay holds one in strict mode): `{"active": true, "holder":
  "Pureplay"}`, `holder` being the holding output's card name. During a
  round only measured shots and values count for the holder, and Auto-chip
  and the mock launch monitor make no shots. Same state as the
  [`round_state`](#round_state) event
- `status`: `"starting"` | `"disconnected"` | `"connected"` | `"reconnecting"`,
  the actor's latest [`actor_status`](#actor_status)
- `telemetry`: actor-specific key/value pairs (all string values)
- `state`: the [connection state](#connectionstate) the dashboard shows,
  derived by the server from `status` and the actor's history since it last
  started:
  - `"connecting"`: not `connected` since it started, or since it was
    restarted by a config change;
  - `"connected"`: `status` is `connected`;
  - `"reconnecting"`: `connected` since it started, and has since lost the
    connection (any other status);
  - `"disabled"`: the card is turned off (its config section has
    `enabled = false`, see [Turning a card off](#turning-a-card-off)). No
    actor runs: `status` is `disconnected`, `telemetry` is empty and
    `red_for_ms` is `0`. Not an error state.
- `red_for_ms`: milliseconds the `reconnecting` state stays an alert (red),
  counted from when the reply was built: what is left of the 10 minutes
  (600000 ms) after the most recent change from `connected` to any other
  status, else `0`. Another loss while already `reconnecting` does not start
  the 10 minutes again; a new `connected` then a loss does. A client counts
  it down from the moment it receives the reply, so its clock need not match
  the server's.

The server keeps each actor's history for as long as it runs, whoever is
connected: a dashboard opened five minutes after a loss still gets
`reconnecting` with about five minutes of `red_for_ms`. A `starting` status,
a restart from a config change, removing the actor (then adding it again)
and turning it off then on again each begin a new history. Live [`actor_status`](#actor_status) events carry
only `status`; a client applies the same rules to them to keep `state` and
the red time current.

Telemetry keys are merged from two sources; no key means two things, and an
actor sends each key from one source only:
- **Device-reported** (from `device_telemetry`): `ready`, `battery_pct`, `tilt`,
  `roll`, `temp_c`, `external_power`, and a device driver's own state keys
  (the mock's `radar_mode`, `tracking_mode`; the Square Golf's
  `device_state`, `zero_spin_discards`; the Trackball's `synthetic`
  (`true`), `input` (the name the device reports), `cpi`, `profile` (the
  club in play) and `handed` (`rh` | `lh`))
- **Actor-framework** (from `actor_status`): `device_info`, `shot_count`,
  `phase`
- **Integration actors**: `club`, `handed`, `name`, `error`
- **Open Connect clients** (GSPro, Pureplay, Muni Golf, generic; from
  `actor_status`):
  `address` (the simulator's, `127.0.0.1:921`), `last_shot` (the
  `ShotNumber` the simulator got last and the local time it was sent,
  `#12 at 16:41:07`), and for the current connection only `shots_sent`
  (`3`), `monitor_ready` (the readiness last sent to the simulator, `true` |
  `false`) and `response` (the simulator's last response code and message,
  `201 GSPro Player Information`). `club` and `handed` are the simulator's
  player information. When the connection ends, `shots_sent`,
  `monitor_ready`, `response`, `club` and `handed` are removed; `address`
  and `last_shot` stay
- **Pureplay Connect outputs** (alpha; from `actor_status`): `address` (Pureplay's,
  `127.0.0.1:9760`), `last_shot` (the ShotId Pureplay got last and the local
  time it was sent, `#4 at 16:41:07`), and for the current link only
  `shots_sent` (`4`), `ready_state` (the ready state last sent, `ready` |
  `not ready` | `processing`) and `strict_mode` (`on` | `off`; `on` while a
  tournament round is in progress, when Auto-chip does not fire). `club` and
  `handed` are the club and hand Pureplay set. `error` says why the handshake
  failed (no reply to Hello, an unsupported protocol version); a refused or
  closed connection sets none. When the link ends, `shots_sent`,
  `ready_state`, `strict_mode`, `club` and `handed` are removed
- **Cameras** (from `actor_status`): `camera` (the camera's own name),
  `position`, `resolution` (`1280x720`), `mode_fps` (the frame rate asked
  for), `fps` (achieved over the last second), `dropped` (frames missing
  since it connected), `error`. `fps`, `dropped` and `position` are sent at
  most every 2 s, and only when they change

---

### GET /api/shots

Shot history (most recent N shots, FIFO, max 1000 stored). Shots the device
produced no data for (no ball, club or impact) are kept for the
[export](#get-apishotsexport) but not listed here.

**Query params**:

- `limit` (optional, default `50`): max shots to return (the dashboard asks
  for `1000`, the whole history). `0` returns `[]`; a value above the number
  stored returns them all; a non-numeric value is `400 Bad Request`
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
    "shot_id": "550e8400-e29b-41d4-a716-446655440000",
    "club_selected": "7I",
    "group": "6f1c2a3b-4d5e-4f60-8a71-92b3c4d5e6f7",
    "handed": "rh",
    "launch": {
      "launch_speed": "67.2mps",
      "launch_azimuth": -1.3,
      "launch_elevation": 14.2,
      "backspin_rpm": 3200,
      "sidespin_rpm": -450
    },
    "flight": {
      "basis": "modeled",
      "carry_distance": "180.5m",
      "carry_offline": "-7.6m",
      "max_height": "28.3m",
      "flight_time": 6.4,
      "descent_angle": 48.2
    },
    "derived": {
      "roll_distance": "11.2m",
      "roll_offline": "-0.9m",
      "total_distance": "191.7m",
      "total_offline": "-8.5m",
      "path": {
        "flight": [[0, 0, 0, 0], [0.464, 25.37, 6.46, -0.63], ...],
        "ground": [[6.4, 180.34, 0, -7.6], ...]
      }
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

With `?units=imperial`:

```json
{
  "launch": {
    "launch_speed": "150.3mph"
  },
  "flight": {
    "basis": "modeled",
    "carry_distance": "197.4yd",
    "carry_offline": "-8.3yd",
    "max_height": "92.8ft"
  },
  "derived": {
    "roll_distance": "12.2yd",
    "roll_offline": "-1.0yd",
    "total_distance": "209.6yd",
    "total_offline": "-9.3yd",
    "path": {
      "flight": [[0, 0, 0, 0], [0.464, 25.37, 6.46, -0.63], ...],
      "ground": [[6.4, 180.34, 0, -7.6], ...]
    }
  },
  "club": {
    "club_speed": "94.2mph",
    "club_offset": "0.2in",
    "club_height": "0.47in"
  },
  "metrics": {
    "curve": "-3.8yd"
  }
}
```

- `launch`: `Launch` or absent. The launch (speed, angles, spin), exactly as
  the device measured it, except on a shot flagged `derived.spin_estimated`:
  its spin is flighthook's estimate; and any value `derived.computed` lists,
  which comes from a model rather than the device's readings. All fields are
  `Option`. Velocity fields are unit-tagged strings (`"67.2mps"`,
  `"150.3mph"`). See [launch](#launch) for the fields.
- `flight`: `FlightResult` or absent. The device's flight values (carry,
  offline, total, roll, apex, flight time, descent), with the `basis` they
  were obtained on; see [flight_result](#flight_result). Absent when the
  device reported none (a launch-only device; a launch-only chip, whose 0
  carry, apex and flight time are no reading). Distance fields are
  unit-tagged strings (`"180.5m"`, `"197.4yd"`).
- `derived`: `DerivedFlight` or absent. Values simulated for fields the device
  didn't report, or for every result field under `mode = "force"` (beta); see [launch](#launch) for the fields.
  Converted by `?units=` like `flight` (distances to yards/meters, `max_height`
  to feet/meters); `path`, when present, is always meters/seconds and is
  never converted.

A shot stored in the shape with one `ball` block (launch and flight values
together) reads back with them split into `launch` and a modeled `flight`,
for example in `POST /api/shots/convert`.
- `club`: `ClubData` or `null`. Club head data. All fields are `Option`.
- `impact`: `FaceImpact` or `null`. Face impact location.
- `metrics`: `ShotMetrics` or absent. Values computed from the measured
  `launch`, the device's `flight` and `club` when the shot finished (see
  [Shot metrics](#shot-metrics)). `curve` is converted by `?units=` like
  `flight`.
- `shot_id`: the shot's `key.shot_id` from its lifecycle events.
- `club_selected`: club in play when the shot was triggered (`PT` for an
  ExPutt shot); absent when none was selected.
- `group`: id of the [set](#get-apishotsgroups) the shot joined. Every shot
  with data has one, whatever the default view (`shot_grouping`).
- `handed`: the player's handedness (`"rh"` | `"lh"`) when the shot was
  triggered; absent when unknown. Face impact is the same for either hand;
  a face-on drawing of it puts a right-hander's toe on the left and a
  left-hander's on the right.
- `provenance`: where the shot comes from, by the type of the actor that
  emitted it (see [Provenance](#provenance)): `"measured"` (a launch monitor
  measured it: one flighthook reads, or one whose app sends its shots to
  the Open Connect input) or `"virtual"` (the Trackball, the mock launch
  monitor, Auto-chip). Absent on a shot stored without it; a tournament round counts
  such a shot as not measured.
- `video` (beta): the cameras' clips of the shot, one per camera, omitted
  when there are none. Each is a `ShotVideo`, the fields of
  [`shot_video`](#shot_video) plus `camera`, the camera's actor ID:

  ```json
  "video": [
    { "camera": "camera.0", "position": "golfer_front", "duration_ms": 5500, "fps": 119.6,
      "trigger_offset_ms": 4000, "impact_offset_ms": 4000, "complete": true, "dropped": 0,
      "pose": true, "swing": true, "rotation": 90, "analyzed_with": "0.0.4" }
  ]
  ```

  `pose` (body tracking, alpha; omitted while false) says the golfer's
  keypoints on every frame of the clip are ready at
  [`{camera}.pose.json`](#get-apishotsshot_idvideocameraposejson); it turns
  true with [`shot_pose`](#shot_pose). In Historical mode a clip is tracked
  only once asked for
  ([`POST .../analyze`](#post-apishotsshot_idvideocameraanalyze)). `swing` (alpha; omitted while false)
  says the clip's swing analysis is ready at
  [`{camera}.swing.json`](#get-apishotsshot_idvideocameraswingjson); it turns
  true with [`shot_swing`](#shot_swing), and stays false for a camera at
  `golfer_back` or `overhead`. `rotation` (omitted at 0) is the camera's
  rotation, clockwise degrees, the clip was tracked with: its keypoints are
  in the picture turned by it. `analyzed_with` (omitted until tracked) is
  the version of the body tracking library that tracked it. Both are kept
  in the clip's `shot.json`; a clip tracked before they existed reads as
  rotation 0 and no version.

  A clip that two launch monitors' triggers share (one swing) is on both
  shots' rows. It is added when the camera finishes the clip, which can be
  before or after the shot finishes, and removed when the clip is deleted
  ([`shot_video_removed`](#shot_video_removed)). Clips are files on disk and
  outlive a restart; the shot history doesn't, so after a restart a clip is
  reachable by its shot ID only. Never in the [export](#get-apishotsexport).

`POST /api/shots/convert` passes `shot_id`, `club_selected`, `group` and
`handed` through unchanged, and drops `video`.

#### Shot metrics

`metrics` holds values flighthook computes from the shot's measured `launch`,
the device's `flight` and `club` when the shot finishes, so consumers don't
each compute them. The
formulas and signs are the Flight Relay Protocol's spin relations and
"Derived Values":

| Field | Type | Formula | Present when |
| --- | --- | --- | --- |
| `total_spin_rpm` | integer | `√(backspin_rpm² + sidespin_rpm²)`, rounded | the device sent both components and neither `total_spin_rpm` nor `spin_axis` |
| `spin_axis` | number (deg) | `atan2(sidespin_rpm, backspin_rpm)`; + = tilted right, curves the ball right | as `total_spin_rpm`, and the total is not 0 |
| `face_to_path` | number (deg) | `face_angle − path`; + = face right of the club path | the device sent both |
| `spin_loft` | number (deg) | `dynamic_loft − attack_angle`, an approximation of the measured 3D spin loft | the device sent both and no `spin_loft` of its own |
| `curve` | Distance | `carry_offline − carry_distance × tan(launch_azimuth)`, in a common unit; + = curved right | the device sent all three (carry and its offline in `flight`) |

Each field is absent when an input is missing (or not a finite number), and
`metrics` is absent when no field is present. Every input is a measured
value: `derived` (the simulation) is never used, so a shot without a measured
`carry_offline` has no `curve`. When the device reports total spin and axis
itself (R10, Square Golf, Open Connect), they are under `launch`, not here;
the same goes for a measured `spin_loft` under `club`. To show total spin and
spin axis for any device, read `launch.total_spin_rpm` and `launch.spin_axis`
first, else `metrics.total_spin_rpm` and `metrics.spin_axis`; likewise
`club.spin_loft`, else `metrics.spin_loft`. Lateral values are
absolute: + is right of the target line for every golfer. `metrics` is on
`/api/shots` and in the [export](#get-apishotsexport). The WebSocket carries
the device's events, which hold no `metrics`: the `flighthook` crate's
`ShotAggregator` computes them when it assembles a shot, and other WebSocket
consumers can use the formulas above or read the shot from `/api/shots`.

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
`pose: true` ([`shot_pose`](#shot_pose)). Served with the same rules as the
`.avi` and `.json`: `404` with a text body before then and once the clip is
deleted, HTTP `Range`, `Cache-Control: private, no-cache`.

```json
{
  "version": 1, "skeleton": "halpe26",
  "model": "rtmpose-m-halpe26", "detector": "yolox-tiny",
  "width": 1280, "height": 720,
  "frames": [
    { "t_ns": 0, "score": 0.81, "bbox": { "x1": 512.0, "y1": 96.0, "x2": 790.0, "y2": 700.0 },
      "keypoints": [[651.2, 160.4, 0.93], ...] },
    { "t_ns": 8358000 },
    ...
  ]
}
```

- One entry per clip frame, in order; `t_ns` is the sidecar's `t_ns` of that
  frame, so frame `i` of the clip is the entry with the same `t_ns`.
- `keypoints`: 26 `[x, y, score]` in frame pixels, in the
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
`video` entry has `swing: true` ([`shot_swing`](#shot_swing)), with the same
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
  the torso length, with `cm` when `player.height` is set) or
  `ratio`. Every value is a 2D measure in this camera's picture. Sideways
  shifts face-on (`hip_sway`, `head_lateral`) are positive toward the
  target and negative away from it, for either hand. The ids and their
  definitions are in
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
any tracked clip, and [`shot_pose`](#shot_pose), then
[`shot_swing`](#shot_swing), go out on the WebSocket. A clip already
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
… WS: shot_pose, then shot_swing
POST /api/shots/550e8400-e29b-41d4-a716-446655440000/video/camera.0/analyze
→ 200 {"state":"done"}
POST /api/shots/550e8400-e29b-41d4-a716-446655440000/video/camera.0/analyze?force=1
→ 202 {"state":"queued"}
… WS: shot_pose, then shot_swing
```

### GET /api/shots/{shot_id}/grades/{camera}

One camera's swing numbers graded against bands from published tour
averages (alpha), for the tour and the amateur reference: what colors the
Cameras pane's tiles and tables (see
[Bands and colors](devices/cameras.md#bands-and-colors)). Computed on each
request from the camera's
[`{camera}.swing.json`](#get-apishotsshot_idvideocameraswingjson), the
shot's `club_selected` and `handed`, and `player` in the settings as they
are now; never stored, so a change to the Player card shows in the next
request. `404` (`{"error": "<reason>"}`) until the camera's `video` entry has
`swing: true`, and once the clip is deleted.

```json
{
  "shot_id": "550e8400-e29b-41d4-a716-446655440000",
  "camera": "camera.0",
  "club_class": "driver",
  "sex": "male",
  "hand": "right",
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
- `sex`: the Player card's, which picks the men's or women's bands. `hand`:
  the shot's (right when unknown). The metrics are measured relative to the
  target, so both hands grade alike.
- `references`: `tour`, then `amateur`. `grades` holds every metric at every
  key `position` (as in the swing file) that has a band; `tempo` the
  `tempo_ratio` and the `downswing_time` (`position` `swing`). A value
  without a band, or a band that is not graded, is absent.
- `value`: in the band's `unit` (`degrees`, `inches`, `ratio`, `seconds`),
  signed as the band: turns positive open (toward the target) and negative
  closed, shifts positive toward the target, the ball or up. A distance is
  put into inches with `player.height`, or an assumed height for the sex
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

  `keypoints`: 26 `[x, y, score]` in frame pixels to 0.1 px, scores to
  0.01, in the [Halpe-26 order](#halpe-26-keypoints), smoothed with the
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

### GET /api/shots/groups

Shot sets ("groups" on the wire) over this webserver's shot history. Sets are
recorded the same way whatever `shot_grouping` says; that field only picks
the Shots tab's default view (see [POST /api/shots/grouping](#post-apishotsgrouping)).

- At most one set is **open**. A shot joins it unless the club in play differs
  from the set's club, in which case the open set closes and a new one starts.
  Returning to an earlier club starts a new set; a closed set never reopens.
- An open set with no shots (one **New set** just started, or one opened by
  a trigger that read nothing) takes the next shot's club instead.
- **New set** ([POST /api/shots/groups](#post-apishotsgroups)) closes the open
  set; the next shot joins a fresh one.
- **New session** ([POST /api/shots/session](#post-apishotssession)) closes the
  open set and counts the session up; the next shot starts a new set in the
  new session.

A shot is assigned its set when it **triggers**, using the club in play at
that moment, and joins it when it finishes with data. A club change or a New
set during a flight applies to the next shot. A shot that never finishes
(the device triggers another shot first, drops out with any status but
`connected`, or 45 s pass) leaves its set uncounted. A device that disconnects and
reconnects keeps the open set: its next shot with the same club joins it.

**Response** `200 OK` -- `ShotGroupsResponse`:

```json
{
  "server": "webserver.0",
  "mode": "by_club",
  "session": 1,
  "current": "0b7e9c1d-2f3a-4b5c-9d6e-7f8091a2b3c4",
  "groups": [
    { "id": "6f1c2a3b-4d5e-4f60-8a71-92b3c4d5e6f7", "number": 3, "club": "7I", "started_at": "2026-10-26T13:58:02.117Z", "open": false, "shots": 10 },
    { "id": "0b7e9c1d-2f3a-4b5c-9d6e-7f8091a2b3c4", "number": 4, "session": 1, "club": "DR", "started_at": "2026-10-26T14:02:40.004Z", "manual": true, "open": true, "shots": 0 }
  ]
}
```

- `server`: actor ID of the webserver answering. Its `shot_group` events carry
  the same `actor`; each `[webserver.N]` keeps its own history and sets.
- `mode`: the configured `shot_grouping`, the Shots tab's default view. It has
  no effect on how sets are assigned.
- `session`: the current session, counting from 0 each run and up with every
  [New session](#post-apishotssession). Omitted while 0.
- `current`: the open set: the one the next shot joins, unless the club in
  play differs from its club (then the next shot starts a new set). Absent
  when no set is open.
- `groups`: the open set, plus every set that holds a stored shot or has a
  shot in flight, ascending `number`.
  - `id`: UUID v4, unique across restarts.
  - `number`: display ordinal, counting from 1 each run. The Shots tab shows
    it as the set number (`SET 3` in By set headers).
  - `session`: the session the set was started in. Omitted while 0.
  - `club`: absent for the set of shots hit with no club selected.
  - `started_at`: when the set was created (host clock, RFC 3339 UTC).
  - `manual`: `true` when started by `POST /api/shots/groups` (the Shots
    tab's **New set**); omitted when `false`.
  - `open`: still receiving shots. A closed set never reopens.
  - `shots`: stored shots with data in the set (the ones `/api/shots` lists).

---

### POST /api/shots/groups

No body. Emits `new_shot_group` on the bus (actor `web`) and returns
`202 Accepted`; `shot_group` events follow. The Shots tab's **New set**
button sends this (or its WS twin).

Closes the open set and opens a fresh one for the selected club, in the
current session. If the next shot is hit with a different club, the empty set
takes that club (as any open set with no shots does). No-op when the open set
is already empty.

---

### POST /api/shots/session

No body. Emits `new_shot_session` on the bus (actor `web`) and returns
`202 Accepted`; the Shots tab's **New session** button sends this. The open
set closes and the session counts up; the next shot starts a new set in the
new session. One `shot_group` for the closed set follows; none when nothing
was open. The Shots tab's By club view combines a club's sets within a
session only, so it starts every club afresh.

---

### POST /api/shots/clear

No body. Emits `clear_shot_history` on the bus (actor `web`) and returns
`202 Accepted`; the Shots tab's **Clear history** (trash icon, at the toolbar's right end, after a
confirmation) sends this or its WS twin. Every webserver then:

- deletes its shot history: `/api/shots` and the
  [export](#get-apishotsexport) are empty, raw frames included;
- drops every set, open or closed, and counts the session up, as
  [New session](#post-apishotssession) does; the next shot starts set 1 in
  the new session, with the club in play unchanged;
- forgets the shots in flight: a device that finishes a shot it triggered
  before the clear stores nothing and changes no set. The next trigger is a
  new shot.

The system actor deletes every camera clip on disk too (beta): the clips
belong to the shots. A clip a camera is still writing is dropped when it
finishes, and a folder that can't be deleted at once (a file still open) is
retried later.

No `shot_group` events follow: every client clears its shots and sets when
it sees [`clear_shot_history`](#clear_shot_history). Nothing is undone; export
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
(see [config timeout](#config-timeout));
the command is already on the bus then, so it may still apply later: re-read
[`GET /api/shots/groups`](#get-apishotsgroups). A `mode` outside the three is
`422 Unprocessable Entity`, a malformed body `400 Bad Request` and a missing
`Content-Type: application/json` `415 Unsupported Media Type`.

---

### POST /api/club

Select the club in play.

**Request**:

```json
{ "club": "7I" }
```

- `club`: a [club code](#club) (`DR`, `2W`, … `PT`), case-insensitive.
  Letter-first codes (`I7`, `H6`) and wedge lofts (`52`, `56`, `60`) are
  accepted, as from GSPro.

**Response**: `202 Accepted` (no body). `400 Bad Request` with a text body
listing the codes when the club is unknown.

Emits `club_info` on the bus (actor `web`). The detection mode is derived from
the club as for GSPro (`chipping_clubs` → chipping, `putting_clubs` →
putting, else full), and when it differs from the open set's club, the next
shot starts a new [set](#get-apishotsgroups). While an Open Connect client is
connected, the simulator's next player message replaces the selection: the
simulator (e.g. GSPro) is the club authority while it runs.

---

### POST /api/hand

Set the player's hand.

**Request**:

```json
{ "handed": "lh" }
```

- `handed`: `"rh"` or `"lh"`, case-insensitive.

**Response**: `202 Accepted` (no body). `400 Bad Request` with a text body
for any other body (another value, a missing field, malformed JSON, a missing
`Content-Type`).

Emits `hand_info` on the bus (actor `web`), like a hand from GSPro. The
system actor stores it and, when it differs from the hand in effect, sends
the devices that follow the player's hand (Square Golf, ExPutt) a
`set_detection_mode` with `handed`; the Open Connect server tells its monitor
too. While an Open Connect client is connected, the UI's hand control is
read-only, as the club picker is, because the simulator's next player message
replaces the hand.

---

### POST /api/auto-chip

Send an Auto-chip (beta): a short, straight chip of a chosen total distance,
for chips the launch monitor can't read.
Off by default; turn it on in Settings → Simulation (useful with radar launch
monitors that miss short chips).

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
`"Auto-chip is disabled in settings"`, and no shot is sent or numbered), while
a tournament round is active (`"Auto-chip is off during a Pureplay
strict-mode round"`, nothing sent or numbered either; see
[`round_state`](#round_state)), or
when the `[simulation]` altitude or temperature is out of range. A body that is not
`{"yards": <non-negative integer>}` is `422 Unprocessable Entity`, and a
missing `Content-Type: application/json` `415 Unsupported Media Type`.

The chip is launched with `launch_azimuth` 0, `launch_elevation` 28°,
`backspin_rpm` = round(100 × ball speed in mph) and `sidespin_rpm` 0, and no
club data. Its ball speed is the one at which flighthook's simulation, with
the `[simulation]` altitude, temperature and surface, comes to rest at
`yards` (within 0.1 yd), whatever the simulation `mode`. On the default
settings that is about 4.4 mph for 1 yd, 7.8 mph for 3 yd and 10.4 mph for
5 yd. The simulator runs its own physics on the launch, so the distance it
shows depends on its own ground (e.g. its green speed).

The shot goes on the bus as actor and `device` `autochip`, numbered from 1
in this run of flighthook (a request that fails takes no number), after a
[`device_telemetry`](#device_telemetry) that carries only `source`
`"virtual"`, with the shot lifecycle a launch monitor emits:
[`shot_trigger`](#shot_trigger),
[`launch`](#launch) with `derived` simulated as for a shot in
chipping mode (also with `[simulation] mode = "off"`: the simulated carry and
total are the chip's only results), and [`shot_finished`](#shot_finished)
(its `raw` has `transport` `"none"`). So it appears in
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
`flight`, `club`, `impact` or `group`, and `raw` shows what the device sent.

Camera clips are never exported: `data` carries no `video`, and nothing
names a camera. Swing video shows the player; it stays on the computer
running flighthook.

**JSONL**: one `ShotExport` object per line (pretty-printed here):

```json
{
  "schema": 3,
  "flighthook_version": "0.1.25",
  "exported_at": "2026-10-25T21:04:11.123Z",
  "time": "2026-10-25T20:58:03.412Z",
  "actor": "mevo.0",
  "shot_id": "550e8400-e29b-41d4-a716-446655440000",
  "shot_number": 7,
  "device": { "kind": "flightscope", "manufacturer": "FlightScope", "model": "Xi", "firmware": "1.23 (Feb 3 2024)" },
  "club_selected": "8I",
  "group": "6f1c2a3b-4d5e-4f60-8a71-92b3c4d5e6f7",
  "settings": {
    "device": { "ball_type": 1, "track_pct": 80.0, "environment": "limited_flight" },
    "simulation": { "altitude": "0ft", "temperature_c": 25.0, "surface": "fairway" },
    "detection_mode": "full",
    "handed": "rh"
  },
  "data": {
    "actor": "mevo.0",
    "shot_number": 7,
    "shot_id": "550e8400-e29b-41d4-a716-446655440000",
    "club_selected": "8I",
    "group": "6f1c2a3b-4d5e-4f60-8a71-92b3c4d5e6f7",
    "handed": "rh",
    "launch": { "launch_speed": "37.9mps", "launch_azimuth": -3.1, "launch_elevation": 23.9, "backspin_rpm": 6897, "sidespin_rpm": 0, "total_spin_rpm": 6897, "spin_axis": 0.0 },
    "flight": { "basis": "modeled", "carry_distance": "95.2m" },
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

- `schema`: export format version (`3`). Bumped when a field changes meaning.
  Schema 3: the raw records of a device on a device driver (the
  FlightScope, the R10, the Square Golf, the ExPutt) carry `label` instead
  of `type`, `src`, `dst` and `channel`, one record per protocol message
  (the R10: one message per record, not one BLE chunk; the ExPutt: each
  message of the session as its JSON text, not the putt's values re-joined;
  the FlightScope: no device free text or debug output and nothing of the
  handshake). Schema 2 has the launch and the device's flight values in
  `data.launch` and `data.flight`; a schema 1 line has both in one
  `data.ball` block. The `flighthook` crate's `ShotExport` reads schema 1
  into the schema 2 shape (`ball`'s flight values become a `flight` with
  `basis` `"modeled"`) and schema 2 and 3 lines as they are.
- `time`: when `shot_trigger` reached the web layer (host clock, RFC 3339 UTC).
- `device`: `kind` (`flightscope`, `square_golf`, `garmin_r10`, `exputt`,
  `trackball`, `openconnect`, `mock`) plus manufacturer, model and firmware as the device reported them.
  FlightScope: `model` is the product (`Mevo+`, `Mevo Gen2`, `Mevo` when the
  generation is unknown, or `Xi`) and `firmware` the device's version and
  build date, e.g. `"1.23 (Feb 3 2024)"`.
- `club_selected`: the club in play when the shot was triggered (the last
  club selected on the bus, e.g. by GSPro or the club picker; `PT` for an
  ExPutt shot); absent when unknown.
- `settings.detection_mode`, `settings.handed`: game state when the shot was
  triggered; absent when unknown.
- `group`: id of the [set](#get-apishotsgroups) the shot joined (a UUID,
  unique across restarts); absent when the shot has no data.
- `settings.device`: the device's config section with `name`, `address`,
  `bind`, unset fields and device driver settings at their default removed. `settings.simulation`: the `[simulation]`
  section, with the same shape as in [GET /api/settings](#get-apisettings)
  (`mode` present unless `"on"`). `data.derived.replaces_measured` marks a
  shot whose simulated results replace the device's, and
  `data.derived.spin_estimated` one whose spin flighthook estimated.
- `data`: exactly the `/api/shots` element for this shot (`ShotData`),
  including `metrics` and `provenance`.
  `data.shot_id`, `data.club_selected` and `data.group`, when set, equal the
  top-level fields, and `data.handed` equals `settings.handed`.
- `raw`: `ShotRaw` (see [shot_finished](#shot_finished)).

**CSV**: one header row, one row per shot, RFC 4180 quoting, empty cell for a
missing value. Units are fixed per column and named in the header (imperial:
mph, yd, ft for heights, in for club offset/height/low point; metric: mps,
m). Face impact is always mm. `environment` is the FlightScope environment in
effect (configured value, else the default). `group` is the set id (empty when
the shot has no data). Unprefixed columns are the device's values (`launch`,
`flight`, `club`, `impact`); `sim_*` columns are the `derived` block and
`metrics_*` columns the [shot metrics](#shot-metrics). With `[simulation]
mode = "force"`, a shot can have both a device column (`carry_*`) and its
`sim_*` column filled; the CSV does not say which one takes precedence (the
JSONL's `data.derived.replaces_measured` does). `spin_estimated` is `true`
when `backspin_rpm` and `sidespin_rpm` are flighthook's estimate
(`derived.spin_estimated`), else empty. Read columns by header name:
columns are added over time.

```
time,actor,device_kind,manufacturer,model,firmware,shot_number,shot_id,
club_selected,group,handed,detection_mode,environment,
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
metrics_spin_loft_deg,metrics_curve_yd,spin_estimated
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
  "launch": {
    "launch_speed": "67.2mps",
    "launch_azimuth": -1.3,
    "launch_elevation": 14.2,
    "backspin_rpm": 3200,
    "sidespin_rpm": -450
  },
  "flight": {
    "basis": "modeled",
    "carry_distance": "180.5m",
    "max_height": "28.3m",
    "total_distance": "195.0m"
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
  "launch": {
    "launch_speed": "150.3mph"
  },
  "flight": {
    "basis": "modeled",
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

Emits `find_mat` (`{"kind":"find_mat","target":"exputt.0"}`, actor `web`) on
the bus. A connected ExPutt then reports `ready` false and `device_state`
`finding mat`, `mat found` and `ready` as the device works. An ExPutt acts on
it once connected (one sent while it connects runs once the session is up);
one sent while it waits to reconnect is dropped.

---

### POST /api/actors/{id}/reconnect

Stop a launch monitor or output, wait for its device or connection to close,
and start it again from the saved settings: what a flighthook restart does for
that one device. Every other actor, the shot history, the sets and the game
state are left alone. The Reconnect button on the Telemetry side of each
Launch monitors and Outputs card sends it.

`id` is the actor ID (`mevo.0`, `square.0`, `openconnect_server.0`,
`openconnect_client.1`, ...); `gspro.N` names `openconnect_client.N`. No
request body.

**Response** `200 OK` once the new instance has started:

```json
{ "restarted": ["mevo.0"] }
```

The card then shows the usual statuses as it connects (`starting`, then
`connected`).

`202 Accepted` with `{ "restarted": [] }` when the actor starts again by
itself later:

- its old connection has not closed in time, or is still closing from an
  earlier request (see below); the actor's `error` telemetry says
  `"The previous connection has not closed yet. It reconnects when it does."`;
- another save or reconnect is restarting it already;
- SystemActor has not finished within 22 s (see
  [config timeout](#config-timeout)). Until it has, the actor counts as in
  flight, so another request for it is `409`.

It has started again when the actor reports `starting`, or a
`config_outcome` lists it in `restarted` (the one without `request_id`
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
a mock or a Trackball without the grab still
closing after that is replaced anyway (it holds nothing the new one needs and
closes itself); so is an Open Connect server once its port is free. A Garmin
R10, a Square Golf, an ExPutt or a Trackball holding the grab is not: the request answers `202`, the
actor's `error` telemetry and an Error alert say the previous connection has
not closed yet, and the actor starts again by itself once it has. Its `error` is
cleared then, and a `config_outcome` without `request_id` lists it in
`restarted`. Pressing Reconnect again meanwhile starts nothing.

An Open Connect client that is reconnected starts its `ShotNumber` count
over, as after a flighthook restart.

**Side effects**: emits a `config_command` with action
`{ "type": "reconnect", "id": "mevo.0" }`, processed by SystemActor, which
answers with a `config_outcome` listing the actor in `restarted`, or with
`restarted` empty when it did not start it.

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
  "shot_grouping": "by_club",
  "simulation": {
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
      "grab": false,
      "tuning": {
        "full_power_speed": 40.0
      }
    }
  },
  "mock_monitor": {},
  "openconnect_server": {
    "0": {
      "name": "Open Connect Server",
      "bind": "0.0.0.0:921"
    }
  },
  "openconnect_client": {
    "0": {
      "name": "GSPro",
      "address": "127.0.0.1:921"
    }
  },
  "random_club": {},
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
  "player": {
    "sex": "female",
    "height": 168.0
  },
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

- Keys are type-prefixed global IDs: `mevo.0`, `r10.0`, `square.0`, `exputt.0`, `trackball.0`, `mock_monitor.0`,
  `openconnect_server.0`, `openconnect_client.0`, `random_club.0`, `webserver.0`,
  `camera.0`
- `shot_grouping`: the Shots tab's default view, [`ShotGrouping`](#shotgrouping),
  `"by_set"` by default. It does not change how
  [sets](#get-apishotsgroups) are assigned. Omitted while at the default
- `simulation` (beta) holds the settings for simulated values: `mode`
  ([`SimMode`](#simmode), default `"on"`, omitted while `"on"`), `altitude`
  (unit-tagged distance, default `"0ft"`, valid 0 to 5000 m), `temperature_c`
  (default `25.0`, valid -30 to 50) and `surface` ([`SimSurface`](#simsurface),
  default `"fairway"`) and `auto_chip` (boolean, default `false`, omitted
  while `false`): whether the dashboard offers [Auto-chip](#post-apiauto-chip)
  and `POST /api/auto-chip` accepts a chip. Off by default; turn it on in
  Settings → Simulation (useful with radar launch monitors that miss short chips). With `"mode": "force"`, every simulated shot's results
  (carry, apex, flight time, descent, offline, roll, total) come from the
  simulation for every device, and its `derived` block carries all of them
  with `replaces_measured: true`; the device's own results stay under `flight`.
  Launch values are never replaced. `simulation` is **omitted while every
  setting is at its default**; an absent `simulation` means the defaults.
  Every key is optional when posting. An out-of-range altitude or temperature
  turns simulation off until it is fixed. Changes apply from the next shot
  without restarting any actor
- All launch monitor config fields are optional (omitted = use defaults)
- R10 sections have `name` and an optional `range`: the distance from the
  device to the ball (unit-tagged, e.g. `"7ft"`), sent to the R10 as its tee
  distance once it wakes up; when absent the device keeps the distance it
  has. No address (BLE auto-discovery). `range` is the R10 driver's setting:
  it must be greater than 0 (a save with `"0ft"` or a negative distance is
  `400` `"r10.0: range: must be a distance greater than 0"`; a file holding
  one loads, and that card does not start and shows the error). A changed
  range restarts the card
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
- `use_estimated` on Mevo sections controls whether estimated (E8) ball flights
  are emitted when no full (D4) result arrives (defaults to `false`). With it
  off, a shot the device only estimated (e.g. a flight too short to read in
  Outdoor mode, or a very short chip) is skipped
- `camera_mode` on Mevo sections is `standard` (default), `fusion`, or
  `raw_fusion`. The Fusion modes request the device's club-data processing and
  are omitted from the response when unset
- `environment` on Mevo sections is `limited_flight` (default; detection mode 9,
  for nets and simulator screens) or `outdoor` (mode 8, full flight). It sets
  the full-swing detection mode (Mevo+ and Xi). Omitted from the
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
  A change restarts the card
- `trackball` sections (alpha) have `name` and the trackball driver's
  settings, each omitted while at its default: `device` (the input to read:
  a `/dev/input/by-id` name, a `/dev/input` path or the name the device
  reports; absent or blank uses the only readable trackball or mouse),
  `club` (the club in play until the game names one, an FRP club code,
  default `"7I"`), `hand` (`"rh"` | `"lh"` until the game names one,
  default `"rh"`), `cpi` (counts per inch of ball travel, default `400.0`),
  `report_limit` (the largest movement one report carries, default `127`),
  `grab` (an exclusive grab, so the desktop pointer does not follow the
  ball; default `true`, and the card writes `false` for a new Trackball), a
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
  change of `device`, `cpi`, `report_limit` or `grab` restarts the card; the
  rest applies to the next swing. No `impact_lead_s`: the cameras cut no clip
  for a Trackball shot
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
  `exputt`, `trackball`, `mock_monitor`, `openconnect_server`, `camera`,
  `openconnect_client`, `pureplay`, `random_club`) takes `enabled`: `true`
  (the default) runs its actor, `false` turns the card off, keeping its
  section and settings. Omitted while `true`, so a config written before the
  field existed is unchanged; only `enabled = false` is written. Web server
  sections have no `enabled`. See [Turning a card off](#turning-a-card-off)
- Every launch monitor section (`mevo`, `r10`, `square`, `exputt`,
  `mock_monitor`, `openconnect_server`) takes an optional `impact_lead_s`:
  seconds, 0 to 5, from the ball's impact to the monitor's `shot_trigger`,
  used to place the cameras' clips (beta). Omitted when unset, which uses the
  device's default (FlightScope, ExPutt and mock 0, Square 1, R10 2, Open
  Connect 2.5, any other 1.5). The ExPutt dates its trigger at the strike,
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
  The device must be in its mobile mode on the same network. Its shots are
  putts: club `PT` and Putting mode whatever club is in play, vertical
  launch 0, no spin
- `dot_bottom_to_face_center_mm` on Square sections overrides allsquare's built-in
  per-club face impact calibration (distance from the bottom edge of the
  sticker's dot down to face center, in mm). Keyed the same way `club` is. Omitted when no
  overrides are set. The spelling `dot_bottom_to_face_centre_mm` is also read (in requests and
  config files); responses and saved files use `dot_bottom_to_face_center_mm`
- `openconnect_server` sections are **launch monitors**, not integrations: they
  listen (`bind`, default `0.0.0.0:921`) and accept inbound shots from any
  monitor that speaks GSPro Open Connect V1 as a client — Uneekor, Foresight,
  SkyTrak, MLM2PRO. This is the inverse of `openconnect_client`, which dials a
  simulator. GSPro listens on 921 too, but moving GSPConnect to 922 (set
  `<OpenAPIUseAltPort>true</OpenAPIUseAltPort>` in
  `C:\GSPro\GSPC\GSPconnect.exe.config`) frees 921 so both can share one host
- `openconnect_server` sections answer each message with a `200` and tell the
  monitor the player's hand and club as GSPro's player information (`201`,
  `Handed` `RH`/`LH`, `Club` GSPro's code such as `W3`, `I7`, `PT`) once the
  club is known: after the monitor's first message and on every change. The
  hand is left out until it is known, and the mini driver goes out as `DR`
- `openconnect_client` sections are **outputs**: each dials `address` (default
  `127.0.0.1:921`), a simulator that accepts GSPro Open Connect V1 — GSPro
  and Pureplay on 921, Muni Golf on 9210 — and sends it every shot. `full_monitor`,
  `chipping_monitor` and `putting_monitor` take a launch monitor's actor ID
  and accept shots in that mode from it alone; absent accepts any monitor.
  [Auto-chip](#post-apiauto-chip) shots are accepted whatever they name.
  Each shot's `ShotNumber` is its `shot_number`, raised when needed to one
  more than the last one the client sent, so the numbers a simulator gets
  always increase, across devices, Auto-chip shots and device reconnects.
  The count starts over when the client itself restarts: a
  [Reconnect](#post-apiactorsidreconnect) of it, a save that restarts it, or
  a flighthook restart
- `pureplay` sections (alpha) are **outputs**: each dials `address` (default
  `127.0.0.1:9760`), Pureplay, and sends it every shot over Pureplay
  Connect, numbered from 1 on each connection. `full_monitor`,
  `chipping_monitor` and `putting_monitor` route as for `openconnect_client`,
  and also pick the launch monitor whose ready state, connection and battery
  Pureplay is told. Pureplay's club and hand commands set flighthook's club
  and hand ([`club_info`](#club_info), [`hand_info`](#hand_info) from actor
  `pureplay.N`). See [Pureplay](outputs/pureplay.md)
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
  [`shot_video_removed`](#shot_video_removed) names the shots that lost
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
  A `golfer_height` in the section is accepted and read as `player.height`
  when the config has none; responses never carry it
- `player` holds facts about the player, each omitted while at its default,
  and the section with them: `sex` (`"male"`, the default, or `"female"`:
  whose tour averages the swing is compared with) and `height` (not set by
  default; the player's height in centimeters, 100 to 250, for the swing
  analysis' distances in centimeters). Values apply from the next swing
  analysis, without restarting anything. A save with a height out of range
  is `400`; a file holding one loads with a warning in the log, and no
  height is used. A config file with `body_tracking.golfer_height` loads it
  as `player.height` and is rewritten without it (the original kept beside
  it as a backup)
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

Config update via event-sourced bus pattern. Emits a `ConfigCommand`
on the bus, waits for `ConfigOutcome` from SystemActor, then returns the response.

**Request**: complete `FlighthookConfig` JSON (same shape as GET response)

**Query parameters**:

- `scope` (optional): a global actor ID (`mevo.0`, `openconnect_client.1`,
  `pureplay.0`, `webserver.0`), `global`, `body_tracking` or `player`. `gspro.N`
  is accepted for `openconnect_client.N`.
  - An actor ID narrows the save to that one actor — only its section is
    written to the config, and only that actor is reconciled. Every other
    section of the live config is left as-is, whatever the request body
    carries for it. A `scope` naming a section the body omits removes that
    actor.
  - `global` writes only the global settings (`default_units`,
    `chipping_clubs`, `putting_clubs`, `shot_grouping`, `simulation`,
    `replay`, `body_tracking`, `player`) from the body; every actor section of the live config is kept, whatever the body
    carries for it. Every actor is reconciled.
  - `body_tracking` writes only the body's `body_tracking` section; every
    other setting of the live config is kept, whatever the body carries for
    it. No actor is reconciled (body tracking is read live). The UI saves
    this way from the model download prompt: when body tracking is turned on
    from `off`, and when the prompt's Cancel or Turn off sets it back to `off`.
  - `player` writes only the body's `player` section the same way; no actor
    is reconciled.
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
with overlapping bind addresses (`0.0.0.0` overlaps all). A section that is
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
| `400 Bad Request` | `scope` is not `global`, `body_tracking`, `player` or a known actor ID |
| `400 Bad Request` | the save removes the web server (`"the web server can't be removed"`) or adds a second one (`"flighthook runs one web server; edit the existing one"`) |
| `400 Bad Request` | a Mevo section it writes has a `launch_only_spin` below 0, above 1000 or not finite (`"mevo.0: launch_only_spin must be 0 to 1000 rpm per mph, not -5"`) |
| `400 Bad Request` | a launch monitor section it writes has an `impact_lead_s` below 0, above 5 or not finite (`"square.0: impact_lead_s 6 is outside 0 to 5 s"`) |
| `400 Bad Request` | a camera section it writes has a `latency_s` below 0, above 10 or not finite (`"camera.0: latency_s 11 is outside 0 to 10 s"`) |
| `400 Bad Request` | a section it writes changes its device driver settings into ones the driver refuses, named by card and field (`"mock_monitor.0: shot_interval_s: must be 0 to 86400"`, `"r10.0: range: must be a distance greater than 0"`, `"mevo.0: track_pct: must be 0 to 100"`); a section whose saved driver settings are refused still saves when it keeps them as they are (its name, `enabled`, `impact_lead_s`, ...) |
| `400 Bad Request` | a camera section it writes has a `rotation_deg` other than 0, 90, 180 or 270 (`"camera.0: rotation_deg 45 is not 0, 90, 180 or 270"`), or a `yaw_deg` or `pitch_deg` outside -30 to 30 or not finite (`"camera.0: yaw_deg 40 is outside -30 to 30"`) |
| `400 Bad Request` | a camera section it writes has no `camera` (`"camera.0: no camera picked"`) or a `mode` that is neither `auto` nor `WIDTHxHEIGHT@FPS` |
| `400 Bad Request` | a whole-config or `global` save has a `replay` value out of range (`"replay: keep_shots 0 is outside 1 to 1000"`) |
| `400 Bad Request` | a whole-config, `global` or `body_tracking` save has a `body_tracking` value out of range (`"body_tracking: min_score 2 is outside 0 to 1"`) |
| `400 Bad Request` | a whole-config, `global` or `player` save has a `player.height` outside 100 to 250 or not finite (`"player: height 99 is outside 100 to 250 cm"`) |
| `409 Conflict` | the save turns on a section, or adds one that is on, at an address an enabled section of the same type has (`"An Open Connect card already uses 127.0.0.1:921"`); nothing is saved |
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

**Side effects**:

- Emits `ConfigCommand` on the bus, processed by SystemActor. Without `scope`
  the action is `ReplaceAll`; with `scope=global`, `ReplaceGlobal`; with
  `scope=body_tracking`, `SetBodyTracking`; with `scope=player`,
  `SetPlayer`; with an
  actor ID, the matching `Upsert*` variant for that section (or `Remove` when
  the body omits it)
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
sends an array of supported versions; the server accepts its own FRP version
when the array has it. If not, the server sends a `critical` alert and closes
the connection.

flighthook speaks FRP `0.2.0` (the `flightrelay` crate's `SPEC_VERSION`), and
only that version.

### Init Handshake (FRP-compliant)

Before streaming begins, the client must complete a handshake:

1. **Client sends** a `start` message:

```json
{
  "kind": "start",
  "version": ["0.2.0"],
  "name": "My Dashboard"
}
```

- `kind` (required): must be `"start"`
- `version` (required): array of supported FRP versions
- `name` (optional): human-readable client identifier for server-side logging.
  Defaults to `"anonymous"` if empty or omitted.
- `raw_frames` (optional): ignored. flighthook sends no `raw_frame` events.

2. **Server responds** with an `init` message:

```json
{
  "kind": "init",
  "version": "0.2.0",
  "actor_id": "ws.a1b2c3d4",
  "global_state": {
    "player_info": null,
    "club_info": {
      "club": "DR"
    },
    "mode": "full",
    "handed": "lh"
  }
}
```

- `kind`: `"init"` — FRP handshake response
- `version`: the FRP version selected by the server
- `actor_id`: unique identifier for this WebSocket session (`ws.{8-hex-chars}`)
- `global_state`: current snapshot of shared state
  - `player_info`: `{ "name": "Player 1" }` or `null`
  - `club_info`: `{ "club": "DR" }` or `null`
  - `mode`: the detection mode or `null`
  - `handed`: the player's hand (`"rh"` | `"lh"`) or `null`

3. **Server replays** the last `actor_status` of every actor and one
   `device_telemetry` per device with its current state (every identity
   field, `source` and `telemetry` key at its latest value, however many
   messages the device sent them in), then sends the current
   [`round_state`](#round_state), active or not.

4. **Server streams** `FlighthookMessage` events (described below).

Messages sent before the `start` handshake (except `close`) are ignored.

### Forwarding

flighthook forwards its devices' shots to every `/frp` client, the
dashboard included, with FRP 0.2.0's forwarding rules:

- A shot event (`shot_trigger`, `launch`, `flight_result`, `club_path`,
  `face_impact`, `shot_finished`) goes out once per device, shot and kind
  within 45 s, flighthook's accumulator timeout; a repeat is dropped.
- `device_telemetry` goes out when it changes something the device last
  reported. The replay on connect is the device's current state (every
  field and key at its latest value) and always goes out. When the device's
  `actor_status` leaves `connected`, this is forgotten: its next session's
  telemetry goes out in full.
- Alerts and flighthook's extension events always go out, a device's
  repeated alert too: each one is raised anew by flighthook, which takes no
  FRP events in.
- `launch` holds only measured values: a value flighthook estimated (the
  spin of a [launch-only chip](devices/flightscope.md#launch-only-chips-beta),
  `derived.spin_estimated`) is left out of it, and goes out in flighthook's
  own `flight_result` instead (next point).
- For a shot whose device sent no `flight_result`, flighthook sends its own
  when it simulated the shot: just before the shot's `shot_finished`, with
  `basis` `"modeled"`, `model` `"flighthook simulation (dimple)"`, the
  simulated flight values, and in `assumed` the launch values it estimated.
  See [flight_result](#flight_result).

`/api/shots` and the export serve the shot as flighthook holds it: the
estimated spin in `launch`, marked by `derived.spin_estimated`, and the
simulated values in `derived`.

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

- `actor`: global ID of the originator (e.g. `"mevo.0"`, `"openconnect_client.0"`, `"system"`).
  Flighthook extension field (FRP consumers ignore unknown fields per spec).
- `device`: FRP device identifier (e.g. Mevo WiFi SSID). Present on shot
  lifecycle and device telemetry events; absent on system/config events.
- `raw_payload`: optional. Binary payloads are lowercase hex strings (no spaces).
  Text payloads (e.g. GSPro JSON) are included as-is. Omitted when not applicable.
- `provenance`: on a `shot_trigger` only: what stands behind the shot it
  starts (see [Provenance](#provenance)), set by flighthook from the type of
  the actor that emits it. Omitted on every other message, and on triggers
  from a source flighthook does not know. Flighthook extension field.
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
- `timestamp` (optional): when the device detected the strike, milliseconds
  since the Unix epoch (UTC). The mock launch monitor sends one; the
  FlightScope sends the moment its driver saw the device's trigger (the
  same moment the trigger reaches the bus); the Square Golf sends the
  arrival of the ball notification; the ExPutt sends the
  arrival of the device's strike message (when it reported one since it was
  last ready), and its trigger comes with the putt's result, seconds after
  that time. The cameras date a trigger by it.

A shot that has not finished within 45 s of its trigger (flighthook's
accumulator timeout, longer than any device driver's own deadline for a
shot) is dropped by flighthook's shot history and outputs; its later events
are still forwarded on this stream.

---

##### launch

The ball's launch, as the device measured it. May arrive before or after
`flight_result` and `club_path`.

```json
{
  "actor": "mevo.0",
  "device": "FS-M2-XXXXXX",
  "event": {
    "kind": "launch",
    "key": { "shot_id": "550e8400-...", "shot_number": 42 },
    "launch": {
      "launch_speed": "67.2mps",
      "launch_azimuth": -1.3,
      "launch_elevation": 14.2,
      "backspin_rpm": 3200,
      "sidespin_rpm": -450
    },
    "derived": {
      "roll_distance": "11.2m",
      "roll_offline": "-0.9m",
      "total_distance": "191.7m",
      "total_offline": "-8.5m"
    }
  }
}
```

All `Launch` fields are `Option`. Missing fields are omitted from the JSON.
`launch` is FRP's `Launch`:

| Field | Type | Meaning |
| --- | --- | --- |
| `launch_speed` | Velocity | Ball speed at launch |
| `launch_azimuth` | number (deg) | Horizontal launch angle, + right of the target line |
| `launch_elevation` | number (deg) | Vertical launch angle |
| `backspin_rpm` | integer | Backspin, + = backspin, − = topspin |
| `sidespin_rpm` | integer | Sidespin, + = curves the ball right |
| `total_spin_rpm` | integer | Total spin, never negative |
| `spin_axis` | number (deg) | Spin axis tilt, + = tilted right (curves the ball right); absent when the total is 0 |

Lateral values are absolute: + is right of the target line for every golfer.
On the bus and in `/api/shots`, the spin of a shot flagged
`derived.spin_estimated` is flighthook's estimate; on `/frp` such a spin is
left out of `launch` (see [Forwarding](#forwarding)).

##### flight_result

The ball's flight: where it landed, where it came to rest, and how it flew.
At most one per shot; may arrive before or after `launch` and `club_path`.

```json
{
  "actor": "mevo.0",
  "device": "FS-M2-XXXXXX",
  "event": {
    "kind": "flight_result",
    "key": { "shot_id": "550e8400-...", "shot_number": 42 },
    "flight": {
      "basis": "modeled",
      "carry_distance": "180.5m",
      "carry_offline": "-7.6m",
      "max_height": "28.3m",
      "flight_time": 6.4,
      "descent_angle": 48.2
    }
  }
}
```

`flight` is FRP's `FlightResult`. `basis` is required: `"tracked"` (the
device followed the ball's flight) or `"modeled"` (a flight model produced
the values, the device's own included). Every other field is optional:

| Field | Type | Meaning |
| --- | --- | --- |
| `basis` | string | `"tracked"` or `"modeled"` |
| `model` | string | Name of the flight model, when known |
| `carry_distance` | Distance | Tee to landing |
| `carry_offline` | Distance | Landing point right (+) or left (−) of the target line |
| `total_distance` | Distance | Tee to rest |
| `total_offline` | Distance | Rest position right (+) or left (−) of the target line |
| `roll_distance` | Distance | Landing to rest |
| `max_height` | Distance | Apex |
| `flight_time` | number (s) | Launch to landing |
| `descent_angle` | number (deg) | Angle below horizontal at landing |
| `assumed` | object | Launch values a `modeled` result assumed because the shot's `launch` lacks them (same fields as `launch`) |

`carry_offline` and `descent_angle` are absent for a ball that did not fly.
A device's flight values go out with `basis` `"modeled"`: FlightScope's come
from its own flight model, and an Open Connect monitor's carry is not known
to be tracked. A device that sends no flight values (R10, Square Golf,
ExPutt) sends no `flight_result`; nor does a FlightScope launch-only chip.

flighthook adds its own `flight_result` on `/frp` for a shot whose device
sent none and that it simulated (see [Forwarding](#forwarding)): `"basis":
"modeled"`, `"model": "flighthook simulation (dimple)"`, the `derived`
flight values, and the estimated spin of a launch-only chip in `assumed`:

```json
"flight": {
  "basis": "modeled",
  "model": "flighthook simulation (dimple)",
  "carry_distance": "11.9m",
  "carry_offline": "-0.2m",
  "total_distance": "14.6m",
  "total_offline": "-0.2m",
  "roll_distance": "2.7m",
  "max_height": "1.6m",
  "flight_time": 1.1,
  "descent_angle": 36.1,
  "assumed": { "backspin_rpm": 1570, "sidespin_rpm": 0 }
}
```

It is never on the bus or in `/api/shots`, which keep the simulated values in
`derived`.

Which launch and flight fields each device sends:

| Device | `flight.carry_offline` | `flight.descent_angle` | `launch.total_spin_rpm` + `launch.spin_axis` |
| --- | --- | --- | --- |
| FlightScope full result (`D4`) | yes (landing position) | yes (landing velocity) | yes, with the components; an Xi's **Spin source (Xi)** on E8 sends the early result's total with D4's axis, or the total alone, no components, when the shot has no club data |
| FlightScope estimate (`E8`) | yes | no | no (backspin only) |
| Garmin R10 | no | no | yes, with the components |
| Square Golf Omni | no | no | yes, with the components |
| Open Connect | no | no | yes: the pair the monitor sent, and the same spin in the other pair |

`total_offline`, `club_path.spin_loft` and `club_path.low_point_distance` are
passed through by flighthook but no supported device reports them. Values a
device doesn't send, flighthook may compute from the ones it does: see
[Shot metrics](#shot-metrics), on the finished shot.

**`derived` (beta)** is a flighthook extension: values simulated from the
measured launch with the [dimple](https://crates.io/crates/dimple) ball-flight
model, for fields the device did not report. Every field under `launch` is
measured and every field under `flight` is the device's; every field under
`derived` is simulated. A measured
`carry_offline` or `descent_angle` keeps the field out of `derived` (except
under `force`). A field is in both only
when the `derived` value is the one to show: a measured carry or flight time
of exactly 0 on a shot that flies is not a reading, so `derived` carries the
simulated value next to the untouched 0, and a ground ball (below) shows 0
for any carry, apex or flight time the device measured other than exactly 0. With
`[simulation] mode = "force"`, `derived` carries
every result field whether or not the device measured it, plus
`"replaces_measured": true`: a consumer shows the `derived` values in place of
the device's, which stay unchanged under `flight`. FRP consumers ignore
`derived` as an unknown field. It is absent when `[simulation] mode` is
`"off"` or when the launch can't be simulated — including a shot with no
spin reading (back and side both 0) — see the README's "Simulated values"
section for the rules, except that a shot whose spin flighthook estimated
always has one: `"derived": {"spin_estimated": true}` when it was not
simulated, and so does a shot with `computed` values (`"derived":
{"computed": [...]}`). It is never sent to GSPro.

`"spin_estimated": true` marks the spin under `launch` (`backspin_rpm`,
`sidespin_rpm`) as flighthook's estimate rather than the device's reading:
a FlightScope result with launch data but no spin
([Launch-only chips](devices/flightscope.md#launch-only-chips-beta)). The
spin stays under `launch` and is sent to GSPro like a measured one; `/frp`
sends it in `assumed` instead ([Forwarding](#forwarding)).

`"computed"` lists the values under `launch` (or the shot's face impact) that
come from a model (aerodynamic, flight or otherwise) rather than from the
device's readings; they stay where they are and go to GSPro as before. A
plain translation of a reading is not computed: the other spin pair worked
out from the one a device reports (the Garmin R10's back and side spin, the
Open Connect input's completed pair, the Xi's E8 total split by its axis), a
unit or sign conversion, or a calibration offset (the Square Golf Omni's
vertical face impact). Names are FRP field names: `launch_elevation`,
`backspin_rpm`, `sidespin_rpm`, `total_spin_rpm`, `spin_axis`, and
`impact_vertical` for the face impact's `vertical`. No supported device
sets it. A tournament round leaves these out, and with `spin_estimated` all
four spin values (Pureplay's strict mode).

A value a device does not measure is left out, never sent as a placeholder:
the ExPutt sends no `launch_elevation`, and an Xi shot whose E8 total has no
axis to go with it sends `total_spin_rpm` alone. An output that needs the
field fills its own default, and a lone total goes out as backspin, with no
sidespin or axis made up: GSPro and the Open Connect clients get a VLA of 0
and such a total as backspin with sidespin and axis 0; Pureplay sends it as
TotalSpin and BackSpin with SideSpin and SpinAxis clear; the CSV puts it in
`backspin_rpm` (and `total_spin_rpm`) with `sidespin_rpm` and
`spin_axis_deg` empty; the dashboard shows it as backspin with no sidespin;
the simulation flies it as backspin.

A ground ball is rolled, not flown: a shot whose measured apex is 0, or,
from a device that reports no apex, whose launch angle is at most 0.5° or
that reports neither a launch angle nor any spin (the ExPutt), whatever the
device, club or detection mode. Carry, apex and flight time are
0 (the device's 0s stay measured), there is no `descent_angle`, and roll and
total come from a roll from the tee, from the launch speed and direction (a
measured total, or a measured roll as the total, wins). With a carry of 0 the
roll is the total, so a measured roll that differs from it is replaced in
`derived`. It rolls on a green
when the shot was taken in Putting mode (the configured `surface` when that
is a green, else `"green"`) and on the configured `surface` otherwise.

A launch-monitor-only device such as the Square Golf Omni gets the full set:

```json
"launch": {
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
  "roll_distance": "4.9m",
  "roll_offline": "-0.5m",
  "total_distance": "160.4m",
  "total_offline": "-7.0m"
}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `carry_distance` | Distance | Tee to landing, horizontal. Only when the device didn't report carry (or reported 0 on a shot that flies, or other than exactly 0 on a ground ball), or `replaces_measured` |
| `carry_offline` | Distance | Landing point right (+) or left (−) of the target line. Only when the device didn't report it (or reported a ground ball's as other than exactly 0), or `replaces_measured` |
| `max_height` | Distance | Apex above the tee. Only when the device didn't report it (or reported a ground ball's apex as not exactly 0), or `replaces_measured` |
| `flight_time` | number (s) | Launch to landing. Only when the device didn't report it (or reported 0 on a shot that flies, or other than exactly 0 on a ground ball), or `replaces_measured` |
| `descent_angle` | number (deg) | Angle below horizontal at landing. Only when the device didn't report it, or `replaces_measured`. Absent for a ground ball |
| `roll_distance` | Distance | Landing to rest. Negative when the ball spins back. Only when the device didn't report roll (or reported a ground ball's roll other than its total), or `replaces_measured` |
| `roll_offline` | Distance | Sideways movement from the displayed landing (the measured `carry_offline` when there is one) to rest, + right |
| `total_distance` | Distance | Tee to rest. Only when the device didn't report total, or `replaces_measured` |
| `total_offline` | Distance | Rest position right (+) or left (−) of the target line |
| `path` | FlightPath | Simulated ball path, tee to rest (see below) |
| `replaces_measured` | bool | `true` when these values replace the device's results (`[simulation] mode` was `"force"` when the shot was simulated): every result field is simulated and takes precedence over `flight`. Omitted when false |
| `spin_estimated` | bool | `true` when `launch.backspin_rpm` and `launch.sidespin_rpm` are flighthook's estimate, not measured. Set with or without the simulation. Omitted when false |
| `computed` | array of string | The measured-block values that come from a model (see above). Set with or without the simulation. Omitted when empty |

Every field is optional. Offline values are absolute: + is right of the target
line for every golfer, as in `launch_azimuth`. Where the device measures carry
(or roll), `total_distance` is that measured carry plus the simulated roll;
where it measures carry and total but not roll, `roll_distance` is the total
minus the carry. Either way carry + roll = total holds for the values a
consumer displays. With
`replaces_measured`, carry, roll and total are all simulated. Roll, roll
offline, total and total offline are omitted if the ground simulation does
not reach rest. Roll offline and total offline are also omitted when the
device measured a carry offline that the path cannot be fitted to (for
example one not inside the carry), since there is then no rest point that
lands on it.

**`path`** is the simulated ball's trajectory: an object with `flight` (tee to
landing) and `ground` (landing to rest, empty when the ground phase doesn't
reach rest), each a list of `[t, x, y, z]` points — seconds since launch,
meters downrange, meters up, meters right of the target line, origin at the
tee. Always meters and seconds, unaffected by `?units=`. Present only on
shots the server simulated, same conditions as the rest of `derived`. The
path is fitted to the displayed values: the landing is at the carry (measured
or simulated), the highest point at the apex, the landing time at the flight
time and the rest point at the total. When the device measured the carry
offline, the path still leaves the tee along the measured `launch_azimuth`
and lands on that offline: the curve (the sideways deviation from the start
line) is scaled to fit, or, when the simulation barely curves, the missing
sideways distance is added, growing with the square of the distance
downrange. The roll then continues from the landing in the direction the
fitted flight arrives. With `replaces_measured` every displayed
value is simulated, so the path is the simulation unchanged. `ground[0]` equals the last `flight`
point. A ground ball's `flight` is the tee alone (`[[0, 0, 0, 0]]`) and its
`ground` the roll from the tee. The offline values are the `z` of the landing and rest points. A few
dozen points per shot, rounded to centimeters and milliseconds.

---

##### club_path

Club path data available. May arrive before or after `launch`.

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

`club` is FRP's `ClubData`, all fields optional: `club_speed`,
`club_speed_post` (Velocity), `path`, `attack_angle`, `face_angle`,
`dynamic_loft`, `swing_plane_horizontal`, `swing_plane_vertical`,
`spin_loft` (deg), `smash_factor`, `club_offset`, `club_height`,
`low_point_distance` (Distance). `path`, `face_angle` and
`swing_plane_horizontal` are + right of the target line for every golfer.
`spin_loft` is only ever a measured value (no supported device measures it);
the `dynamic_loft − attack_angle` approximation is in
[Shot metrics](#shot-metrics). `low_point_distance` is + on the target side
of the ball, − behind it.

---

##### face_impact

Face impact location, in millimeters from the center of the club face.
Positive `lateral` is toward the toe, positive `vertical` is above center.
Both axes are golfer-relative (toe/heel, high/low) and mean the same for
either hand.

Emitted only by devices whose reading is referenced to the face center. The
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
        { "t_ms": 0, "dir": "rx", "label": "E5 AVR>APP", "hex": "0c42414c4c..." }
      ]
    }
  }
}
```

- `raw` (flighthook extension, omitted when absent): `ShotRaw`, the device
  frames captured for this shot. FRP consumers ignore it.
  - `transport`: `flightscope-tcp`, `square-ble`, `r10-ble`, `exputt-wifi`
    (`text` records: the session's messages as JSON text),
    `openconnect-json`, or `none` (mock).
  - `frames`: `RawRecord[]`, in arrival order:
    - `t_ms`: milliseconds relative to the shot trigger (negative = before it)
    - `dir`: `rx` (device → flighthook) or `tx` (flighthook → device)
    - `type`, `src` / `dst`, `channel`: schema 2 exports only, a FlightScope
      frame's message type (`"0xD4"`), bus addresses (`APP`, `AVR`, `DSP`,
      `PI`) and `tcp`; a schema 3 record carries `label` instead
    - `label`: the device driver's own name for the frame, stored as it gave
      it. FlightScope: `"<type> <src>><dst>"`, the message type in hex and
      the bus addresses (`D4 AVR>APP`, `B0 APP>AVR`), one record per framed
      message (`hex`: the unstuffed payload without header or checksum);
      the device's free text and debug output and the handshake's messages
      are never recorded. R10: `notify <message type>` /
      `write <message type>` (the type in hex), one record per protocol
      message (`hex`: the whole message as the driver framed it). Square
      Golf: `notify 11 02` (a notification's family and type byte in the
      `0x11` family; `notify 91` for the other families, family byte only)
      and `cmd 87` (a command's type byte). ExPutt: the message's name
      (`PuttResult`, `PuttShot`, `PuttReady`, `Test`, `N_OK`, `N_ALIVE`,
      `resyncToMat`, `PM_API_MatchBegin`, ...), both directions, as `text`;
      the putt's own result is the `PuttResult` among its shot's records
    - `hex`: payload as lowercase hex (binary protocols)
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
separately via `actor_status`. No key means two things in the two event
types; a launch monitor on a device driver reports its own state keys here
(the mock's `radar_mode` and `tracking_mode`).

```json
{
  "actor": "mevo.0",
  "device": "FS-M2-XXXXXX",
  "event": {
    "kind": "device_telemetry",
    "manufacturer": "FlightScope",
    "model": "Mevo+",
    "firmware": "1.23 (Feb 3 2024)",
    "source": "measured",
    "telemetry": {
      "ready": "true"
    }
  }
}
```

- `manufacturer`, `model`, `firmware`: optional identity fields (present on first emission)
- `source`: FRP's device source, where the device's shots originate:
  `"measured"` or `"virtual"`, the shot source's [provenance](#provenance),
  set by flighthook from the actor's type on every `device_telemetry` of a
  shot source. Absent (unknown) on telemetry from anything else. Auto-chip
  (device `autochip`) sends one with `source` `"virtual"` alone before each
  chip.
- `telemetry`: optional key/value map. Standard keys: `"ready"`, `"battery_pct"`,
  `"tilt"`, `"roll"`, `"temp_c"`, `"external_power"`

---

##### player_info

Player info update (name). Flighthook extension event.

```json
{
  "actor": "openconnect_client.0",
  "event": {
    "kind": "player_info",
    "player_info": { "name": "Player 1" }
  }
}
```

- `player_info.name`: optional player name string

---

##### club_info

Club selection update. Sources include GSPro's player message (actor
`openconnect_client.N`), Pureplay's club information (actor `pureplay.N`),
[`POST /api/club`](#post-apiclub) and the WebSocket [`club`](#club) command.

```json
{
  "actor": "openconnect_client.0",
  "event": {
    "kind": "club_info",
    "club_info": { "club": "7I" }
  }
}
```

---

##### hand_info

Player handedness update. Every source sets the hand this way: GSPro's player
message (actor `openconnect_client.N`), Pureplay's club information (actor
`pureplay.N`), [`POST /api/hand`](#post-apihand)
(actor `web`) and the WebSocket [`hand`](#hand) command (the socket's actor
ID).

```json
{
  "actor": "web",
  "event": {
    "kind": "hand_info",
    "hand_info": { "handed": "lh" }
  }
}
```

- `hand_info.handed`: `"rh"` | `"lh"`

---

##### round_state

A tournament round started or ended. Emitted by the system (actor
`system`) whenever the round policy changes, and sent to every WebSocket
client after the init handshake. While a round is active, the output holding
it (Pureplay in strict mode) takes only shots a launch monitor measured,
without the values that are not the device's reading; its ready state counts
only measured launch monitors; and Auto-chip and the mock launch monitor make
no shots. The dashboard shows a **Tournament** banner under the header and
disables the Auto-chip chip.

```json
{
  "actor": "system",
  "event": {
    "kind": "round_state",
    "round": { "active": true, "holder": "Pureplay" }
  }
}
```

- `round.active`: whether a round is active
- `round.holder`: the card name of the output holding the round; omitted
  when none is active (`{"active": false}`)

---

##### set_detection_mode

FRP controller command. Sets the shot detection mode, player handedness and/or
club in play. All fields are optional and latched independently — the most
recent value for each field is the active value. Omitting a field does not
reset it. The member order and codes are the spec's.

```json
{
  "actor": "system",
  "event": {
    "kind": "set_detection_mode",
    "mode": "chipping",
    "handed": "rh",
    "club": "SW"
  }
}
```

- `mode` (optional): `"full"` | `"putting"` | `"chipping"`
- `handed` (optional): `"rh"` | `"lh"`
- `club` (optional): a club code, `"DR"` | `"MD"` | `"2W"`–`"9W"` |
  `"1H"`–`"9H"` | `"1I"`–`"9I"` | `"PW"` | `"GW"` | `"SW"` | `"LW"` | `"PT"`.
  The club the player is about to hit; a hint for the next shot, never a shot
  record and never implying `mode`. A field that is absent is left out of the
  JSON.

The system actor sends it with `mode` and `club` when the club changes
(`club_info`) and with `handed` alone when the hand changes (`hand_info`);
`POST /api/mode` and the WebSocket `mode` command send `mode` alone. Game state
takes only `mode` from it: the club is set with `club_info` and the hand with
`hand_info`. flighthook's own `/frp` endpoint takes the `{ "cmd": ... }`
commands listed under [Client -> Server](#client---server), not an FRP
`set_detection_mode`.

---

##### actor_status

Actor/connection lifecycle update. Emitted by all actors (launch monitors
and integrations) when their connection status or actor-framework state changes.

**Ownership split**: `actor_status` carries what flighthook knows about the
actor (connection lifecycle, operational mode). Device-reported state (ready,
battery, tilt, temp) is conveyed separately via `device_telemetry`. No key
means two things in the two event types, and an actor sends each key in one
of them only (see `device_telemetry`).

```json
{
  "actor": "mevo.0",
  "event": {
    "kind": "actor_status",
    "status": "connected",
    "telemetry": {
      "phase": "measuring",
      "shot_count": "12",
      "device_info": "FlightScope Mevo Gen2"
    }
  }
}
```

- `status`: `"starting"` | `"disconnected"` | `"connected"` | `"reconnecting"`
- `telemetry`: actor-framework key/value pairs (all string values). Each key
  replaces the one held for the actor; an empty value (`""`) removes the key.
  Keys not present are left as they were, except when a connection is lost
  (`connected`, then `disconnected` or `reconnecting`) and on every
  `starting` (a new instance): the actor's other telemetry, device-reported
  keys included, is dropped first, so only the keys that status carries
  remain. `/api/status` and the replay a new WebSocket client gets follow the
  same rule.

Common keys for launch monitors: `device_info`.
The mock launch monitor adds `shot_count`; its `tracking_mode`, `radar_mode`
and `ready` come as `device_telemetry`, and it reports its model (`Mock Launch
Monitor`) there.
The FlightScope adds `shot_count` (shots finished since the card started)
and `phase` (`connecting`, `handshaking`, `configuring` while it connects,
removed by the `connected` status; then `measuring` once the device is
armed, and `configuring` while a mode change re-arms it); `device_info` is
`FlightScope Mevo+`, `FlightScope Mevo Gen2` or `FlightScope Xi`.
The ExPutt adds `shot_count` (shots finished since the card started) and
`phase` (`discovering`, `connecting`, then `measuring`, which the
`connected` status keeps); `device_info` is `ExPutt RG (<ip>)`.
The Trackball adds `shot_count` (shots finished since the card started)
and, while it connects, `phase` (`connecting`; removed by the `connected`
status); `device_info` is `thumbswing (<the name the device reports>)`.
The R10 adds `shot_count` (shots finished since the card started) and, while
it connects, `phase` (`discovering`, `handshaking`; the `connected` status
removes it with `"phase": ""`); `device_info` is `Garmin R10 (<address>)`.
The Square Golf adds `shot_count` (shots finished since the card started)
and, while it connects, `phase` (`discovering`; removed by the `connected`
status); `device_info` is `Square Golf <model> <name> (<address>)`.
Integration actors: `club`, `handed`, `name`, `error`.

An `openconnect_client` sets `error` when this flighthook's own
`openconnect_server` refused its connection (the client points at the server,
which would loop shots back into flighthook): `Refused by this flighthook's
own Open Connect Server "<server name>": this address points at it, which would
loop shots back. Use the simulator's port instead.` The server raises an
`alert`: `Refused Open Connect Client "<client name>" from <peer>: it is this
flighthook's own client, which would loop shots back. Point it at the
simulator's port.` The client reports `connected` once a connection has lasted
1 s, and clears `error` (`""`) then, when an attempt fails another way, and
when it starts.

A `webserver` sets `error` to `bind failed` while it cannot bind its address
(status `disconnected`, retrying every 3 s), and clears it (`""`) with every
other status.

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

`action.type` is one of `replace_all`, `replace_global`, `upsert_webserver`,
`upsert_mevo`, `upsert_r10`, `upsert_square`, `upsert_exputt`,
`upsert_trackball`, `upsert_open_connect_server`, `upsert_open_connect_client`, `upsert_pureplay`,
`upsert_mock_monitor`, `upsert_random_club`, `upsert_camera`, `remove`,
`set_shot_grouping` (`{ "type": "set_shot_grouping", "mode": "by_club" }`, from
`POST /api/shots/grouping` or the WS `grouping` command) and `reconnect`
(`{ "type": "reconnect", "id": "mevo.0" }`, from
`POST /api/actors/{id}/reconnect`). `upsert_gs_pro` is also accepted as
`upsert_open_connect_client`, and a `remove` or `reconnect` of `gspro.N` as
one of `openconnect_client.N`.

---

##### config_outcome

Config mutation outcome (emitted by SystemActor after processing). Always
emitted after a `config_command`, even for fire-and-forget commands. SystemActor
also emits one without `request_id` when it starts an actor again by itself
once that actor's old connection has closed (see
[reconnect](#post-apiactorsidreconnect)), listing it in `restarted`.

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
- `restarted`, `stopped`, `started`: lists of affected actor IDs (omitted when empty).
  A `reconnect` lists its actor in `restarted`, or nothing when it did not start
  it

---

##### new_shot_group

Command: start a new set for the selected club. Emitted by
`POST /api/shots/groups` (actor `web`) or the WS `new_group` command (the
socket's actor ID); every webserver acts on it. Flighthook extension event.

```json
{ "actor": "web", "event": { "kind": "new_shot_group" } }
```

---

##### new_shot_session

Command: close the open set and start a new session. Emitted by
`POST /api/shots/session` or the WS `new_session` command; every webserver
acts on it. Flighthook extension event.

```json
{ "actor": "web", "event": { "kind": "new_shot_session" } }
```

---

##### clear_shot_history

Command: delete the shot history and every set, and start a new session.
Emitted by `POST /api/shots/clear` (actor `web`) or the WS `clear_history`
command (the socket's actor ID); every webserver acts on it (see
[POST /api/shots/clear](#post-apishotsclear)), and the system actor deletes
every camera clip. A dashboard that sees it clears its shot list, sets and
shown shot. Flighthook extension event.

```json
{ "actor": "web", "event": { "kind": "clear_shot_history" } }
```

---

##### shot_group

A set record changed. Emitted by each webserver, with its own actor ID
(the `server` of [GET /api/shots/groups](#get-apishotsgroups)), whenever a
set is created, a shot joins it, it closes, or a shot leaves the history.
Flighthook extension event.

```json
{
  "actor": "webserver.0",
  "event": {
    "kind": "shot_group",
    "group": { "id": "6f1c2a3b-4d5e-4f60-8a71-92b3c4d5e6f7", "number": 3, "club": "7I", "started_at": "2026-10-26T13:58:02.117Z", "open": true, "shots": 10 },
    "shot": { "actor": "mevo.0", "key": { "shot_id": "550e8400-e29b-41d4-a716-446655440000", "shot_number": 12 } }
  }
}
```

- `group`: the full record, as in `GET /api/shots/groups`.
- `shot`: present when the change is a shot joining; its `/api/shots` element
  carries the same `group`. Absent for a created set (`shots: 0`), a closed
  one (`open: false`) and a count going down as the oldest shot leaves the
  1000-shot history.
- `open: false, shots: 0`: the set is closed and empty. It is gone unless a
  shot was in flight when it closed: that shot's join follows
  when it finishes with data. A shot that finishes without data sends the
  set's last record, `open: false, shots: 0`, again.

---

##### shot_video

A camera finished a clip of a shot (beta). Emitted by the camera actor (its
actor ID), once per clip, when the clip's files are complete on disk. The
clip itself is served over HTTP
([`/api/shots/{shot_id}/video`](#get-apishotsshot_idvideo)); no frame goes
on the bus. Flighthook extension event.

```json
{
  "actor": "camera.0",
  "event": {
    "kind": "shot_video",
    "keys": [
      { "shot_id": "550e8400-e29b-41d4-a716-446655440000", "shot_number": 42 },
      { "shot_id": "7c9e6679-7425-40de-944b-e07fc1f90ae7", "shot_number": 17 }
    ],
    "position": "golfer_front",
    "duration_ms": 5500,
    "fps": 119.6,
    "trigger_offset_ms": 4000,
    "impact_offset_ms": 4000,
    "complete": true,
    "dropped": 0
  }
}
```

- `keys`: every shot the clip belongs to, one per launch monitor whose
  `shot_trigger` joined it (two monitors seeing one swing). The same monitor
  triggering again starts a new clip.
- `position`: where the camera stands ([`CameraPosition`](#cameraposition)).
- `duration_ms`: first frame to last.
- `fps`: frames per second captured over the clip.
- `trigger_offset_ms`: the first `shot_trigger`'s time (its `timestamp`
  when it carries one, else its arrival), after the clip's first frame.
- `impact_offset_ms`: the estimated impact, after the first frame (negative
  when it precedes the clip). A FlightScope triggers at impact and the
  ExPutt's trigger is dated at the strike; other launch monitors report the
  shot once it is measured, so the impact is estimated a fixed time before
  their trigger (beta estimates: Square Golf 1 s, R10 2 s, Open Connect
  2.5 s). With two monitors it is the first trigger's.
- `complete`: the whole clip was captured with no frames missing at the
  trigger or the impact. `false` when the camera stopped first.
- `dropped`: frames missing inside the clip.

[Auto-chip](#post-apiauto-chip) shots are never recorded.

---

##### shot_video_removed

These shots' clips are gone (beta): drop their `video`.
Emitted by the camera actor whose clip made room under
[`replay.keep_shots`](#get-apisettings), and by `system` when the clip
folder setting changes (only the new folder's clips are listed) or
when flighthook starts and deletes old clips. Clear history deletes every
clip without this event.

```json
{ "actor": "camera.0", "event": { "kind": "shot_video_removed", "keys": [ { "shot_id": "550e8400-...", "shot_number": 42 } ] } }
```

---

##### shot_pose

Body tracking of a camera's clip is done (alpha): the golfer's keypoints on
every frame are served at
[`/api/shots/{shot_id}/video/{camera}.pose.json`](#get-apishotsshot_idvideocameraposejson).
The envelope's `actor` is the camera, as for [`shot_video`](#shot_video);
`keys` are the clip's shots; `rotation` (omitted at 0) is the camera's
rotation, clockwise degrees, the clip was tracked with. Sent once per clip
when its tracking is done: in Live mode a few seconds after its
`shot_video`, in Historical mode after the clip was asked for with
[`POST .../analyze`](#post-apishotsshot_idvideocameraanalyze); no keypoints
go on the bus. Flighthook extension event.

```json
{ "actor": "camera.0", "event": { "kind": "shot_pose", "keys": [ { "shot_id": "550e8400-...", "shot_number": 42 } ], "rotation": 90 } }
```

---

##### shot_swing

The swing analysis of a camera's clip is done (alpha): its events, tempo
and metrics are served at
[`/api/shots/{shot_id}/video/{camera}.swing.json`](#get-apishotsshot_idvideocameraswingjson).
The envelope's `actor` is the camera; `keys` are the clip's shots. Sent once
per face-on or down-the-line clip, right after its
[`shot_pose`](#shot_pose); no numbers go on the bus. Flighthook extension
event.

```json
{ "actor": "camera.0", "event": { "kind": "shot_swing", "keys": [ { "shot_id": "550e8400-...", "shot_number": 42 } ] } }
```

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

#### club

Select the club in play. Emits `club_info` (see [POST /api/club](#post-apiclub);
a connected simulator replaces it with its next player message).

```json
{ "cmd": "club", "club": "7I" }
```

#### hand

Set the player's hand. Emits `hand_info` (see [POST /api/hand](#post-apihand);
a connected simulator replaces it with its next player message, and the UI
shows the hand control read-only). Any other `handed` value is ignored.

```json
{ "cmd": "hand", "handed": "lh" }
```

#### new_group

Start a new set. Emits `new_shot_group` (see
[POST /api/shots/groups](#post-apishotsgroups)).

```json
{ "cmd": "new_group" }
```

#### new_session

Close the open set and start a new session. Emits `new_shot_session` (see
[POST /api/shots/session](#post-apishotssession)).

```json
{ "cmd": "new_session" }
```

#### clear_history

Delete the shot history and every set, and start a new session. Emits
`clear_shot_history` (see [POST /api/shots/clear](#post-apishotsclear)).

```json
{ "cmd": "clear_history" }
```

#### grouping

Set and persist the Shots tab's default view (`shot_grouping`). Emits a
`config_command` with `set_shot_grouping` and no `request_id`. Sets are
assigned the same way in every view.

```json
{ "cmd": "grouping", "mode": "by_club" }
```

- `mode` (required): `"none"` | `"by_club"` | `"by_set"`

Each command is sent on the bus with the socket's actor ID (`ws.xxxxxxxx`). An
unknown club code or mode is ignored.

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

### ConnectionState

`"connecting"` | `"connected"` | `"reconnecting"` | `"disabled"` (see
[`GET /api/status`](#get-apistatus) for how it is derived)

### ShotDetectionMode

`"full"` | `"putting"` | `"chipping"`

### Handedness

`"rh"` | `"lh"`

### Provenance

`"measured"` | `"virtual"`: a launch monitor measured the shot, either one
flighthook reads directly (FlightScope, Garmin R10, Square Golf, ExPutt) or
one whose own app sends its shots to the Open Connect input; no launch
monitor measured the shot (the Trackball, the mock launch monitor,
Auto-chip).

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
(a section per club, combining its sets since the last New session) |
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

Clubs serialize digit-first as above, and `chipping_clubs`/`putting_clubs`
take these exact codes. A club given as a free-form string (`POST /api/club`,
the WebSocket `club` command, a Square section's `club` and calibration keys,
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
every result value, replacing the device's (`derived.replaces_measured`).

### SimSurface

`"fairway"` | `"green"` | `"firm_green"` -- flat ground for simulated landing
and roll (beta). A ground ball taken in Putting mode rolls on `"green"` when
the setting is `"fairway"`.

### Velocity (unit-tagged string)

Format: `{number}{suffix}`. Suffixes: `mph` (miles/hour), `mps` (meters/second),
`kph` (km/hour), `fps` (feet/second).

### Distance (unit-tagged string)

Format: `{number}{suffix}`. Suffixes: `yd` (yards), `ft` (feet), `in` (inches),
`m` (meters), `cm` (centimeters).
