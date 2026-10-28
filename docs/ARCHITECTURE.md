# Flighthook Architecture

Multi-launch-monitor bridge with unified bus messaging and club-based shot
routing.

## Config Model

Type-prefixed config sections. The section prefix encodes the component type;
no `type` discriminator field needed. Section keys are integer indices starting
at `"0"`.

```toml
chipping_clubs = ["GW", "SW", "LW"]
putting_clubs = ["PT"]

[webserver.0]
name = "Web Server"
bind = "0.0.0.0:5880"

[mevo.0]
name = "Mevo WiFi"
address = "192.168.2.1:5100"
ball_type = 1
track_pct = 80.0
tee_height = "1.5in"
range = "8ft"
surface_height = "0in"

[r10.0]
name = "Garmin R10"

[gspro.0]
name = "Local GSPro"
address = "127.0.0.1:921"
# full_monitor = "mevo.0"       # optional: route full-swing shots from specific monitor
# chipping_monitor = "mevo.0"   # optional: route chipping shots
# putting_monitor = "mevo.0"    # optional: route putting shots
```

- `[mevo.<idx>]` -- FlightScope device instance (Mevo/Mevo+, or Xi — detected at connect)
- `[r10.<idx>]` -- Garmin R10 BLE device instance (auto-discovers via Bluetooth)
- `[square.<idx>]` -- Square Golf Omni BLE device instance
- `[openconnect_server.<idx>]` -- GSPro Open Connect ingest listener (Uneekor)
- `[mock_monitor.<idx>]` -- mock launch monitor instance
- `[gspro.<idx>]` -- GSPro integration instance
- `[random_club.<idx>]` -- random club cycling integration instance
- `[webserver.<idx>]` -- web server instance
- `name` is **required** -- the user-visible name, editable (rename) in settings UI
- Radar settings (ball_type, tee_height, etc.) are per-mevo only (not applied to an Xi)
- `use_estimated` is per-mevo (defaults to `false`) -- controls whether estimated
  (E8) ball flights are emitted when no full (D4) result arrives; with it off,
  estimated-only shots (a flight too short to read in Outdoor mode, very short
  chips) are skipped
- `environment` is per-mevo (`limited_flight` default / `outdoor`) -- picks the
  full-swing detection mode (9 / 8); on an Xi it is the only mode sent
- `camera_mode` is per-mevo (`standard` default / `fusion` / `raw_fusion`) --
  the Fusion modes request the full club data set (radar club data arrives in
  every mode), applied after a camera warmup delay
