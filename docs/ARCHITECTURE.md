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

[openconnect_client.0]
name = "GSPro"
address = "127.0.0.1:921"
# full_monitor = "mevo.0"       # optional: route full-swing shots from specific monitor
# chipping_monitor = "mevo.0"   # optional: route chipping shots
# putting_monitor = "mevo.0"    # optional: route putting shots
```

- `[mevo.<idx>]` -- FlightScope device instance (Mevo/Mevo+, or Xi — detected at connect)
- `[r10.<idx>]` -- Garmin R10 BLE device instance (auto-discovers via Bluetooth)
- `[square.<idx>]` -- Square Golf Omni BLE device instance
- `[exputt.<idx>]` -- ExPutt RG putting device over WiFi (optional IP address;
  found on the local network when absent)
- `[openconnect_server.<idx>]` -- OpenConnect server: GSPro Open Connect ingest listener (Uneekor)
- `[mock_monitor.<idx>]` -- mock launch monitor instance
- `[openconnect_client.<idx>]` -- OpenConnect client: dials a simulator that
  accepts GSPro Open Connect (GSPro on 921, Muni Golf on 9210). `[gspro.<idx>]`
  sections are read as `[openconnect_client.<idx>]`; when both name an index,
  `openconnect_client` is used and a warning is logged. Saves write only
  `[openconnect_client.<idx>]`, and the file is rewritten that way on the next
  start (see [Loading and migration](#loading-and-migration))
- `[random_club.<idx>]` -- random club cycling integration instance
- `[camera.<idx>]` -- swing camera (beta): records a clip of every shot (see
  [Cameras and replay](#cameras-and-replay-beta))
- `[webserver.<idx>]` -- web server instance
- `[replay]` -- global camera clip settings (beta: `keep_shots`, `pre_roll_s`, `post_roll_s`, `start_before_impact_s`, `clip_dir`), omitted at its defaults
  like `[simulation]`
- `[body_tracking]` -- body tracking on the cameras' pictures (alpha:
  `mode`, `model`, `min_score`, `smoothing`, `detect_every`,
  `golfer_height`), each field
  omitted at its default and the section with them (see
  [Body tracking](#body-tracking-alpha))
- `name` is **required** -- the user-visible name, editable (rename) in settings UI
- Radar settings (ball_type, tee_height, etc.) are per-mevo only. A Mevo+ gets
  them through ironsight (`SessionConfig::to_avr_settings` → `MevoSettings` →
  `BinaryClient::configure_avr`); a mode change sends the detection mode alone
  (`AvrSettings::mode_only`). An Xi is
  configured through ironsight (`SessionConfig::to_xi_settings` →
  `BinaryClient::configure_xi`): `range` and `surface_height` as its radar
  calibration, Full → a full-swing session in the environment's mode,
  Chipping and Putting → the pitching shot type (mode 9); `ball_type`,
  `tee_height` and `track_pct` are not sent. A mode change reconfigures
  after a disarm; Chipping ↔ Putting sends nothing. ironsight acknowledges
  each Xi flight result and sets the DSP power mode at connect; both go
  through the connection's send hook, so the audit log and the shot export
  record them as tx. A refused configuration step arrives as
  `BinaryEvent::StepRefused`, which the actor logs as a warning with the hex
- `use_estimated` is per-mevo (defaults to `false`) -- controls whether estimated
  (E8) ball flights are emitted when no full (D4) result arrives; with it off,
  estimated-only shots (a flight too short to read in Outdoor mode, very short
  chips) are skipped
- `environment` is per-mevo (`limited_flight` default / `outdoor`) -- picks the
  full-swing detection mode (9 / 8), on a Mevo+ and an Xi alike
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
- `launch_only_spin` is per-mevo (rpm per mph, default `100`, `0` = off,
  valid `0..=MevoSection::MAX_LAUNCH_ONLY_SPIN` (1000); a value outside that
  in the file is treated as `0` with a warning at actor start, and
  `POST /api/settings` rejects one with `400`) -- for a D4 with launch data
  but no spin and no flight (carry, apex and flight time all absent),
  launched at 5° or more (`launch_only`), `estimate_launch_only_spin` runs
  after the Xi spin source: when the setting is above 0, the mode is not
  Putting and the spin is still 0/0, it sets backspin to `launch_only_spin` ×
  ball speed (mph), sidespin 0, and unsets carry, apex and flight time before
  the simulation runs. Otherwise the D4 keeps its zeros and is a ground ball.
  Read on every shot, so a change applies without a restart; omitted from
  the config at the default
- R10 sections show name and Monitor-to-Ball (`range`, sent to the device as
  its tee distance; BLE auto-discovery, no address field)
- ExPutt sections show name and an optional IP Address (blank finds the
  device on the network); the putting hand follows the game's handedness
- Mock sections show only name (no address or radar fields)
- Global IDs = `"{type_prefix}.{index}"` (e.g. `mevo.0`, `openconnect_client.0`)
- WebSocket actor IDs = `"ws.{8-hex-chars}"`

### Loading and migration

`state::config::load()` reads the file once at startup (`SystemState::new`).
A missing file is created with the defaults. The text is parsed as the current
format (which also reads `[gspro.<idx>]` as `[openconnect_client.<idx>]`), or
failing that as the singleton-`[webserver]` format, which loads as
`[webserver.0]` (named "Web Server" when unnamed).

After a successful load the config is serialized exactly as a save writes it
(`toml::to_string_pretty`), and that text and the file's are both parsed as
`toml::Value` and compared: tables by key regardless of order, an integer
equal to the float of the same value, NaN equal to NaN. Comments, key order
and whitespace never count. When they differ, the file is brought up to date:

1. The original bytes go to a new file next to it, `config.pre-<version>.toml`
   (`<version>` = this build), or `config.pre-<version>.<n>.toml` with the
   first free `n`; it is opened with create-new, so a backup is never
   overwritten. If it can't be written the file is not rewritten.
2. The new text is written to a temporary file in the same directory, synced,
   and renamed over the config file (a replacing rename on Windows too).
3. An `info` line names the file and the backup.

Any difference rewrites the file, including values the file spells out at
their defaults where the save leaves them out and keys the config doesn't
read: a deliberate one-time normalisation. The rewritten text is the save
format, which parses back to the same config, so the next start finds no
difference. A failed backup or rewrite logs a warning and the app
runs on the config it loaded. Every save (`save_to`) uses the same atomic
write.

A file that fails to parse, or can't be read (any error but "not found"), is
not written at startup; the defaults are used. A fallback to the older
format is tried only for a file with a singleton `[webserver]` table, so a
fault in any other file (an unknown `[body_tracking]` `mode`, say) fails the
load instead of loading with the sections the older format doesn't know
dropped. `SystemConfig` keeps the fault as a `ConfigLoadError` (the file's
path, and the line, column and message, or the read error) and
`GET /api/status` reports it as `config_error`, so the dashboard shows an
error frame over every tab: one line saying the file couldn't be read and the
defaults are in use, then the path and the error. The first save over such a
file, from a settings request or from the app itself, copies it first to the
`config.pre-<version>.toml` backup the migration uses (the file as it is on
disk then, never overwriting an older backup), logs the backup's path at
`warn`, and clears the error, so the frame goes away with the next status
read; only then is the file replaced. A settings request that can't make the
copy answers `500` and saves nothing; an internal save skips the write. The
backup's path goes to the log only: the frame is gone once the save has made
the copy, so there is no frame left to show it in.

### Default config and setup wizard

On first startup the default config contains only a webserver — no devices or
integrations. The UI detects this (`FlighthookConfig::has_user_actors()`) and
shows a setup wizard instead of the normal dashboard. The wizard presents
checkboxes for available devices (Mevo, Square Golf Omni, R10, Uneekor
(OpenConnect)) and simulators (GSPro, as an OpenConnect client). On
confirmation it builds a config with defaults for each selection, posts it via
the existing settings save path, and dismisses. Users can also skip the wizard
and configure manually on the Devices tab.

`MevoSection`, `R10Section`, and `OpenConnectClientSection` each implement
`Default` with sensible values (e.g. Mevo at `192.168.2.1:5100`, the
OpenConnect client named GSPro at `127.0.0.1:921`).

### Rust types

**lib/src/config.rs** (shared config types, used by both app and UI):

```rust
pub struct FlighthookConfig {
    pub default_units: UnitSystem,
    pub chipping_clubs: Vec<Club>,
    pub putting_clubs: Vec<Club>,
    pub simulation: SimulationSection,     // [simulation], omitted at defaults
    pub replay: ReplaySection,             // [replay], omitted at defaults
    pub body_tracking: BodyTrackingSection, // [body_tracking], omitted at defaults
    pub webserver: HashMap<String, WebserverSection>,
    pub mevo: HashMap<String, MevoSection>,
    pub r10: HashMap<String, R10Section>,
    pub square: HashMap<String, SquareSection>,
    pub exputt: HashMap<String, ExPuttSection>,
    pub mock_monitor: HashMap<String, MockMonitorSection>,
    pub openconnect_server: HashMap<String, OpenConnectServerSection>,
    pub openconnect_client: HashMap<String, OpenConnectClientSection>,  // `gspro` also read
    pub random_club: HashMap<String, RandomClubSection>,
    pub camera: HashMap<String, CameraSection>,
}

pub struct ReplaySection { pub keep_shots: u32, pub pre_roll_s: f64, pub post_roll_s: f64, pub start_before_impact_s: f64, pub clip_dir: Option<String> }  // 20, 4.0, 1.5, 2.0, None
pub struct CameraSection { pub name: String, pub camera: String, pub camera_name: Option<String>, pub position: CameraPosition, pub mode: String }  // mode "auto" | "WxH@fps"
pub enum CameraPosition { TargetLine, GolferFront, GolferBack, Overhead }  // default GolferFront
pub struct SimulationSection { pub mode: SimMode, pub altitude: Distance, pub temperature_c: f64, pub surface: SimSurface, pub auto_chip: bool }  // auto_chip default false
pub enum SimMode { Off, On, Force }                 // default On
pub enum SimSurface { Fairway, Green, FirmGreen }
pub struct WebserverSection { pub name: String, pub bind: String }
pub struct MevoSection { pub name: String, pub address: Option<String>, pub ball_type: Option<u8>, pub tee_height: Option<Distance>, pub range: Option<Distance>, pub surface_height: Option<Distance>, pub track_pct: Option<f64>, pub use_estimated: Option<bool>, pub camera_mode: Option<CameraMode>, pub environment: Option<FlightScopeEnvironment>, pub spin_source: SpinSource, pub launch_only_spin: f64 }
pub enum CameraMode { Standard, Fusion, RawFusion }
pub enum FlightScopeEnvironment { LimitedFlight, Outdoor }
pub enum SpinSource { D4, E8, Auto }
pub struct R10Section { pub name: String, pub range: Option<Distance> }
pub struct SquareSection { pub name: String, pub address: Option<String>, pub club: Option<Club>, pub advanced_spin: Option<bool>, pub discard_non_putting_zero_spin: Option<bool> }
pub struct ExPuttSection { pub name: String, pub address: Option<String> }
pub struct MockMonitorSection { pub name: String }
pub struct OpenConnectServerSection { pub name: String, pub bind: Option<String> }
pub struct OpenConnectClientSection { pub name: String, pub address: Option<String>, pub full_monitor: Option<String>, pub chipping_monitor: Option<String>, pub putting_monitor: Option<String> }
pub struct RandomClubSection { pub name: String }
```

**lib/src/api.rs** (shared REST API types, used by both app and UI):

```rust
pub struct StatusResponse { pub actors: HashMap<String, ActorStatusResponse>, pub mode: Option<ShotDetectionMode> }
pub struct ActorStatusResponse {
    pub name: String, pub status: ActorStatus, pub telemetry: HashMap<String, String>,
    pub state: ConnectionState, pub red_for_ms: u64,  // derived, see "Connection state" below
    /* private, not serialised: StatusHistory */
}
pub struct PostSettingsResponse { pub restarted: Vec<String>, pub stopped: Vec<String> }
pub struct ModeRequest { pub mode: ShotDetectionMode }
```

**app/src/actors/mod.rs** (actor resolution from config):

```rust
pub struct ResolvedActor {
    pub id: String,             // global ID (e.g. "mevo.0", "r10.0", "openconnect_client.0")
    pub name: String,           // user-visible name
    pub actor: Box<dyn Actor>,  // concrete actor ready to start
}

