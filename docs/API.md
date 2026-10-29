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
   `derived` values flighthook simulated for fields the device didn't report,
   or for every result field under `[simulation] mode = "force"` (beta; see
   [ball_flight](#ball_flight))
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
- `shot_group` -- which [set](#get-apishotsgroups) a finished shot joined
  (and so its club); the shot's `/api/shots` element carries the same
  `group`

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

The API has no authentication; it is meant for a trusted LAN. Any client that
reaches the port can change settings or clear the shot history, and the
bodiless `POST`s (New set, New session, Clear history) are CORS simple
requests, so a web page open in a browser on the LAN can send them too.

### GET /api/status

Comprehensive system state (all actors -- launch monitors and integrations).

**Response** `200 OK`:

```json
{
  "mode": "full",
  "club": "7I",
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
      "state": "connected",
      "red_for_ms": 0
    },
    "openconnect_client.0": {
      "name": "GSPro",
      "status": "connected",
      "telemetry": {
        "club": "DR"
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
    connection (any other status).
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
a restart from a config change and removing the actor (then adding it again)
each begin a new history. Live [`actor_status`](#actor_status) events carry
only `status`; a client applies the same rules to them to keep `state` and
the red time current.

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
    "ball": {
      "launch_speed": "67.2mps",
      "launch_azimuth": -1.3,
      "launch_elevation": 14.2,
      "carry_distance": "180.5m",
      "carry_offline": "-7.6m",
      "max_height": "28.3m",
      "flight_time": 6.4,
      "descent_angle": 48.2,
      "backspin_rpm": 3200,
      "sidespin_rpm": -450
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
  "ball": {
    "launch_speed": "150.3mph",
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

- `ball`: `BallFlight` or `null`. Launch conditions and distances, exactly as
  the device measured them. All fields are `Option`.
  Velocity fields are unit-tagged strings (`"67.2mps"`, `"150.3mph"`).
  Distance fields are unit-tagged strings (`"180.5m"`, `"197.4yd"`).
- `derived`: `DerivedFlight` or absent. Values simulated for fields the device
  didn't report, or for every result field under `mode = "force"` (beta); see [ball_flight](#ball_flight) for the fields.
  Converted by `?units=` like `ball` (distances to yards/meters, `max_height`
  to feet/meters); `path`, when present, is always metres/seconds and is
  never converted.
- `club`: `ClubData` or `null`. Club head data. All fields are `Option`.
- `impact`: `FaceImpact` or `null`. Face impact location.
- `metrics`: `ShotMetrics` or absent. Values computed from the measured
  `ball` and `club` when the shot finished (see [Shot metrics](#shot-metrics)).
  `curve` is converted by `?units=` like `ball`.
- `shot_id`: the shot's `key.shot_id` from its lifecycle events.
- `club_selected`: club in play when the shot was triggered; absent when none
  was selected.
- `group`: id of the [set](#get-apishotsgroups) the shot joined. Every shot
  with data has one, whatever the default view (`shot_grouping`).

`POST /api/shots/convert` passes `shot_id`, `club_selected` and `group`
through unchanged.

#### Shot metrics

`metrics` holds values flighthook computes from the shot's measured `ball`
and `club` when the shot finishes, so consumers don't each compute them. The
formulas and signs are the Flight Relay Protocol's spin relations and
"Derived Values":

| Field | Type | Formula | Present when |
| --- | --- | --- | --- |
| `total_spin_rpm` | integer | `√(backspin_rpm² + sidespin_rpm²)`, rounded | the device sent both components and neither `total_spin_rpm` nor `spin_axis` |
| `spin_axis` | number (deg) | `atan2(sidespin_rpm, backspin_rpm)`; + = tilted right, curves the ball right | as `total_spin_rpm`, and the total is not 0 |
| `face_to_path` | number (deg) | `face_angle − path`; + = face right of the club path | the device sent both |
| `spin_loft` | number (deg) | `dynamic_loft − attack_angle`, an approximation of the measured 3D spin loft | the device sent both and no `spin_loft` of its own |
| `curve` | Distance | `carry_offline − carry_distance × tan(launch_azimuth)`, in a common unit; + = curved right | the device sent all three |

Each field is absent when an input is missing (or not a finite number), and
`metrics` is absent when no field is present. Every input is a measured
value: `derived` (the simulation) is never used, so a shot without a measured
`carry_offline` has no `curve`. When the device reports total spin and axis
itself (R10, Square Golf, Open Connect), they are under `ball`, not here; the
same goes for a measured `spin_loft` under `club`. To show total spin and
spin axis for any device, read `ball.total_spin_rpm` and `ball.spin_axis`
first, else `metrics.total_spin_rpm` and `metrics.spin_axis`; likewise
`club.spin_loft`, else `metrics.spin_loft`. Lateral values are
absolute: + is right of the target line for every golfer. `metrics` is on
`/api/shots` and in the [export](#get-apishotsexport). The WebSocket carries
the device's events, which hold no `metrics`: the `flighthook` crate's
`ShotAggregator` computes them when it assembles a shot, and other WebSocket
consumers can use the formulas above or read the shot from `/api/shots`.

---

### GET /api/shots/groups

Shot sets ("groups" on the wire) over this webserver's shot history. Sets are
recorded the same way whatever `shot_grouping` says; that field only picks
the Shots tab's default view (see [POST /api/shots/grouping](#post-apishotsgrouping)).

- At most one set is **open**. A shot joins it unless the club in play differs
  from the set's club, in which case the open set closes and a new one starts.
  Returning to an earlier club starts a new set; a closed set never reopens.
- An open set with no shots (one **+ New set** just started, or one opened by
  a trigger that read nothing) takes the next shot's club instead.
- **New set** ([POST /api/shots/groups](#post-apishotsgroups)) closes the open
  set; the next shot joins a fresh one.
- **New session** ([POST /api/shots/session](#post-apishotssession)) closes the
  open set and counts the session up; the next shot starts a new set in the
  new session.

A shot is assigned its set when it **triggers**, using the club in play at
that moment, and joins it when it finishes with data. A club change or a New
set during a flight applies to the next shot. A shot that never finishes
(the device triggers again first, or drops out with any status but
`connected`) leaves its set uncounted. A device that disconnects and
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
    tab's **+ New set**); omitted when `false`.
  - `open`: still receiving shots. A closed set never reopens.
  - `shots`: stored shots with data in the set (the ones `/api/shots` lists).

---

### POST /api/shots/groups

No body. Emits `new_shot_group` on the bus (actor `web`) and returns
`202 Accepted`; `shot_group` events follow. The Shots tab's **+ New set**
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
`202 Accepted`; the Shots tab's **Clear history** (trash icon, after a
confirmation) sends this or its WS twin. Every webserver then:

- deletes its shot history: `/api/shots` and the
  [export](#get-apishotsexport) are empty, raw frames included;
- drops every set, open or closed, and counts the session up, as
  [New session](#post-apishotssession) does; the next shot starts set 1 in
  the new session, with the club in play unchanged;
- forgets the shots in flight: a device that finishes a shot it triggered
  before the clear stores nothing and changes no set. The next trigger is a
  new shot.

No `shot_group` events follow: every client clears its shots and sets when
it sees [`clear_shot_history`](#clear_shot_history). Nothing is undone; export
first to keep the shots.

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
(`{"mode":"by_club"}`). `504 Gateway Timeout` when it has not within 10 s;
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
shot starts a new [set](#get-apishotsgroups). While an OpenConnect client is
connected, the simulator's next player message replaces the selection: the
simulator (e.g. GSPro) is the club authority while it runs.

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
device produced no data for are included: their `data` has no `ball`, `club`,
`impact` or `group`, and `raw` shows what the device sent.

**JSONL**: one `ShotExport` object per line (pretty-printed here):

```json
{
  "schema": 1,
  "flighthook_version": "0.1.14",
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
- `club_selected`: the club in play when the shot was triggered (the last
  club selected on the bus, e.g. by GSPro or the club picker); absent when
  unknown.
- `settings.detection_mode`, `settings.handed`: game state when the shot was
  triggered; absent when unknown.
- `group`: id of the [set](#get-apishotsgroups) the shot joined (a UUID,
  unique across restarts); absent when the shot has no data.
- `settings.device`: the device's config section with `name`, `address`,
  `bind` and unset fields removed. `settings.simulation`: the `[simulation]`
  section, with the same shape as in [GET /api/settings](#get-apisettings)
  (`mode` present unless `"on"`). `data.derived.replaces_measured` marks a
  shot whose simulated results replace the device's.
- `data`: exactly the `/api/shots` element for this shot (`ShotData`),
  including `metrics`.
  `data.shot_id`, `data.club_selected` and `data.group`, when set, equal the
  top-level fields.
- `raw`: `ShotRaw` (see [shot_finished](#shot_finished)).

**CSV**: one header row, one row per shot, RFC 4180 quoting, empty cell for a
missing value. Units are fixed per column and named in the header (imperial:
mph, yd, ft for heights, in for club offset/height/low point; metric: mps,
m). Face impact is always mm. `environment` is the FlightScope environment in
effect (configured value, else the default). `group` is the set id (empty when
the shot has no data). Unprefixed columns are the device's measured values
(`ball`, `club`, `impact`); `sim_*` columns are the `derived` block and
`metrics_*` columns the [shot metrics](#shot-metrics). With `[simulation]
mode = "force"`, a shot can have both a device column (`carry_*`) and its
`sim_*` column filled; the CSV does not say which one takes precedence (the
JSONL's `data.derived.replaces_measured` does). Read columns by header name:
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
metrics_spin_loft_deg,metrics_curve_yd
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
  "openconnect_client": {
    "0": {
      "name": "GSPro",
      "address": "127.0.0.1:921"
    }
  },
  "random_club": {}
}
```

- Keys are type-prefixed global IDs: `mevo.0`, `r10.0`, `square.0`, `mock_monitor.0`,
  `openconnect_server.0`, `openconnect_client.0`, `random_club.0`, `webserver.0`
- `shot_grouping`: the Shots tab's default view, [`ShotGrouping`](#shotgrouping),
  `"by_set"` by default. It does not change how
  [sets](#get-apishotsgroups) are assigned. Omitted while at the default
- `simulation` (beta) holds the settings for simulated values: `mode`
  ([`SimMode`](#simmode), default `"on"`, omitted while `"on"`), `altitude`
  (unit-tagged distance, default `"0ft"`, valid 0 to 5000 m), `temperature_c`
  (default `25.0`, valid -30 to 50) and `surface` ([`SimSurface`](#simsurface),
  default `"fairway"`). With `"mode": "force"`, every simulated shot's results
  (carry, apex, flight time, descent, offline, roll, total) come from the
  simulation for every device, and its `derived` block carries all of them
  with `replaces_measured: true`; the device's own results stay under `ball`.
  Launch values are never replaced. `simulation` is **omitted while every
  setting is at its default**; an absent `simulation` means the defaults.
  Every key is optional when posting. An out-of-range altitude or temperature
  turns simulation off until it is fixed. Changes apply from the next shot
  without restarting any actor
- All launch monitor config fields are optional (omitted = use defaults)
- R10 sections have `name` and an optional `range`: the distance from the
  device to the ball (unit-tagged, e.g. `"7ft"`), sent to the R10 as its tee
  distance once it wakes up; when absent the device keeps the distance it
  has. No address (BLE auto-discovery)
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
- `spin_source` on Mevo sections is `d4` (default), `e8`, or `auto`, and
  applies to a FlightScope Xi only. `d4` reports the Xi's final spin, which is
  what FS Golf displays. `e8` reports the Xi's second spin value, with the
  spin axis from `d4` when the shot has club data and 0 otherwise, and falls
  back to `d4` when the shot has no E8 spin; on chips and pitches `e8` isn't
  measured, so choose it knowingly. `auto` is `e8` when
  flighthook is in Full mode, and `d4` otherwise -- a chip hit while flighthook
  is still in Full mode gets `e8`'s speed-scaled value. See
  [Spin on the Xi](devices/flightscope.md#spin-on-the-xi). Omitted while at the
  default. Changes apply from the next shot without restarting the actor
- `dot_bottom_to_face_centre_mm` on Square sections overrides allsquare's built-in
  per-club face impact calibration (distance from the bottom edge of the
  sticker's dot down to face centre, in mm). Keyed the same way `club` is. Omitted when no
  overrides are set
- `openconnect_server` sections are **launch monitors**, not integrations: they
  listen (`bind`, default `0.0.0.0:921`) and accept inbound shots from any
  monitor that speaks GSPro Open Connect V1 as a client — Uneekor, Foresight,
  SkyTrak, MLM2PRO. This is the inverse of `openconnect_client`, which dials a
  simulator. GSPro listens on 921 too, but moving GSPConnect to 922 (set
  `<OpenAPIUseAltPort>true</OpenAPIUseAltPort>` in
  `C:\GSPro\GSPC\GSPconnect.exe.config`) frees 921 so both can share one host
- `openconnect_client` sections are **outputs**: each dials `address` (default
  `127.0.0.1:921`), a simulator that accepts GSPro Open Connect V1 — GSPro on
  921, Muni Golf on 9210 — and sends it every shot. `full_monitor`,
  `chipping_monitor` and `putting_monitor` take a launch monitor's actor ID
  and accept shots in that mode from it alone; absent accepts any monitor
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
  `webserver.0`) or `global`. `gspro.N` is accepted for
  `openconnect_client.N`.
  - An actor ID narrows the save to that one actor — only its section is
    written to the config, and only that actor is reconciled. Every other
    section of the live config is left as-is, whatever the request body
    carries for it. A `scope` naming a section the body omits removes that
    actor.
  - `global` writes only the global settings (`default_units`,
    `chipping_clubs`, `putting_clubs`, `shot_grouping`, `simulation`) from the
    body; every actor section of the live config is kept, whatever the body
    carries for it. Every actor is reconciled.
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
- `stopped`: actors that were removed from the config. Omitted when empty.

**Errors**: a JSON body `{"error": "<reason>"}`; the save is not reported
as applied.

| Status | When |
|---|---|
| `400 Bad Request` | `scope` is not `global` and not a known actor ID |
| `400 Bad Request` | the save removes the web server (`"the web server can't be removed"`) or adds a second one (`"flighthook runs one web server; edit the existing one"`) |
| `504 Gateway Timeout` | SystemActor did not answer within 10 s |

A body that is not a valid `FlighthookConfig` is rejected by the JSON
extractor with a `4xx` and a plain-text reason.

A save that changes the Web Server's own `bind` restarts the server that is
answering, so the reply can be lost even though the config was saved: read
`GET /api/settings` to see what was applied.

**Side effects**:

- Emits `ConfigCommand` on the bus, processed by SystemActor. Without `scope`
  the action is `ReplaceAll`; with `scope=global`, `ReplaceGlobal`; with an
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

- `actor`: global ID of the originator (e.g. `"mevo.0"`, `"openconnect_client.0"`, `"system"`).
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
      "carry_offline": "-7.6m",
      "max_height": "28.3m",
      "flight_time": 6.4,
      "descent_angle": 48.2,
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

All `BallFlight` fields are `Option`. Missing fields are omitted from the JSON.
`ball` is FRP's `BallFlight`, as the device measured it:

| Field | Type | Meaning |
| --- | --- | --- |
| `launch_speed` | Velocity | Ball speed at launch |
| `launch_azimuth` | number (deg) | Horizontal launch angle, + right of the target line |
| `launch_elevation` | number (deg) | Vertical launch angle |
| `carry_distance` | Distance | Tee to landing |
| `carry_offline` | Distance | Landing point right (+) or left (−) of the target line |
| `total_distance` | Distance | Tee to rest |
| `total_offline` | Distance | Rest position right (+) or left (−) of the target line |
| `roll_distance` | Distance | Landing to rest |
| `max_height` | Distance | Apex |
| `flight_time` | number (s) | Launch to landing |
| `descent_angle` | number (deg) | Angle below horizontal at landing |
| `backspin_rpm` | integer | Backspin, + = backspin, − = topspin |
| `sidespin_rpm` | integer | Sidespin, + = curves the ball right |
| `total_spin_rpm` | integer | Total spin, never negative |
| `spin_axis` | number (deg) | Spin axis tilt, + = tilted right (curves the ball right); absent when the total is 0 |

Lateral values are absolute: + is right of the target line for every golfer.
`carry_offline` and `descent_angle` are absent for a ball that did not fly.
Which fields each device sends:

| Device | `carry_offline` | `descent_angle` | `total_spin_rpm` + `spin_axis` |
| --- | --- | --- | --- |
| FlightScope full result (`D4`) | yes (landing position) | yes (landing velocity) | no (components only) |
| FlightScope estimate (`E8`) | yes | no | no (backspin only) |
| Garmin R10 | no | no | yes, with the components |
| Square Golf Omni | no | no | yes, with the components |
| Open Connect | no | no | yes: the pair the monitor sent, the other computed from it |

`total_offline`, `club_path.spin_loft` and `club_path.low_point_distance` are
passed through by flighthook but no supported device reports them. Values a
device doesn't send, flighthook may compute from the ones it does: see
[Shot metrics](#shot-metrics), on the finished shot.

**`derived` (beta)** is a flighthook extension: values simulated from the
measured launch with the [dimple](https://crates.io/crates/dimple) ball-flight
model, for fields the device did not report. Every field under `ball` is
measured; every field under `derived` is simulated. A measured
`carry_offline` or `descent_angle` keeps the field out of `derived` (except
under `force`). A field is in both only
when the `derived` value is the one to show: a measured carry or flight time
of exactly 0 on a shot that flies is not a reading, so `derived` carries the
simulated value next to the untouched 0, and a ground ball (below) shows 0
for any carry, apex or flight time the device measured other than exactly 0. With
`[simulation] mode = "force"`, `derived` carries
every result field whether or not the device measured it, plus
`"replaces_measured": true`: a consumer shows the `derived` values in place of
the measured ones, which stay unchanged under `ball`. FRP consumers ignore
`derived` as an unknown field. It is absent when `[simulation] mode` is
`"off"` or when the launch can't be simulated — including a shot with no
spin reading (back and side both 0) — see the README's "Simulated values"
section for the rules. It is never sent to GSPro.

A ground ball is rolled, not flown: a shot whose measured apex is 0, or,
from a device that reports no apex, whose launch angle is at most 0.5°,
whatever the device, club or detection mode. Carry, apex and flight time are
0 (the device's 0s stay measured), there is no `descent_angle`, and roll and
total come from a roll from the tee, from the launch speed and direction (a
measured total, or a measured roll as the total, wins). With a carry of 0 the
roll is the total, so a measured roll that differs from it is replaced in
`derived`. It rolls on a green
when the shot was taken in Putting mode (the configured `surface` when that
is a green, else `"green"`) and on the configured `surface` otherwise.

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
| `replaces_measured` | bool | `true` when these values replace the device's results (`[simulation] mode` was `"force"` when the shot was simulated): every result field is simulated and takes precedence over `ball`. Omitted when false |

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
metres downrange, metres up, metres right of the target line, origin at the
tee. Always metres and seconds, unaffected by `?units=`. Present only on
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
dozen points per shot, rounded to centimetres and milliseconds.

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

Club selection update.

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
- `telemetry`: actor-framework key/value pairs (all string values). Each key
  replaces the one held for the actor; an empty value (`""`) removes the key.
  Keys not present are left as they were.

Common keys for launch monitors: `detection_mode`, `radar_mode`, `device_info`.
Mock launch monitors add: `shot_count`, `tracking_mode`.
Integration actors: `club`, `handed`, `name`, `error`.

An `openconnect_client` sets `error` when this flighthook's own
`openconnect_server` refused its connection (the client points at the server,
which would loop shots back into flighthook): `Refused by this flighthook's
own OpenConnect Server "<server name>": this address points at it, which would
loop shots back. Use the simulator's port instead.` The server raises an
`alert`: `Refused OpenConnect Client "<client name>" from <peer>: it is this
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

`action.type` is one of `replace_all`, `upsert_webserver`, `upsert_mevo`,
`upsert_r10`, `upsert_square`, `upsert_open_connect_server`,
`upsert_open_connect_client`, `upsert_mock_monitor`, `upsert_random_club`,
`remove` and `set_shot_grouping`
(`{ "type": "set_shot_grouping", "mode": "by_club" }`, from
`POST /api/shots/grouping` or the WS `grouping` command). `upsert_gs_pro` is
also accepted as `upsert_open_connect_client`, and a `remove` of `gspro.N` as
one of `openconnect_client.N`.

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
[POST /api/shots/clear](#post-apishotsclear)). A dashboard that sees it
clears its shot list, sets and shown shot. Flighthook extension event.

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

`"connecting"` | `"connected"` | `"reconnecting"` (see
[`GET /api/status`](#get-apistatus) for how it is derived)

### ShotDetectionMode

`"full"` | `"putting"` | `"chipping"`

### Handedness

`"rh"` | `"lh"`

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