- `spin_source` is per-mevo (`d4` default / `e8` / `auto`), FlightScope Xi
  only -- `d4` keeps D4's spin; `e8` takes total spin from E8 (D4 axis with
  club data, else 0) and falls back to D4 when there is no E8 spin; `auto`
  applies the `e8` rule when flighthook's detection mode is Full at shot time
  (the mode captured at the shot's trigger), and `d4` otherwise -- a chip hit
  while flighthook is still in Full mode gets `e8`'s speed-scaled value. Read
  on every shot, so a change applies without a restart; omitted from the
  config at the default
- R10 sections show name and Monitor-to-Ball (`range`, sent to the device as
  its tee distance; BLE auto-discovery, no address field)
- Mock sections show only name (no address or radar fields)
- Global IDs = `"{type_prefix}.{index}"` (e.g. `mevo.0`, `gspro.0`)
- WebSocket actor IDs = `"ws.{8-hex-chars}"`

### Default config and setup wizard

On first startup the default config contains only a webserver — no devices or
integrations. The UI detects this (`FlighthookConfig::has_user_actors()`) and
shows a setup wizard instead of the normal dashboard. The wizard presents
checkboxes for available devices (Mevo, Square Golf Omni, R10, Uneekor
(OpenConnect)) and integrations (GSPro). On
confirmation it builds a config with defaults for each selection, posts it via
the existing settings save path, and dismisses. Users can also skip the wizard
and configure manually on the Devices tab.

`MevoSection`, `R10Section`, and `GsProSection` each implement `Default` with
sensible values (e.g. Mevo at `192.168.2.1:5100`, GSPro at `127.0.0.1:921`).

### Rust types

**lib/src/config.rs** (shared config types, used by both app and UI):

```rust
pub struct FlighthookConfig {
    pub default_units: UnitSystem,
    pub chipping_clubs: Vec<Club>,
    pub putting_clubs: Vec<Club>,
    pub simulation: SimulationSection,     // [simulation], omitted at defaults
    pub webserver: HashMap<String, WebserverSection>,
    pub mevo: HashMap<String, MevoSection>,
    pub r10: HashMap<String, R10Section>,
    pub square: HashMap<String, SquareSection>,
    pub mock_monitor: HashMap<String, MockMonitorSection>,
    pub openconnect_server: HashMap<String, OpenConnectServerSection>,
    pub gspro: HashMap<String, GsProSection>,
    pub random_club: HashMap<String, RandomClubSection>,
}

pub struct SimulationSection { pub mode: SimMode, pub altitude: Distance, pub temperature_c: f64, pub surface: SimSurface }
pub enum SimMode { Off, On, Force }                 // default On
pub enum SimSurface { Fairway, Green, FirmGreen }
pub struct WebserverSection { pub name: String, pub bind: String }
pub struct MevoSection { pub name: String, pub address: Option<String>, pub ball_type: Option<u8>, pub tee_height: Option<Distance>, pub range: Option<Distance>, pub surface_height: Option<Distance>, pub track_pct: Option<f64>, pub use_estimated: Option<bool>, pub camera_mode: Option<CameraMode>, pub environment: Option<FlightScopeEnvironment>, pub spin_source: SpinSource }
pub enum CameraMode { Standard, Fusion, RawFusion }
pub enum FlightScopeEnvironment { LimitedFlight, Outdoor }
pub enum SpinSource { D4, E8, Auto }
pub struct R10Section { pub name: String, pub range: Option<Distance> }
pub struct SquareSection { pub name: String, pub address: Option<String>, pub club: Option<Club>, pub advanced_spin: Option<bool>, pub discard_non_putting_zero_spin: Option<bool> }
pub struct MockMonitorSection { pub name: String }
pub struct OpenConnectServerSection { pub name: String, pub bind: Option<String> }
pub struct GsProSection { pub name: String, pub address: Option<String>, pub full_monitor: Option<String>, pub chipping_monitor: Option<String>, pub putting_monitor: Option<String> }
pub struct RandomClubSection { pub name: String }
```

**lib/src/api.rs** (shared REST API types, used by both app and UI):

```rust
pub struct StatusResponse { pub actors: HashMap<String, ActorStatusResponse>, pub mode: Option<ShotDetectionMode> }
pub struct ActorStatusResponse { pub name: String, pub status: ActorStatus, pub telemetry: HashMap<String, String> }
pub struct PostSettingsResponse { pub restarted: Vec<String>, pub stopped: Vec<String> }
pub struct ModeRequest { pub mode: ShotDetectionMode }
```

**app/src/actors/mod.rs** (actor resolution from config):

```rust
pub struct ResolvedActor {
    pub id: String,             // global ID (e.g. "mevo.0", "r10.0", "gspro.0")
    pub name: String,           // user-visible name
    pub actor: Box<dyn Actor>,  // concrete actor ready to start
}

pub fn resolve_actors(config: &FlighthookConfig, current_mode: Option<ShotDetectionMode>) -> Vec<ResolvedActor>;
pub fn start_actor(id: String, actor: Box<dyn Actor>, state: &Arc<SystemState>, bus_tx: &broadcast::Sender<FlighthookMessage>);
pub fn actor_names(config: &FlighthookConfig) -> HashMap<String, String>;
```

Helper functions: `global_id(prefix, index) -> String` builds `"{prefix}.{index}"`,
`load(path)` / `save_to(path, config)` for TOML I/O.

## Type Boundary Rule

**All types that cross the app<->UI boundary (REST API, WebSocket, config)
MUST live in `flighthook`.** The UI crate never defines its own
"response" mirror types. Both crates import the canonical type from schemas.
This eliminates drift between serializer (app) and deserializer (UI) --
e.g., `ActorStatus` enum vs string, `Severity` enum vs string.

## Unified FlighthookMessage Bus

All communication between components flows through a single
`broadcast<FlighthookMessage>(1024)` channel. Every message carries an
optional raw payload (hex-first policy) and a typed event.
Producers create messages; consumers subscribe and filter by event kind.

```rust
pub struct FlighthookMessage {
    pub actor: String,                    // actor framework ID (e.g. "mevo.0")
    pub device: Option<String>,           // FRP physical device ID (e.g. Mevo SSID)
    pub raw_payload: Option<RawPayload>,
    pub event: FlighthookEvent,
}
```

All messages use the FRP envelope shape: `{ actor, device?, event: { kind, ... } }`.
`actor` is a flighthook extension field (actor framework ID). FRP consumers
ignore unknown fields and unknown `kind` values per spec, so flighthook
extension kinds (`actor_status`, `player_info`, etc.) are transparent.

### RawPayload

Wire data attached to bus messages. Follows the hex-first policy.

```rust
pub enum RawPayload {
    Binary(Vec<u8>),    // serializes as lowercase hex string (no spaces)
    Text(String),       // serializes as-is (e.g. GSPro JSON)
}
```

### FlighthookEvent

The typed event payload. Tagged with `kind` in JSON serialization. All variants
are flat struct variants with named fields (no wrapper types).

```rust
pub enum FlighthookEvent {
    // -- FRP-compliant shot lifecycle (correlated by ShotKey) --
    ShotTrigger { key: ShotKey },
    BallFlight { key: ShotKey, ball: Box<BallFlight>, derived: Option<Box<DerivedFlight>> },
    ClubPath { key: ShotKey, club: Box<ClubData> },
    FaceImpact { key: ShotKey, impact: Box<FaceImpact> },
    ShotFinished { key: ShotKey, raw: Option<Box<ShotRaw>> },

    // -- FRP: Device status --
    DeviceTelemetry { manufacturer: Option<String>, model: Option<String>, firmware: Option<String>, telemetry: Option<HashMap<String, String>> },
    Alert { severity: Severity, message: String },

    // -- FRP: Controller commands --
    SetDetectionMode { mode: Option<ShotDetectionMode>, handed: Option<Handedness> },

    // -- Flighthook extensions --
    PlayerInfo { player_info: PlayerInfo },
    ClubInfo { club_info: ClubInfo },
    ConfigCommand { request_id: Option<String>, action: ConfigAction },
    ConfigOutcome { request_id: Option<String>, restarted: Vec<String>, stopped: Vec<String>, started: Vec<String> },
    ActorStatus { status: ActorStatus, telemetry: HashMap<String, String> },
    NewShotGroup,                                                     // command: new set for the selected club
    NewShotSession,                                                   // command: close the open set, next session
    ClearShotHistory,                                                 // command: delete shots and sets, next session
    ShotGroup { group: ShotGroupInfo, shot: Option<GroupedShot> },    // a set record changed (web layer)
}
```

Match variants with `..` (e.g. `FlighthookEvent::BallFlight { key, ball, .. }`)
unless every field is needed, so a field added to a variant does not break the
pattern.

### ShotKey -- shot lifecycle correlation

```rust
pub struct ShotKey {
    pub shot_id: String,      // UUID v4 string, globally unique
    pub shot_number: u32,     // session-level monotonic counter per-actor
}
```

Shot data arrives as correlated events sharing a `ShotKey` and `actor`:
`ShotTrigger` -> `BallFlight` / `ClubPath` / `FaceImpact` (any order) -> `ShotFinished`. Consumers
use `ShotAccumulator` to collect events into a final `ShotData`
(`set_ball` for the measured `BallFlight`, `set_derived` for its `derived` block).
When it finishes the shot, the accumulator computes `ShotData.metrics`
(`ShotMetrics`, lib `metrics.rs`) from the measured ball and club data.

### Shot metrics

`ShotMetrics::compute` implements the Flight Relay Protocol's spin relations
and "Derived Values" once, for every consumer that assembles shots (the web
layer's history and export, the GSPro bridge, `ShotAggregator` users):

- total spin and spin axis from backspin and sidespin, via the SDK's
  `BallFlight::complete_spin`, only when the device sent both components and
  neither total nor axis (a total of 0 has no axis);
- face to path, `face_angle − path`;
- spin loft, the `dynamic_loft − attack_angle` approximation, only when the
  device sent no `spin_loft`;
- curve, `carry_offline − carry_distance × tan(launch_azimuth)`, in metres.

Each field needs every input measured and finite; `metrics` is `None` when no
field is set. `derived` is never an input: the metrics describe the device's
shot, the simulation stays in `DerivedFlight`. Lateral values are absolute,
+ right of the target line. `ShotMetrics` is `#[non_exhaustive]`; `curve`
converts with the unit system like the rest of `ShotData`.

### Simulated values (beta)

The `BallFlight` event's `derived` field carries values flighthook simulated
for fields the device did not report: carry, apex, flight time, descent angle,
carry/roll/total offline, roll and total (`DerivedFlight`, lib crate). A
measured `carry_offline` or `descent_angle` (FlightScope) is not simulated
over, except under `force`. `ball`
is never modified, so a field's location says where it came from: under
`ball` it was measured, under `derived` it was simulated. A field set in both
(a measured 0 carry or flight time on a shot that flies, a ground ball's 0
carry, apex and flight time over a device's other reading, both below, or any
result field under `force`) is shown with its `derived` value. `ShotData.derived` carries the same block after
accumulation.

`[simulation] mode` (`SimulationSection::mode`, `SimMode`) picks what is
simulated: `off` nothing, `on` (the default) the fields the device did not
report, `force` every result field. Under `force` the simulated
results replace the device's for every device: `derive` treats nothing as measured,
fills every result field in `derived` and sets
`DerivedFlight::replaces_measured`, and every consumer gives those values
precedence over `ball`. `ball` is still never modified, so the device's
results stay on the wire, in `/api/shots` and in the export. The flag is per
shot, set from the settings in effect when the shot was simulated; a shot
that can't be simulated gets no `derived` and shows the device's values.
Launch values (speed, angles, spin, club data) are never replaced.

The simulation runs in `app/src/sim.rs` with the
[dimple](https://crates.io/crates/dimple) crate, an app-only dependency (the
lib crate and the WASM UI never link it). Every device actor builds its
`BallFlight` event through `sim::ball_flight_event` (the Mevo+/Xi through
`sim::ball_flight_event_in_mode`, passing the detection mode captured at the
shot's trigger), on the device thread, so the event already carries `derived` when it reaches the bus, before
`ShotFinished`. It costs a few milliseconds per shot. A shot is simulated only
when:

- `[simulation] mode` is not `off`;
- the altitude (0 to 5000 m) and temperature (-30 to 50 °C) are in range;
- for a ground ball (below): the speed is finite and positive and the
  launch direction finite and within ±90°;
- for any other shot: all five launch values (speed, launch angle, launch
  direction, backspin, sidespin) are present and finite, speed is above 0,
  launch angle is between 0 and 90°, and launch direction is within ±90°;
  and the simulated flight is valid (positive flight time, finite results).
  A slow, low shot with lift (3 mph at 2°) flies.

Otherwise `derived` is absent. Roll, total and the roll/total offlines also
need the ground phase to reach rest. Where the device measures carry, total is
the measured carry plus the simulated roll (the simulated carry plus roll
under `force`).

**Ground balls (beta).** A shot is a ground ball when the device measured
its apex as 0 (finite, within 0.01 m), or, on a device that reports no apex
(launch data only), when its launch angle is at most 0.5° (above −90°):
too little lift to fly. Anyone can blade a shot, so the device, club and
detection mode don't decide what is a ground ball; a putt is a ground ball
because its apex is 0, not because it is a putt, and a putting-mode shot
with an apex above 0 flies like any other. A ground ball never leaves the ground: carry, apex
and flight time are 0 (in `derived` unless the device measured exactly 0
for that field, so a measured carry or flight time above 0 shows as 0),
there is no descent angle, and the carry offline is 0 (in `derived` unless the
device measured exactly 0). Its roll comes from
dimple's ground model (`GroundModel::settle`), started at the tee with the
launch speed along the launch direction and no spin, so the ball skids,
then rolls to rest; launch angle and spin are not used. The detection
mode picks the surface (`sim::ground_surface`): in Putting mode it rolls on
a green (`green`, 11.4 ft on the stimpmeter, unless the configured surface
is already `green` or `firm_green`, which is kept); in any other mode on the
configured surface. The mode is the one in effect for the shot: for the
Mevo+/Xi the mode captured at its trigger (the same capture the Xi `Auto`
spin source uses), for other devices the game mode when the shot is
processed. A straight 3 m/s ground ball rolls about 2.6 m on the default
`fairway` (8 m/s: about 10.8 m) and about 5.0 m on `green` (8 m/s: about
18.8 m). Roll and total are the simulated roll
unless the device measured the total (shown as measured, the roll fitted
to it) or the roll (total = roll); the roll and total offlines are the
rest point's `z`. `force` rolls a ground ball the same way, with nothing
counted as measured. This holds under `on` and `force` for every device.

**Zero readings.** On a shot that flies (not a ground ball), a measured
carry or flight time of exactly 0 is not a reading (the Mevo+ sends them
with every shot, 0 when it has none). It counts as not measured: `derived`
carries the simulated value, alongside the 0 left untouched in `ball`, and
the path is fitted only to the values the device really measured.

`derived.path` (`FlightPath`) is the simulated ball path from the tee to rest:
`[t, x, y, z]` points in seconds and metres (downrange, up, right of the target
line), split into `flight` (tee to landing) and `ground` (landing to rest,
empty when the ball did not reach rest). A ground ball's `flight` is the tee
alone, `[[0, 0, 0, 0]]`, and its `ground` the roll from the tee. It is fitted to the
values the cards display before anything is read from it:

- Flight: `x` and `z` scale by displayed carry / simulated carry, `y` by the
  apex ratio and `t` by the flight-time ratio, each 1 when the device did not
  measure it. The landing keeps its bearing and sits at the displayed carry;
  the apex and landing time are the displayed ones.
- Ground: a polar fit about the tee. Each point keeps its bearing and height;
  its distance from the tee becomes `C + r × (ρ − C_sim)`, with
  `r = displayed roll / simulated roll` when the device measured total or
  roll and 1 otherwise, and its time shifts with the landing. The ground
  segment starts exactly at the landing and the rest point lies at the
  displayed total.
- When a measured roll is fitted and the simulated roll is under 0.3 m, runs
  the other way (spin-back, or a measured total below the carry), or the
  ratio is outside 0.25 to 4, or a fitted distance would be negative, the
  ground segment is a straight run from the landing to the displayed total
  on the landing's bearing.
- Offline: with a measured carry offline `O`, the fitted path is then made
  to land on it while leaving the tee on the measured launch direction `az`.
  The landing moves to `(X', O)`, `X' = √(C² − O²)`, so it stays at the
  displayed carry `C`; `x` scales by `X' / X` and the lateral becomes
  `z' = x'·tan(az) + s·(z − x·tan(az))`, the curve off the start line scaled
  by `s = (O − X'·tan(az)) / (Z − X·tan(az))` (`(X, Z)` the landing before
  this step). When the simulated curve at the landing is under 0.3 m, or `s`
  is not finite or beyond ±4, the curve is kept and the missing lateral is
  added as `(O − Z')·(x / X)²` instead, which leaves the tee on the same
  line. The ground segment is turned about the landing by the change in the
  flight's arrival heading, so the roll continues in the direction the
  fitted flight arrives, and its run from the landing is scaled to keep the
  rest at the displayed total. An offline that is not inside the carry gives
  no path, and then no roll or total offline either.
- A ground ball's roll is fitted the same way after a landing at the tee
  (radius 0); its straight run keeps the simulated rest's bearing.
- A measured carry, apex or flight time that is negative or not finite, or
  a measured total that is not finite and positive, gives no path (a
  measured 0 makes a ground ball or counts as not measured, see above).
- Under `force` nothing counts as measured, so every ratio is 1 and
  the path is the raw simulation.

The carry, roll and total offlines are read from the fitted path: the
landing's and the rest point's `z`. Where the device measures carry they are
the offlines at the measured landing, so the path, the offlines and the
measured distances agree (beta). A measured carry offline is the displayed
one (not repeated in `derived`), and the roll offline runs from it. The path is then decimated: the tee, apex,
landing, rest and the contacts and peaks of hops over 5 cm are kept and the
rest thinned by Ramer-Douglas-Peucker at 0.25 m, while the segment fits under
its cap (40 flight and 24 ground points); above the cap the segment is
resampled uniformly in time, keeping its ends (the flight also keeps its
apex). Points are rounded to centimetres and milliseconds: about 0.7 KB of
JSON per path on average, at most 64 points. It is never converted between unit systems, rides on the
WebSocket event, `/api/shots` and the JSONL export, and is not in the CSV.

The device actor reads the `[simulation]` section from `SystemConfig` for every
shot, so settings changes apply on the next shot with no actor restart. The
GSPro mapper reads `shot.ball` only, so GSPro output does not change in any
simulation mode. The console gets one `sim (estimated, beta): ...`
line under the device's ball line (ending in `replaces_measured` when set); it is a derived summary, not wire data, so the hex-first audit rules do
not apply to it.

### Raw capture and shot export

`ShotFinished.raw` (a flighthook extension, omitted when `None`) carries the
device frames behind the shot as a `ShotRaw` (lib `export.rs`): a transport
label, the `RawRecord`s (`t_ms` relative to the trigger, direction, type,
bus addresses or channel, lowercase hex or JSON text), a `dropped` count and
an `unavailable` reason.

Each device actor captures on its own thread with `RawCapture`
(`app/src/raw_capture.rs`). Frames are pushed as they cross the transport.
Outside a shot they go to a pre-roll ring; `mark_trigger()` (right before the
actor emits `ShotTrigger`) opens the shot window with the ring as its head,
and `take()` closes it for `ShotFinished`. Excluded frames are dropped before
the ring and every cap.

| Device | Hook | Window | Excluded | Pre-roll | Max frames | Max bulk bytes |
| --- | --- | --- | --- | --- | --- | --- |
| FlightScope | ironsight `set_on_recv` / `set_on_send` (the same hooks log hex and keep the last D4) | BALL TRIGGER to the device re-armed, both directions | status/keepalive, device and product info, DSP query, network config, WiFi scan, calibration, time sync, AVR config query | 8 | 256 | 128 KiB (PRC, club PRC, speed profile) |
| Square Golf | `Tap<T>` around the allsquare transport; characteristic reads are never recorded | pre-roll through the shot event; enabled after the identity exchange | heartbeats and acks, query reply | 64 | 64 | 0 |
| R10 | `Tap<T>` around the tenover transport; a per-direction `FrameFilter` holds each chunk until the GFDI frames it carries are complete and classified | pre-roll through the shot event; enabled at `HandshakeComplete` | every chunk carrying part of a device information (5024), FIT/file transfer (5003, 5004, 5011, 5012) or authentication (5101-5112) frame, an ACK of one, or a frame that does not decode | 256 | 256 | 0 |
| Open Connect | the inbound JSON text and our reply | the one message | none | - | - | - |
| Mock | none: `transport = "none"`, `unavailable` says why | - | - | - | - | - |

The web layer keeps each shot as a `StoredShot` (`app/src/actors/web/mod.rs`):
when `shot_trigger` arrived, a `ShotContext` snapshot taken then (device kind,
maker, model and firmware; the club the web layer last saw in a `club_info`
on the bus, which is also the club its sets use; mode and handedness from
game state; the
device's config section without `name`/`address`/`bind`; the `[simulation]`
section; identity strings to scrub), the `ShotData`, and the `ShotRaw`. A
shot with raw frames but no decoded data is stored too; `/api/shots` serialises
`data` only and leaves those out, so its JSON is unchanged. Raw frames share
a 16 MiB session memory budget across the 1000-shot history, each frame
charged its hex (two characters per payload byte) or text length plus 256
bytes for the record and its metadata: past it, the oldest shots lose their
frames first (`unavailable = "evicted: session raw budget"`),
and decoded data stays until the shot leaves the history.

`GET /api/shots/export` (`web/export.rs`, see docs/API.md) streams one JSONL
`ShotExport` or CSV row per stored shot. It never serialises the FRP `device`
ID, actor names, telemetry or configured addresses, and replaces identity
strings in text payloads and device labels with `<redacted>`; hex is never
rewritten, which is why identity frames are excluded at capture.

Every bus subscriber gets its own clone of `shot_finished`, so a heavy shot
(a few hundred KiB for FlightScope Fusion with PRC data) is copied once per
consumer. Acceptable on a LAN; WebSocket consumers can use `shot_trigger` for
timing.

Each stored `ShotData` is stamped by the web layer at `shot_finished`:
`shot_id` (the `ShotKey`'s), `club_selected` (the trigger-time club from the
`ShotContext`) and `group` (below). FRP events are untouched; these fields
exist only on `/api/shots`, the export and `/api/shots/convert`, and are
omitted when unset, so their JSON is unchanged without them.

### Shot sets

`WebState.groups` holds a `GroupState` (`app/src/actors/web/groups.rs`): pure
bookkeeping over the shot history, driven by the web updater from bus events
in bus order. The UI calls a shot group a **set**; the API, events, export and
code keep "group". Sets are assigned one way, whatever the persisted
`shot_grouping` field says (lib `group.rs`: `ShotGrouping { None, ByClub,
BySet }`, default `BySet`): that field is only the Shots tab's default view,
and `GET /api/shots/groups` reads it from config as `mode`.

- At most one set is open. A shot joins it unless the club in play differs
  from its club; then the open set closes and a new one opens. Returning to an
  earlier club opens a new set too: a closed set never reopens.
- An open set with no shots and none in flight (a fresh New set, or one
  opened by a trigger that read nothing) adopts the next shot's club instead.
- A session counter starts at 0 and counts up with every New session. Each
  set carries the `session` it was created in (`ShotGroupInfo.session`,
  omitted at 0), and `GET /api/shots/groups` reports the current `session`.
  The UI's By club view combines a club's sets within one session.

Sets are keyed on `GroupKey { golfer, club }`; `golfer` is reserved and
always `None`. Sets are global across devices. A record (`ShotGroupInfo`) has
a UUID v4 `id`, a per-run display `number`, `session`, `club`, `started_at`,
`manual`, `open` and `shots` (stored shots with data).

| Bus event | Effect |
| --- | --- |
| `shot_trigger` | the device's earlier unfinished shots end uncounted; then `assign`: the open set when its club is the trigger-time club (or it is empty), else a new set; held on the `PendingShot` |
| `actor_status` other than `connected` | the device's unfinished shots end uncounted; the open set stays open |
| `shot_finished` | the shot is stamped and `finish` counts it when it has data; a shot with no data and no raw frames still ends its assignment |
| a shot leaves the 1000-shot history | `release`: count down; a closed set at zero is dropped |
| `club_info` | `select`: the club `new_shot_group` and the next trigger use |
| `new_shot_group` | no-op while the open set is empty; otherwise close it and open a manual set for the selected club |
| `new_shot_session` | close the open set and count the session up |
| `clear_shot_history` | empty the shot history (raw byte count to 0), drop every set, forget the shots in flight, count the session up; set numbers restart at 1; no `shot_group` is emitted |

Because the set is decided at the trigger, a club change or New set during
a flight applies to the next shot. A device that drops out and reconnects
keeps the open set. Every change is emitted as `shot_group` with the
webserver's own actor ID; each `[webserver.N]` keeps its own history, ids,
sets and session, and acts on the same commands. A record is kept while it is
open, has stored shots, or has a shot in flight; the change that drops it has
`open: false, shots: 0`. Lock order where both are held: `groups`, then
`shots`. Every stored shot with data carries its set's `group`, in every
view; the export carries it too (JSONL top level and `data`, and a CSV column
after `club_selected`). The display `number` and the `session` are not
exported.

### Game state events

Game state is mutated via typed events on the bus:

- `PlayerInfo { player_info }` -- player name
- `ClubInfo { club_info }` -- club selection
- `SetDetectionMode { mode?, handed? }` -- detection mode and/or handedness (both optional, latched)

`POST /api/club` and the WS `club` command emit `ClubInfo` like any
integration, so a club picked in the UI drives the mode the same way. GSPro
is a club source only (flighthook never sends it a club) and re-sends its club
with every player message, so while it is connected its selection wins.

The `SystemActor` auto-derives detection mode from `ClubInfo` (using
`config.club_mode()` to map club to detection mode via the configurable
`chipping_clubs`/`putting_clubs` lists). Integrations only
need to emit `ClubInfo`; the SystemActor handles mode derivation
centrally. Launch monitor actors react to `SetDetectionMode` events to reconfigure
the device — they latch the mode and only re-arm when the mode actually changes.
Handed-only updates (mode omitted) do not trigger re-arming. Mode is global
state, not per-device.

### DeviceTelemetry / ActorStatus split

Device state and actor lifecycle are conveyed via two separate event variants
with **no overlapping telemetry keys**:

- **`DeviceTelemetry`** (FRP-compliant) — what the device reports about itself.
  Emitted any time a device-reported value changes. Carries identity
  (manufacturer, model, firmware) and device telemetry. `ready` is the single
  readiness signal (all device conditions met for a shot, including ball
  detection where applicable).

  Standard telemetry keys: `ready`, `battery_pct`, `tilt`, `roll`, `temp_c`,
  `external_power`.

- **`ActorStatus`** (flighthook extension) — what flighthook knows about the
  actor/connection. Carries a typed status enum and actor-framework telemetry.
  FRP-only consumers ignore this per spec (unknown `kind` value).

  Standard telemetry keys: `detection_mode`, `radar_mode`, `device_info`.
  Mock launch monitors add: `shot_count`, `tracking_mode`.
  Integration actors: `club`, `handed`, `name`, `error`.

```rust
pub enum ActorStatus { Starting, Disconnected, Connected, Reconnecting }
```

`ActorStatusResponse` (in `lib/src/api.rs`) adds a `name` field and is used
as the cached per-actor state in both the web layer and the UI. The web layer
merges telemetry from both `DeviceTelemetry` and `ActorStatus` events into
a single per-actor telemetry map for API consumers.

### ConfigCommand / ConfigOutcome -- event-sourced config mutations

Config mutations are routed through the bus as typed events, processed
exclusively by SystemActor. This parallels the GameState pattern (sole
writer via `GameStateWriter`).

```rust
// ConfigCommand fields (on FlighthookEvent)
request_id: Option<String>,  // correlation ID for request-reply
action: ConfigAction,

pub enum ConfigAction {
    ReplaceAll { config: FlighthookConfig },        // POST /api/settings
    ReplaceGlobal { config: FlighthookConfig },     // POST /api/settings?scope=global: global fields only
    UpsertMevo { index: String, section: MevoSection },
    UpsertR10 { index: String, section: R10Section },
    UpsertSquare { index: String, section: SquareSection },
    UpsertOpenConnectServer { index: String, section: OpenConnectServerSection },
    UpsertGsPro { index: String, section: GsProSection },
    UpsertWebserver { index: String, section: WebserverSection },
    UpsertMockMonitor { index: String, section: MockMonitorSection },
    UpsertRandomClub { index: String, section: RandomClubSection },
    Remove { id: String },                          // "mevo.0", "gspro.1", "webserver.0", etc.
    SetShotGrouping { mode: ShotGrouping },         // POST /api/shots/grouping, WS `grouping`
}

// ConfigOutcome fields (on FlighthookEvent)
request_id: Option<String>,
restarted: Vec<String>,
stopped: Vec<String>,
started: Vec<String>,
```

The POST handlers (`/api/settings`, `/api/shots/grouping`) use request-reply:
emit `ConfigCommand` with a `request_id`, subscribe to the bus, and wait for
`ConfigOutcome` with the matching ID. `SetShotGrouping` persists the default
view and reconciles no actor; no set changes. `/api/settings` answers `400`
for an unknown scope and `504` when no `ConfigOutcome` arrives in 10 s.
`ConfigOutcome` is always emitted after processing, even for fire-and-forget
commands (where `request_id` is `None`).

## GameState

`GameState` in `state/game.rs` -- read-only handle for the current round's
`PlayerInfo` (player name), `ClubInfo` (club selection), and
`ShotDetectionMode` (global detection mode). Lives inside `SystemState` as
the `game` field. Only exposes `snapshot()`. All actors and the web layer
can read via `state.game.snapshot()`.

Mutations flow through `GameStateWriter`, a separate write handle that only
the `SystemActor` holds. `GameState::new()` returns `(GameState, GameStateWriter)`;
both share the same `Arc<GameStateInner>`. This enforces at the type level
that only `SystemActor` can mutate game state.

```rust
struct GameStateInner {
    player_info: RwLock<Option<PlayerInfo>>,
    club_info: RwLock<Option<ClubInfo>>,
    mode: RwLock<Option<ShotDetectionMode>>,
}

pub struct GameState { inner: Arc<GameStateInner> }        // read-only
pub struct GameStateWriter { inner: Arc<GameStateInner> }  // write handle (SystemActor only)

pub struct GameStateSnapshot {
    pub player_info: Option<PlayerInfo>,
    pub club_info: Option<ClubInfo>,
    pub mode: Option<ShotDetectionMode>,
}
```

## ActorStatus Lifecycle

Generic actor lifecycle. All actors (launch monitors and integrations) emit
`ActorStatus` events on the bus.

```
Starting -> Connected -> (error) -> Reconnecting -> Connected -> ...
   |                                     ^
   +-> Disconnected --------------------+
```

| ActorStatus  | Meaning                                                    |
| ------------ | ---------------------------------------------------------- |
| Starting     | Attempting first connection (including retries before ever connecting) |
| Connected    | Active and operational                                     |
| Reconnecting | Lost a prior connection, retrying with backoff             |
| Disconnected | Stopped/shutdown (terminal state)                          |

All actors use linear backoff on connection failure: starts at 1s, adds 1s per
failed attempt, capped at 15s (3s for the GSPro bridge, a loopback/LAN TCP
connect). The GSPro and Square Golf actors restart at 1s after a successful
connection; the Mevo and R10 actors keep the grown delay. Backoff waits poll
the actor's shutdown flag, so stopping an actor never waits out the delay.

**Mevo actor mapping** (from internal phases):

- `Connecting | Handshaking | Configuring | Arming` -> `Starting`
- `Armed` -> `Connected` with `telemetry["radar_mode"] = "armed"`
- `Shooting` -> `Connected` with `telemetry["radar_mode"] = "idle"`
- Connection lost (never connected) -> `Starting` (retrying)
- Connection lost (previously connected) -> `Reconnecting`

Post-shot cycle: `Connected(radar_mode=idle)` -> `Starting` (re-arm) -> `Connected(radar_mode=armed)`.

**R10 actor**: BLE auto-discovery via `tenover::BleTransport::auto_connect()`.
Emits `Starting` while scanning/connecting, `Connected` after handshake
completes. Emits `DeviceTelemetry` with `manufacturer: "Garmin"`,
`model: "R10"`. Telemetry keys: `ready`, `tilt`, `roll`, `device_state`
(`waiting` | `recording` | `processing`). Uses BLE address as FRP device ID.

**GSPro actor**: emits `Starting` before first TCP connect attempt, `Connected`
on success. Connection loss after a successful session emits `Reconnecting`;
failure before ever connecting stays at `Starting`. A refused, failed or
closed connection is shown only through this status and the log, with no alert:
GSPro not running is normal when another simulator is in use.

**GSPro shot delivery**: the bridge drains the bus the whole time it runs,
connected or not, so nothing queues up while GSPro is away. Every shot that
finishes while GSPro is connected is sent, in order, including several that
finish together. A shot that finishes while GSPro is not connected (stopped,
starting up, or between reconnects) is dropped, as is a connected shot that
is more than 5s old by the time the bridge can send it (e.g. the connection
dropped and came back slowly). Dropped shots are logged at debug level only,
with no alert: GSPro not running is normal when another simulator is in use.
A shot whose write fails is not retried. The JSON sent for a shot is the same
whatever happened to the connection before it.

**Integration readiness**: the GSPro actor tracks device readiness from
`DeviceTelemetry` (`"ready"` key) and uses it to set both
`launch_monitor_is_ready` and `launch_monitor_ball_detected` in heartbeats
and shot messages.

## SystemActor

The `SystemActor` (`actors/system.rs`) is a default actor that always runs,
independent of config. It holds the sole `GameStateWriter`, `Arc<SystemState>`,
and `broadcast::Sender`, enforcing at the type level that only `SystemActor`
can mutate game state and process config mutations. It handles:

- **Game state**: subscribes to `PlayerInfo`, `ClubInfo`, and
  `SetDetectionMode` bus events, updates game state via the writer.
  **Auto-derives detection mode from club selection**: when `ClubInfo` is
  received, calls `club_mode()` and emits `SetDetectionMode` on the bus.
  This centralizes mode derivation so all integrations trigger mode changes.
- **Config mutations**: subscribes to `ConfigCommand` bus events (from the
  REST API). Applies the mutation to `SystemConfig`, calls
  `apply_config_reload()` to reconcile actors, and emits a `ConfigOutcome`
  on the bus. This provides natural sequencing -- all config mutations are
  processed one at a time on the SystemActor thread.
- Ensures game state and config are consistent even without the web server

Created via `SystemActor::new(writer, state, bus_tx)` in `main()` before
config-driven actors, registered with ID `"system"`. Skipped by
`apply_config_reload()` (not config-driven).

`actors/system.rs` also contains the `apply_config_reload()` reconciliation
function.

The web layer's `state_updater` task does NOT update game state -- it only
caches club and player name values in the per-actor telemetry map for the UI. It
also handles `ConfigOutcome` events to refresh actor name caches.

## Component Identity

All components are identified by type-prefixed global IDs: `mevo.0`, `r10.0`,
`gspro.0`, `mock_monitor.0`, `random_club.0`, `webserver.0`, `ws.a1b2c3d4`. The `system`
actor has a fixed ID of `"system"`. The type prefix encodes the component type;
the index is the key within that type's config section.

`FlighthookMessage.actor` carries the global ID of the message originator.
Inner event types carry no ID or name fields; identity lives on the
`FlighthookMessage` envelope.

## SystemState

Root entry point for all managed application state, passed as
`Arc<SystemState>` to all actors and the web layer.

```rust
pub struct SystemState {
    pub system: SystemConfig,     // cached config (RwLock + disk persistence + reload lock)
    pub game: GameState,          // read-only game state for the current round
    actors: RwLock<HashMap<String, (Box<dyn Actor>, Arc<AtomicBool>)>>,
}
```

`SystemState::new()` returns `(Self, GameStateWriter)`. The writer is passed
to `SystemActor` at construction time.

`system` provides cached read/write access to the persisted TOML config.
Config mutations are serialized through the bus (SystemActor processes
`ConfigCommand` events one at a time on its thread), so no external lock
is needed. `game` is a read-only handle for player info and club selection;
mutations go through `GameStateWriter` held by `SystemActor`. `actors` is a
registry of all running actors, keyed by global ID. Each entry stores the
actor and its per-actor shutdown flag (`Arc<AtomicBool>`). Methods:
`register_actor(id, actor, shutdown)`, `stop_actor(id)` (sets flag + calls
`actor.stop()`), `remove_actor(id)`, `actor_ids()`.
Actors are registered after construction and before `start()` is called.

## Log

The Log tab in the UI streams all bus events in real-time with per-message-type
filter checkboxes. Raw wire data is carried via `raw_payload` on bus messages.
Binary payloads serialize as hex strings; GSPro JSON payloads serialize as-is.
Events without a `raw_payload` (e.g. `actor_status`, `config_outcome`) display
the event's JSON serialization as the payload instead. The log retains the last
500 events. Protocol-level tracing is always emitted to the `audit` tracing
target (filtered by `RUST_LOG`, not shown by default).

## WebSocket Init Protocol

WS connection at `/frp`, three-phase handshake with version negotiation:

1. Client sends: `{ "kind": "start", "version": ["0.1.0"], "name": "My Dashboard" }`
2. Server negotiates version (highest mutually supported). If no compatible
   version, sends a `critical` alert and closes the connection.
3. Server responds: `{ "kind": "init", "version": "0.1.0", "actor_id": "ws.a1b2c3d4", "global_state": { ... } }`
4. Server streams `FlighthookMessage` events (serialized directly as JSON)

Supported FRP versions: `0.1.0`. A client that offers no mutually supported
version — including one that sends no `version` array — receives a `critical`
alert and is disconnected.

Bus events are serialized using the FRP envelope shape with `kind` inside
`event`. Consumers filter by `event.kind`.

Client -> server commands:

```json
{ "cmd": "mode", "mode": "putting" }
{ "cmd": "club", "club": "7I" }
{ "cmd": "new_group" }
{ "cmd": "new_session" }
{ "cmd": "clear_history" }
{ "cmd": "grouping", "mode": "by_club" }
```

Mode commands emit `SetDetectionMode` on the bus (mode is global,
not per-device). `club` emits `ClubInfo`, `new_group` / `new_session` /
`clear_history` emit `NewShotGroup` / `NewShotSession` / `ClearShotHistory`,
and `grouping` emits a fire-and-forget
`ConfigCommand` (`SetShotGrouping`), each with the socket's actor ID. Each has
a REST twin (`POST /api/club`, `/api/shots/groups`, `/api/shots/session`,
`/api/shots/clear`, `/api/shots/grouping`). Config updates go through `POST /api/settings` ->
`ConfigCommand` on the bus -> SystemActor processes -> `ConfigOutcome` reply.

## Threading Model

```
main thread (eframe)       std::thread per actor              tokio runtime (background)
+-----------------+        +------------------------+        +------------------------+
| eframe::run_    |        | actors::system         |        | axum web server        |
|   native() or   |        |   (GameState updates)  |        | state_updater task     |
| ctrl-c (headless|        | actors::mevo           |        | drain subscriber       |
+-----------------+        | actors::r10            |        +------------------------+
                           | actors::mock::launch   |
  GUI --ehttp/ws---->      | polls bus via poll()    |            ^
  (to local web server)    +------------------------+            |
                                     ^                            |
                                     +--- single broadcast<FlighthookMessage>(1024) ---+
                                     |                            |
                           +--------------------+        +--------------------+
                           | actors::gspro       |        | actors::mock::     |
                           | GSPro TCP bridge    |        |   randomclub       |
                           +--------------------+        +--------------------+
```

**Bus-based command delivery**: actor threads poll the bus via `BusReceiver::poll()`
in their event loop. They filter for `SetDetectionMode` events (for mode
changes). The `SystemActor` auto-derives detection mode from `ClubInfo`
using `club_mode()`, so integrations only need to emit `ClubInfo`. There is
no per-device `mpsc` channel and no centralized router task.

**Actor trait and bus wrappers**: all actors implement the `Actor` trait
(`start()` + `stop()` + `reconfigure()`) and receive a `BusSender`/`BusReceiver`
pair on startup. `BusSender` wraps `broadcast::Sender` and auto-stamps the
actor's global ID on every outbound message. `BusSender` provides
`send()` for bus messages and `subscribe()` for creating new receivers.
`BusReceiver` wraps `broadcast::Receiver` with `poll()` (non-blocking drain;
returns `Ok(None)` if empty, `Err(PollError::Shutdown)` if the per-actor
shutdown flag is set or bus is closed) and `is_shutdown()`.
Actor structs hold their config; `start()` clones what it
needs and spawns a thread. `reconfigure()` compares current config against
construction params and returns `Applied` or `RestartRequired`.
Actors are registered in `SystemState.actors` for dynamic
start/stop/reconfigure.

**Per-actor spawning**: the `SystemActor` is created first (always-on, not
config-driven). Then each config section constructs an actor struct and calls
`actor.start(state, sender, receiver)`. Each actor gets a per-actor
`Arc<AtomicBool>` shutdown flag shared between the `BusReceiver` and the
registry. Launch monitor actors (mevo, r10, mock::launch) run event loops polling
the bus. Integration actors (gspro, mock::randomclub) subscribe to the bus
and emit `ClubInfo` (for club changes) and `ActorStatus`
(for connection status) back onto the bus.

**Drain subscriber**: a tokio task that consumes all bus messages to keep the
broadcast channel healthy when no other subscriber is active.

## Config Reload

Config reloads are event-sourced through the bus. All config mutations flow
as `ConfigCommand` events, processed exclusively by `SystemActor`:

- **REST API** -- `POST /api/settings` emits `ConfigCommand` (action:
  `ReplaceAll`) on the bus with a `request_id`, then awaits the matching
  `ConfigOutcome` (request-reply pattern, 10s timeout; `504` with a JSON
  `error` body when it doesn't come).
- **Global settings** -- `POST /api/settings?scope=global` emits
  `ReplaceGlobal`: units, chipping/putting clubs, default view and
  simulation from the body, every actor section of the live config kept.
- **Per-section upserts** -- `POST /api/settings?scope=<id>` emits a scoped
  `ConfigAction` variant (e.g. `UpsertMevo`), writing only that section and
  reconciling only that actor. A scope naming a section the body omits emits
  `Remove`; an unknown scope is rejected with `400` without saving anything.

`SystemActor::handle_config_command()`:

1. Applies the `ConfigAction` mutation to `SystemConfig` (replace, reload,
   upsert, or remove)
2. Calls `apply_config_reload()` to reconcile actors
3. Emits `ConfigOutcome` on the bus (always, not just for request-reply)

`apply_config_reload()` (in `actors/system.rs`) orchestrates actor lifecycle:

1. Snapshot the new config and build a `ResolvedConfig`
2. Compute current vs expected actor IDs
3. **Deleted actors** (current but not expected): stop + remove
4. **Existing actors**: call `reconfigure()` on each
   - `RestartRequired`: stop old actor, create and start new one
   - `Applied`: no change needed, skip
5. **New actors** (expected but not current): create and start

The web layer's `state_updater` handles `ConfigOutcome` events to refresh
actor name caches and remove stopped actors.

`ReconfigureOutcome` enum:

- `Applied` -- config unchanged or applied without restart (default for mock actors)
- `RestartRequired` -- address/routing/settings changed; must stop and recreate

## Devices Tab

The Devices tab is where the app opens. It holds every launch monitor and
output, with its live telemetry and its settings, plus the global settings.
Three groups of cards, each group a wrapping grid: as many columns as fit at
a 420 px minimum card width, cards at most 620 px wide, one column on a
narrow window; card `i` goes in column `i % columns`.

```
  [Devices] [Shots] [Log]  |  [Full] [Chipping] [Putting]  CLUB [7I]  FLIGHTHOOK

  GENERAL
  | Global                           [API Docs] | Simulation [beta]
  | Default Units:     [Imperial v]             | Simulation:   [On v]  (Off | On | Force)
  | Default shot view: [By set | By club | None]| Altitude:     [0] [ft v]   (greyed while Off)
  | Chipping Clubs:    [PW GW SW LW]            | Temperature:  [77] °F      (°C when Metric)
  | Putting Clubs:     [PT]                     | Surface:      [Fairway v]

  LAUNCH MONITORS (+)
  | Mevo WiFi   [CONNECTED] [Telemetry|Settings] | Garmin R10 [R10]      [Telemetry|Settings]
  |   battery_pct  85                             | Name:            [Garmin R10]
  |   ready        true                           | Monitor-to-Ball: [8] [ft v]
  |   tilt         0.5                            |                               [Remove]

  OUTPUTS (+)
  | Local GSPro [GSPro]        [Cancel] [Save]   | Web Server  [CONNECTED] [Telemetry|Settings]
  |                  Save or cancel your changes |   bind        0.0.0.0:5880
  | Name:             [Local GSPro]              |   requests    12
  | Address:          [127.0.0.1:921]            |   websockets  1
  | Full Monitor:     [Mevo WiFi v]              |
  | Chipping Monitor: [Any v]                    |
  | Putting Monitor:  [Any v]                    |
  |                                     [Remove] |
```

Groups and order (`ui/src/panels/cards.rs`):

| Group | Cards, in order | (+) offers |
|---|---|---|
| General | Global card, Simulation card | -- |
| Launch monitors | `mevo`, `r10`, `square`, `mock_monitor`, `openconnect_server` | Mevo, R10, Square Golf Omni, Uneekor (OpenConnect) |
| Outputs | `gspro`, `webserver`, `random_club` (and any type the UI does not know) | GSPro, Web Server |

Within a group, cards sort by type (the order above), then by instance index
(numerically). The Open Connect server is a launch monitor: it receives shots.
Random Club consumes shots (it picks the next club on each one), so it sits
with the outputs. A card is every configured instance plus every running
actor the settings don't list (`ui/src/panels/devices.rs`
`device_card_order`), so two GSPro connections or two launch monitors are two
cards, and the cards show (Telemetry only) before the settings have loaded.

### Device cards

Each launch monitor and output card has a small segmented toggle in its
header, **Telemetry | Settings**, Telemetry by default, remembered per card
for the session. It flips the card body:

- **Telemetry**: the actor's name and status badge (green connected, gold
  starting/reconnecting, red disconnected), then its telemetry key/value
  pairs one per row, sorted by key. Detection mode, battery, tilt, roll,
  temperature, club, handed and the rest come from the actor's telemetry map.
- **Settings**: the instance's name, its type tag (hover: what the type is
  and how it connects) and its fields in a label column, ending in
  **Remove**, which asks for confirmation in a modal dialog and then saves
  the removal (`?scope=<actor_id>`, the section left out); the card goes
  when the server confirms it.

The coloured stripe down the left edge of a card is the colour of the actor's
status, on both sides, dim while the actor is not running (unsaved, or not
started yet). The General cards' stripe is the heading blue; they have no
toggle (settings only).

### Unsaved changes

When a card's settings have unsaved changes, its toggle is replaced by
**Cancel** and **Save** (Save accent-filled, disabled while that card's
fields are invalid; each reason shows under its field), with the hint "Save
or cancel your changes" under them. Save
persists only that card (scoped request, below); Cancel puts the card back to
the settings last loaded or saved. The card stays on its Settings side after
either.

The Global and Simulation cards share one save request (`build_global_request`).
Each shows the pair while it has unsaved edits, and an edit on either card
leaves both editable: Save on either saves both, Cancel on either reverts
both; the Shots tab's units, which follow a change of default units, go
back to what they were before it.

While anything is unsaved the tab is **locked** to it: the tab bar, the other
cards' toggles and edit controls, and both (+) buttons are disabled (the
mode selector and club picker in the header stay live). Escape doesn't
discard. While a save is out the tab is locked too, and every card's
controls (the saving card's fields, Save, Cancel and Remove included) wait
for its reply.

A save that fails (no reply, a non-2xx status, or a body that isn't a save
response) unlocks its card with the edits kept and a "Save failed: <reason>"
line under the header. The settings are then read again: an edit the server
already holds turns clean, since a save can be applied while its reply is
lost (a Web Server `bind` change restarts the server that answers). A reply
marks clean only what it posted; an entry that differs from it, or that the
server doesn't hold, stays unsaved. A `ConfigOutcome` on the bus (any
client's save) also reads the settings again; entries without unsaved edits
take the server's values.

### Adding

A (+) after the Launch monitors and Outputs headings (Codicons `add`, muted,
accent on hover; hover text "Add a launch monitor" / "Add an output") opens a
menu of that group's types. The new card appears on its Settings side,
unsaved, with Save and Cancel; Cancel removes it. The (+) is disabled until
the settings have loaded.

### Fields

Config sections are type-prefixed; their keys are integer indices (never
displayed).

- Name is **required** and serves as the rename mechanism. Changing a device name
  updates the name cache in WebState, so subsequent status responses and shot
  data use the new name.
- `Use Estimated` is per-mevo (checkbox, off by default) -- controls whether
  estimated (E8) ball flights are emitted when no full (D4) result arrives
- `Environment` is per-mevo (dropdown: Limited flight (sim/indoor) / Outdoor
  (full flight)) -- the full-swing detection mode; a change restarts the actor
- `Camera Mode` is per-mevo (dropdown: Standard / Fusion / Raw Fusion) --
  the Fusion modes request the full club data set from the device (radar club
  data arrives in every mode)
- R10 cards show name and Monitor-to-Ball, the tee distance sent to the device
  (BLE auto-discovery; no address or radar fields)
- Mock launch monitor and Random Club are developer-only types (must be added
  manually to the config TOML; no (+) menu offers them)
- Add/remove modify the form Vec, save writes back to TOML
- New entries get the lowest integer index their type does not use yet (0, 1,
  2...) and default names
- Per-card save: a device card's Save builds a scoped config (saved config +
  only that card's changes) and passes `?scope=<actor_id>` to the backend.
  The backend only reconfigures the target actor. The Global and Simulation
  cards save with `?scope=global`, which keeps every actor section of the
  live config. The UI keeps the server's config from the last load or save
  as the base of each request and as what Cancel goes back to.
- Simulated values are global settings, saved with the Global card. The
  form rejects altitudes outside 0 to 5000 m and temperatures outside -30 to
  50 °C; temperature is edited in the display unit and stored in °C
- Config changes auto-reconfigure actors (all changes trigger restart)
- Webserver bind changes trigger a restart (same as address changes for other actors)

### API

`GET/POST /api/settings` returns/accepts the full `FlighthookConfig` shape
(type-prefixed maps + webserver section). `POST` emits a `ConfigCommand`
(`ReplaceAll`; `ReplaceGlobal` for `scope=global`; an `Upsert*` or `Remove`
for an actor scope) on the bus and waits for a `ConfigOutcome` reply.

### Header

Detection mode is **global state** (not per-device). The mode selector
buttons are in the tab bar, between the tab selectors and the title.
Clicking a mode button emits `SetDetectionMode` on the bus.
Launch monitor actors react to `SetDetectionMode` events to reconfigure the device.

The **club picker** sits beside the mode selector on every tab: a pill with
the club in play (`—` when none), from `/api/status.club`, the WebSocket
`init.global_state` and every `club_info` event. Clicking it opens a popup of
all 33 clubs in bag rows (driver + mini driver, woods, hybrids, irons, wedges,
putter); hovering a
club names the mode it switches to when that is Chipping or Putting. A pick
sends the WebSocket `club` command (`POST /api/club` when the socket is down),
and the SystemActor then derives the detection mode as for a GSPro club
change. While any `gspro.*` actor is Connected the pill is read-only, with
the hover text "Set by GSPro": GSPro replaces the club on its next player
message, so its telemetry `club` and the pill agree.

The tab bar is a segmented control, the same look as the mode selector beside
it (`theme::segmented`). If any actor has status `starting`, `disconnected`,
or `reconnecting`, the Devices segment carries a small red notification dot at
the top centre, overlapping its top edge, whatever its selection state. The
web layer caches telemetry from `ActorStatus` events plus club from `ClubInfo`
and name from `PlayerInfo` events so reconnecting clients recover latest
values immediately.

## Shots Tab

```
  VIEW [Cards|Table]  UNITS [Imperial|Metric]               EXPORT SESSION JSONL CSV [trash]

  Layout: the charts are always shown, sized from the window (not resizable)

  Cards, window >= 1200 px wide            Cards, narrower
  +------+---------------------+-------+   +------+----------------------------+
  | shot | header, hero cards, | Dis-  |   | shot | header, hero cards, range  |
  | list | range bars, Ball /  | per-  |   | list | bars, Ball / Flight / Club |
  |      | Flight / Club cards | sion  |   |      | cards (shrink to fit)      |
  |      | (shrink to fit)     |       |   |      +----------------+-----------+
  |      +---------------------+       |   |      | Flight (62 %)  | Dispersion|
  |      | Flight              |       |   |      |                | (38 %)    |
  +------+---------------------+-------+   +------+----------------+-----------+
  Dispersion 24 % of the width (260-420     bottom row 27 % of the height
  px); Flight 30 % of the height            (210-320 px)
  (180-320 px)

  Table, window >= 1200 px wide            Table, narrower
  +-----------------------------+-------+   +-----------------------------------+
  | shot table                  | Dis-  |   | shot table                        |
  |                             | per-  |   +-----------------------------------+
  |                             | sion  |   | Dispersion (35 % of the height,   |
  +-----------------------------+-------+   | 266-400 px)                       |
  Dispersion as in Cards                    +-----------------------------------+

  Cards (default), wide
  SHOTS 42             Shot #42  LATEST          (older shot: [^ Back to latest])
  [By set|By club|None][ Ball speed ] [ Carry ] [ Total ] [ Launch ] [ Backspin ]
  SET 3 · 7I · 10 ---                         DEVICE [All|Mevo+|Omni] (2+ devices)
  [#42      ]          SET 3 · 7I · 10 SHOTS ------  range | avg ○ median ● this shot
  [carry tot]          TOTAL      148 |  [====○====●====|]   217  avg 192 · med 191
  [#41      ]          CARRY      148 [==●=○==|]             217  avg 174 · med 174
  SET 2 · DR · 4 ----  LATERAL    L     [=====○=●===|]        R  avg -1.1 · med -0.5
  [#40      ]          BALL --------  FLIGHT --------------  CLUB ----------------------
  (list scrolls)       [Direction]    [Apex]    [Offline]    [Club speed] [Smash] [Path]
  ------------------   [Sidespin]     [Descent] [Roll]       [AoA]        [Face]  [Loft]
  Next shot starts ... +---------------------- Flight -----------------------+
  [+ New set][New sess]| FLIGHT yd · ft     APEX 94 ft · CARRY 173 · TOTAL 185 |
                       | 90-       .-- 94 ft --.      (faint: rest of session) |
                       |  0- ^----'             '---o~~o---------------------- |
                       |     0     50    100    150 173 185   200   250        |
                       +-------------------------------------------------------+

  Table  (toolbar also shows: italic = simulated)
  +-------------------------------------------------------------------------------+
  | Device  #  Club  Ball  VLA ...  (header stays put while the rows scroll down) |
  | shot table (one row per shot, latest row highlighted, sticks to the bottom)   |
  +-------------------------------------------------------------------------------+

  Dispersion
  +- DISPERSION yd   [Total|Carry] [2σ|95 %] -+
  |           DEVICE [All|Mevo+|Omni]          |  Table only, 2+ devices
  | 225 -------------------:------------------ |
  | 200 ---------  .-( ·  ·:+ · )-.  --------- |  ellipse, gold + at the mean
  |                '-( ●   :  · )-'            |  gold dot at rest (Total),
  | 175 ------------ |     :  ---------------- |  gold roll line from landing
  |                        :                   |  faint: rest of the session
  |  50    ------------    :    ------------   |  (rings about the tee)
  |                       '^'                  |
  |  L  20                 :               20 R|
  +--------------------------------------------+
```

A toolbar (view toggle, units, export links and Clear history) sits above
both views. Only
Table adds the "italic = simulated" legend, since only Table draws simulated
values in italics (`ShotsView::shows_simulated_legend`). The chosen view
(`ShotsView` in `panels/shots.rs`) lasts for the session and is not saved.
Both views always show their charts, in one fixed layout per window size
(`chart_slots` in `panels/shots.rs`); nothing is toggled or dragged, and
every size follows from the window's, so the layout holds still until the
window changes.
With the window at least 1200 px wide, **Dispersion** fills a full-height
column on the right, 24 % of the window's width (260-420 px), in both views,
and Cards has **Flight** along the bottom of its centre column (between the
shot list and the Dispersion column), 30 % of the window's height (180-320
px). Narrower, Cards puts Dispersion in the bottom row beside Flight (62 % of
the row for Flight, 38 % for Dispersion, widened to at least 312 px, up to
half the row, so its header keeps to one row), so the cards keep the
centre's width; the row is 27 % of the window's height (210-320 px). Narrow
Table puts Dispersion under the table, 35 % of the window's height (266-400
px). The floors hold a Dispersion plot (inside its label gutters) of at
least 144 px in the Cards row, the most the cards leave at 1024×768, and
200 px under the Table. Each extra header row (30 px: the switches on their
own row in a panel narrower than 312 px, and Table's device filter) raises
the row by its height, so the plot keeps that minimum.

**Cards** has a resizable shot list down its left-hand side (200-320 px):
a header line with the shot count, then one mini card per shot, newest
first, with the shot number (gold for the latest shot), the device name, and
carry and total as plain upright numerals, measured or simulated alike, with
no simulated distinction -- a dash when there's no value. The device name is
shown only when the session's shots come from more than one device
(`groups::multiple_devices`, by actor identity via `same_actor`: a device
keeps its actor id across a reconnect, so it still counts once); with one
device it is left out everywhere a device name would otherwise appear
(here, the shot header, and the Table view's Device column). Everywhere it
shows (these, the device filter and the chart tooltips) it is the actor's
configured name, or its actor id when the name is empty or the actor is
unknown (`chart::device_label`). Shots with no
data are listed dimmed; once such a shot has finished (`shot_finished`)
with no ball or club data, its entry reads **No read** in place of carry
and total. That is a device non-read, or an estimated-only shot skipped by
`use_estimated = false`. Clicking an entry shows that
shot; the entry on show has an accent outline and stripe. The view follows
the latest shot, the newest one with data: when a new one arrives it is
selected, scrolled into view and glows with the cards, even if an older shot
was on show. While an older shot is on show, the header's LATEST chip is
replaced by a **Back to latest** chip. The selection survives the shot list
being reloaded (e.g. after a reconnect): the shot is found again by actor and
shot id when both sides have one, else by actor and shot number, and if it is gone the
view falls back to the latest shot (`ShotFocus` in `panels/shots.rs`).

Under the count line, a selector picks the **view** (By set | By club |
None). It starts on the `shot_grouping` setting (default By set); a click
applies it at once and saves it as the default view, over the WebSocket,
else `POST /api/shots/grouping`. The view only changes what the tab shows:
sets are recorded the same way in every view. The list scrolls between the
selector and its **foot**, pinned at the bottom of the column: a hairline, a
one-line hint when there is one, then **+ New set** (`new_group`; disabled
while the open set has no shots) and **New session** (disabled while no set
is open). The hint reads "Next shot starts a new set (DR)" in By set when the
club in play differs from the open set's, and "No club selected" when no club
is in play. The view `none` has no foot.

**None** shows the last shot only: the list is a plain newest-first list with
no headers, and the cards have no range bars and no device filter. **By set**
keeps the list chronological: a header goes before each run of consecutive
rows of one set, reading `SET 3 · 7I · 10 SHOTS` with the local start time,
accent while the set is open and muted once closed. The set the next shot
joins, while it is empty, gets a gold header at the top (`SET 4 · 7I · NEXT
SHOT`). **By club** clusters the list: one section per club and session,
combining every set with that club since the last New session, newest shot
first, under a header named by its club (`club_name` in `groups.rs`):
`7 IRON · 2 SETS · 14 SHOTS` (the set count shows when there are two or
more), `NO CLUB · …` for shots hit with no club, and a muted `7 IRON ·
EARLIER · 4 SHOTS` for a section from an earlier session (its set count is
in the hover text, which keeps the title short). A title too long for the
list is cut with an ellipsis before its last part, so the shot count always
shows whole. + New set does not
split a club's section; only New session does. While the open set is empty
and the club in play has no section in the session yet, a gold `7 IRON ·
NEXT SHOT` header sits at the top. The section of the club in play in the
current session comes first; every other section follows by last activity,
newest first. A section's activity is its newest row, or, in the current
session, the last time its club was selected, whichever is later; an earlier
session's section only sinks. Selections are tracked client-side
(`ShotGroups::observe_club`, each frame), each placed after the newest row
with data at the time, so shots and selections order on one history.

The UI learns the session from `GET /api/shots/groups` (`session`), counts it
up on every `new_shot_session` and `clear_shot_history` it sees on the
WebSocket, and never holds it
below a listed set's `session` (`ShotGroups::session`). Header counts are the
server's, so they cover shots older than the list holds. Rows without data
(a shot in flight, a dead trigger) never break a run. The rows before the
first row of a known set (a shot in flight, rows with data whose set is not
known yet) come first, under a dim `NOT IN A SET` header when a header is
above them. Later rows in no known set get the same header: in place in By
set, gathered in one last section in By club.

**Clear history** is the trash icon after the CSV link in the toolbar's
export group: a Codicons glyph (`theme::icon` with `Icon::Trash`; the
font is `ui/assets/fonts/codicon.ttf`, compiled in with `include_bytes!` and
registered as its own font family by `theme::install`), as tall as the
export links, muted at rest and alert red on hover. It is disabled while
there are no shots. A click opens a modal: "Delete all N shots and sets?
This can't be undone. Export first if you want to keep them." with Cancel
and a red Delete; Escape or a click outside cancels. Delete sends
`clear_history` over the WebSocket, else `POST /api/shots/clear`. Every
dashboard that sees `clear_shot_history` on the WebSocket clears at once
(`FlighthookApp::clear_shot_history`): shots, sets (the session counts up),
the shown shot, the device filter, the caches and an open confirmation. The
Shots tab then shows its empty state.

The UI loads the shots with `GET /api/shots?limit=1000` (`net::SHOTS_LIMIT`,
the server's whole history) and the sets with `GET /api/shots/groups`, on
connect and whenever the sets fall out of step. Each fetch is tagged with
the UI's history generation (`FlighthookApp::history_generation`), which
counts up on every `clear_shot_history` and `new_shot_session`. A shots or
sets reply requested before a Clear history is dropped when it lands, so a
fetch still in flight can't bring the cleared rows back; the empty history
and the live events since are all there is. A sets reply requested before a
New session is dropped and fetched again, so it can't wind the session back.

The cards show a header (shot number, device name, LATEST or Back to
latest), a headline row of five large cards, then the Ball, Flight and Club
groups of smaller cards. The groups sit side by side in two rows when each
card gets at least 96 px; otherwise in two columns two cards wide (Ball over
Flight, then Club) when that leaves 96 px; otherwise they stack with their
cards wrapping. Detail cards stay shorter than the headline cards.

The centre column always tries to show everything above the panel under
it (`cards_layout`, from the height between the toolbar and that panel,
every frame). With room to spare, the cards grow with the height up to a
limit and the spare height is split above and below. Short of room, every
height in the column (the header line, headline and detail cards, the
range bar rows, label lines and gaps) shrinks towards a compact size by
the same share, just enough to fit; card padding and labels shrink with
the cards. The compact sizes keep the numerals readable: headline cards
68 px tall with numerals of at least 28 px, detail cards 52 px with at least
18 px, bar rows 26 px. At 1920×1080, 1600×900, 1440×900, 1280×800 and
1024×768 every card fits with no scrolling, with the shot list at its
default width or at the widest it opens at. Only a shorter centre (e.g.
900×700) scrolls: then the cards stay at their compact sizes, a solid scroll
bar shows beside them, and the column ends a chart gap below the last row so
it clears the panel when scrolled to the end. Numerals shrink to fit their
card width.

The view `none` tracks only the last shot, so it has no range bars and no
device filter. In By set and By club, three **range bars** sit between the
headline row and the detail groups: Total, Carry and Lateral over the view's
members around the shown shot (`chart::members_for`). By set: the shown
shot's set. By club: that club's shots in the shot's session (for a shot of
the current session, the club since the last New session). A shot with no set
uses the set the next shot joins; with none, the tracks are empty and a note
says the next shot starts one. The section's title matches the list header
(`SET 3 · 7I · 8 SHOTS`, or `7 IRON · 2 SETS · 14 SHOTS` in By club; accent
open or current, muted closed or earlier, gold for an empty open set), with a
legend on the right; when the title and the legend (with its device chips)
don't both fit on the line, the legend moves to a row of its own under the
title (`legend_place`), so the two never overlap. With shots from two or more devices, a **device filter**
(`DEVICE [All | <device>…]`) sits above the bars; it applies before the view,
and with All each bar's band splits into one sub-band per device, coloured by
device. The members are cached on `chart::MembersKey` (shot and set
revisions, view, device filter, shown shot, club in play). Each bar is
drawn on a track, painted in this order: the set's range as a translucent
accent band with its min and max beside it; the median as a hollow white
ring sitting on the track's centre line, its radius just inside the track's
height; the shown shot as a filled gold dot (full-strength `theme::GOLD`),
sized to sit inside the ring (filling it when the two coincide), with a thin
dark halo so it reads against the band; and, on top of everything, the
average as a thin gold line (`theme::GOLD`, with a thin dark halo of its
own) that sticks out past the track, top and bottom. A value shared by
every marker therefore reads as a white ring filled with a gold dot, with a
gold line through it poking out above and below. Total and Carry share one
domain, the set's extent padded by 15 % of its range and at least 10 yd (9 m) a side,
so roll reads as the offset between the bands; the domain's ends are printed
dimly at the track ends. Lateral is symmetric about the target line (a tick
at 0; L and R at the ends) out to 1.2× the widest value, and at least ±6
units. A single shot draws a 16 px pip, wide enough that it still peeks out
past the ring and dot. Total and Carry are picked like the cards' values
(below), and the stats run over the picked values;
Lateral is the carry offline, picked the same way (+ right of the target
line, for both hands). The bars carry no simulated marks; that distinction is the cards'
`SIM` tags and Table's italics. A stats column shows `avg · med`, then `sd` (two or
more values) and `k/n` when some of the set's shots lack the value, or
`no data`. Hovering a track lists n, min, max, avg, median and sd. The stats
come from `flighthook::Stats` over the rows the UI holds (the server's whole
history, up to the last 1000 shots, plus live ones), so for a session longer
than that they can cover fewer shots than the header's count.

**Flight** (beta, `ui/src/panels/flight.rs`) is a side view of the ball
flights in the Cards view's bottom row (see the layout above). The chart is
painted with egui's painter (no plotting crate). It plots each shot's
simulated path (`derived.path`): horizontal
distance from the tee, `hypot(x, z)`, so the landing sits at the carry and
the rest at the total, against height, in yd and ft (imperial) or m
(metric). The two axes scale independently ("stretched") over the bright
shots, the distance end at 1.06 × the furthest total rounded up to 25 (at
least 50; when every one of those shots is a ground ball, rounded up to 5, at
least 10) and the height end at 1.15 × the highest apex rounded up to 20 ft
or 10 m (at least 30 ft or 10 m); ticks thin out when they would sit closer
than 70 px (distance) or 24 px (height). What is drawn follows the view's
members (`chart::members_for`, after the device filter), like the range
bars. Paths are drawn as smooth curves through their points
(`chart::smooth_path`): a centripetal Catmull-Rom spline, which passes
through every point without loops or cusps, cut in screen space into as many
sub-segments as keep it within about 0.25 px of the curve (at most 12 per
segment, none much shorter than 6 px) and held inside each segment's
bounding box, so it never overshoots a point (the apex stays the top). An
end segment bends like the parabola through its first three points, so a
hop (contact, peak, contact) leaves the ground steeply and rounds over its
peak. The flight is one curve from the tee to the landing; the ground segment is cut
at every point on the ground (at most 5 mm up), so each hop is its own curve
and a bounce keeps its corner. The curves are built with the scene, not per
frame:

- **Gold**: the shown shot, over a dark halo, with a tick and label at its
  apex, dots at its landing and rest, and its carry and total under the
  ground line in place of the distance labels they would overlap. It is
  drawn only when it is a member, as on the bars. The header repeats its
  apex, carry and total.
- **Bright**: the view's other members (By set: the shown shot's set; By
  club: that club's shots in its session), in accent, or coloured by device
  (`theme::DEVICE_COLOURS`) when the filter is on All and there are two or
  more devices, with their landing and rest dots.
- **Faint**: the other shots of the current session, from the filtered
  device, newest 200, at 14 % alpha, for context. They do not set the
  scale and are clipped at the plot's edge.
- **None** draws the shown shot alone.

A ground ball (`flight::rolls`: a path whose flight is the tee alone and
whose ground segment is the roll) is drawn as a line along the ground from
the tee to rest, with the flight's stroke and colour rules (gold for the
shown shot, bright, faint), a rest dot but no landing dot, and, in gold, only
its total labelled; it is hovered and clicked like any path and draws on
along the roll. A shot without a path to draw (no simulation: no spin
reading, simulation off) gets 6 px ticks on the ground line at its displayed
carry and total, picked like the cards' values (`pick`); a path without a
ground segment gets a tick at its total. The latest
shot draws on along its path's time column during the new-shot pulse
(`ShotFocus::progress`, 1.5 s), cutting the cached curves by time
(`chart::Curve::prefix`), its labels appearing as the ball reaches them; a REST reload starts no pulse, so it draws whole. Hovering the panel
picks the nearest landing or rest dot or ground tick within 8 px, else the
nearest curve within 5 px (faint shots are never picked), brightens it, and
shows the shot's number, device (two or more devices), club, carry, total
and apex. A click shows that shot, as a click in the shot list does, and
scrolls the list to it. There is no zoom or pan. The projected geometry
(`flight::Scene`, panel-local points) is cached on `chart::ChartKey` (the
members key, units and panel size in whole points), so it is rebuilt only
when the members, units, shown shot, device filter or panel size change; a
frame copies the cached points into the painter.