pub fn resolve_actors(config: &FlighthookConfig, current_mode: Option<ShotDetectionMode>) -> Vec<ResolvedActor>;
pub fn start_actor(id: String, actor: Box<dyn Actor>, state: &Arc<SystemState>, bus_tx: &broadcast::Sender<FlighthookMessage>);
pub fn actor_names(config: &FlighthookConfig) -> HashMap<String, String>;
```

`resolve_actors` lists the web servers first, and `main` starts actors in
that order. A web server subscribes to the bus in `start`, before it returns,
so the first status of every actor started after it reaches `/api/status`.

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
    SetDetectionMode { mode: Option<ShotDetectionMode>, handed: Option<Handedness>, club: Option<Club> },

    // -- Flighthook extensions --
    PlayerInfo { player_info: PlayerInfo },
    ClubInfo { club_info: ClubInfo },
    HandInfo { hand_info: HandInfo },
    ConfigCommand { request_id: Option<String>, action: ConfigAction },
    ConfigOutcome { request_id: Option<String>, restarted: Vec<String>, stopped: Vec<String>, started: Vec<String> },
    ActorStatus { status: ActorStatus, telemetry: HashMap<String, String> },
    NewShotGroup,                                                     // command: new set for the selected club
    NewShotSession,                                                   // command: close the open set, next session
    ClearShotHistory,                                                 // command: delete shots and sets, next session
    FindMat { target: String },                                       // command: the ExPutt `target` locates its mat again
    ShotGroup { group: ShotGroupInfo, shot: Option<GroupedShot> },    // a set record changed (web layer)
    ShotVideo { keys: Vec<ShotKey>, position: CameraPosition, duration_ms: u64, fps: f64,
                trigger_offset_ms: i64, impact_offset_ms: i64, complete: bool, dropped: u32 },  // a camera clip (beta)
    ShotVideoRemoved { keys: Vec<ShotKey> },                          // these shots' clips are gone (beta)
    ShotPose { keys: Vec<ShotKey> },                                  // a camera clip's keypoints are written (alpha)
    ShotSwing { keys: Vec<ShotKey> },                                 // a camera clip's swing analysis is written (alpha)
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
`DerivedFlight::spin_estimated` marks the spin in `ball` as flighthook's
estimate (the mevo actor's launch-only chips); the actor sets it after the
simulation, on a `derived` that holds only the flag when the shot was not
simulated, so `derive` never sees it.

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
| FlightScope | ironsight `set_on_recv` / `set_on_send` (the same hooks log hex and keep the last D4) | BALL TRIGGER to the device re-armed, both directions | what ironsight classifies as identity-bearing (`protocol::carries_identity`): status/keepalive, device and product info, DSP query, network config, WiFi scan, calibration, sensor activation, time sync, AVR config query | 8 | 256 | 128 KiB (PRC, club PRC, speed profile) |
| Square Golf | `Tap<T>` around the allsquare transport; characteristic reads are never recorded | pre-roll through the shot event; enabled after the identity exchange | heartbeats and acks, query reply | 64 | 64 | 0 |
| R10 | `Tap<T>` around the tenover transport; a per-direction tenover `capture::IdentityFilter` holds each chunk until the GFDI frames it carries are complete and classified | pre-roll through the shot event; enabled at `HandshakeComplete` | what tenover classifies as identity-bearing: every chunk carrying part of a device information, FIT/file transfer or authentication frame, an ACK of one, or a frame that does not decode | 256 | 256 | 0 |
| ExPutt | none: the putt's 16 values, comma-separated, as one text record (`transport = "exputt-wifi"`) | the putt | none | - | 1 | 0 |
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
`ShotContext`), `handed` (the trigger-time handedness from the same
context) and `group` (below). FRP events are untouched; these fields
exist only on `/api/shots`, the export and `/api/shots/convert`, and are
omitted when unset, so their JSON is unchanged without them. `video`, the
shot's camera clips (beta), is stamped from `shot_video` whenever it arrives
(see [Cameras and replay](#cameras-and-replay-beta)) and is on `/api/shots`
only: the export and `/api/shots/convert` drop it.

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
| `hand_info` | none (the source's card gets `handed`; the shot takes the trigger-time hand from game state) |
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
- `HandInfo { hand_info }` -- player handedness
- `SetDetectionMode { mode?, handed?, club? }` -- the FRP controller command the
  devices follow: detection mode, handedness and/or club (all optional,
  latched; `club` is a hint and never implies `mode`). Game state takes only
  its `mode`.

The club and the hand are set the same way by every source: GSPro's player
message, the UI's club picker and hand control, `POST /api/club` /
`POST /api/hand`, and the WS `club` / `hand` commands all emit `ClubInfo` /
`HandInfo`. The last one wins. GSPro is a club and hand source only
(flighthook never sends it either) and re-sends both with every player
message, so while it is connected its selection wins.

The `SystemActor` projects both onto the controller command. From `ClubInfo`
it derives the detection mode (using `config.club_mode()` to map club to
detection mode via the configurable `chipping_clubs`/`putting_clubs` lists)
and emits `SetDetectionMode { mode, club }`, carrying the selected club next to
the mode it implies. From `HandInfo` it emits
`SetDetectionMode { handed }` when the hand differs from the stored one (or
none was stored), so a source that repeats the hand does not re-command the
devices. Sources only emit `ClubInfo` / `HandInfo`; no source emits
`SetDetectionMode` with a hand. Launch monitor actors react to
`SetDetectionMode` events to reconfigure the device — they latch the mode and
only re-arm when the mode actually changes. Handed-only updates (mode
omitted) do not re-arm a launch monitor. The ExPutt starts a new round, which
re-locates the mat; the Square Golf actor re-sends the active club with the
new hand. Both also take the hand from game state when their session starts.
Mode and hand are global state, not per-device.

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
  OpenConnect clients add: `address`, `last_shot`, `shots_sent`,
  `monitor_ready`, `response` (see
  [OpenConnect client telemetry](#openconnect-client-telemetry)).

```rust
pub enum ActorStatus { Starting, Disconnected, Connected, Reconnecting }
```

`ActorStatusResponse` (in `lib/src/api.rs`) adds a `name` field and is used
as the cached per-actor state in both the web layer and the UI. The web layer
merges telemetry from both `DeviceTelemetry` and `ActorStatus` events into
a single per-actor telemetry map for API consumers. A key in an
`ActorStatus` replaces the one held; an empty value removes it (how the
OpenConnect client and the web server clear `error`). The UI merges the same
way. A web server sets `error` to `bind failed` while it cannot bind its
address, and clears it with every other status.

A card never shows a lost connection's values. When an actor's status goes
from `Connected` to `Disconnected` or `Reconnecting`, and on every `Starting`
(a new instance), the web layer and the UI drop all the telemetry held for
the actor (device-reported keys, `club`, `handed`, `name`, status keys) and
then merge the keys that status carries, so `error` and the keys an actor
sends with a status stay. Later statuses that do not end a connection merge
as above. Both sides call `ActorStatusResponse::apply_status`. The web layer
also drops the actor's cached `DeviceTelemetry`, so a WebSocket client that
connects later is not replayed the lost device's values; the export's device
identity stays. An actor that reports a static key once per instance sends it
again with the statuses that clear (the OpenConnect client's `address` and
`last_shot`, see below); the device actors send their keys again with each
connection. The Devices card then shows its empty state until the actor
reports: the spinner while not Connected, the connected mark when Connected
with no rows.

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
    UpsertExPutt { index: String, section: ExPuttSection },          // "upsert_exputt"
    UpsertOpenConnectServer { index: String, section: OpenConnectServerSection },
    UpsertOpenConnectClient { index: String, section: OpenConnectClientSection },  // `upsert_gs_pro` also read
    UpsertWebserver { index: String, section: WebserverSection },
    UpsertMockMonitor { index: String, section: MockMonitorSection },
    UpsertRandomClub { index: String, section: RandomClubSection },
    UpsertCamera { index: String, section: CameraSection },
    Remove { id: String },                          // "mevo.0", "openconnect_client.1", "webserver.0", etc.; "gspro.N" = "openconnect_client.N"
    SetShotGrouping { mode: ShotGrouping },         // POST /api/shots/grouping, WS `grouping`
    SetBodyTracking { section: BodyTrackingSection },  // POST /api/settings?scope=body_tracking
    Reconnect { id: String },                       // POST /api/actors/{id}/reconnect
}

// ConfigOutcome fields (on FlighthookEvent)
request_id: Option<String>,
restarted: Vec<String>,
stopped: Vec<String>,
started: Vec<String>,
```

`ConfigAction` is `#[non_exhaustive]`: a consumer that matches on it keeps a
wildcard arm.