**Dispersion** (beta, `ui/src/panels/dispersion.rs`) is a top-down view of
where the shots came to rest or landed, beside or under Cards and Table
(see the layout above). The header holds two switches, which last for the
session only: `Total | Carry`
(default Total: rest positions; Carry: landing points) and `2σ | 95 %`
(the ellipse size, default 2σ), on a second row when the panel is too
narrow for one. The axes are equal (one yard across is one yard up), with
the tee at the bottom centre, downrange up and right to the right
(target-relative for both hands, like offline), in yd or m; distance
rings about the tee every 25 (thinned below 60 px) are labelled where they
meet the plot's left edge, a dashed target line runs up the middle, and
lateral ticks with `L`/`R` sit along the bottom. The domain is 1.08 × the
furthest point rounded up to 25 (at least 50) by 1.3 × the widest lateral
rounded up to 5 (at least 15 either side) over the bright points, and the
axis with room to spare shows more.

A point is the shot's simulated path's landing (last flight point) or rest
(last ground point), lateral = `z` (a ground ball lands at the tee, and its
roll runs from there); without a path, the displayed carry or
total (picked like the cards) is the distance from the tee and the carry or
total offline (picked the same way) the lateral. A shot without both is left
out.

The drawn sets are Flight's (`flight::drawn_sets`): **gold** is the shown
shot when it is a member, a 5 px dot over a halo with its roll drawn as a
gold line from the landing to the rest; **bright** are the view's other
members, in accent or coloured by device when the filter is on All with two
or more devices; **faint** the rest of the current session from the
filtered device, newest 200, at 18 % alpha, clipped and not setting the
scale; **None** draws the shown shot alone. In Table view the panel is
built around the latest shot (its rows cannot be picked), and the panel's
header carries the device filter (two or more devices, not in view None),
which Cards keeps above the range bars. One covariance ellipse covers the
bright and gold points (three or more), or with the filter on All and two
or more devices one per device in its colour: centred on the mean, axes
along the eigenvectors of the sample covariance (n − 1), semi-axes k·√λ
with k = 2 for 2σ (about 86 % of a 2D normal pattern) or 2.448 for 95 %
(`chart::ELLIPSE_K_2SIGMA`, `chart::ELLIPSE_K_95`), each at least 1 yd or
1 m; filled at 10 % and outlined, with a gold `+` at the mean. The latest
shot flies out from the tee along its path's top view during the new-shot
pulse, smoothed like Flight's curves and built with the scene, its dot
appearing when the ball reaches the plotted end; the roll line is straight.