The POST handlers (`/api/settings`, `/api/shots/grouping`) use request-reply:
emit `ConfigCommand` with a `request_id`, subscribe to the bus, and wait for
`ConfigOutcome` with the matching ID. `SetShotGrouping` persists the default
view and reconciles no actor; no set changes. `SetBodyTracking` persists the
`[body_tracking]` section alone and reconciles no actor (it is read live). `/api/settings` answers `400`
for an unknown scope and `504` when no `ConfigOutcome` arrives within
`CONFIG_TIMEOUT` (`web/routes.rs`): the longest stop wait of any actor
(`actors::MAX_STOP_WAIT`, the ExPutt's 18s) plus 4s, so 22s. A command stops
the actors it restarts or removes together, so it waits at most one longest
stop wait, and it never waits behind another command's (see below). The UI's
save and reconnect requests wait 60s (`net::CONFIG_REQUEST_TIMEOUT`, asserted
longer than `CONFIG_TIMEOUT` at compile time), so they get the server's answer.
`ConfigOutcome` is always emitted after processing, even for fire-and-forget
commands (where `request_id` is `None`). `POST /api/actors/{id}/reconnect`
emits `Reconnect` the same way (see [Reconnect](#reconnect)); its outcome
lists the actor in `restarted`, or nothing when it was not started.

SystemActor never waits for a stopping actor on its own thread. A command
that stops actors (`system::StopWaits`) marks every actor it touches as
settling (`SystemState::begin_settling`), and a `stop-wait:<ids>` helper thread
waits for the stopped ones, each up to its stop wait, the waits overlapping.
When they are over the helper reports back over a channel, and SystemActor,
between bus messages, finishes the command (`system::finish`): it releases
each stopped actor, starts what is configured now and emits the command's
`ConfigOutcome`. A command that stops nothing finishes at once. Meanwhile
SystemActor goes on handling `ClubInfo`, `HandInfo`, `SetDetectionMode` and
further commands. A command that touches a settling actor follows the rule
for an actor whose old instance is still closing: a removal lists it in
`stopped` at once and mutes the old instance; a save or re-add starts
nothing, and the waiting command starts it from the settings current when it
finishes; a reconnect starts nothing more (its request answers `202`). So
there is never more than one instance of an ID, and a new instance always
starts after its old one has ended or been retired.

## GameState

`GameState` in `state/game.rs` -- read-only handle for the current round's
`PlayerInfo` (player name), `ClubInfo` (club selection),
`ShotDetectionMode` (global detection mode) and `Handedness` (player hand).
Lives inside `SystemState` as the `game` field. Only exposes `snapshot()`. All actors and the web layer
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
    handed: RwLock<Option<Handedness>>,
}

pub struct GameState { inner: Arc<GameStateInner> }        // read-only
pub struct GameStateWriter { inner: Arc<GameStateInner> }  // write handle (SystemActor only)

pub struct GameStateSnapshot {
    pub player_info: Option<PlayerInfo>,
    pub club_info: Option<ClubInfo>,
    pub mode: Option<ShotDetectionMode>,
    pub handed: Option<Handedness>,
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

### Connection state

What the dashboard shows is not the raw `ActorStatus` but a `ConnectionState`
(`lib/src/connection.rs`), derived from it and the actor's history since it
last started. Every actor is treated alike (launch monitors, outputs, the
OpenConnect server, the web server):

| ConnectionState | When | Colour |
| --------------- | ---- | ------ |
| Connecting      | Not `Connected` since it started, or since a config change restarted it | yellow |
| Connected       | `ActorStatus` is `Connected` | green |
| Reconnecting    | `Connected` since it started, and has since lost the connection (any other status) | red for 10 minutes (`RECONNECT_ALERT_MS`) after the most recent `Connected` -> other change, yellow after |

`StatusHistory` holds the latest status, whether the actor has been
`Connected` since it started, and when the alert for its last loss ends.
Another loss while already Reconnecting keeps the window; a new `Connected`
then a loss starts it again. A `Starting` status begins a new history, and so
does a restart (`ConfigOutcome.restarted`: only whether it is `Connected` now
is kept). Removing an actor drops its entry, so one added again starts fresh.
An `error` telemetry value does not affect the state or its colour. Times are
milliseconds on a monotonic clock each side picks: the web layer's
`WebState::now_ms` (from `WebState::epoch`), the UI's frame input time.

The web layer records every `ActorStatus` into its per-actor
`ActorStatusResponse` (`observe`), for as long as it runs, so the history
does not depend on any UI being connected. `GET /api/status` stamps each
actor's `state` and `red_for_ms` (the alert time left) at reply time. The UI
rebuilds the history from a reply (`received`, the red time counting down from
receipt on its own clock), then applies live `actor_status` events and
`ConfigOutcome.restarted` to it the same way the server does.

All actors use linear backoff on connection failure: starts at 1s, adds 1s per
failed attempt, capped at 15s (3s for the OpenConnect client, a loopback/LAN
TCP connect). The OpenConnect client, Square Golf and ExPutt actors restart at 1s after a connection
that lasted at least 10s; one that drops sooner counts as a failed attempt, so
a link that keeps connecting and dropping still backs off to the cap. The
OpenConnect server restarts at 1s after a successful bind. The Mevo and R10
actors keep the grown delay. Every actor polls its shutdown flag during the
backoff wait (the OpenConnect server during its bind retry delay too), so
stopping one never waits out the delay. The Mevo and OpenConnect client TCP
connects (up to 5s) run on a thread of their own: a stop ends the actor at
once, and a connect that completes afterwards closes again. The ExPutt's
session start cannot be cut short: the TCP connect and the device's greeting
(up to 3s each), its two handshake replies (up to 3s each) and its mat search
(up to 5s).
The ExPutt caps its first 3 reconnect delays at 1s after a session that was
measuring, then uses the shared backoff. After a session lost to silence it
waits 15s before the first attempt and leaves at least 15s between attempts
until a session succeeds again; it makes one attempt at a time.

### Reconnect

The Reconnect button on a Launch monitors or Outputs card
(`POST /api/actors/{id}/reconnect`) stops that one actor, waits for it to let
go of its device or connection, and starts a new instance from the saved
settings: what a flighthook restart does for that device. It goes through
SystemActor as `ConfigAction::Reconnect`, so it is sequenced with saves and
removals; the config is not written. Cameras, the web server and the system
actor are refused (`400`), an ID no launch monitor or output has is `404`,
and a second request for the same actor while one is in flight is `409`.

`system::reconnect()` checks the ID; while an earlier instance of it is still
closing, or another command is restarting it, nothing more is done and the
request answers `202` (it starts again by itself). Otherwise it stops the
actor and the command waits for it on a helper thread (see
[ConfigCommand / ConfigOutcome](#configcommand--configoutcome----event-sourced-config-mutations));
`system::finish` then restarts it the way config changes do too:

1. Stop: set the shutdown flag and call `stop()` (`SystemState::stop_actor`),
   then wait up to `Actor::stop_wait()` for the actor's thread to end
   (`SystemState::wait_for_exit`): 3s by default; 5s for a Square Golf,
   which disarms and disconnects; 18s for an ExPutt, whose session start
   cannot be cut short; none for the web server (the save that moves it is
   answered by the server being stopped, and its successor retries the bind).
2. If the thread is still running, the actor's `replaceable_while_stopping()`
   decides: a Mevo/Xi, an OpenConnect client, the mock actors and the web
   server hold nothing a new instance needs (a TCP client, nothing, or a port
   the new one retries), and an OpenConnect server is replaceable once its
   listen port binds again. A replaceable one is retired: taken out of the
   registry, its bus sender muted so nothing it still sends speaks for its
   successor, its thread left to end. An R10, Square Golf, ExPutt or camera
   is not: it is retired the same way and watched
   (`SystemState::close_in_background`: a `closing:<id>` thread joins it),
   nothing is started, and the card's `error` telemetry and an Error alert
   say "The previous connection has not closed yet. It reconnects when it
   does."; a reconnect request answers `202`. Once the old thread has ended,
   SystemActor, between bus messages, starts the actor again from the saved
   settings by itself (`system::restart_closed`) and emits a `ConfigOutcome`
   without `request_id` listing it in `restarted`. Until then a reconnect, a
   save or a re-add of that ID starts nothing.
3. Emit `ActorStatus { Starting }` for the card, clearing an earlier `error`,
   and start the new instance (`start_actor`). The outcome lists it in
   `restarted`, so the card begins a new history; it then shows the actor's
   own statuses as it connects.

A removal stops and releases the actor the same way, without starting
anything; one still closing is retired and muted at once, and its card goes
with the outcome.

`POST /api/actors/{id}/reconnect` holds the actor's ID in the web state's
in-flight set until the outcome arrives. When the outcome has not arrived
within `CONFIG_TIMEOUT` the request answers `202` and a task keeps the ID in
the set until it does (for up to another `CONFIG_TIMEOUT`), so a second press
cannot queue a second restart. An outcome that lists nothing restarted while
the actor is closing or settling also answers `202`: it starts again by
itself. Any other empty outcome is `409` with the reason.

At exit, `main` stops every actor at once and waits for them
(`SystemState::stop_all_and_wait`), each up to its stop wait but 3s in all,
so a Square Golf disconnects the Omni before the process ends.

What a stop does, by actor:

| Actor | Stops at once when | Bounded by |
|---|---|---|
| Mevo/Xi | always: the client is non-blocking and polls every 50 ms; the connect runs on its own thread | -- (an old instance still inside the driver is replaced beside it: a second socket to the device can overlap the old one until it closes) |
| OpenConnect client | always: 50 ms reads, 2s write timeout, the connect on its own thread; its local-client registration ends with the connection | a write in progress (2s) |
| OpenConnect server | always: 100 ms accepts and reads, 2s write timeout, the bind retry wait polls the flag; the listen port is free once it ends | a write in progress (2s) |
| Square Golf | in a session or a backoff wait: it disarms the device, then disconnects it, with BlueZ and with btleplug | the scan and connect inside allsquare (up to 20s scan + 20s service resolution on Linux) |
| Garmin R10 | in a session or a backoff wait | the scan and connect inside tenover |
| ExPutt | measuring (100 ms event wait, the session closes at once) or waiting to reconnect | a session start (up to 17s: connect and greeting, two handshake replies, mat search) |
| Mock monitor, Random club | always (100 ms loop) | -- |

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

**ExPutt actor**: runs a `threeputt::Session` on its own thread. With no
address it waits for the device to announce itself on the network
(`actor_status` `starting`, `phase: discovering`), then connects (`phase:
connecting`) and starts measuring with the putting hand from the game state
(`connected`, `phase: measuring`). The device card's Find mat button
(`POST /api/find-mat`, a `FindMat` bus command) has the device locate the mat
again without reconnecting: `ready` is false until the device is ready again.
A handedness change (from any source, via the system actor's
`SetDetectionMode { handed }`) starts a new round with the new hand,
which locates the mat the same way. Mat searches are at least 10 s apart (the
one at session start counts): requests inside that time are folded into one
search with the latest hand, run when it ends, and a hand changed back to the
one in effect needs none; `ready` is false while one waits.
Emits `DeviceTelemetry` with `model: "ExPutt RG"` (and the device software
version as `firmware` when it was discovered). Telemetry keys: `ready` (true when the device is ready for a
putt, false from a strike until it is ready again, and false when the
connection is lost, before `reconnecting`), `device_state` (`finding mat` |
`mat found` | `ready` | `ball struck`; a putt result changes neither key).
After 30 s of discovery without finding the device, one `alert` (`warn`)
suggests entering its IP address on the device card.
FRP device ID `"ExPutt RG (<ip>)"`. Each putt is a shot: ball speed, launch
direction as `launch_azimuth`, `launch_elevation` 0, no spin; `ClubPath` with
path and face angle when the putter was tracked, none otherwise; simulated in
Putting mode (rolled on the green). The web layer files it with club `PT` and
detection mode Putting whatever club is in play. The session is closed on
stop; a closed connection, or no event for 90 s (120 s after a strike, until
the putt's result or the device is ready again), reconnects with backoff. A
quiet spell past 10 s logs one warning and makes `ready` false until the
device sends again (then it is what the device last reported). When
connection attempts get no answer from a device that accepts them, and
the first such attempt of an outage is 2 minutes old, one `alert` (`warn`)
says the ExPutt is not answering and suggests turning it off and on again;
it is sent once until a session succeeds, and the per-attempt error alerts
stop meanwhile (attempts that fail another way do not restart the clock).
`ShotFinished.raw` carries the putt's 16 values as one text record (transport
`exputt-wifi`).

An OpenConnect client logs a warning at start when an ExPutt is configured
and its Putting Monitor is unset (Any).

**OpenConnect client actor** (GSPro, Muni Golf, ...): emits `Starting` before
first TCP connect attempt, `Connected` on success. Connection loss after a
successful session emits `Reconnecting`; failure before ever connecting stays
at `Starting`. A refused, failed or closed connection is shown only through
this status and the log, with no alert: a simulator not running is normal when
another one is in use.

**OpenConnect self-loop guard**: an OpenConnect client pointed at this
process's own OpenConnect server (e.g. client `127.0.0.1:921`, server bind
`0.0.0.0:921`, GSPro closed) would send every shot client -> server -> bus ->
client, forever. `SystemState.local_clients` (`state/local_clients.rs`) holds
the local socket address of every connected OpenConnect client, registered
right after `connect` and before anything is sent, and removed when the
connection ends. The server looks up each peer it accepts, comparing IP and
port with IPv4-mapped IPv6 read as IPv4 and every loopback address as
`127.0.0.1`. It looks the peer up again after each read, before handling it,
so the connection's data never reaches the bus even if the client registered
a moment after the accept. The server reports `Connected` only once the first
read (up to 100 ms) has passed that look-up too. A match is closed at once,
unread, and refused:

- the server raises one `Error` alert per burst of that client's reconnect
  attempts (refusals less than 30 s apart are one burst), shown in the Log
  after the server's name: "Refused OpenConnect Client "<client name>" from
  <peer>: it is this flighthook's own client, which would loop shots back.
  Point it at the simulator's port.";
- the client sets its telemetry `error` key, shown after the client's name in
  the banner: "Refused by this flighthook's own OpenConnect Server "<server
  name>": this address points at it, which would loop shots back. Use the
  simulator's port instead." It keeps retrying with its usual backoff.

The names are the ones the cards show (the actor ID when blank). A client
reports `Connected` only once a connection has lasted 1 s, so a refused
connection never shows as connected. The `error` key is cleared (an empty
value) when a connection outlasts that, when an attempt fails any other way,
and when the actor starts.

A connection from anything else, on the same host or not, is served as usual:
a monitor's connector on 921 feeding a server whose client sends on to GSPro
on 922 is unaffected. A stopped or unreachable simulator is unaffected too:
still no alert and no dot.

**OpenConnect client shot delivery**: the bridge drains the bus the whole time
it runs, connected or not, so nothing queues up while the simulator is away.
Every shot that finishes while the simulator is connected is sent, in order,
including several that finish together. A shot that finishes while it is not
connected (stopped,
starting up, or between reconnects) is dropped. A shot is also dropped if it
is more than 5s old when the bridge can send it, its age counted from when the
bridge took the finished shot off the bus. A connected bridge sends a shot in
the same loop pass that takes it off the bus, so in practice this only drops
shots left queued behind a failed write when the reconnect takes longer than
5s. Dropped shots are logged at debug level only, with no alert: a simulator
not running is normal when another one is in use. A shot whose write fails
is not retried. A write to the simulator that blocks for more than 2s fails like any
other, ending the connection and starting a reconnect, with no alert. The JSON
sent for a shot is the same whatever happened to the connection before it.

**Integration readiness**: the OpenConnect client actor tracks device readiness from
`DeviceTelemetry` (`"ready"` key) and uses it to set both
`launch_monitor_is_ready` and `launch_monitor_ball_detected` in heartbeats
and shot messages.

#### OpenConnect client telemetry

The OpenConnect client (GSPro, Muni Golf or a generic simulator; one actor)
reports what it knows as `ActorStatus` telemetry (`gspro::Telemetry`), each
key only when its value changes:

| Key | Value | Example | Holds for |
|---|---|---|---|
| `address` | the simulator's address it dials | `127.0.0.1:921` | the actor |
| `last_shot` | the `ShotNumber` the simulator got last and the local time it was written | `#12 at 16:41:07` | the actor |
| `shots_sent` | shots written on this connection (`0` from the connect) | `3` | the connection |
| `monitor_ready` | the readiness last sent in a heartbeat (`true`/`false`) | `true` | the connection |
| `response` | the simulator's last response, code and message | `201 GSPro Player Information` | the connection |

`club` and `handed` on the card are the simulator's player information: the
client puts `ClubInfo`/`HandInfo` on the bus and the web layer (and the UI)
show them on the client's card. Until the 1 s refusal verdict the keys wait
and go out with `Connected`; after it, with a `Connected` status as they
change. When the connection ends, the status that says so removes the
connection's keys and `club` and `handed` (empty values). The first status of
a new instance sets `address` and removes every other key an earlier
instance left. Because a `Starting` or a lost connection drops the card's
telemetry, every `Starting` and `Reconnecting` status carries `address`,
`last_shot` and a held refusal `error` again, so a reconnecting card still
shows where it dials. Nothing here adds traffic to the simulator.

## Cameras and replay (beta)

USB cameras record a clip of every swing; the Shots tab replays it beside
the shot. Capture, the pre-roll and clip cutting are the
[stance](https://github.com/divotmaker/stance) crate's; flighthook
supplies the triggers, stores the clips and serves them. Frames never travel
on the bus: only one `shot_video` event per finished clip.

### Camera actor

One `CameraActor` (`app/src/actors/camera.rs`) per `[camera.N]`, on its own
thread. It opens its source with stance's `Capture::start` (which
blocks while the camera opens; the camera then runs on a capture thread of
its own, frames going into a bounded ring) and loops:

- every waiting bus message is read first; each `shot_trigger` becomes a
  recorder trigger (below);
- frames newer than its cursor (`Capture::wait_frames_after`, waiting at
  most 10 ms) are restamped (below) and go to a `ClipRecorder`, which keeps
  the pre-roll;
- `ClipRecorder::poll` runs every pass, so clips finish when frames stop;
- every 500 ms the newest frame goes to the actor's slot in
  `SystemState::cameras`, for the still;
- while the slot is watched (below), the newest frame of the pass goes to
  its live slot, at most `LIVE_FPS` (15) a second, or with body tracking
  in Live mode through the camera's body tracking worker, which puts it
  there with its pose (see [Body tracking](#body-tracking-alpha));
- every 2 s, telemetry (`fps`, `dropped`, `position`) goes out in an
  `actor_status`, only when it changed; `fps` moves only when the measured
  rate is 2 % of the mode's rate (at least 1 fps) away from the value last
  sent, so a camera hovering around its rate stays quiet; one `warn` alert
  when the frame rate stays under half the mode's for 10 s.

The source is the test picture for `camera = "test-pattern"` (stance's
`TestPattern`, set up by `TestPattern::config_for(mode)`: `auto` picks from
its listed modes like a camera's, `WxH@fps` sets it), else
`stance::open(camera, mode)`. When the id is not connected and the
live section has a `camera_name`, the camera with that name is opened
instead, with a warning in the log.

Status follows the other device actors: `starting` while opening (and while
the first attempts fail), `connected` with `camera`, `position`,
`resolution`, `mode_fps` (and `error` cleared) once open, `reconnecting`
with `error` after a loss, retried with the shared linear backoff (1 s up to
15 s, reset after a session of 10 s). A lost camera's open clips are
finished at once (`ClipRecorder::flush`): one that holds its impact frame is
saved with `complete: false`, one without is dropped (logged). Where the
platform has no camera backend (macOS) the actor reports `disconnected` with
`error: "cameras are not supported on macOS yet"` and waits to be stopped.
`reconfigure` restarts the actor when `camera` or `mode` changed; the name,
`camera_name` (used at the next open) and position are read live.

### Live frames

The Shots tab's Live view shows each camera live through the actor's **live slot** in
`SystemState::cameras` (`state/cameras.rs`): one frame, the JPEG as the
camera sent it, with a sequence number and its time. The number counts up
over every camera's live frames and never repeats while the server runs;
the time is the frame's host time less the camera's latency
(`stance::clock` nanoseconds, the clock of the clips' sidecars).

- **Watched.** Each `GET /api/cameras/{actor}/live` marks the camera
  watched (`Cameras::watch_live`). The actor fills the live slot only while
  a request came within the last 3 s (`LIVE_WATCH`), so an unwatched camera
  costs nothing beyond the still. A request after a lapse empties the slot
  first, so a new viewer never gets a frame from an earlier viewing; a
  camera that stops (lost or reopened) empties it too.
- **Rate.** At most 15 frames a second (`LIVE_FPS`) go to the slot, on a
  schedule of one per 1/15 s (`camera::LiveGate`): a faster camera's frames
  in between are left out, a slower camera's all go, and a schedule more
  than an interval behind restarts from the frame at hand. Only the newest
  frame of a capture pass is a candidate.
- **Long poll.** The slot is a `tokio::sync::watch` channel. The route
  answers at once when the slot holds a frame numbered above `after`, else
  waits for one up to 1 s (`LIVE_WAIT`) and answers `204`. At most 8
  requests (`LIVE_WAITERS`) wait on one camera; one more gets `503` at
  once. Every request is answered within the wait, so nothing waits
  unbounded, and no frame goes on the bus.

The still slot and its route are separate and unchanged by watching.

The **Cameras pane** (`ui/src/panels/cameras.rs`) is part of the Shots
tab, shown when the server can record from cameras (`cameras_supported`,
as for the Devices tab's Cameras (+)) and the saved config has a camera
(`FlighthookApp::cameras_shown`). The toolbar gets a **Data | Cameras**
switch as its first control in both views (`ShotsPane`, kept in local UI
storage under `shots_pane`): Data | Cameras is the parent, Cards | Table
the way Data is shown. Right of it the Cards | Table switch shows while
Data is showing; while the Cameras pane shows (`cameras_pane_shown`) an
on/off **Live** switch takes its slot (`render_live_toggle`,
`theme::switch_pill`, `live_selected`) and the Cards | Table choice is kept
for Data. One helper says what the tab lays out,
`FlighthookApp::shots_layout` -> `ShotsLayout` `Cards | Table | Cameras`
(Cameras whenever the pane is chosen, whatever the view); the panel, the
toolbar's legend and the card clips all go by it. With Cameras picked the
pane takes everything right of the Cards view's shot list (the list keeps
the Cards view's panel and default width, so the screen is the same from
either view): the Dispersion column and the bottom chart row are not laid
out, and the card row's camera clips are dropped. Live is part of the
Cameras pane: picking Data leaves Live, picking a shot leaves Live, turning
Live off shows the shot the list shows, and a new shot keeps Live (the
newest shot is selected underneath, as in Data).

Each time the pane comes on screen (`sync_cameras_pane`, comparing with
`cameras_pane_was_shown`; called at the start of each update, in the
toolbar after the Data | Cameras switch and after the toolbar, so a click
that shows the pane is applied before it draws), the shot focus follows the
latest shot again (any pin dropped) and Live is turned on exactly when
there is then no shot to show under the device filter. Clear history marks
the pane as not shown, so a pane on screen turns Live on.

- **A shot** (`render_shot_cameras`): the shot's header (number, LATEST /
  Back to latest, Replay), then the inline player (`FlighthookApp::
  shot_player`, a `replay::ReplayView` made `with_swing`, kept while the
  shown shot and its cameras stay the same): the clips side by side, each
  picture at most 62 % of the pane's height, the replay's scrubber and
  transport across them, looping from the start point with the skeleton.
  Besides the green start tick and the gold impact tick, the scrubber has a
  small grey tick for each swing event the reports found other than impact
  (`swing_markers` in `panels/replay.rs`: address, takeaway, top, finish),
  at the mean over the cameras that found it of the camera's clip start plus
  the event's `t_ns` (the impact tick's time base); an event no report found
  has none, and none shows with body tracking Off or while no report has a
  swing. Markers under 6 px apart share one hover (the names, in swing order)
  and one click (which seeks to the first); hover shows the event name, a
  click seeks there (marks `<prefix>:event:<name>`, lower case, `+` between
  merged names). The replay modal fetches no reports and so shows none.
  It holds its clock and fetches nothing while the replay modal is open.
  Under the columns, the swing analysis, fetched once per clip when
  `video[].swing` is set or `shot_swing` arrives: one row of tempo tiles
  across all the columns (the tempo of the cameras that measured one,
  averaged: the mean backswing and downswing, and the ratio of those means;
  one camera shows its own; mark `swing_tempo`), right under it one row of
  six tiles of the same style and width with the body at the playhead
  (`frame_tiles` / `frame_row`, mark `frame_metrics`, `frame_metric:<id>` per
  tile): Hip turn, Shoulder turn, Hip sway (`hip_sway`), Head sway
  (`head_lateral`) from face-on reports, Forward bend
  (`spine_forward_bend`) and Hip depth (`early_extension`) from
  down-the-line reports; with no down-the-line report the spine tile is
  face-on `lateral_spine_tilt` ("Side bend"). The labels (`metric_label`,
  the tiles, the live posture tiles) name what is measured.
  Each report is sampled from its `series` at the frame its picture shows:
  `render_shot_player` takes the playhead the pictures are drawn at and
  returns, per column, `Track::shown_t_ns` (the sidecar `t_ns` of
  `Track::frame_at(playhead)`, `None` before the clip's first frame or after
  its last); `SwingReportFile::sample(id, t_ns)` takes the nearest series
  frame (none outside the series) and the centimetres from `scale`. Several
  reports of one view average; a tile with no value is a dash. Formatting is
  the metrics table's (`metric_shown`). Then under each column that
  camera's metrics at address, top and impact
  (degrees whole; `torso` metrics in inches or centimetres by the Units
  switch when the report has `cm`, else as a share of the torso). The
  shifts from address show their magnitude and a direction word
  (`direction_word`, none when the value shows as 0): `hip_sway` and
  `head_lateral` (signed towards the target) from `signs::target`
  (`Toward` / `Away`), `early_extension` (signed towards the ball) from
  `signs::ball` (`Toward ball` / `Back`) and `head_vertical` (signed up)
  from `signs::rise` (`Up` / `Down`), painted after the
  value on its baseline at the Cards view's sign-word size, shrinking to the
  cards' floor before the numeral does (`worded_value`, shared by the tiles
  and the table). A report
  whose `impact_source` is `given` (no swing found near the impact) shows
  only the line "No swing found in this clip" there, in the clip status
  lines' style. The table has Hip turn and Shoulder turn and leaves out
  `hip_width_proxy` and `loss_of_posture` (the Forward bend row's change
  from address), which stay in the file. The UI reads the file through
  lenient mirror types (`SwingReportFile`, with `scale` and `series`; a
  series of the wrong length is dropped, a file without one reads with an
  empty one), which also read the earlier layout with the view in a `setup`
  object and the `hip_width_ratio` id, and leave out the `sway` and `slide`
  ids (metrics and series). The pane
  scrolls (a solid bar) when the numbers don't fit. A shot without clips
  shows each saved camera's status in its picture's place.
- **Live** (`render_live`): the saved cameras side by side, each fitted to
  the pane at its aspect under its name and position, with room left for
  the posture tiles and the readout line. Each camera has a `LiveView` with
  its own fetch loop: the next request goes out on the screen update after
  the last one is answered, asking `after` the newest frame it holds; a
  frame is decoded (`ensure_huffman` + zune-jpeg) into the view's texture on
  the next update, and only a frame asks for a repaint. A failed request
  waits 1 s. The views exist only while Live is on screen (the Shots tab,
  the Cameras pane in either view, Live on, the window not minimised, not a
  hidden browser tab); otherwise they are dropped and their late replies
  ignored.
  A camera that is not Connected shows its Devices card's badge and `error`
  instead of a picture. Each picture carries the skeleton of the pose that
  came with it, and under it the posture tiles of the camera's view from the
  same `X-Pose` (golfer_front: Side bend, Shoulder tilt, Lead elbow;
  target_line: Forward bend, Lead knee flex, Trail knee flex, Trail elbow;
  none for
  golfer_back and overhead). A readout line under the row names the body
  tracking values and each camera's live rate and pose time (see
  [Body tracking](#body-tracking-alpha)).

Keys: while the pane is on screen and neither a text field nor a dialog has
the keyboard, it takes the presses bound in `cameras::KEY_BINDINGS` before
any widget sees them, each in its scope: the tuning keys on Live only, `B`
on Live and on a shot's clips. The zoom shortcuts are applied before it.

### Frame restamping (latency)

`[camera.N] latency_s` (`Option<f64>`, 0 to 10 s, serde-omitted when unset =
0; out of range is refused with `400` on save and ignored with a warning at
load) is how long a frame is old when it reaches flighthook, for a camera
that delivers late (a buffered network stream). Every frame's host
timestamp, which the platform sets to its arrival, becomes
`arrival − latency` (`camera::Restamp`; saturating at 0, the device time
moves with it) before it reaches the `ClipRecorder`; the still slot carries
no time and takes the frame as it is. Triggers are stamped when read, so
frames, triggers and impact estimates share real time: the clip window, the
sidecar times and the replay's sync between cameras need nothing else. The
latency is read from the live config on every capture pass (`reconfigure` is
`Applied` for a latency-only change; the camera is not reopened).

The recorder takes frames in strictly increasing time and ignores one that
is not newer than the newest it holds, so `Restamp` passes on only frames
dated after the previous one: raising the latency by `d` drops the
camera's next `d` of frames (their new dates overlap frames already held);
lowering it leaves a gap of `d` that the clip counts as dropped frames.

The recorder finishes a clip when a frame past its window arrives, or
through `ClipRecorder::poll(now)` once `now` is `stall` (1 s) past the
window. `poll` compares `now` with the frame dates, so the actor passes
`camera::frame_clock(now, latency)` = `now − latency`: with latency `L`
the frames of a window's end arrive `L` after it, and a plain wall clock
would finish (or discard, for want of the impact frame) any clip of a
camera more than about `stall` behind. A clip of a camera with latency
therefore finishes `L` later. The ring holds the dated frames, which stop
`L` short of a trigger's time, so the history (pre-roll + longest impact
lead + 0.5 s) needs nothing for the latency.

### Triggers

A `shot_trigger` from a launch monitor is stamped with the host monotonic
clock (`stance::clock::now_ns`) when the actor reads it: FRP
triggers carry no time. The actor reads the bus at least every 10 ms while
capturing, so a stamp lags the trigger's arrival by 10 ms at most (100 ms
while the camera is being reopened, when there are no frames to cut). The
trigger also records the clip store's generation (see below). A repeat of a shot the actor has seen (same actor
and `shot_id`, among the last 64) is dropped; the recorder counts every
trigger. The trigger's impact lead (`camera::impact_lead`, beta estimates) is read
from the live config at every trigger: the source's `impact_lead_s`
(`Option<f64>` on each launch monitor section, 0 to 5 s; out of range is
refused with `400` on save and ignored with a warning at load), else the
default for its actor type (`flighthook::impact_lead_default_s`). A change
applies to the next shot; neither the camera nor the monitor restarts
(`reconfigure` is `Applied`). The defaults:

| Source | Default impact lead |
| --- | --- |
| `mevo`, `mock_monitor` | 0 |
| `square` | 1000 ms |
| `exputt` | 1500 ms |
| `r10` | 2000 ms |
| `openconnect_server` | 2500 ms |
| any other launch monitor (`DEFAULT_IMPACT_LEAD_S`) | 1500 ms |
| `autochip` | not recorded |

Pre- and post-roll come from the live `[replay]` section at every trigger;
the recorder's history is the pre-roll plus the longest impact lead in
effect over the configured launch monitors (settings and defaults, at most
5 s) plus 0.5 s. It is re-applied at every trigger, so a setting raised
above the history takes effect at its next shot. The lead used is recorded
in the clip's `shot.json`. The
recorder's rules (stance): the window is `[impact − pre_roll,
latest trigger + post_roll]`; a trigger from another source whose impact
estimate is within 500 ms of an open clip's joins it (one swing, two
monitors); a trigger from a source already in an open clip cuts that clip
(`truncated`) and starts a new one.

### Clip store

A finished clip is saved on a thread of its own (`clip:camera.N`). Shots
whose trigger came before a Clear history are left out; a clip with none
left is not written. `ClipStore::begin` (`app/src/state/clips.rs`, one store
in `SystemState`) picks the clip folder: the folder of a clip that already holds one of the
clip's shots (another camera's clip of the same swing), else a new one,
named `YYYYMMDD-HHMMSS-mmm-xxxxxxxx` (UTC, so names sort by time) under the
clip folder (`[replay] clip_dir`, default `dirs::data_local_dir()/flighthook/clips`).
`stance::write_clip` writes `<camera-actor>.avi` and `.json` there
(each as `.part`, synced and renamed, the sidecar last), then
`ClipStore::finish` records the camera's `ShotVideo`, rewrites the folder's
`shot.json` (`version`, `shots`: key, device kind and impact lead per shot,
`cameras`: each camera's `ShotVideo`) and prunes. `begin` counts a writer
on the clip and `finish`/`abandon` count it off; the camera checks the slot
is still current (`ClipStore::is_current`) before writing, so a clip
cleared meanwhile costs no write. The camera actor then emits
`shot_video` (`keys`, `position`, `duration_ms`, `fps`, `trigger_offset_ms`,
`impact_offset_ms`, `complete`, `dropped`, from the sidecar) and, when the
prune deleted clips, `shot_video_removed { keys }`. A failed write raises a
`warn` alert and leaves no folder.

The store indexes shot id → clip. Pruning keeps the newest `keep_shots`
clip folders (read live, held to 1 to 1000) after every clip and at startup
(`main` loads the folder, indexes every folder with a `shot.json`, deletes
the rest and emits `shot_video_removed` as `system`). It skips folders with
a writer and the clip just finished; those stay over the count until a later
prune. Clear history (`clear_shot_history`, handled by the system actor)
deletes every clip and counts the store's generation up; a clip begun before
it is dropped when its camera finishes. A folder that can't be deleted (a
file still open, on Windows) is retried at the next clip and at startup.
Only folders named like clip ids are ever read or deleted.

When a config change moves `clip_dir`, the system actor calls
`ClipStore::reindex`: the store forgets the clips of other folders (their
files stay), keeps clips still being written until they finish, indexes the
new folder like at startup, and the shots that lost their clips go out in
one `shot_video_removed` from `system`. Clips in an earlier folder are never
deleted by flighthook afterwards.

### Web

Each web server stamps a `shot_video` onto the shots it names, found by
`shot_id` whatever actor took them: a pending shot keeps it until
`shot_finished`, a stored shot gets it in `data.video` (one entry per camera,
replacing that camera's earlier one). `shot_video_removed` clears `video` on
its shots, `shot_pose` sets `pose` on its camera's entry and `shot_swing`
sets `swing`. Routes
(`web/routes.rs`):

- `GET /api/shots/{shot_id}/video`: the clip's finished cameras
  (`ClipStore::videos`).
- `GET /api/shots/{shot_id}/video/{camera}.json | .avi | .pose.json | .swing.json`:
  a finished camera's sidecar, AVI, keypoints or swing analysis
  (`ClipStore::file`: only cameras the store
  recorded as finished, so no path from the URL reaches the file system),
  served by tower-http's `ServeFile` with `Range` support and
  `Cache-Control: private, no-cache`.
- `GET /api/cameras`: `Cameras::list`, on a blocking thread, reused for 3 s
  (`500` with a text body when that thread fails); `in_use_by` from the
  actors' slots; stance never opens a held camera to list it; the
  test picture (`TestPattern::camera_info`) last.
- `GET /api/cameras/{actor}/still`: the slot's newest JPEG, `no-store`.
- `GET /api/cameras/{actor}/live?after=<seq>`: the live slot's frame
  numbered above `after` (see [Live frames](#live-frames)), with
  `X-Frame-Seq`, `X-Frame-Time-Ns` and `no-store`, the frame's pose in
  `X-Pose` when body tracking ran on it, and body tracking's state in
  `X-Pose-State` / `X-Pose-Error` (on the `204` too); `204` after 1 s
  without one, `404` when `actor` is not a configured camera, `503` when
  8 requests already wait on it. An `after` above every number this
  process has given out (a viewer from before a restart) counts as 0, and
  the UI takes a frame numbered below its last one, so a view survives a
  server restart.
- `GET /api/shots/{shot_id}/video/{camera}.pose.json` and `.swing.json`: a
  camera clip's keypoints, and its swing analysis, once each is done,
  served like the `.avi`.
- `POST /api/shots/{shot_id}/video/{camera}/analyze`: track that clip now
  for a viewer (`pose::clips::request`): `202 {"state":"queued"}`, `200
  {"state":"done"}` when its keypoints are there, `409` in Off mode, `404`
  without the clip.
- `GET /api/status`: `cameras_supported` (`stance::camera::SUPPORTED`)
  and `clip_dir`.

The export and `/api/shots/convert` drop `video`.

### Body tracking (alpha)

The golfer's keypoints on the cameras' pictures, live and over each shot's
clips, found on this computer's CPU by stance's `pose` feature: a
YOLOX-Tiny person detector finds the golfer, RTMPose-m or -s finds 26 Halpe
keypoints in that box (tract, pure Rust, no native libraries); and golf
numbers measured on them by stance's golf analysis (`stance::golf`): each
recorded swing's events, tempo and metrics, and the live posture. The code
is `app/src/pose/` (`models.rs`, `live.rs`, `clips.rs`, `priority.rs`,
`swing.rs`, the one module that calls `stance::golf`), held in `SystemState::pose`. `[body_tracking]` is read live
through `SystemConfig::body_tracking()` (out-of-range values at their
defaults): no change restarts a camera.

| Field | Default | Range | Used by |
| --- | --- | --- | --- |
| `mode` | `"off"` | `"off"` \| `"historical"` \| `"live"` | everything below |
| `model` | `"rtmpose-m"` | `"rtmpose-m"` \| `"rtmpose-s"` | live and recorded |
| `min_score` | 0.3 | 0 to 1 | drawing (UI): keypoints below it are hidden |
| `smoothing` | 1.0 | 0 (off), or 0.1 to 10 Hz | live only: One Euro cutoff at rest |
| `detect_every` | 10 | 1 to 60 | live only: frames between detections |
| `golfer_height` | unset | 100 to 250 cm | swing analysis: distances in cm |

Out-of-range values are refused with `400` on save (`BodyTrackingSection::error`)
and replaced by their default, with a warning, from the file. `mode` is an
enum like `model`: an unknown value is an invalid body (the JSON extractor's
`4xx`) or an unparsable file. A section with `enabled` and no `mode`
(`BodyTrackingRaw`, `#[serde(from)]`) reads `false` as `off` and `true` as
`historical`; a section with neither is `off`. Only `mode` is written, so
such a file is brought up to date with a backup at load, and a config
without the section stays byte-identical. The Body
tracking card (General) edits `mode`, `model` and `golfer_height` (typed
in inches with imperial default units, centimetres with metric; stored in
centimetres); the Live keys tune the rest and save through
`POST /api/settings?scope=body_tracking` (`SetBodyTracking`: that section
alone, so a tuning key never writes another setting).

**Modes.** `mode` decides what runs:

| Mode | Live views | Recorded clips | Models |
| --- | --- | --- | --- |
| `off` | pictures only | none | never wanted; let go on entering Off |
| `historical` | pictures only | a clip when a viewer asks for it (`POST .../analyze`), at once | wanted by the first clip asked for |
| `live` | tracked while watched | every clip, queued when saved, 3 s delay; also any clip a viewer asks for | wanted by each camera at start and every 2 s |

The camera actor reads the mode per frame (`LiveRoute`: a worker only in
Live; leaving Live drops it at the next frame) and per saved clip (a
`ClipJob` only in Live). After every config command the system actor calls
`BodyTracking::follow` with the settings in effect: on entering Off it drops
every queued clip (`ClipQueue::clear`; a pass already running finishes),
lets the models go (`Models::unload`: the map is emptied and a generation
counter moves, so a load running now stores nothing when it ends; instances
already made keep their weights until dropped), stops a model download
(`Models::cancel_download`) and forgets the cameras' live failures. A queued recorded clip whose turn comes outside
Live is skipped (`clips::wanted`); a viewer's clip runs in Historical and
Live.

**Models** (`pose::models`). Stored in `<local data dir>/flighthook/models/`
beside the default clip folder, as `<name>.onnx`. Nothing is ever downloaded
on its own: no mode, camera start, clip, retry or startup path fetches a
file. A model is loaded the first time it is wanted, on a thread of its own
(`pose-models:<model>`), one load at a time, once both its files are in the
folder and match their SHA-256 (`Source::verified`; the official source
remembers each file's result by size and modification time, so a file is
hashed once until it changes). A file missing or mismatched leaves the model
`Missing` (logged once, info): it is not tried again until the files are
there, which every later `want` checks by stat alone. A load that fails on
files that are there is `Failed` until the models are let go or a download
finishes. In Live mode each camera actor asks for the model in effect when
its capture starts and every 2 s, so the models are ready before anyone
watches; in Historical mode the first clip a viewer asks for does, and the
clip waits for the load (a clip whose models are missing or failed is left
untracked; the viewer asks again on its next view). stance's loaded models
are not `Sync`, so they sit behind a mutex (`models::Shared`) and every user
takes its own instances with `fork`, which share the weights; no file is
read twice. Asking for a model lets go of the other one once it is loaded
and unused (only the map holds it; instances already made keep their
weights).

**Model downloads** (`Models::download`, `cancel_download`, `files`). The
only download path is `POST /api/body_tracking/models/download`, which the
UI sends after the user confirms the download dialog. It fetches the files
the configured model lacks, one after the other, on a `pose-download`
thread, through stance's `fetch_model_with` (OpenMMLab's zip for RTMPose,
Megvii's GitHub release for YOLOX; HTTPS, hash-verified, written through a
`.part` file and a rename) with a progress callback and a cancel flag. One
download runs at a time: a POST while one runs starts nothing (`202`), a
POST with every file there answers `200`. Its progress (per file: bytes done
and the response's length, the model's size until it is known; the file
being fetched; overall done and total) is kept in a slot that
`GET /api/body_tracking/models` reads (`BodyTrackingModels`: `missing`,
`downloading`, `ready` or `failed`, the files with their labels, hosts,
sizes and whether they are there, and the folder). The UI polls it at
about 4 Hz while its overlay is open; nothing goes on the bus.
`DELETE /api/body_tracking/models/download` (and entering Off) sets the
cancel flag and empties the slot at once; the download's thread notices
between two reads, stance deletes the partial file, and the state is
`missing` again with no error. A failed download keeps its error for that
model (`failed`) until the next download or a cancel. When a download
finishes, every `Missing` or `Failed` model entry is dropped, so the next
`want` loads. A `.part` file unwritten for 10 min, left by a process stopped
mid-download, is deleted before the next download. Tests never reach the
network: `SystemState` in tests has an empty models folder of its own and a
stand-in source (`models::fake`), which writes a placeholder file in steps,
honours the cancel flag and can be made to fail.

**Live** (`pose::live`). While a camera's live view is watched and body
tracking is in Live mode, the camera actor runs a worker thread `pose:camera.N` and
offers it every frame through a one-frame handoff (`LiveWorker::offer`):
each offer replaces the frame waiting, which is skipped, never queued. The
worker takes the newest frame when it is free, at most 15 a second (one
start per 1/15 s), decodes it, runs stance's `PoseTracker` (detection on
the first frame, every `detect_every` frames and when the golfer is lost;
otherwise a crop around the last keypoints; inference on up to 4 cores),
smooths the keypoints with a One Euro filter per coordinate at the
`smoothing` cutoff (beta 0.05, reset when the golfer is lost, the cutoff
changes, or a frame is dated no later than the one before, as after a
latency change), and puts the frame in the live slot together with its pose, so
the picture and the skeleton always match. The live rate is the pose rate,
at most 15. A new `model` or `detect_every` makes a new tracker. When the
watch lapses or body tracking leaves Live mode the worker is stopped (its
tracker with it) and frames go to the slot directly, without a pose; while
the models are not ready the worker passes frames on without one. Dropping
the worker sets its stop flag without joining it, so the capture loop never
waits on a pose or a model build; it ends within one pose and publishes
nothing more. A worker whose thread ended by itself (a panic;
`LiveWorker::alive`) is replaced at the next frame, up to 3 times while
watched; after that the camera's live frames go out without a pose and
`X-Pose-State` is `failed` for it (`BodyTracking::set_live_failed`) until
the watch lapses or tracking leaves Live mode. For a
camera at golfer_front or target_line (`swing::live_setup`, read per frame
with the player's hand from the game state and `swing::mirrored`), the
worker also measures the found pose's posture for that view
(`stance::golf::posture`, on `stance::golf::unmirror`ed keypoints for a
mirrored picture), smoothed by a
`PostureFilter` at the same cutoff (raw when smoothing is off; reset with
the keypoint filter). The live route sends the pose as `X-Pose`, compact
JSON (`skeleton`, one `[x, y, score]` per keypoint at 0.1 px and 0.01,
`bbox`, `ms`, and `posture`: the view and its angles to 0.1°), about 1 KB
per frame, and `X-Pose-State` (`off` outside Live mode, `loading`, `missing`
while the model's files are not downloaded, `downloading` while a model
download runs, `ready`, `failed` with `X-Pose-Error`: the model's load, or
the camera's live worker) on every answer, the `204` included.

**Recorded swings** (`pose::clips`). In Live mode a camera's clip is queued
for body tracking when it is saved and announced (`shot_video`), to start no
sooner than 3 s after it was written (`CLIP_DELAY`), so the pass stays clear
of the shot itself. A viewer's request (`clips::request`, from
`POST /api/shots/{shot_id}/video/{camera}/analyze`; the UI sends it in
Historical and Live mode for each clip it shows without `pose`) builds the
same `ClipJob` from the clip store (`ClipStore::camera_clip`: folder, AVI,
shots, position; the hand from `shot.json`'s first shot, `ClipShot.hand`;
the impact prior from the sidecar; the camera's current latency for the log
line) with `on_demand` set: no delay, since the clip is not the shot just
hit. Requests are deduplicated: a clip whose `pose` is set answers `done`,
and one waiting or being tracked (`ClipQueue::has`: queue by folder id and
camera, plus the running job) is not queued again; a recorded clip viewed
while it waits keeps its place and its delay. One worker thread
(`pose:clips`) takes the clips one at a time: a viewer's clip goes to the
front (the clip on screen is the one wanted now; a clip asked for earlier
was likely left behind), recorded clips to the back, oldest first. The
queue holds at most `keep_shots` × the number of cameras, dropping the clip
queued longest ago with a warning when full; a clip pruned or cleared before
its turn is skipped, and so is everything while the models are missing or
fail. Each clip
runs stance's `analyze_clip` (detection on key frames, interpolated boxes,
pose on every frame, a fix-up pass; detection every 10 frames) on a quarter
of the logical CPUs (`clip_threads`, at least 1) at below-normal priority
(`pose::priority`: the `pose:clips` worker lowers itself, and the pass's
`PoseConfig::on_worker_start` hook has each of stance's worker threads lower
itself before it takes a frame; nice 10 on Linux, below normal on Windows,
no other thread touched). A panic in a clip's pass is
caught and logged, and the queue goes on; the worker clears its running
flag however it ends. Then `PoseTrack::write_beside` writes
`<clip folder>/<camera>.pose.json`: one entry per clip frame on the sidecar's
time base (`t_ns`), raw keypoints (no smoothing), empty where nobody was
found. `ClipStore::set_pose` then marks the camera's `ShotVideo` (`pose:
true`, kept in `shot.json`) and the queue sends one `shot_pose { keys }`
with the camera as its actor, straight on the bus (a fact about files, sent
even if the camera actor has been restarted since). A clip pruned or
cleared while it ran keeps no pose file. Pose files are deleted with their
clip folder (pruning, Clear history).

**Swing analysis** (`pose::swing`). Right after a clip's pose file is
written and announced, the same worker runs `stance::golf::analyze_swing` on
the track, for a camera at golfer_front (face-on) or target_line (down the
line); golfer_back and overhead get none. It is given the view, the hand at
the shot (the game's hand when the camera saw the trigger, carried with the
recorder's tag; right-handed when unknown), the clip's estimated impact (the
sidecar's `impact_ns`, the launch monitor's impact shifted by its camera
timing, when it falls inside the clip's frames: at or after the first, at
or before the last), `golfer_height`, and whether the
camera's picture is mirrored (`swing::mirrored`, the one place a mirror
setting plugs in; no camera is mirrored yet). stance takes the impact as a
prior: it moves it to the hands' low point within 0.3 s (`refined`), or,
with no swing there, keeps it and reports the impact alone (`given`). The
refined impact minus the estimate is logged at info per shot and camera,
with the launch monitor kind, its camera timing and the camera's latency
(`ClipJob.timing`, from the clip's first shot), to calibrate each
monitor's camera timing; a `given` report logs that no swing was found. A
`handed_guess` that disagrees with the hand at 0.6 confidence or more is
logged as a warning (a mirrored picture). `SwingReport::write_beside`
writes `<clip folder>/<camera>.swing.json` (events, tempo, the metrics at
the key positions and every metric at every frame); `ClipStore::set_swing` marks
the camera's `ShotVideo` (`swing: true`, kept in `shot.json`) and one
`shot_swing { keys }` follows, as for the pose. A track the analysis can't
use (fewer than three frames with the golfer) leaves no swing file (logged
at info). `GET /api/shots/{shot_id}/video/{camera}.swing.json` serves it
with the `.avi` rules; swing files go with their clip folder.

**UI.** The Live view draws each view's pose (the one that came with the
picture on screen) into the picture's rect with `ui/src/pose.rs`: the frame
fitted to the rect at its aspect, keypoints under `min_score` hidden with
their bones, the lead side (a right-hander's left; the game's hand) in teal,
the trail side in orange, the midline light. While the models are not
ready a band at the bottom of the picture says what they are doing. The
replay and the Cameras pane's player fetch each tracked clip's
`.pose.json` once (when the row's video has `pose`, or when `shot_pose`
arrives while they are open) and draw the pose frame whose `t_ns` equals
the shown frame's, with the shot's hand.
In Off mode nothing is fetched or drawn: no `.pose.json` / `.swing.json`
fetch, no skeleton, no swing panel, no analyze request, also for clips
tracked earlier. Outside Live mode the Live view shows the pictures alone
(no skeleton, posture tiles or readout line). In Historical and Live mode
the player and the replay send one analyze request per clip without `pose`
(`Fetch::Analyze`, once per view); the answer only matters when it is `200`
(done already: the files are fetched at once), otherwise `shot_pose` and
`shot_swing` bring them as for a recorded clip, and until then the clip
plays without them, as while any clip is tracked.
Keys (`cameras::KEY_BINDINGS`, no modifier; none in Off mode): `B` skeleton
on/off (Live and a shot's clips, Historical and Live mode; this device:
browser local storage, or `flighthook/ui.json` in the local data folder
natively; the replay follows it); on Live in Live mode only, `M` model,
`[` `]`
`min_score` ∓ 0.05, `,` `.` smoothing less/more along off, 10, 5, 3, 2, 1,
0.5, 0.3, 0.2, 0.1 Hz, `D` `detect_every` 1 → 5 → 10 → 30. A key changes
the values shown at once; they are saved one request at a time, the newest
winning, and shown until the reloaded settings hold them (a failed save
shows the saved ones again). In Live mode a line under the live views reads
out the values and each camera's measured live rate and pose time.

## SystemActor

The `SystemActor` (`actors/system.rs`) is a default actor that always runs,
independent of config. It holds the sole `GameStateWriter`, `Arc<SystemState>`,
and `broadcast::Sender`, enforcing at the type level that only `SystemActor`
can mutate game state and process config mutations. It handles:

- **Game state**: subscribes to `PlayerInfo`, `ClubInfo`, `HandInfo` and
  `SetDetectionMode` bus events, updates game state via the writer.
  **Projects the club and the hand onto the controller command**: when
  `ClubInfo` is received, calls `club_mode()` and emits
  `SetDetectionMode { mode, club }`; when `HandInfo` carries a hand that differs
  from the stored one (or none was stored), emits
  `SetDetectionMode { handed }`. From `SetDetectionMode` only the mode is
  stored, so its own command coming back round changes nothing. This
  centralizes the device commands so every source triggers them the same way.
- **Config mutations**: subscribes to `ConfigCommand` bus events (from the
  REST API). Applies the mutation to `SystemConfig`, calls
  `apply_config_reload()` to reconcile actors, and emits a `ConfigOutcome`
  on the bus once the actors it stopped have ended. This provides natural
  sequencing -- all config mutations are processed one at a time on the
  SystemActor thread; the stop waits run on helper threads, so game events
  and further commands are handled meanwhile.
- **Clear history**: on `clear_shot_history` it deletes every replay clip
  (`SystemState::clips`), once for the process, whichever web server or
  client sent it.
- **Clip folder**: after a config command that moves `[replay] clip_dir`, it
  reindexes the clip store (see [Clip store](#clip-store)).
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
`openconnect_client.0`, `mock_monitor.0`, `random_club.0`, `camera.0`,
`webserver.0`, `ws.a1b2c3d4`. The `system`
actor has a fixed ID of `"system"`. The type prefix encodes the component type;
the index is the key within that type's config section. `autochip` is the
one id with no index and no config section: the virtual source of Auto-chip
shots (`POST /api/auto-chip`).

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
    pub local_clients: Arc<LocalClients>,  // this process's Open Connect clients
    pub clips: ClipStore,         // replay clips on disk (beta)
    pub cameras: Cameras,         // camera actors' latest frames + the camera list (beta)
    pub pose: BodyTracking,       // body tracking models + the recorded-clip queue (alpha)
    actors: RwLock<HashMap<String, Registered>>,  // the actor + its ActorHandle
}
```

`SystemState::new()` returns `(Self, GameStateWriter)`. The writer is passed
to `SystemActor` at construction time.

`system` provides cached read/write access to the persisted TOML config.
Config mutations are serialized through the bus (SystemActor processes
`ConfigCommand` events one at a time on its thread), so no external lock
is needed. `game` is a read-only handle for player info, club, mode and hand;
mutations go through `GameStateWriter` held by `SystemActor`. `actors` is a
registry of all running actors, keyed by global ID. Each entry stores the
actor and its `ActorHandle`: the per-actor shutdown flag (`Arc<AtomicBool>`),
the flag that mutes its bus sender, and its thread (the `JoinHandle` `start()`
returns). Methods: `register_actor(id, actor, handle)`, `stop_actor(id)`
(sets the flag, calls `actor.stop()`, returns the actor's `stop_wait()`),
`wait_for_exit(id, deadline)` (wait for the thread to end: `Exited` takes it
out of the registry and joins it, `Lingering` leaves it there flagged),
`retire_actor(id)` (out of the registry and muted, its thread left to end),
`close_in_background(id)` (retired and watched: `is_closing(id)` until its
thread has ended, then `take_closed()` hands the ID to SystemActor to start
again), `stop_all_and_wait(max)` (exit), `actor_ids()`. A thread that
panicked is logged when it is joined.
Actors are registered right after `start()` returns their thread.

## Log

The Log tab in the UI streams all bus events in real-time with per-message-type
filter checkboxes. Raw wire data is carried via `raw_payload` on bus messages.
Binary payloads serialize as hex strings; GSPro JSON payloads serialize as-is.
Events without a `raw_payload` (e.g. `actor_status`, `config_outcome`) display
the event's JSON serialization as the payload instead. The log retains the last
500 events. Protocol-level tracing is always emitted to the `audit` tracing
target (filtered by `RUST_LOG`, not shown by default). `shot_video` and
`shot_video_removed` sit in their own Camera filter group, each with a
one-line summary (the clip's shots, camera, length, frame rate,
completeness and dropped frames; how many shots' clips were removed).

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
{ "cmd": "hand", "handed": "lh" }
{ "cmd": "new_group" }
{ "cmd": "new_session" }
{ "cmd": "clear_history" }
{ "cmd": "grouping", "mode": "by_club" }
```

Mode commands emit `SetDetectionMode` on the bus (mode is global,
not per-device). `club` emits `ClubInfo`, `hand` emits `HandInfo`,
`new_group` / `new_session` /
`clear_history` emit `NewShotGroup` / `NewShotSession` / `ClearShotHistory`,
and `grouping` emits a fire-and-forget
`ConfigCommand` (`SetShotGrouping`), each with the socket's actor ID. Each has
a REST twin (`POST /api/club`, `/api/hand`, `/api/shots/groups`, `/api/shots/session`,
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
                           | actors::camera (beta)  |
                           | actors::mock::launch   |
  GUI --ehttp/ws---->      | polls bus via poll()    |            ^
  (to local web server)    +------------------------+            |
                                     ^                            |
                                     +--- single broadcast<FlighthookMessage>(1024) ---+
                                     |                            |
                           +--------------------+        +--------------------+
                           | actors::gspro       |        | actors::mock::     |
                           | OpenConnect client  |        |   randomclub       |
                           +--------------------+        +--------------------+
```

**Bus-based command delivery**: actor threads poll the bus via `BusReceiver::poll()`
in their event loop. They filter for `SetDetectionMode` events (mode, hand
and club changes). The `SystemActor` derives these commands from
`ClubInfo` (mode and club, using `club_mode()`) and `HandInfo` (hand), so integrations
only need to emit `ClubInfo` and `HandInfo`. There is
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
needs, spawns a thread and returns its `JoinHandle`, which ends once the actor
has let go of its device or connection. `stop_wait()` (default 3s) bounds how
long that may take after a stop, and `replaceable_while_stopping()` (default
no) says whether a new instance may start before it has. `reconfigure()` compares current config against
construction params and returns `Applied` or `RestartRequired`.
Actors are registered in `SystemState.actors` for dynamic
start/stop/reconfigure.

**Per-actor spawning**: the `SystemActor` is created first (always-on, not
config-driven). Then each config section constructs an actor struct and calls
`actor.start(state, sender, receiver)`. Each actor gets a per-actor
`Arc<AtomicBool>` shutdown flag shared between the `BusReceiver` and the
registry. Launch monitor actors (mevo, r10, mock::launch) run event loops polling
the bus. Integration actors (the OpenConnect client in `actors::gspro`, mock::randomclub) subscribe to the bus
and emit `ClubInfo` / `HandInfo` (for club and hand changes) and `ActorStatus`
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
  `ReplaceGlobal`: units, chipping/putting clubs, default view, simulation,
  replay and body tracking from the body, every actor section of the live
  config kept.
- **Body tracking** -- `POST /api/settings?scope=body_tracking` emits
  `SetBodyTracking`: the body's `[body_tracking]` alone.
- **Per-section upserts** -- `POST /api/settings?scope=<id>` emits a scoped
  `ConfigAction` variant (e.g. `UpsertMevo`), writing only that section and
  reconciling only that actor. A scope naming a section the body omits emits
  `Remove`; an unknown scope is rejected with `400` without saving anything.
- **One web server** -- a save that would remove a `webserver.N` section
  (scoped or whole-config), or add one while the config already holds one,
  is rejected with `400` before any `ConfigCommand` is emitted.

`SystemActor::handle_config_command()`:

1. Applies the `ConfigAction` mutation to `SystemConfig` (replace, reload,
   upsert, or remove)
2. Calls `apply_config_reload()` to stop what goes or restarts and plan the
   rest
3. Has `StopWaits` wait for the stopped actors on a helper thread, then
   finishes the plan (`system::finish`) and emits `ConfigOutcome` on the bus
   (always, not just for request-reply; at once when nothing was stopped)

`apply_config_reload()` (in `actors/system.rs`) orchestrates actor lifecycle:

1. Snapshot the new config and build a `ResolvedConfig`
2. Compute current vs expected actor IDs
3. **Deleted actors** (current but not expected): stopped, then released
4. **Existing actors**: call `reconfigure()` on each
   - `RestartRequired`: stopped, then restarted as a reconnect is (see
     [Reconnect](#reconnect)): a new instance starts once the old one has
     ended or when it holds nothing the new one needs, otherwise once the old
     one has ended, and the actor is listed in neither `restarted` nor
     `started`
   - `Applied`: no change needed, skip
5. **New actors** (expected but not current): created and started, unless an
   old instance of the ID is still closing or settling (it starts once that
   is over)

Every actor a command stops (deleted or restarted) is stopped first and then
waited for, so the waits overlap. The releases and starts happen in
`system::finish` once the waits are over, in that order (removals, restarts,
new actors), each from the settings current then.

The web layer's `state_updater` handles `ConfigOutcome` events to refresh
actor name caches (on every outcome: a rename is `Applied`, so it restarts
nothing) and remove stopped actors. `GET /api/status` names each actor from
the config as it is at the request, so it has a new name before the bus
event reaches that cache. Every UI fetches the status again on any
`ConfigOutcome`, so a rename shows in every open window without a reload:
card titles, the Shots tab's device filter and labels, and alert banners.

`ReconfigureOutcome` enum:

- `Applied` -- config unchanged or applied without restart (default for mock actors)
- `RestartRequired` -- address/routing/settings changed; must stop and recreate

## UI Zoom

The UI (native window and browser) zooms itself every frame to fit the
window (`ui/src/scale.rs`). The layout is designed for windows from 1024×768
to 1920×1080 points. With `w` × `h` the window's size in points at zoom 1
(physical pixels over the screen's native pixels per point, so the display
scale or device pixel ratio counts), the zoom is `min(w / 1920, h / 1080)`
(at most 3.0) when that is above 1, else `min(w / 1024, h / 768)` (at least
0.8, so a phone keeps readable text and scrolls) when that is below 1, else
1. Both are rounded down to a multiple of 1/32, so resizing a window changes
the zoom only now and then and the design size still fits. A 4K screen at
100 % therefore looks almost as it does at 200 %, at 300 % it zooms out until
768 points of height fit, and a 1920×1080 screen at 150 % lays out at about
1365×768 points.

In the native window, Ctrl/Cmd + `=` or `+` and Ctrl/Cmd + `-` step the zoom
by a factor of 1.1 on top of that (up to 6 steps either way, for the
session), Ctrl/Cmd + `0` goes back to the automatic zoom, and the result
stays within 0.5–4.0. In the browser those keys are the browser's own zoom,
which the fit absorbs while the page is larger than 1920×1080 CSS pixels.
The browser also caps the effective pixel ratio at 2.0 on screens above 2.0,
so text and layout use at most 2 pixels per point there.

## Devices Tab

The Devices tab is where the app opens. It holds the global settings, the
web server, and every launch monitor, camera and output, each device with
its live telemetry and its settings.
Four groups of cards, each group a wrapping grid: as many columns as fit at
a 420 px minimum card width, cards at most 620 px wide, one column on a
narrow window; card `i` goes in column `i % columns`. Every card is the same
height, so the rows line up and the tab scrolls as a whole when there are
more rows than fit (see [Card height](#card-height)).

```
  [Devices] [Shots] [Log]  |  [Full] [Chipping] [Putting]  [RH|LH]  CLUB [7I]  [##] FLIGHTHOOK

  GENERAL
  | Global                           [API Docs] | Simulation [beta]
  | Units:             [Imperial v]             | Simulation:   [On v]  (Off | On | Force)
  | Default shot view: [By set | By club | None]| Altitude:     [0] [ft v]   (greyed while Off)
  | Chipping Clubs:    [PW GW SW LW]            | Temperature:  [77] °F      (°C when Metric)
  | Putting Clubs:     [PT]                     | Surface:      [Fairway v]
  | Web Server [CONNECTED] [Telemetry|Settings]
  |   bind        0.0.0.0:5880
  |   websockets  1

  LAUNCH MONITORS (+)
  | Mevo WiFi [Mevo] [CONNECTED] [Telemetry|Settings] | Garmin R10 [R10]       [Telemetry|Settings]
  |   battery_pct  85                                  | Name:            [Garmin R10]
  |   ready        true                                | Monitor-to-Ball: [8] [ft v]
  |   tilt         0.5                                 |
  |                                     [Reconnect]   |                               [Remove]

  CAMERAS (+)
  | Golfer Back [Camera] [CONNECTED] [Telemetry|Settings]
  |   camera      Test pattern
  |   fps         60
  |   position    golfer_back
  |   resolution  1280x720
  |                                   [Still frame]

  OUTPUTS (+)
  | GSPro [OpenConnect Client] [Cancel] [Save]   | Muni Golf [OpenConnect Client] [CONNECTED] [Telemetry|Settings]
  |                  Save or cancel your changes |   address     127.0.0.1:9210
  | Name:             [GSPro]                    |                                [Reconnect]
  | Address:          [127.0.0.1:921]            |
  | Full Monitor:     [Mevo WiFi v]              |
  | Chipping Monitor: [Any v]                    |
  | Putting Monitor:  [Any v]                    |
  |                                     [Remove] |
```

At 1600 px wide the General row is Global, Simulation, Replay, with the Web
Server card under Global; at 1024 px Replay sits under Global and the Web
Server under Simulation.

Groups and order (`ui/src/panels/cards.rs`):

| Group | Cards, in order | (+) offers |
|---|---|---|
| General | Global card, Simulation card, Replay card, `webserver` | -- (no (+)) |
| Launch monitors | `mevo`, `r10`, `square`, `exputt`, `mock_monitor`, `openconnect_server` | Mevo, R10, Square Golf Omni, ExPutt, Uneekor (OpenConnect), OpenConnect Server |
| Cameras | `camera` | Camera (only while `/api/status` says `cameras_supported`; without it the group shows the cameras already configured and no (+)) |
| Outputs | `openconnect_client`, `random_club` (and any type the UI does not know) | GSPro (OpenConnect), Muni Golf (OpenConnect), OpenConnect Client |

Each (+) entry is a preset (`cards::AddPreset`): an actor type plus the new
card's name and address. The OpenConnect presets differ only in those:

| Entry | Type | Name | Address |
|---|---|---|---|
| Uneekor (OpenConnect) | `openconnect_server` | Uneekor | bind `0.0.0.0:921` |
| OpenConnect Server | `openconnect_server` | OpenConnect Server | bind `0.0.0.0:921` |
| GSPro (OpenConnect) | `openconnect_client` | GSPro | `127.0.0.1:921` |
| Muni Golf (OpenConnect) | `openconnect_client` | Muni Golf | `127.0.0.1:9210` |
| OpenConnect Client | `openconnect_client` | OpenConnect Client | `127.0.0.1:921` |

The generic OpenConnect Server and OpenConnect Client entries are for any
other monitor or simulator that speaks Open Connect.

A card's type label names the actor type, "OpenConnect Server" or
"OpenConnect Client", whichever preset created it.

flighthook runs one web server, the one serving this UI. Its card has no
Remove and no menu adds another; the config API refuses both (`400`, see
[API.md](API.md)). A config file that already holds several `[webserver.N]`
sections loads and runs them all, a card each, every one editable.

Within a group, cards sort by type (the order above), then by instance index
(numerically). The Open Connect server is a launch monitor: it receives shots.
Random Club consumes shots (it picks the next club on each one), so it sits
with the outputs. A card is every configured instance plus every running
actor the settings don't list (`ui/src/panels/devices.rs`
`device_card_order`), so two simulator connections or two launch monitors are two
cards, and the cards show (Telemetry only) before the settings have loaded.

### Device cards

Each web server, launch monitor, camera and output card has a small segmented
toggle in its header, **Telemetry | Settings**, Telemetry by default,
remembered per card for the session. The card's name sits on the left of
the header; on the right, just left of the toggle, sits the status badge on
the Telemetry side and the type tag on the Settings side:

```
Telemetry:          [Name …]  ........  [STATUS BADGE] [Telemetry|Settings]
Settings:           [Name …]  ........  [type tag] [Telemetry|Settings]
Settings, unsaved:  [Name …]  ........  [type tag] [Cancel] [Save]
```

Hovering the name or the tag says what the type is and how it connects (the
name's hover on the Telemetry side too). The tag is left out when the name
reads the same as the type, ignoring case: the web server at its default
name "Web Server", or an "OpenConnect Server" / "OpenConnect Client" card at
its default name. The toggle, Save, Cancel and the status badge keep their
size; a long name is cut short with "…" first, then the type tag, which is
left out once it would be cut to a stub. The toggle flips the card body:

- **Telemetry**: the actor's name and connection state badge
  (`CONNECTING` yellow, `CONNECTED` green, `RECONNECTING` red for 10 minutes
  after the connection was lost, then yellow; see
  [Connection state](#connection-state)), then its
  telemetry key/value
  pairs one per row, sorted by key. Detection mode, battery, tilt, roll,
  temperature, club, handed and the rest come from the actor's telemetry map.
  The key column is as wide as the longest key, up to 45 % of the card; a key
  or value too long for its column is cut short with "…" and shown in full
  on hover (a Square Golf Omni's `device_info`, for one). With no row to
  show, the body shows the actor's state in its middle instead, 48 points
  across: a spinner (a faint ring with an arc going round it) in the card's
  status colour while the actor is starting, connecting or reconnecting, and
  a check in a ring in the connected green once it is connected
  (`monitoring::no_telemetry`, `theme::loading_ring` / `connected_ring`). It
  goes as soon as a row arrives; a camera's still frame takes its place too.
  Only a spinner on screen asks for repaints (about 30 a second). Every launch
  monitor and output card (Mevo/Xi, R10, Square Golf, ExPutt, the
  OpenConnect server, the mock monitor, the OpenConnect clients, Random club
  and any other output) has **Reconnect** in its action row: it stops the
  actor, waits for its connection to close and starts it again from the saved
  settings (see [Reconnect](#reconnect)); the card then shows the usual
  statuses as it connects. Reconnect is disabled while that card's request is
  out and while the tab is locked. After a `202` (it starts again by
  itself later) it stays disabled until the actor reports `Starting`,
  or a `config_outcome` lists it in `restarted` (one without
  `request_id` included), for up to a minute (`app::RECONNECT_WAIT_MS`). A
  request that fails (refused, no answer from the server, a network error),
  or a `202` that has not started within that minute (unless the actor's
  own `error` row, such as the old connection still closing, already says
  why), shows its reason as the card's `error` row and in the error banner,
  as an error the actor reports does, until the actor's next status or the
  next press. An ExPutt's **Find mat** sits beside it (enabled while the ExPutt is connected), and a camera has **Still frame**
  instead, its frame shown in the card's body. The General cards and the
  cameras have no Reconnect.
- **Settings**: the instance's name, its type tag and its fields in a label
  column, with **Remove** in its action row (every card but the web
  server's), which asks for confirmation in a modal dialog and then saves the
  removal (`?scope=<actor_id>`, the section left out); the card goes when the
  server confirms it.

#### Card height

Every card on the tab (General, Launch monitors, Cameras, Outputs) is one
fixed height, 280 points (`devices::CARD_HEIGHT`, scaled with the UI zoom like
every other size), on both its Telemetry and Settings sides. Telemetry that
arrives, a side flip, an edit, a validation message, an error line or a still
frame never changes a card's height or moves another card. Inside a card:

- the header (name, badge or type tag, toggle, or Save and Cancel while it has
  unsaved edits) stays at the top, with the "Save or cancel your changes" hint
  and a failed save's line under it;
- the action row (Reconnect, Find mat, Still frame, Remove) stays at the
  bottom, right-aligned;
- the body between them scrolls inside the card when it does not fit, with
  egui's thin scroll bar shown only then. The mouse wheel over a card scrolls
  that card first and the tab only once the card can scroll no further. Each
  card keeps its own scroll position, keyed by its ID, for the session.

At 280 the Telemetry side of a connected Mevo+, Square Golf Omni and
OpenConnect client and the Settings side of the Global card fit without
scrolling, as do the other General cards and most Settings sides. A Mevo's
Settings side and a camera card showing a still frame scroll.

Every card is exactly its grid column's width, on both sides: long names in
the header and in text fields, and long entries in a dropdown, are cut short
inside the card rather than widening it over its neighbour or the cards
under it.

The coloured stripe down the left edge of a card is the colour of the actor's
connection state (the badge's), on both sides, dim while the actor is not running (unsaved, or not
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
both. The Shots tab's units are the Global card's **Units** value
(`FlighthookApp::units` reads `SettingsForm::default_units`, the persisted
`default_units`; there is no separate toolbar toggle), so they follow an edit
at once and Cancel puts the saved value back.

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
- OpenConnect Server cards show name and Bind Address, the `ip:port` the
  server listens on; blank means `0.0.0.0:921`, anything else must parse as
  `ip:port`. A bind change restarts the actor
- OpenConnect Client cards show name, Address (`ip:port` of the simulator)
  and the three routing dropdowns
- The Web Server card shows name and Bind Address, the `ip:port` it listens
  on (`0.0.0.0` accepts other computers, `127.0.0.1` only this one). A bind
  change restarts it
- Camera cards (beta) show name, Camera (a dropdown of
  [`GET /api/cameras`](API.md#get-apicameras), fetched the first time a
  camera card shows its settings, with a refresh icon; a camera another card
  uses reads "(in use by camera.N)", a saved camera that is not connected
  "(not connected)"), Position, and Video mode (Auto or one of the picked
  camera's modes, `1280×720 @ 120`; a saved mode the list lacks stays
  selectable). Save needs a camera and a mode. They are left out of the
  Open Connect clients' routing dropdowns. The Telemetry side ends in
  **Still frame**, enabled while the camera is connected: it fetches
  [`GET /api/cameras/{actor}/still`](API.md#get-apicamerasactorstill) and
  shows the frame at the card's width (decoded like the replay's frames)
- The Body tracking card (alpha, General, after Replay) holds
  `[body_tracking]` `mode` (Off | Historical | Live, pills like Model's),
  `model` (RTMPose-m |
  RTMPose-s) and Golfer height (in inches with imperial default units,
  centimetres with metric, converted when the units change; empty is none;
  checked like the server's); it saves and cancels with the other General
  cards. The other values are tuned with the Live keys, which save with
  `?scope=body_tracking` (that section alone). Picking Historical or Live
  while the mode in effect is Off is not a form edit: it goes through the
  model download prompt (`panels/body_models.rs`, below), which saves the
  mode itself
- The model download prompt (`panels/body_models.rs`, `ModelsPrompt`): the
  card's Off → on first asks `GET /api/body_tracking/models`. `ready`: the
  mode is saved at once through the tuning saves (`Tuning`,
  `?scope=body_tracking`, one at a time, the newest winning) and shown on the
  card. `missing` or `failed`: an `egui::Modal` dialog lists the missing
  files (label, MB, host), the total in whole MB, the models folder and that
  body tracking runs locally afterwards, with **Download** and **Cancel**
  (Cancel, Escape or the backdrop: nothing saved). Download saves the mode,
  `POST`s the download and turns the modal into the progress overlay: a bar
  per file and an overall bar with MB done/total, the current host, and
  **Cancel** (`DELETE`, then the mode saved Off). The overlay polls the state
  every 0.25 s once the POST is answered, one request at a time; `ready`
  closes it and has the replay and the inline player ask again for every
  clip without `pose` (`ReplayView::ask_again`); `failed` (or no answer to
  the POST) shows the error, a one-line firewall/proxy hint, **Retry** and
  **Turn off**. With the mode already on, the same check runs once per
  model per on-period the first time something needs the models (a shot's
  clips in the Cameras pane, Live in Live mode, Replay); its Cancel leaves
  the mode as it is. The camera keys are off while the prompt is up. No
  hover text
- The Replay card (beta, General) holds `[replay]`: Keep clips, Pre-roll,
  Post-roll and Start before impact in seconds, and the clip folder,
  read-only (`/api/status` `clip_dir`). It saves and cancels with the Global and Simulation cards
  (`scope=global`), and its fields are checked like the server's
  (`ReplaySection::error`)
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

The **club picker** sits right of the hand control on every tab: a pill with
the club in play (`—` when none), from `/api/status.club`, the WebSocket
`init.global_state` and every `club_info` event. Clicking it opens a popup of
all 33 clubs in bag rows (driver + mini driver, woods, hybrids, irons, wedges,
putter); hovering a
club names the mode it switches to when that is Chipping or Putting. A pick
sends the WebSocket `club` command (`POST /api/club` when the socket is down),
and the SystemActor then derives the detection mode as for a simulator club
change. While any `openconnect_client.*` actor is Connected the pill is
read-only, with the hover text "Set by the simulator": the simulator replaces
the club on its next player message, so its telemetry `club` and the pill
agree.

The **hand control** sits between the mode selector and the club picker: an
RH | LH segmented control with the player's hand, from `/api/status.handed`,
the WebSocket `init.global_state` and every `hand_info` event; RH while no hand
is set, the hand the devices use then. A click sends the WebSocket `hand`
command (`POST /api/hand` when the socket is down); the SystemActor then tells
the devices, as for a hand from the simulator. Like the club pill it is
read-only while any `openconnect_client.*` actor is Connected (a static pill
with the hover text "Set by the simulator"): the simulator replaces the hand
with its next player message. Hovering the live control says "Player's hand.
Devices that follow it are told on a change."

The **Auto-chip** chip sits right of the club picker, a gap after it, and
shows on every tab (see [Auto-chip](#auto-chip-beta) below).

**Battery indicators** sit left of the FLIGHTHOOK title, one small battery per
device that is shown Connected and reports `battery_pct` as a whole number from
0 to 100, in Devices card order (the first device furthest left). The fill is
that percentage of the battery and is red below 20 %, otherwise green; hovering
one says which device it is for and what is left (`Square Omni 74%`). A device
that is not Connected, or has no readable charge, has none.

The tab bar is a segmented control, the same look as the mode selector beside
it (`theme::segmented`). While any card is red (an actor of any kind lost
its connection less than 10 minutes ago, see
[Connection state](#connection-state)), the Devices segment carries a small
red notification dot at the top centre, overlapping its top edge, whatever its
selection state. Nothing else lights it: not an actor that has not connected
since it started, and not an `error` in an actor's telemetry, which shows on
its card and in the error banner instead. The UI asks for a repaint when the
first red card is due to turn yellow, so the dot goes out on time without a
new event. The
web layer caches telemetry from `ActorStatus` events plus club from `ClubInfo`
and name from `PlayerInfo` events so reconnecting clients recover latest
values immediately.

## Shots Tab

```
  [Data|Cameras]  [Cards|Table]  DEVICE [All|Mevo+|Omni]                 [export] [trash]
  (cameras only,  (Data only;      (2+ devices)                          (JSONL | CSV menu)
   both views)     Cameras: Live)

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
  SET 3 · 7I · 10 ---                          +--------+ +--------+ +--------+
  [#42      ]          SET 3 · 7I · 10 -------- |STATS   | |FACE-ON | |4 toe · |
  [carry tot]          ●avg ○med |shot ---------| AVG MED| | (clip  | | .----. |
  [#41      ]          TOTAL 148 [==○|=●] 217   |TOTAL..2| |  loop) | |( ●+ /  |
  SET 2 · DR · 4 ----  CARRY 148 [|○●]    217   |CARRY..4| |        | | '--'   |
                       LATERAL L [=○|●]     R   |LATERAL | |        | |TOE  mm |
                       (cards: stats, cameras,  +--------+ +--------+ +--------+
                        face impact; up to 3, click to cycle)
  [#40      ]          BALL --------  FLIGHT --------------  CLUB ----------------------
  (list scrolls)       [Direction]    [Apex]    [Offline]    [Club speed] [Smash] [Path]
  ------------------   [Sidespin]     [Descent] [Roll]       [AoA]        [Face]  [Loft]
  Next shot starts ... +---------------------- Flight -----------------------+
  [New set][New sess]  | FLIGHT yd · ft     APEX 94 ft · CARRY 173 · TOTAL 185 |
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

A toolbar (the Data | Cameras switch when cameras are configured, in both
views, the Cards | Table switch, which Cameras hides, the device filter, the
export icon and Clear history) sits above both views. Only
the Table view's Data pane adds the "italic = simulated" legend, since only Table draws simulated
values in italics (`ShotsView::shows_simulated_legend`). The chosen view
(`ShotsView` in `panels/shots.rs`) lasts for the session and is not saved.

The **device filter** (`DEVICE [All | <device>…]`, its own group after the
units, styled like them) shows in both views, in every grouping, while the
session's shots come from two or more devices (`device_options`); each
device is labelled by `chart::device_label`. It lasts for the session
(`FlighthookApp::device_filter`) and falls back to All when its device
leaves the history. One filter applies to everything on the Shots tab,
through one predicate (`chart::in_filter`): the shot list and its section
counts, the shot on show, the Table's rows, the view's members (range bars,
stats and face impact cards, Flight, Dispersion) and the charts' faint
shots. An Auto-chip is no device, so a filter hides it like any other
device's shot. With All, the bars, Flight and Dispersion keep their device
colours and sub-bands. The JSONL/CSV export and the server are not
filtered: the export is always the whole session. Every cache that depends
on the filter holds it in its key: the members (`chart::MembersKey`, and so
the chart scenes) and the Table's rows (`FlighthookApp::filtered_rows`, on
the shots revision and the filter).

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
200 px under the Table. The switches' own row (30 px, in a panel narrower
than 312 px) raises the row by its height, so the plot keeps that minimum.

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

Under a device filter the list holds only that device's shots: a set or club
section counts only that device's shots in its header (`chart::set_shot_count`
over the rows the UI holds), one with none of them is not shown, and the
count line's total is the number of that device's shots. The latest shot is that device's newest with
data, so the shot on show, its header, LATEST and Replay are that device's;
a shot from another device never glows, takes the selection or moves the
shot on show. Picking a filter while a shot of another device is on show
moves to the filtered device's newest shot (without a glow); a shot of the
filtered device stays on show.

Under the count line, a selector picks the **view** (By set | By club |
None). It starts on the `shot_grouping` setting (default By set); a click
applies it at once and saves it as the default view, over the WebSocket,
else `POST /api/shots/grouping`. The view only changes what the tab shows:
sets are recorded the same way in every view. The list scrolls between the
selector and its **foot**, pinned at the bottom of the column: a hairline, a
one-line hint when there is one, then **New set** (`new_group`; disabled
while the open set has no shots) and **New session** (disabled while no set
is open). The hint reads "Next shot starts a new set (DR)" in By set when the
club in play differs from the open set's, and "No club selected" when no club
is in play. The view `none` has no foot.

**None** shows the last shot only: the list is a plain newest-first list with
no headers, and the cards have no range bars. **By set**
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
shows whole. New set does not
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

**Export** is an icon immediately left of the trash (`export_menu` in
`panels/shots.rs`: `theme::icon` with `Icon::Export`, Codicons `export`
U+EBAC, the same height as the trash, muted at rest and accent on hover,
hover text "Export session"). A click opens an `egui::Popup::menu` listing
**JSONL** and **CSV** (`EXPORT_FORMATS`); a pick opens
`GET /api/shots/export?format=…&units=…` in the shown units, the same URLs the
toolbar's links used. It is always enabled (an empty session exports an empty
file).

**Clear history** is the trash icon at the toolbar's right end: a Codicons glyph
(`theme::icon` with `Icon::Trash`; the
font is `ui/assets/fonts/codicon.ttf`, compiled in with `include_bytes!` and
registered as its own font family by `theme::install`), as tall as the
export icon, muted at rest and alert red on hover. It is disabled while
there are no shots. A click opens a modal: "Delete all N shots and sets?
This can't be undone. Export first if you want to keep them." with Cancel
and a red Delete; Escape or a click outside cancels. Delete sends
`clear_history` over the WebSocket, else `POST /api/shots/clear`. Every
dashboard that sees `clear_shot_history` on the WebSocket clears at once
(`FlighthookApp::clear_shot_history`): shots, sets (the session counts up),
the shown shot, the device filter, the caches and an open confirmation. The
Shots tab then shows its empty state.

**Replay** (beta): a shot whose row has `video` gets a **Replay** chip (a
video icon) on the Cards view's shot header line, after LATEST or Back to
latest, and the Table view a **Replay** column after Club, shown only while
some row has clips. Rows take `video` from `/api/shots`, from each
`shot_video` (every row whose `shot_id` is in its `keys`, whatever device
took it, before or after `shot_finished`; a camera's newer clip replaces its
older one) and lose it on `shot_video_removed`.

The chip opens the replay, an `egui::Modal` over the Shots tab
(`ui/src/panels/replay.rs`, logic in `ui/src/replay.rs`): the shot's cameras
side by side, each labelled with its card's name and position and followed
by a download icon (Codicons `download`, hover "Download"; the `.avi`); a scrubber with the start point as a green tick
and the estimated impact as a gold tick; play/pause; previous/next frame;
speed 1× · ½ · ¼ · ⅛; a Loop toggle (on whenever a replay opens, UI state only). Close, Escape
or the backdrop closes it, and so do Clear history and a
`shot_video_removed` naming its shot. Each camera's track
is its sidecar: frame `i` is at `start_ns + frames[i].t_ns` on the server's
clock, so the cameras line up, and each shows its last frame at or before
the playhead. It opens playing from `[replay] start_before_impact_s` before the
estimated impact (the trigger mark when the clip has no impact estimate, the
clip start when it has neither; never before the clip start). The playhead
holds there, shown as playing, until every camera that is loading normally
holds the frame at the start point (a camera whose clip or frames failed does
not count) or 1.5 s have passed since the first sidecar arrived; a pause,
scrub or frame step before then cancels the autoplay. With Loop on,
playback reaching the clip end jumps to that point and keeps playing at the
current speed; with it off it stops at the end and play restarts from that
point. A frame step moves to the next
(or previous) frame time of any camera and pauses. Frames come in HTTP range
requests of one window of consecutive frames each (100 ms behind to 400 ms
ahead of the frame, at most 8 MiB; while playing, the window 300 ms ahead
too; two requests per camera at most), kept in a 24 MiB byte cache per
camera. A failed request is asked again once; after a second failure its
error is drawn over that camera's picture whenever the playhead is in that
window (the rest of the clip still loads), and a clip whose sidecar failed
shows its error the same way. Only the frame on screen is decoded, at most
one per camera per screen update (`jpeg::ensure_huffman`, then zune-jpeg),
into a texture cache of 32 frames (8 on screens under 800 pt wide). The
playhead follows the wall clock, so when decoding can't keep up (1× on a
phone) frames are skipped and the clip keeps its time. The view never
fetches a whole clip.

<a id="auto-chip-beta"></a>**Auto-chip** (beta) is a menu chip (`theme::menu_chip`: a `chip_button`
with a drawn chevron) in the header, right of the club picker
(`panels/auto_chip.rs`; it is not part of the Shots tab, which shows live and
historical data rather than controls). It shows while `[simulation]
auto_chip` (default `false`, serde-omitted at the default; the Simulation
card's **Auto-chip** checkbox; `SimulationSection::auto_chip`) is on in the
*saved* settings, and not before they have loaded. It is off by default; turn
it on in Settings → Simulation (useful with radar launch monitors that miss
short chips). With it off the chip is not drawn and `POST /api/auto-chip` answers `409`. The header has room for
it down to the smallest window the UI fits, 1024 px wide. Its menu (`egui::Popup::menu`)
lists `1 Yard`, `3 Yard`, `5 Yard` (`flighthook::AUTO_CHIP_YARDS`); a pick
sends `POST /api/auto-chip` (`net::post_auto_chip`). The button is disabled,
with the reason on hover, while no `openconnect_client.*` actor is Connected
(`app::set_by_simulator`) and while a chip is waiting for its reply
(`auto_chip_in_flight`). A failed reply shows in the error banner above the
tabs (`Auto-chip: <status> <error>`) until the next pick. Auto-chip shots
already recorded display as before when the setting is turned off.

On the server (`app/src/autochip.rs`), `solve_speed` bisects the ball speed
(0.1-60 mph, to 1e-4 mph) at which dimple, with the `[simulation]`
environment and surface and `BallModel::TOUR`, rests at the chosen total
for a straight 28° launch with backspin `round(DEFAULT_LAUNCH_ONLY_SPIN ×
mph)` and no sidespin; the total rises with speed. `send` emits
`ShotTrigger`, `sim::simulated_ball_flight_event(.., Some(Chipping))`
(simulated whatever `[simulation] mode`, `off` included) and `ShotFinished`
as actor `autochip` (`flighthook::AUTO_CHIP_ACTOR`), keyed by a fresh UUID
and the next `WebState::auto_chip_shots`, from `spawn_blocking`; the counter
moves only once the solve has succeeded. `shot_context` stamps an Auto-chip's
`detection_mode` Chipping, whatever the game mode. The UI labels the actor
`Auto-chip` (`chart::device_label`); it has no config section and sends no
status, so it gets no Devices card. The Open Connect client's routing check
(`shot_matches_routing`) takes `autochip` shots in every mode whatever
monitor is routed: the chip stands in for a read the routed monitor missed.
The client's `Relay::next_shot` sends `max(shot_number, last sent + 1)` as
the `ShotNumber`, so device and Auto-chip numbers never repeat.

The server sets count an Auto-chip like any shot. The UI keeps it out of
every statistic through one predicate, `chart::is_auto_chip`: it is never a
view member (`chart::members_for`), so the range bars, stats and face impact cards,
dispersion ellipse and flight set overlay leave it out; it is not in
`chart::device_ids`, so it has no device filter option, band or colour; and
list and range-bar headers count a set as `chart::set_shot_count` (the
set's `shots` less its Auto-chip rows). It stays in the shot list under its
set, in the Table and on the Cards; shown, it is the charts' gold shot
(`flight::drawn_sets`) but not part of the ellipse.

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

The view `none` tracks only the last shot, so it has no range bars. In By
set and By club, three **range bars** sit between the
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
title (`legend_place`), so the two never overlap. The device filter (in the
toolbar, above) applies before the view; with All and two or more devices,
each bar's band splits into one sub-band per device, coloured by device. The
members are cached on `chart::MembersKey` (shot and set revisions, view,
device filter, shown shot, club in play) by
`FlighthookApp::refresh_members`, which the bars, the stats and face impact
cards, Flight and Dispersion all take them from. Each bar is
drawn on a track, painted in this order: the set's range as a translucent
accent band with its min and max beside it; the median as a hollow white
ring sitting on the track's centre line, its radius just inside the track's
height; the average as a filled gold dot (full-strength `theme::GOLD`),
sized to sit inside the ring (filling it when the two coincide), with a thin
dark halo so it reads against the band; and, on top of everything, the
shown shot as a thin gold line (`theme::GOLD`, with a thin dark halo of its
own) that sticks out past the track, top and bottom. A value shared by
every marker therefore reads as a white ring filled with a gold dot, with a
gold line through it poking out above and below. Total and Carry share one
domain, the set's extent padded by 15 % of its range and at least 10 yd (9 m) a side,
so roll reads as the offset between the bands; the track's ends carry no
numbers (only the band's own min and max are labelled). Lateral is symmetric
about the target line (a tick at 0) out to 1.2× the widest value, and at least ±6
units. A single shot draws a 16 px pip, wide enough that it still peeks out
past the ring and dot. Total and Carry are picked like the cards' values
(below), and the stats run over the picked values;
Lateral is the carry offline, picked the same way, and prints with its
side: `3.2 L`, `1.4 R` (`0.0` on the line; L and R are the same for both
hands). The bars carry no simulated marks; that distinction is the cards'
`SIM` tags and Table's italics. The stats card (below) holds each bar's
average, median and spread. Hovering a track lists n, min, max, avg, median and sd. The stats
come from `flighthook::Stats` over the rows the UI holds (the server's whole
history, up to the last 1000 shots, plus live ones), so for a session longer
than that they can cover fewer shots than the header's count.

**The card row.** Right of the range bars, 16 pt from them, sit one to
three cards (only with the bars, so not in the view None). Each is one
headline column wide, under the rightmost headline columns and flush right,
12 pt apart like the headline cards so their edges line up, and runs from
the headline cards to the hairline of the first detail section's label with
the same 12 pt gap at both ends. The bars and their legend take the rest,
and no height changes. There are as many cards as there are
sources, at most three, while the bars and the gap keep the two leftmost
headline columns and a plot of at least 170 pt (`card_columns`); always at
least one. When not even one column-wide card fits, one card narrows to a
square as tall as the bars' title line to their last bar, and when that
still leaves the plot under 170 pt the bars keep 170 pt and the card takes
what is left; under a point of it, the card is not drawn and the bars run
the full width. The legend goes on its own row when the bars' line can't hold
it beside the title (`settle_legend`).

The sources, in cycle order (`ui/src/panels/card_row.rs`): **Stats**,
always (below). **Camera** (beta), one per camera actor that is
Connected, in Devices card order (below). **Face impact**, while any shot in
the history has face impact on either axis. By default the leftmost card is
Stats, face impact is the rightmost, and cameras fill the cards between in
order. The first time face impact is available in the session (and again
after Clear history) it takes the rightmost card, as a new card when one
fits, else in place of what was there; after that the user's choices stand.
A click on a card (except on a face impact dot, which selects its shot)
moves it on to the next source in cycle order that no other card shows,
wrapping round, and does nothing when every source is on screen; the cursor
is a pointer over the cards, with no hover text. A camera that disconnects
frees its card (it takes the first source not on screen other than face
impact, or the row loses a card; when face impact is all that is left, it
takes the rightmost card, so it never moves into the middle); one that
connects takes a new card when one fits, left of face
impact, else it is a click away. When a resize leaves fewer cards they go
from the right, but face impact keeps the rightmost card when it was on
screen. Nothing shows on two cards at once. The choices are UI state for
the session (`CardRow`).

The **stats card** (`stats_card` in `panels/shots.rs`) reads on its own,
wherever it sits in the row. `STATS` sits in its top-left in the headline
cards' label style. Under it, a table: column headers `AVG`, `MED`, `SD` in
small muted capitals, then one row per bar in bar order, named like the bars
(`TOTAL`, `CARRY`, `LATERAL`) with the unit in small muted text. The values
are printed like the bars' (`StatsRow`): average and median as whole yards
or metres, lateral to a tenth with its side (`3.2 L`), the standard
deviation to a tenth; a value the row lacks is an em dash (every column
when the bar has no values, the SD with only one), and when only k of the n
shots with data have the value, a small muted `k/n` ends the row. With room
(`stats_table`), each row's name and unit stack in a column left of the
values and the `k/n` sits under the SD; in a narrow card each row's name,
unit and `k/n` share a line over its values. The values start at 13 pt and
shrink to fit the card before anything is cut (to 11 pt with the names
beside, then to 8 pt with the names over them), so every listed window
shows the whole table. The rows sit on the bars' rows when the table fits
there under the title, else they spread evenly down the card.

A **camera card** (beta, `ui/src/panels/replay.rs` `CardClip`) shows the
camera's name (its `[camera.N]` `name`) in capitals in its top-left (the face impact readout's
font and size) and, under it, the shown shot's clip from that camera:
playing at 1×, looping from the replay's start point (`[replay]
start_before_impact_s` before the impact, `replay::start_point`) to the end
of the last frame, starting once the frame at the start point is in. Each
frame is cut to the card body's aspect about its centre (no letterbox) and
scaled by box averaging (nearest when scaling up) to the body's pixel size
(`ctx.pixels_per_point()`, each side rounded up to an even number). Nothing else is drawn on the
picture. Without a clip of the shown shot from that camera (still recording,
an older shot, a discarded recording) the card shows its name over an empty
body and fetches nothing; the clip starts when it arrives (`shot_video`, the
row's `video`). It fetches like the replay view: the sidecar, then range
windows around the playhead and 300 ms ahead (from the start point again
past the end), two requests at most, into a 24 MiB byte cache. Each frame is
decoded once and kept as a texture while the card shows that shot's clip, as
long as the whole loop fits 48 MiB of textures; past that the card keeps the
replay view's 32 most recent frames. The camera cards share one decode per
screen update between them (`FlighthookApp::decode_card_frame`: the card
that has waited longest for one goes first). While the whole loop is kept,
a card decodes the first frame not decoded yet from the playhead on, in loop
order, so its loop fills in ahead of the playhead; otherwise the frame at
the playhead. A decoded frame shows on the next screen update. A new shown
shot, another camera on the card, Clear history or a new card size drops
what it holds (a new size also the picture on screen). The card row holds
clips only while it is on screen: the Table view, the view None, another tab
and the replay view of the shown shot drop every card's clip, and the cards
fetch and decode again when the row returns. While it plays a card asks for
a repaint when the next frame is due (`request_repaint_after`), never sooner
than 1/60 s, so a faster camera skips frames to keep its time; it asks none
while the replay view is open. A click cycles the card; the shot header's
Replay chip opens the replay.

**Face impact** (`ui/src/panels/impact.rs`) is a source of the card row. It draws the shown
shot's club face, face-on as the ball sees it: a right-hander's toe on the
left and hosel on the right, a left-hander's mirrored (the shot's
handedness, else the game state's, else right). Face impact itself is
golfer-relative and never mirrored. The outline is one of six generic
families by club (driver and mini driver; fairway woods; hybrids; irons
1I-9I, PW and an unknown club; wedges GW, SW, LW; putter), each a closed
chain of cubic Béziers in face millimetres with the origin at the face
centre (the midpoint of both chords through it). Each family is scaled on
its own to fill the panel (`FaceView::fit`, the one place the scale is set:
the whole silhouette, hosel included, as large as fits 94 % of the plot's
width and 86 % of its height, centred), so the families are not drawn to
one scale: a wood and a hybrid look about the same size. The cubics are
flattened in screen space by Wang's formula to within 0.1 pt, and the face
is filled with a triangle mesh (ear clipping, built once per family and
scale, since egui fills only convex paths) with the outline stroked over it.
Inside: a dashed crosshair at the face centre; the view's other members
(the bars' members, after the device filter) as small dots, accent or in
device colours by the same rule as the Flight and Dispersion charts; the
shown shot as a gold dot, or a gold line across the face when only one
axis was read. The shown shot
is marked only when it is a member, as on the bars; otherwise there is no
gold mark and the readout is `-`.
A mark off the face is drawn as a ring, pulled into the panel when it is
off it. The readout above the face reads `4 toe · 2 low` in whole
millimetres (`-` when the shown shot has no reading), 12.5 pt, shrinking to
10 pt in a narrow panel and cut short with an ellipsis only if it is still
too wide there; TOE, HEEL and mm sit under
the face's ends. Hovering a dot shows its shot (`#5282 · 7I`, the device
with two or more) and `4 mm toe · 2 mm low`; a click shows that shot, as
on the charts.

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
  apex and dots at its landing and rest. Its apex (gold, with its unit),
  carry and total are labelled at 14 pt on a background box, each centred
  above the point it labels (`flight::place_marks`): carry and total 7 pt
  above the ground line, the apex over its tick. When the carry and total
  boxes would come within 6 pt of each other (a short roll, or a wedge that
  spins back), total stays on its dot and carry lifts one row; an apex box
  that would meet either lifts above the higher of them; each box stays
  inside the plot's width. The height axis keeps 35 pt above the gold apex
  for its label, raised past the nice domain only when needed, its ticks
  still on the step. It is drawn only when it is a member, as on the bars.
  The header repeats its apex, carry and total.
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
built around the latest shot of the filtered device (its rows cannot be
picked). One covariance ellipse covers the
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
one row per shot (under a device filter, per shot of that device; the
latest row is that device's newest), with a Club column after `#`, built with
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
shot was triggered (`-` when none), with the shot's set number on hover (`Set 3`).
After Smash, **Impact H** and **Impact V** give face impact in whole
millimetres with a side word (`4 toe`, `6 heel`, `2 high`, `5 low`; `0`
under half a millimetre; `-` without a reading), the same in both unit
systems. Both views show a "waiting for the first shot" message
until there is one.

A metric card shows the simulated value upright with a `SIM` tag and hover,
else the measured one upright, else a dash. Directional cards show the
value's magnitude with a word for its sign after the unit
(`ui/src/panels/signs.rs`): Direction and Offline `L` / `R`; Sidespin
`Draw` / `Fade`; Path `In-to-out` / `Out-to-in`; AoA `Up` / `Down`; Face
`Open` / `Closed`. Lateral values are absolute (+ right of the target line
for every golfer), so the shape words follow the shot's handedness: + face
is open and + path in-to-out for a right-hander, closed and out-to-in for a
left-hander; + sidespin curves the ball right, a right-hander's fade and a
left-hander's draw. A value shown as 0 has no word. Roll keeps its sign.
The word starts at the unit's size and shrinks (to 9 pt) to fit its card;
only when that is not enough does that card's numeral shrink. The Table
keeps signed numbers. A table cell shows the simulated
value in italics with the same hover, else the measured one plainly, else a
dash. Backspin and Sidespin (card and column) always show the `ball` value,
styled as simulated (`SIM` tag, italics) when `derived.spin_estimated` is
set. Derived wins when both are present: the server sets a field in both
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