Hovering picks the nearest bright or gold dot within 8 px (faint shots are
never picked) and shows the shot's number, device (two or more devices),
club, carry, total and the plotted offline; inside an ellipse, the hover
gives its device, shot count, mean, size name, coverage and semi-axes in
display units. A click on a dot shows that shot and scrolls the list to it
(Cards; Table has hover only). There is no zoom or pan. The scene
(`dispersion::Scene`) is cached on `dispersion::DispersionKey` (the chart
key plus the two switches), so it is rebuilt only when the members, units,
shown shot, device filter, panel size or a switch changes.

**Table** shows
one row per shot, with a Club column after `#`, built with
`egui_extras::TableBuilder`: only the rows in view are laid out, so a
1000-shot session costs no more per frame than a short one. One scroll area
covers both directions, with its scroll bars on the view's edges; the
header (`TableHeader`) is painted over the top of the view after the rows,
so it stays put while they scroll down and moves with them sideways, and it
takes the pointer from the rows under it. Columns size to the widest header
or cell seen; a change of units or of the Device column sizes them afresh.
Its first column (Device when
it shows, else `#`) is inset so the latest row's gold stripe on the left edge
never covers its text. The Club column is the club selected when the
shot was triggered (`-` when none), with the shot's set number on hover (`Set 3`). Both views show a "waiting for the first shot" message
until there is one.

A metric card shows the simulated value upright with a `SIM` tag and hover,
else the measured one upright, else a dash. A table cell shows the simulated
value in italics with the same hover, else the measured one plainly, else a
dash. Derived wins when both are present: the server sets a field in both
only when the simulated value is the one to show (a `force` shot's
`replaces_measured`, a measured 0 carry or flight time on a shot that flies,
a ground ball's 0 carry, apex and flight time). One helper, `pick` in
`ui/src/panels/shots.rs`, makes this choice for the cards, the shot list, the
table, the range bars and the Flight and Dispersion charts. Both views follow the
Imperial/Metric toggle. When the
device reports no smash factor, the Smash card and table column show ball
speed over club speed when both are known; this is display only, and the bus,
GSPro output and export carry the device's value. When a new shot arrives the
cards glow with the accent colour, a few pixels wide and following their
rounded corners, and the table's latest row gets an accent outline; both fade
over 1.5 s.

The look is defined once in `ui/src/theme.rs` (palette, egui style, pill,
badge and card helpers) and installed at startup for both the web and native
UI; the dashboard is always dark.
