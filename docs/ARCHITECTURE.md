# Flighthook Architecture

A bridge between launch monitors, games and simulators, cameras and a web or
native dashboard. Inside, every launch monitor, output, game and the flight
model is a Flight Relay Protocol (FRP) 0.3.0 actor on one internal bus.
flighthook itself is one more actor: it plays the session (the turn), the
practice (the Sets) and the collector (the shot store, the dashboard, the
export) roles.

## Config Model

Type-prefixed config sections. The section prefix encodes the component type;
no `type` discriminator field needed. Section keys are integer indices starting
at `"0"`.

```toml
default_units = "imperial"
selected_golfer = "flighthook.5b0c4d2e-0000-4000-8000-000000000001"  # the golfer picked last

[frp]
actor_id = "..."             # flighthook's own actor, generated once
auto_chip_actor_id = "..."   # the Auto-Chip launch monitor

[simulation]
actor_id = "..."             # the Simulation card's flight model actor

[[golfers]]
identifiers = ["flighthook.5b0c4d2e-0000-4000-8000-000000000001", "pureplay.thill"]
name = "Eric"
handed = "rh"

[webserver.0]
name = "Web Server"
bind = "0.0.0.0:5880"

[mevo.0]
name = "Mevo WiFi"
actor_id = "..."             # every card that is an FRP actor has one
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
# full_monitor = "..."          # optional: the actor id of the launch monitor routed for full swings
# chipping_monitor = "..."      # optional: the same for chipping shots
# putting_monitor = "..."       # optional: the same for putts
# chipping_clubs = ["GW", "SW", "LW"]   # clubs the simulator holds for a chip (default)
# putting_clubs = ["PT"]                # clubs the simulator holds for a putt (default)
```

- `[frp]` -- flighthook's own FRP actors: `actor_id` (the session, practice and
  collector actor) and `auto_chip_actor_id` (the Auto-Chip launch monitor).
  Generated at the first start and kept, so flighthook is the same actor
  across restarts; omitted while empty
- `selected_golfer` -- the `flighthook.<uuid>` id of the golfer picked last in
  flighthook; the golfer up at start. Omitted while unset
- `[[golfers]]` (beta) -- the golfers, in display order (see
  [Golfers](#golfers-beta)); omitted while empty
- `[simulation]` -- the flight model (the Simulation card): `mode`, `altitude`,
  `temperature_c`, `surface`, `auto_chip`, `tournament` and the flight model's
  `actor_id`; each field omitted at its default
- `actor_id` -- every launch monitor, output and Pureplay Accessory section
  carries the card's FRP actor id, generated once and kept (a card's id goes
  when the card is removed). Cameras and the web server are not FRP actors
- `[mevo.<idx>]` -- FlightScope device instance (Mevo/Mevo+, or Xi — detected at connect)
- `[r10.<idx>]` -- Garmin R10 BLE device instance (auto-discovers via Bluetooth)
- `[square.<idx>]` -- Square Golf Omni BLE device instance
- `[exputt.<idx>]` -- ExPutt RG putting device over WiFi (optional IP address;
  found on the local network when absent)
- `[trackball.<idx>]` -- Trackball (alpha): a trackball or mouse on Linux
  turned into a launch monitor (thumbswing's driver; its settings `device`,
  `club`, `hand`, `cpi`, `report_limit` and the `thresholds` and `tuning`
  tables sit in the same table; a `grab` key is ignored, see "Trackball
  card")
- `[openconnect_server.<idx>]` -- Open Connect server: GSPro Open Connect ingest listener (launch monitor software that sends Open Connect)
- `[mock_monitor.<idx>]` -- mock launch monitor instance (flightrelay's mock
  driver; its settings `shot_interval_s`, `arm_delay_s`, `ball_ready_delay_s`,
  `post_shot_delay_s` and `serial` sit in the same table)
- `[openconnect_client.<idx>]` -- Open Connect client: dials a simulator that
  accepts GSPro Open Connect (GSPro and Pureplay on 921, Muni Golf on 9210).
  `chipping_clubs` (default GW, SW, LW) and `putting_clubs` (default PT) are
  the clubs the simulator holds for a chip and a putt: the card sets the shot
  type from the simulator's club by them, and sends no shot type when both are
  empty. `[gspro.<idx>]`
  sections are read as `[openconnect_client.<idx>]`; when both name an index,
  `openconnect_client` is used and a warning is logged. Saves write only
  `[openconnect_client.<idx>]`, and the file is rewritten that way on the next
  start (see [Loading and migration](#loading-and-migration))
- `[pureplay.<idx>]` -- Pureplay output (alpha): dials Pureplay (default
  `127.0.0.1:9760`) over Pureplay Connect; same routing keys as
  `openconnect_client` (see [Pureplay output](#pureplay-output-alpha))
- `[pureplay_accessory.<idx>]` -- Pureplay Accessory card (beta), under
  Software: dials Pureplay's Accessory API (default `127.0.0.1:9770`),
  takes Pureplay's club and hand, its local players' shots (`shots`:
  `auto` | `from_pureplay` | `from_flighthook`) and its results;
  `impact_lead_s` as other launch monitors; the pairing token kept outside
  the config (see [Pureplay Accessory card](#pureplay-accessory-card-beta))
- `[random_club.<idx>]` -- random club cycling integration instance
- `[camera.<idx>]` -- swing camera (beta): records a clip of every shot (see
  [Cameras and replay](#cameras-and-replay))
- `[webserver.<idx>]` -- web server instance
- `[replay]` -- global camera clip settings (beta: `keep_shots`, `pre_roll_s`, `post_roll_s`, `start_before_impact_s`, `clip_dir`, `shot_cards`), omitted at its defaults
  like `[simulation]`
- `[body_tracking]` -- body tracking on the cameras' pictures (alpha:
  `mode`, `model`, `min_score`, `smoothing`, `detect_every`,
  `show_skeleton`), each field
  omitted at its default and the section with them (see
  [Body tracking](#body-tracking-alpha))
- A golfer's `sex` (`male` default | `female`) and `height` (cm, 100 to 250,
  unset by default) feed the swing analysis (see
  [Body tracking](#body-tracking-alpha)); they sit on the golfer's record
- `name` is **required** -- the user-visible name, editable (rename) in settings UI
- `enabled` is on every launch monitor, camera and output section (not
  `[webserver.<idx>]`): default `true`, omitted from the config then, so a
  file written before the field existed loads and saves byte-identical;
  `enabled = false` turns the card off. `FlighthookConfig::disabled_actors`
  lists those sections and `actor_enabled(id)` asks for one;
  `resolve_actors` leaves them out, so no actor runs for them (see
  [Turning a card off](#turning-a-card-off))
- FlightScope sections (`MevoSection`) are the card's own fields (`name`,
  `enabled`, `impact_lead_s`, `launch_only_spin`) with ironsight's driver
  settings flattened next to them (`MevoConfig`: `address`, `ball_type`,
  `tee_height`, `range`, `surface_height`, `track_pct`, `use_estimated`,
  `camera_mode`, `environment`, `spin_source`); each driver setting is left
  out of the file while unset (`spin_source` while `d4`), and the driver uses
  its own default then. The driver turns them into the device's
  configuration: on a Mevo+ or Gen2 the radar settings, on an Xi `range` and
  `surface_height` as its radar calibration (`ball_type`, `tee_height`,
  `track_pct` and `camera_mode` do not apply to it). Full selects a
  full-swing session in the environment's mode; on the Xi, Chipping and
  Putting select the same pitching session, so a change between them sends
  nothing. The driver's `validate()` refuses `track_pct` outside 0 to 100, a
  negative distance, a `ball_type` other than 0 or 1 and an `address` that is
  not an IP address and port
- `use_estimated` (default off) -- whether a shot whose full result never
  arrives is sent from the device's early result; with it off, such a shot
  (a flight too short to read in Outdoor mode, a very short chip) is skipped
- `environment` (`limited_flight` default / `outdoor`) -- picks the
  full-swing detection mode, on a Mevo+ and an Xi alike
- `camera_mode` (`standard` default / `fusion` / `raw_fusion`) -- the Fusion
  modes request the full club data set (radar club data arrives in every
  mode), applied after the camera's warmup
- `spin_source` (`d4` default / `e8` / `auto`), FlightScope Xi only -- `d4`
  keeps the full result's spin (D4, what FS Golf shows); `e8` takes the
  total from the early result (split into back and side by D4's axis with
  club data, else sent as the total alone) and keeps D4's when the early
  result has no spin; `auto` applies the `e8` rule when the detection mode
  at the shot's trigger is Full, and `d4` otherwise -- a chip hit while still
  in Full mode gets `e8`'s value. Applied live (the driver's `Live` cell), so
  a change takes effect without a restart; any other driver setting restarts
  the card
- `launch_only_spin` (the card's, rpm per mph, default `100`, `0` = off,
  valid `0..=MevoSection::MAX_LAUNCH_ONLY_SPIN` (1000); a value outside that
  in the file is treated as `0` with a warning at actor start, and
  `POST /api/settings` rejects one with `400`) -- the card's estimate for a
  launch-only chip (the **FlightScope card** paragraph under
  [Reconnect](#reconnect)). Applied live (a host field)
- R10 sections show name, Monitor-to-Ball (`range`, sent to the device as
  its tee distance; BLE auto-discovery, no address field) and Aim offset
  (`aim_offset_deg`, beta: degrees added to the launch direction and to a
  tracked club's face angle and path; blank is 0; applied from the next shot
  without a restart). Both are the R10 driver's settings (tenover's `Config`,
  flattened into the section); a range that isn't greater than 0, or an aim
  offset outside -10 to 10, is refused (the card shows why and Save stays
  off; a save is `400`; from the file, the card does not start)
- ExPutt sections show name and an optional IP Address (blank finds the
  device on the network); the putting hand follows the game's handedness.
  `address` is the ExPutt driver's setting (threeputt's `Config`, flattened
  into the section); one that is not an IP address is refused (the card
  shows why and Save stays off; a save is `400`; from the file, the card
  does not start)
- Trackball sections show name and every setting of the trackball driver
  (thumbswing's `Settings`, flattened into the section): Device (`device`,
  blank for the only trackball or mouse), Starting club and hand, CPI,
  Report limit, the tuning (full power speed, max club and putt
  speeds, the face, putt line and curve gains, backswing power, power curve,
  max face angle and face-to-path, the curve sense, mirror for left-handed
  players), and the swing detection thresholds in a collapsed section. A
  blank number is the driver's default. A value the driver refuses (a CPI
  or speed of 0, a stop speed not below the start speed, a backswing power
  over 1, ...) or a number that isn't one is refused (the card shows why
  and Save stays off; a save is `400`; from the file, the card does not
  start). No Camera timing: the cameras cut no clip for its shots
- Mock sections show only name and Camera timing; the mock driver's settings
  are set in the file or through the API
- Global IDs = `"{type_prefix}.{index}"` (e.g. `mevo.0`, `openconnect_client.0`)
- Actor IDs (FRP) = the `actor_id` of the section, a generated identifier kept
  across restarts; the routing fields of an output (`full_monitor`,
  `chipping_monitor`, `putting_monitor`) hold the launch monitor's actor id.
  An id that names no card is kept as written, routes nothing, and the picker
  shows it as "Missing monitor" until another launch monitor is picked

### Loading and migration

`state::config::load()` reads the file once at startup (`SystemState::new`).
A missing file is created and written at once, with the defaults and the
generated actor ids, so flighthook's actors keep their ids from the first start
on. The text is parsed as the current
format (which also reads `[gspro.<idx>]` as `[openconnect_client.<idx>]`), or
failing that as the singleton-`[webserver]` format, which loads as
`[webserver.0]` (named "Web Server" when unnamed).

A file written by an earlier version is read as follows, then rewritten once
(below):

- Every actor that has no `actor_id`, or whose id an earlier actor holds,
  gets a generated one: `[frp] actor_id`, `auto_chip_actor_id`,
  `[simulation] actor_id` and the `actor_id` of every launch monitor, output
  and Pureplay Accessory section.
- An output's routing (`full_monitor`, `chipping_monitor`, `putting_monitor`)
  that names a card by its key (`mevo.0`) names its actor id from then on. A
  key that names no card stays as it is and routes nothing.
- The global `chipping_clubs` and `putting_clubs` lists are copied into every
  Open Connect client section and then left out.
- `[player]` (sex and height) moves onto the golfers: the first golfer takes
  them where it has none; with no golfer, one named "Golfer" is made with
  them. A `golfer_height` in `[body_tracking]` is read as the height when the
  player has none.
- A golfer without a `flighthook.*` identifier gets one. An identifier two
  golfers list stays with the first in file order (a warning names the
  other).

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
read: a deliberate one-time normalization. The rewritten text is the save
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
shows a setup wizard instead of the normal dashboard, once `/api/status` has
said whether the server reads a trackball. The wizard lists the same presets as
the Settings sections' (+) menus (`cards::welcome_options`): every launch monitor
(the Open Connect Server included), the Trackball only where the server
reads one, the Pureplay Accessory card (a Software card, listed with the
launch monitors as a way to get shots), and every output (GSPro, Pureplay
and Muni Golf over Open Connect). No cameras. GSPro and Pureplay over Open
Connect add the same connection (an Open Connect client at `127.0.0.1:921`), so
checking one unchecks the other. On confirmation it adds each checked preset as
its (+) menu entry does (`SettingsForm::add_preset`), posts the whole config via
the existing settings save path, and dismisses. Users can also skip the wizard
and configure manually in Settings.

### Rust types

**lib/src/config/** (shared config types, used by both app and UI):

```rust
pub struct FlighthookConfig {
    pub default_units: UnitSystem,
    pub frp: FrpSection,                   // [frp]: flighthook's own actor ids
    pub selected_golfer: Option<String>,   // the golfer picked last
    pub shot_grouping: ShotGrouping,       // default Shots view
    pub golfers: Vec<GolferRecord>,        // [[golfers]], display order
    pub simulation: SimulationSection,     // [simulation], omitted at defaults
    pub replay: ReplaySection,             // [replay], omitted at defaults
    pub body_tracking: BodyTrackingSection, // [body_tracking], omitted at defaults
    pub webserver: HashMap<String, WebserverSection>,
    pub mevo: HashMap<String, MevoSection>,
    pub r10: HashMap<String, R10Section>,
    pub square: HashMap<String, SquareSection>,
    pub exputt: HashMap<String, ExPuttSection>,
    pub trackball: HashMap<String, TrackballSection>,
    pub mock_monitor: HashMap<String, MockMonitorSection>,
    pub openconnect_server: HashMap<String, OpenConnectServerSection>,
    pub openconnect_client: HashMap<String, OpenConnectClientSection>,  // `gspro` also read
    pub pureplay: HashMap<String, PureplaySection>,  // name, address, full/chipping/putting_monitor
    pub pureplay_accessory: HashMap<String, PureplayAccessorySection>,
    pub random_club: HashMap<String, RandomClubSection>,
    pub camera: HashMap<String, CameraSection>,
}

pub struct ReplaySection { pub keep_shots: u32, pub pre_roll_s: f64, pub post_roll_s: f64, pub start_before_impact_s: f64, pub clip_dir: Option<String>, pub shot_cards: bool }  // 20, 4.0, 1.5, 2.0, None, true (written only when false)
pub struct CameraSection { pub name: String, pub camera: String, pub camera_name: Option<String>, pub position: CameraPosition, pub mode: String }  // mode "auto" | "WxH@fps"
pub enum CameraPosition { TargetLine, GolferFront, GolferBack, Overhead }  // default GolferFront
pub struct SimulationSection { pub mode: SimMode, pub altitude: Distance, pub temperature_c: f64, pub surface: SimSurface, pub auto_chip: bool, pub tournament: bool, pub actor_id: Option<String> }  // auto_chip and tournament default false; tournament is written only by POST /api/tournament
pub struct GolferRecord { pub identifiers: Vec<String>, pub name: String, pub handed: Option<Handedness>, pub bag: Vec<Club>, pub sex: Option<Sex>, pub height: Option<f64> }  // identifiers: own `flighthook.<uuid>` first, then game ids
pub enum SimMode { Off, On, Force }                 // default On
pub enum SimSurface { Fairway, Green, FirmGreen }
pub struct WebserverSection { pub name: String, pub bind: String }
pub struct MevoSection { pub name: String, pub enabled: bool, pub launch_only_spin: f64, pub impact_lead_s: Option<f64>, #[serde(flatten)] pub driver: MevoConfig }  // ironsight's Config: address, ball_type, tee_height, range, surface_height, track_pct, use_estimated, camera_mode: Option<CameraMode>, environment: Option<FlightScopeEnvironment> (each Option), spin_source: SpinSource
pub enum CameraMode { Standard, Fusion, RawFusion }               // ironsight's, re-exported
pub enum FlightScopeEnvironment { LimitedFlight, Outdoor }        // ironsight's, re-exported
pub enum SpinSource { D4, E8, Auto }                              // ironsight's, re-exported
pub struct R10Section { pub name: String, pub enabled: bool, pub impact_lead_s: Option<f64>, #[serde(flatten)] pub driver: R10Config }  // tenover's Config: range: Option<Distance>, aim_offset_deg: Option<f64>
pub struct SquareSection { pub name: String, pub enabled: bool, pub impact_lead_s: Option<f64>, #[serde(flatten)] pub driver: SquareConfig }  // allsquare's Config: address: Option<String>, club: String ("7I"), advanced_spin: bool (true), discard_non_putting_zero_spin: bool (true), dot_bottom_to_face_center_mm: BTreeMap<String, f64>
pub struct ExPuttSection { pub name: String, pub enabled: bool, pub impact_lead_s: Option<f64>, #[serde(flatten)] pub driver: ExPuttConfig }  // threeputt's Config: address: Option<String>
pub struct TrackballSection { pub name: String, pub enabled: bool, pub driver: TrackballConfig }  // serde via a flat table (thumbswing's Settings flattened): device: Option<String>, club, hand, cpi, report_limit, thresholds, tuning, each left out at its default; driver.config.grab is always false, a `grab` key is read and ignored, none is written; DeviceSection::driver_settings() hands the driver grab = false
pub struct MockMonitorSection { pub name: String, pub enabled: bool, pub impact_lead_s: Option<f64>, #[serde(flatten)] pub driver: MockConfig }  // flightrelay's MockConfig
pub struct OpenConnectServerSection { pub name: String, pub bind: Option<String> }
pub struct OpenConnectClientSection { pub name: String, pub enabled: bool, pub actor_id: Option<String>, pub address: Option<String>, pub full_monitor: Option<String>, pub chipping_monitor: Option<String>, pub putting_monitor: Option<String>, pub chipping_clubs: Vec<Club>, pub putting_clubs: Vec<Club> }  // the monitors are actor ids
pub struct RandomClubSection { pub name: String }
```

**lib/src/api.rs** (shared REST API types, used by both app and UI):

```rust
pub struct StatusResponse { pub cards: HashMap<String, ActorStatusResponse>, pub actors: HashMap<String, StatusActor>, pub turn: Option<GolferTurn>, pub cameras_supported: bool, pub trackball_supported: bool, pub clip_dir: Option<String>, pub config_error: Option<ConfigLoadError>, pub round: RoundState }  // cards by card key, actors by FRP actor id
pub struct ActorStatusResponse {
    pub name: String, pub status: ActorStatus, pub telemetry: HashMap<String, String>,
    pub state: ConnectionState, pub red_for_ms: u64,  // derived, see "Connection state" below
    /* private, not serialized: StatusHistory */
}
pub struct PostSettingsResponse { pub restarted: Vec<String>, pub stopped: Vec<String>, pub golfers: Vec<GolferRecord> }
```

**app/src/actors/mod.rs** (actor resolution from config):

```rust
pub struct ResolvedActor {
    pub id: String,             // card key (e.g. "mevo.0", "r10.0", "openconnect_client.0")
    pub name: String,           // user-visible name
    pub actor: Box<dyn Actor>,  // concrete actor ready to start
}

pub fn resolve_actors(config: &FlighthookConfig) -> Vec<ResolvedActor>;
pub fn start_actor(id: String, actor: Box<dyn Actor>, state: &Arc<SystemState>, bus_tx: &broadcast::Sender<BusMsg>);
pub fn actor_names(config: &FlighthookConfig) -> HashMap<String, String>;
```

`resolve_actors` lists the web servers first, and `main` starts actors in
that order. A web server subscribes to the bus in `start`, before it returns,
so the first status of every actor started after it reaches `/api/status`.

Helper functions: `global_id(prefix, index) -> String` builds `"{prefix}.{index}"`,
`load(path)` / `save_to(path, config)` for TOML I/O.

## Actors and the internal bus

Every component talks over one `broadcast` channel (capacity 1024) that
carries two kinds of message (`app/src/bus.rs`):

```rust
pub enum BusMsg {
    /// An FRP message: from a local actor (`peer: None`) or from a hub peer.
    /// `cause` is set only on the session's own `golfer.turn`: the actor id
    /// whose `golfer.select` produced it. Internal; never on the wire.
    Frp { msg: FrpMessage, peer: Option<PeerId>, cause: Option<String> },
    /// flighthook's own event. `card` is the card key ("mevo.0") or "system" / "web".
    Local { card: String, event: LocalEvent },
}
```

`FrpMessage` is the flightrelay SDK's message (`from`, `body`); a sender's
`BusSender` stamps `from` with the card's actor id. `peer` exists only for the
hub rule "never back to the connection it came from". Producers publish;
consumers subscribe and filter.

### Actors

| Actor | Name | Actor id kept in | FRP roles |
|---|---|---|---|
| flighthook | `flighthook` | `[frp] actor_id` | `session`, `practice`, `collector`; issues the golfer namespace `flighthook` |
| Simulation card | `Simulation` | `[simulation] actor_id` | `flight_model` (off the bus while `[simulation] mode = off`) |
| Each launch monitor card | the card's name | the card's section | `launch_monitor`, with `device.source` |
| Auto-Chip | `Auto-Chip` | `[frp] auto_chip_actor_id` | `launch_monitor`, source `virtual` |
| Each output card (Open Connect client, Pureplay Connect, Random Club) | the card's name | the card's section | none: they send `golfer.select` |
| Pureplay Accessory card | the card's name | the card's section | `game` (and `launch_monitor` when its Shots setting ingests), namespace `pureplay`, source `virtual` |

Cameras are not FRP actors. Every local actor publishes its `bus.actor` every
2 s on the internal bus (one timer), so the hub forwards it to peers and the
dashboard's state sees it; local cards never expire.

### FRP messages flighthook uses

| Message | Sent by | Meaning |
|---|---|---|
| `shot.trigger`, `shot.launch`, `shot.flight`, `shot.club`, `shot.impact`, `shot.finished` | launch monitors; the flight model sends `shot.flight` | One shot's life. `shot.launch` holds what the device measured only |
| `bus.actor`, `bus.alert` | every actor | Presence, blockers (readiness), device identity, battery, telemetry; alerts |
| `golfer.select` | the dashboard, REST, simulator cards, random club, FRP peers | A request to change golfer, club, hand or mode |
| `golfer.turn` | flighthook (session) | The current turn: golfer, club, hand, mode |
| `golfer.profile` | flighthook (saved golfers), the Pureplay Accessory card (players) | A golfer's name, hand, bag, sex, height |
| `practice.block`, `practice.new_block` | flighthook (practice) / requesters | Sets (see [Sets](#sets)) |
| `game.round`, `game.ball`, `game.scores`, `game.stroke` | the Pureplay Accessory card | The round, ball lie, scores and each stroke |

### Local events

`LocalEvent` never leaves flighthook:

| Event | Meaning |
|---|---|
| `ConfigCommand` / `ConfigOutcome` | Configuration changes and their result (see below) |
| `CardStatus` | A card's lifecycle (`Starting`, `Connected`, ...) and its display rows (error, address, device label) |
| `Alert` | An alert of something that is no FRP actor (a camera, the web server) |
| `RoundState` | The tournament round started or ended |
| `ClearShotHistory` | Delete the shot history |
| `FindMat` | The ExPutt card locates its mat again |
| `ShotVideo`, `ShotVideoRemoved`, `ShotPose`, `ShotSwing` | Camera clips and their analysis (beta, alpha) |
| `ShotCapture` | A card's raw frames of a shot, for the export |
| `ShotPath` | The flight model's ball path (FRP has no path field) |
| `ActorGone` | A remote actor missed its heartbeats and left |

### Hub and remote actors

`/frp` on the web server port is a plain FRP 0.3.0 hub. A peer that connects
gets every actor's `bus.actor` and state, then a `bus.sync`; after that every
message is forwarded to every other peer and onto the internal bus, never back
to its source (`shot.raw` is never sent). flighthook answers a `bus.sync` by
republishing its local state. There is no handshake. A message whose `from` is
one of this process's own actors is dropped, and the link shows an error row on
the Web Server card (a relay loop, or a second PC restored from this one's
settings).

A remote actor (beta) is tracked from its `bus.actor`; three missed heartbeats
(2 s each by default) remove it, its open shots are discarded, and the
dashboard is told (`ActorGone`). Remote launch monitors appear as read-only
cards on the Settings Devices section, other remote actors under Software,
showing their name, device, blockers, battery and telemetry. Two actors that
both claim `session`, `practice` or `game` (or issue the same namespace)
each show an error row naming the other; flighthook keeps acting in its own
roles. flighthook's launch monitor cards, collector, practice actor and
outputs read only the `golfer.turn` and `practice.block` that flighthook's
own actor published.

### Type boundary

Types that cross the app/UI boundary (REST, `/api/ws`, config) live in the
`flighthook` lib crate; the FRP types are the `flightrelay` crate's, which
`flighthook` re-exports.

### Shot composing and the store

The SDK's `ShotAggregator` composes each shot from its messages until
`shot.finished` and stamps flighthook's own `golfer.turn` and `practice.block`
at the trigger. A shot not finished 45 s after its trigger (`SHOT_TIMEOUT`;
longer than a driver's own shot deadline) is discarded, and the messages of a
finished or discarded shot are ignored for another 45 s. The Open Connect
client, the Pureplay Connect output and the collector each keep an aggregator.

The collector stores every finished shot (`ShotData`, `GET /api/shots`) with:
the launch as the launch monitor sent it; every `shot.flight` with its sender
(`flights`); club and impact data; the flight model's `path`; the `turn` at the
trigger (golfer, club, hand, mode) and the `block` it belongs to; the game's
`stroke`; `golfer_name`; the metrics of what is shown; and camera clips.

A message for a shot already in the store (the flight model's `shot.flight`, a
Pureplay `game.stroke`) attaches by `shot_id` whenever it arrives, as long as
the shot is in the 1000-shot history; afterwards it is dropped. Each attach
updates the stored shot and sends it to the dashboard again. Only a message
from the shot's own launch monitor after its own `shot.finished` is logged as a
warning.

**What is shown** is composed per field from the shot's flights: the first
tracked flight's value, else the launch monitor's own, else the flight model's
(`force` shows the flight model's first). A value from the flight model or from
an `assumed` block is shown as simulated (SIM tags, italics in the Table).
`ShotData::shown` composes the shown launch, the launch monitor's own flight,
the simulated values (`derived`) and the metrics; metrics are recomputed when
an addendum arrives.

### Shot metrics

`ShotMetrics::compute` implements the Flight Relay Protocol's spin relations
and "Derived Values" once, for every consumer that assembles shots (the web
layer's history and export, the Open Connect client):

- total spin and spin axis from backspin and sidespin, via the SDK's
  `Launch::complete_spin`, only when the device sent both components and
  neither total nor axis (a total of 0 has no axis);
- face to path, `face_angle − path`;
- spin loft, the `dynamic_loft − attack_angle` approximation, only when the
  device sent no `spin_loft`;
- curve, `carry_offline − carry_distance × tan(launch_azimuth)`, in meters,
  from the launch monitor's own flight and the launch.

Each field needs every input measured and finite; `metrics` is `None` when no
field is set. Simulated values are never an input: the metrics describe the
device's shot. Lateral values are absolute,
+ right of the target line. `ShotMetrics` is `#[non_exhaustive]`; `curve`
converts with the unit system like the rest of `ShotData`.

### Simulated values

The flight model (the Simulation card, `app/src/actors/flight_model.rs`) flies
every shot a launch monitor finishes and publishes what it computed as one
`shot.flight` addendum under its own actor id (`basis: modeled`, model
`flighthook <version> (dimple <version>)`). The addendum holds only the result
fields it computed: carry, apex, flight time, descent angle, carry/roll/total
offline, roll and total. Nothing waits for it; it attaches to the stored shot
by `shot_id` whenever it arrives (see [Shot composing and the
store](#shot-composing-and-the-store)). The launch monitor's launch and flight
are never modified, so a value's place says where it came from: in the launch,
measured; in the launch monitor's own flight, the device reported it; in the
flight model's flight, simulated. A field set in both (a measured 0 carry or
flight time on a shot that flies, a ground ball's 0 carry, apex and flight
time over a device's other reading, both below, or any result field under
`force`) is shown with the simulated value.

`[simulation] mode` (`SimulationSection::mode`, `SimMode`) picks what is
simulated: `off` nothing (the flight model leaves the bus), `on` (the default)
the fields the launch monitor's own flight did not report, anchored to its
measured values, `force` every result field for every launch monitor, with
nothing counted as measured. Under `force` the launch monitor's own results
stay on the shot, in `/api/shots` and in the export, but the simulated ones are
shown. Launch values (speed, angles, spin, club data) are never replaced.

The flight model's input is the shot's launch as the SDK's
`CompletedShot::launch_for_flight` gives it: the measured values, with a value
the launch monitor lacks filled from its own flight's `assumed` block. A shot it
cannot fly, or has no field to add to, gets no addendum.

**Launch-only spin.** The FlightScope card estimates the spin of a launch-only
chip itself: when the device sent no flight, the card's `launch_only_spin`
(rpm per mph, default 100, 0 turns it off) is above 0, the launch qualifies
(speed, no measured spin, the launch angle floor) and the shot's mode is not
Putting, the card publishes one `shot.flight` of its own before its
`shot.finished`, holding only `assumed { backspin_rpm, sidespin_rpm: 0 }`. The
flight model flies the shot with it, the Open Connect client and Pureplay
Connect output send it as the spin, and the dashboard shows the spin as
simulated. It does not depend on `[simulation] mode`. Strict (tournament) mode
ignores every flight, so it never sends the estimate.

A value a device does not measure is left out rather than sent as a
placeholder: the ExPutt sends no launch elevation, and an Xi shot under the
E8 spin source without club data sends the E8 total alone (no axis, no
components). Outputs that need such a field fill their own default, and a
lone total goes out as backspin with no sidespin or axis made up: the GSPro
mapper (every Open Connect client) sends VLA 0 and the total as backspin
with sidespin and axis 0 (`flighthook::spin_components`), Pureplay sends it
as TotalSpin and BackSpin with the SideSpin and SpinAxis bits clear and the
CSV in `backspin_rpm` with sidespin and axis empty (both
`flighthook::output_backspin`), the dashboard's cards and table show it as
backspin with no sidespin (`spin_components`), and the simulation flies it
as backspin.

The simulation runs in `app/src/sim.rs` with the
[dimple](https://crates.io/crates/dimple) crate, an app-only dependency (the
lib crate and the WASM UI never link it), on the flight model's thread. It
costs a few milliseconds per shot. A shot is simulated only when:

- `[simulation] mode` is not `off`;
- the altitude (0 to 5000 m) and temperature (-30 to 50 °C) are in range;
- for a ground ball (below): the speed is finite and positive and the
  launch direction finite and within ±90°;
- for any other shot: all five launch values (speed, launch angle, launch
  direction, backspin, sidespin; a total spin with no components counts as
  that backspin with no sidespin) are present and finite, speed is above 0,
  launch angle is between 0 and 90°, and launch direction is within ±90°;
  and the simulated flight is valid (positive flight time, finite results).
  A slow, low shot with lift (3 mph at 2°) flies.

Otherwise the shot gets no addendum. Roll, total and the roll/total offlines
also need the ground phase to reach rest. Where the device measures carry,
total is the measured carry plus the simulated roll (the simulated carry plus
roll under `force`).

**Ground balls (beta).** A shot is a ground ball when the device measured
its apex as 0 (finite, within 0.01 m), or, on a device that reports no apex
(launch data only), when its launch angle is at most 0.5° (above −90°):
too little lift to fly, or when it reports neither a launch angle nor any
spin (a putt measured along the ground, as the ExPutt reports it). Anyone can blade a shot, so the device, club and
detection mode don't decide what is a ground ball; a putt is a ground ball
because its apex is 0, not because it is a putt, and a putting-mode shot
with an apex above 0 flies like any other. A ground ball never leaves the ground: carry, apex
and flight time are 0 (simulated values unless the device measured exactly 0
for that field, so a measured carry or flight time above 0 shows as 0),
there is no descent angle, and the carry offline is 0 (a simulated value unless the
device measured exactly 0). Its roll comes from
dimple's ground model (`GroundModel::settle`), started at the tee with the
launch speed along the launch direction and no spin, so the ball skids,
then rolls to rest; launch angle and spin are not used. The detection
mode picks the surface (`sim::ground_surface`): in Putting mode it rolls on
a green (`green`, 11.4 ft on the stimpmeter, unless the configured surface
is already `green` or `firm_green`, which is kept); in any other mode on the
configured surface. The mode is the one in effect for the shot: the session's mode at its trigger, or the launch monitor's fixed mode (the ExPutt's Putting). A straight 3 m/s ground ball rolls about 2.6 m on the default
`fairway` (8 m/s: about 10.8 m) and about 5.0 m on `green` (8 m/s: about
18.8 m). Roll and total are the simulated roll
unless the device measured the total (shown as measured, the roll fitted
to it) or the roll (total = roll); the roll and total offlines are the
rest point's `z`. `force` rolls a ground ball the same way, with nothing
counted as measured. This holds under `on` and `force` for every device.

**Zero readings.** On a shot that flies (not a ground ball), a measured
carry or flight time of exactly 0 is not a reading (the Mevo+ sends them
with every shot, 0 when it has none). It counts as not measured: the flight model
adds the simulated value, alongside the 0 left untouched in the device's
flight, and the path is fitted only to the values the device really measured.

The flight model's `path` (`FlightPath`) is the simulated ball path from the tee to rest:
`[t, x, y, z]` points in seconds and meters (downrange, up, right of the target
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
one (not repeated by the flight model), and the roll offline runs from it. The path is then decimated: the tee, apex,
landing, rest and the contacts and peaks of hops over 5 cm are kept and the
rest thinned by Ramer-Douglas-Peucker at 0.25 m, while the segment fits under
its cap (40 flight and 24 ground points); above the cap the segment is
resampled uniformly in time, keeping its ends (the flight also keeps its
apex). Points are rounded to centimeters and milliseconds: about 0.7 KB of
JSON per path on average, at most 64 points. It is never converted between unit systems, rides on the
`/api/ws` shot message, `/api/shots` and the JSONL export, and is not in the CSV.

The flight model reads the `[simulation]` section for every shot, so
settings changes apply on the next shot with no restart. The Open Connect
client sends the launch monitor's own flight only, never the flight model's,
so GSPro output does not change in any simulation mode. The console gets one
`sim (estimated, beta): ...` line per simulated shot; it is a summary, not wire
data, so the hex-first audit rules do not apply to it.

### Raw capture and shot export

A launch monitor card sends the device frames behind a shot just before its
`shot.finished` as the local `ShotCapture` event, as a `ShotRaw` (lib
`export.rs`); the frames never go on FRP. A `ShotRaw` holds a transport
label, the `RawRecord`s (`t_ms` relative to the trigger, direction, type,
bus addresses or channel, lowercase hex or JSON text), a `dropped` count and
an `unavailable` reason.

Each device card captures on its own thread with `RawCapture`
(`app/src/raw_capture.rs`). Frames are pushed as they arrive
(`push`, with their time and whether they are bulk data). Outside a shot
they go to a pre-roll ring; `mark_trigger_at` (at the shot's trigger) opens
the shot window with the ring as its head, and `take()` closes it for
`ShotCapture`, frames sorted by time (a stable sort, so frames of one time
keep their order).

| Device | Hook | Window | Excluded | Pre-roll | Max frames | Max bulk bytes |
| --- | --- | --- | --- | --- | --- | --- |
| Open Connect | the inbound JSON text and our reply | the one message | none | - | - | - |
| Mock, Trackball | none: `transport = "none"`, `unavailable` says why | - | - | - | - | - |

A device on a flightrelay device driver ([Device drivers](#device-drivers))
records the driver's `raw_frame` events instead: each frame is one protocol
message, cleared of identity by the driver, stored with the driver's `label`
as it gave it (no `type`, `src`, `dst` or `channel`), `hex` (or `text` for a
text protocol), and placed in time by the frame's own timestamp (a driver
stamps in UTC milliseconds; the card maps them onto its clock from one
anchor, so frames of one millisecond keep the order the driver gave them).
Pre-roll, caps and the bulk budget come from the driver's raw profile. The
mock and the trackball have no wire protocol and record none. The FlightScope (ironsight,
`transport = "flightscope-tcp"`) records every framed message once the
handshake is done, both directions, each labeled with its message type and
direction, pre-roll 8, at most 256 frames, 128 KiB of bulk per-shot data
(the large tracking records); ironsight withholds every message that can
identify the unit, its network or the host, the device's free text and debug
output, and everything of the handshake (audit log only, at trace level). The R10 (tenover, `transport = "r10-ble"`)
records one frame per message after the handshake, labeled by its type and
direction, pre-roll 256, at most 256 frames, no bulk
data; tenover withholds every message that carries the device's identity or
keys, every one it cannot classify and everything before the handshake
finished (those reach the audit log only, at trace level). The Square Golf
(allsquare, `transport = "square-ble"`) records each notification and each
command once the device's identity has been read, labeled by kind and type,
pre-roll 64, at most
64 frames, no bulk data; characteristic reads, where the identity lives, are
never frames, and allsquare withholds heartbeats and their acks, the status
query and its reply, and every notification it does not recognize (audit
log only, at trace level). The ExPutt (threeputt, `transport =
"exputt-wifi"`, text) records every message of the session as the JSON text
that crossed the socket, both ways, labeled with its name, pre-roll 64, at most 64
frames, no bulk data; the putt's own result is among its shot's frames.
Camera images (the player's surroundings) and messages threeputt does not
recognize are never frames (audit log only, at trace level).

The web layer keeps each shot as a `StoredShot` (`app/src/actors/web/mod.rs`):
when `shot.trigger` arrived, an export context taken then (device kind, maker,
model and firmware; the device's config section without `name`/`address`/
`bind`; the `[simulation]` section; identity strings to scrub), the composed
shot (`ShotData`, with the turn and block stamped at the trigger) and the
`ShotRaw`. A shot with raw frames but no decoded data is stored too;
`/api/shots` serializes `data` only and leaves those out. Raw frames share a
16 MiB session memory budget across the 1000-shot history, each frame charged
its hex (two characters per payload byte) or text length plus 256 bytes for
the record and its metadata: past it, the oldest shots lose their frames first
(`unavailable = "evicted: session raw budget"`), and decoded data stays until
the shot leaves the history.

`GET /api/shots/export` (`web/export.rs`, see docs/API.md) streams one JSONL
`ShotExport` or CSV row per stored shot. It never serializes device ids, actor
names, telemetry or configured addresses, and replaces identity strings in
text payloads and device labels with `<redacted>`; hex is never rewritten,
which is why identity frames are excluded at capture. The row carries the
shot's `golfer_id` (the `flighthook.*` id of the golfer the lookup rule finds,
else the turn's golfer id as sent), its `block`, and `data` with the shot's
`flights` and the game's `stroke`. Game golfer ids (such as
`pureplay.thill`, a Pureplay player's name) may appear in `golfer_id`,
`data.turn.golfer` and `data.stroke.golfer`.

Every bus subscriber gets its own clone of a message, so a heavy shot's frames
(a few hundred KiB for FlightScope Fusion with PRC data) are copied once per
consumer. Acceptable on a LAN. The shot's `video` (camera clips, beta) is
stamped from `ShotVideo` whenever it arrives (see [Cameras and
replay](#cameras-and-replay)) and is on `/api/shots` only: the export and
`/api/shots/convert` drop it.

### Sets

The dashboard calls a practice block a **Set**; the API, the wire and the
export say block. The practice actor (`app/src/state/practice.rs`, driven by
the system actor) publishes `practice.block` and keeps one open block per
**resolved golfer** (see [Golfers](#golfers-beta)); `number` counts from 1 per
golfer within a session, and `session` starts at 1 on each run. A block that has
had no shot yet is empty; a shot counts for the open block of the golfer up at
its trigger. A shot that finishes with no data (a no-read), or never finishes,
counts for nothing, so no gap is left in the numbers.

| Event | Effect |
|---|---|
| Start | Opens a block for the golfer up with the turn's club |
| `golfer.turn` changes | For the golfer up: no open block opens one. An empty open block is republished in place (same `id` and `number`) with the turn's club and golfer. An open block with shots and another club is closed and a new one opens. The same club keeps the block. Other golfers' open blocks stay open |
| `practice.new_block` | For the named golfer (default: the golfer up): an empty open block is left as it is; otherwise it is closed and a new one opens with that golfer's own club (their open block's club, the turn's club when they have none) |
| `practice.new_block { new_session: true }` | Closes every open block, counts `session` up and opens a block for the golfer up |
| Clear history | The collector forgets its shots and blocks, then a new session starts |

A golfer who comes back on the club they left with continues their block, and
alternating golfers each keep their own. A golfer whose block has shots on 7I
who changes to DR and back to 7I without hitting gets a new 7I block: a closed
block is history. A header club pick that a connected simulator snaps back
splits a block the same way when it has shots.

The collector (`app/src/actors/web/blocks.rs`) keeps every block seen this run,
open or closed, with its stored-shot count, for headers and history
(`GET /api/blocks`). Membership is the block the aggregator stamped on the
shot at its trigger. The `shot_grouping` setting (`none`, `by_club`, `by_set`)
is the Shots navigation's default view; By club combines blocks with the same
golfer, club and session. Every stored shot carries its block in every view,
and the export carries it as `block`.

### Actor presence and card status

Two things describe an actor, and **no key means two things**: an actor sends
each key through one of them only.

- **`bus.actor`** (FRP) is what the actor says about itself and its device:
  `device` (maker, model, firmware, source `measured` / `virtual`, battery),
  `blockers` (what keeps it from taking a shot; `ready` means none) and
  `telemetry` (standard keys `ready`, `battery_pct`, `tilt`, `roll`, `temp_c`,
  `external_power`; a driver may add its own, such as the mock's `radar_mode`
  and `tracking_mode`). A launch monitor with no session publishes `not_armed`
  and clears battery, attitude and telemetry.
- **`CardStatus`** (local) is what flighthook knows about the card and its
  connection: a status enum and the card's display rows. Standard keys:
  `device_info`; launch monitors on a device driver add `shot_count` and, while
  a driver reports one, `phase`; integration cards add `club`, `handed`,
  `name`, `error`. Open Connect clients add `address`, `last_shot`,
  `shots_sent`, `monitor_ready`, `response` (see
  [Open Connect client telemetry](#open-connect-client-telemetry)); Pureplay
  outputs add `address`, `last_shot`, `shots_sent`, `ready_state`,
  `strict_mode` (see [Pureplay output](#pureplay-output-alpha)).

```rust
pub enum ActorStatus { Starting, Disconnected, Connected, Reconnecting }
```

`ActorStatusResponse` (in `lib/src/api.rs`) adds a `name` field and is the
cached per-card state in both the web layer and the dashboard. Both merge a
card's `bus.actor` telemetry and its status rows into one map per card. A key
in a status replaces the one held; an empty value removes it (how the Open
Connect client and the web server clear `error`). A web server sets `error` to
`bind failed` while it cannot bind its address, and clears it with every other
status.

A card never shows a lost connection's values. When a card's status goes from
`Connected` to `Disconnected` or `Reconnecting`, and on every `Starting` (a new
instance), the web layer and the dashboard drop all the telemetry held for the
card (device-reported keys, `club`, `handed`, `name`, status keys) and then
merge the keys that status carries, so `error` and the keys a card sends with a
status stay. Later statuses that do not end a connection merge as above. Both
sides call `ActorStatusResponse::apply_status`. A card that reports a static key
once per instance sends it again with the statuses that clear (the Open Connect
client's `address` and `last_shot`, see below); the device cards send their
keys again with each connection. The card then shows its empty state until the
actor reports: the spinner while not Connected, the connected mark when
Connected with no rows.

### ConfigCommand / ConfigOutcome -- event-sourced config mutations

Config mutations are routed through the bus as typed local events, processed
exclusively by SystemActor, the sole writer of the configuration and of the
session.

```rust
// ConfigCommand fields (LocalEvent)
request_id: Option<String>,  // correlation ID for request-reply
action: ConfigAction,

pub enum ConfigAction {
    ReplaceAll { config: FlighthookConfig },        // POST /api/settings
    ReplaceGlobal { config: FlighthookConfig },     // POST /api/settings?scope=global: global fields only
    UpsertMevo { index: String, section: MevoSection },
    UpsertR10 { index: String, section: R10Section },
    UpsertSquare { index: String, section: SquareSection },
    UpsertExPutt { index: String, section: ExPuttSection },          // "upsert_exputt"
    UpsertTrackball { index: String, section: TrackballSection },
    UpsertOpenConnectServer { index: String, section: OpenConnectServerSection },
    UpsertOpenConnectClient { index: String, section: OpenConnectClientSection },  // `upsert_gs_pro` also read
    UpsertPureplay { index: String, section: PureplaySection },
    UpsertPureplayAccessory { index: String, section: PureplayAccessorySection },
    UpsertWebserver { index: String, section: WebserverSection },
    UpsertMockMonitor { index: String, section: MockMonitorSection },
    UpsertRandomClub { index: String, section: RandomClubSection },
    UpsertCamera { index: String, section: CameraSection },
    Remove { id: String },                          // "mevo.0", "openconnect_client.1", "webserver.0", etc.; "gspro.N" = "openconnect_client.N"
    SetShotGrouping { mode: ShotGrouping },         // POST /api/shots/grouping, UI `grouping`
    SetBodyTracking { section: BodyTrackingSection },  // POST /api/settings?scope=body_tracking
    SetTournament { on: bool },                     // POST /api/tournament
    SetGolfers { golfers: Vec<GolferRecord> },      // POST /api/settings?scope=golfers
    Reconnect { id: String },                       // POST /api/actors/{id}/reconnect
}

// ConfigOutcome fields (LocalEvent)
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
`CONFIG_TIMEOUT` (`web/routes/settings.rs`): the longest stop wait of any actor
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
SystemActor goes on handling `golfer.select`, `practice.new_block` and
further commands. A command that touches a settling actor follows the rule
for an actor whose old instance is still closing: a removal lists it in
`stopped` at once and mutes the old instance; a save or re-add starts
nothing, and the waiting command starts it from the settings current when it
finishes; a reconnect starts nothing more (its request answers `202`). So
there is never more than one instance of an ID, and a new instance always
starts after its old one has ended or been retired.

## Session and golfers

### The turn

flighthook's session (`app/src/state/session.rs`, applied by the system actor)
owns `golfer.turn`: one complete turn of `golfer`, `club`, `handedness` and
`mode`. It answers every `golfer.select` from any sender (the dashboard,
`POST /api/select`, an Open Connect client, Pureplay Connect, the Pureplay
Accessory card, random club, an FRP peer) and publishes the turn when it
changed, at start and on `bus.sync`. Launch monitor cards read only
flighthook's own turn and hand the driver its hints (club, hand, mode) when it
changes, so a repeated select that changes nothing sends a driver nothing.

- **Start.** The turn is the `selected_golfer` (else the first golfer, else no
  golfer), that golfer's hand (else right-handed), no club and mode full.
  Handedness is always present: right-handed until a hand is chosen.
- **Fields.** A select's `club`, `handedness` and `mode` each replace the
  turn's; a field the select leaves out stays. **The mode is never derived
  from the club**: only a select that carries `mode` changes it. A select from
  flighthook itself (`POST /api/select`) that carries a hand also saves it on
  the golfer up.
- **Golfer.** A select's golfer is taken in any namespace, known or not. When
  it changes the golfer (by the lookup rule below) and gives no club, the
  golfer gets their own last club this run back (the current club when they
  have none); when it gives no hand, the hand of the golfer the lookup rule
  finds. The last club is kept per resolved golfer.
- **Nothing is refused.** No select is answered `not_allowed`; the last writer
  wins. Clubs outside a bag are accepted.
- **A game golfer that is gone.** When the golfer up is a game's and that
  golfer's profile is removed (the round ended, the card disconnected), the
  turn goes to the saved golfer who lists that game id, else back to the start
  golfer, in mode full.
- **Cause.** With each turn it publishes, the session records the sender of the
  select that produced it (`cause`, internal to flighthook).
- `selected_golfer` (the `flighthook.<uuid>` last picked in flighthook) is
  saved.

### Games and simulators re-assert

Three card types speak for a game or simulator: the Open Connect client,
Pureplay Connect and the Pureplay Accessory. Without locks, each keeps the turn
in line with what its game holds (`app/src/actors/simout.rs`):

1. **Complete selects.** Whenever it sends `golfer.select`, it sends everything
   it holds: golfer, club and hand for the Accessory while a local player is
   up; club, hand and, when it holds one, the mode for the Open Connect client
   and Pureplay Connect. It sends one whenever its simulator reports them.
2. **Re-assert.** While connected, when flighthook's turn differs from what the
   card holds, it sends its complete select again at once, unless the turn's
   cause is a game or simulator card (itself included). Turn changes from the
   dashboard, `POST /api/select`, random club, the Open Connect server input or
   an FRP peer are re-asserted against.
3. **Last sender wins** between game and simulator cards. Nothing loops: a card
   never answers another card's change.

So a header pick with a simulator connected is snapped back to the simulator's
club and hand, and the header always shows the result. A card that holds
nothing yet re-asserts nothing.

**Shot type.** The Open Connect client sets the mode from the simulator's club
by its own lists, `chipping_clubs` and `putting_clubs` (a putting club:
putting, a chipping club: chipping, any other: full); with both lists empty it
sends no mode. Pureplay Connect applies one shared rule (`simout.rs`, `mode_for_lie`): the
putter is putting; a `SetShotContext` distance under 30 yards is chipping; any
other club or distance is full. The Pureplay Accessory sets the mode itself on
each turn by the same rule. Shot Type in the header sends a select carrying
`mode`.

**Shot Type visibility.** The header's Shot Type control shows while an enabled
launch monitor card uses the hint (FlightScope, Trackball and Mock do; the
other drivers ignore it), or an enabled output routes Chipping or Putting to a
specific launch monitor. When neither holds and the mode is not Full, flighthook
sets it to Full once (`ShotTypeReset`), unless a simulator card holds the mode.

### Golfers (beta)

`[[golfers]]` holds the golfers. Each record has `identifiers` (always its own
`flighthook.<uuid>`, written when the golfer is created, plus any game ids it
is linked to, e.g. `pureplay.thill`), `name`, and optionally `handed`, `bag`
(club codes), `sex` and `height`. An id may appear in one golfer only; a save
that lists one id twice is refused (`400`), and a file that does it keeps the id
with the first golfer. flighthook publishes each golfer as `golfer.profile`
under its `flighthook.*` id (name, hand, bag, sex, height) at start, on every
change and on `bus.sync`; game ids are never published by flighthook.

**The lookup rule.** Every place that needs to know which flighthook golfer a
golfer id means uses one rule (`FlighthookConfig::golfer_of` and `golfer_key`):
the golfer whose `identifiers` contain it. No match means the id stands on its
own, shown under its own `golfer.profile` name and keyed by itself; no golfer
(`null`) is its own key. The rule serves the header's golfer name, the Shots
views, the export's `golfer_id`, the session's last club, the practice actor's
blocks and the game cards' re-assert. With no `[[golfers]]` the turn's golfer is
`null` and shots carry no golfer; the first saved golfer becomes the golfer up.

The Golfers card in Settings adds, edits and removes golfers; a game id is
added from a picker of the profiles the game currently publishes and removed
with its Remove button.

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
Open Connect server, the web server):

| ConnectionState | When | Color |
| --------------- | ---- | ------ |
| Connecting      | Not `Connected` since it started, or since a config change restarted it | yellow |
| Connected       | `ActorStatus` is `Connected` | green |
| Reconnecting    | `Connected` since it started, and has since lost the connection (any other status) | red for 10 minutes (`RECONNECT_ALERT_MS`) after the most recent `Connected` -> other change, yellow after |
| Disabled        | The card is turned off in the settings (`enabled = false`): no actor runs | gray (`theme::STATUS_OFF`), never red |

`StatusHistory` holds the latest status, whether the actor has been
`Connected` since it started, and when the alert for its last loss ends.
Another loss while already Reconnecting keeps the window; a new `Connected`
then a loss starts it again. A `Starting` status begins a new history, and so
does a restart (`ConfigOutcome.restarted`: only whether it is `Connected` now
is kept). Removing an actor drops its entry, so one added again starts fresh.
An `error` telemetry value does not affect the state or its color.

Disabled is a displayed state taken from the config, not an `ActorStatus`:
an actor that is off has no running instance, so nothing on the bus could
report it, and the wire `ActorStatus` stays what a running actor says about
itself. `StatusHistory::off` (`ActorStatusResponse::disabled(name)`: status
`Disconnected`, no telemetry) reads Disabled until a `Starting` status, the
first status of an instance started when the card is turned on again, begins
a new history. The web layer keeps an entry per configured actor and brings
it in line with `disabled_actors` at startup and on every `ConfigOutcome`
(`follow_disabled`): a card turned off gets a Disabled entry, its cached
status and device telemetry are dropped. `GET /api/status` sends it as
`"state": "disabled"`, and the UI's `received` keeps it. Times are
milliseconds on a monotonic clock each side picks: the web layer's
`WebState::now_ms` (from `WebState::epoch`), the UI's frame input time.

The web layer records every `ActorStatus` into its per-actor
`ActorStatusResponse` (`observe`), for as long as it runs, so the history
does not depend on any UI being connected. `GET /api/status` stamps each
actor's `state` and `red_for_ms` (the alert time left) at reply time. The UI
rebuilds the history from a reply (`received`, the red time counting down from
receipt on its own clock), then applies live card status events and
`ConfigOutcome.restarted` to it the same way the server does.

All actors use linear backoff on connection failure: starts at 1s, adds 1s per
failed attempt, capped at 15s (3s for the Open Connect client, a loopback/LAN
TCP connect). The Open Connect client actor restarts at 1s after a connection
that lasted at least 10s; one that drops sooner counts as a failed attempt, so
a link that keeps connecting and dropping still backs off to the cap. The
Open Connect server restarts at 1s after a successful bind. A card on a
device driver (the FlightScope, the R10, the Square Golf, the ExPutt, the
trackball, the mock) waits the delay its driver asks for: the FlightScope,
the R10, the Square Golf and the trackball use the SDK's `Backoff`, 1 s per
failed attempt up to 15 s, back to 1 s after a session of at least 10 s; the
ExPutt adjusts the same backoff (below). The trackball's driver gives up off
Linux (the card ends Disconnected with the error). Every actor polls its shutdown flag during the backoff wait
(the Open Connect server during its bind retry delay too), so stopping one
never waits out the delay. The Open Connect client's TCP connect (up to 5s)
runs on a thread of its own: a stop ends the actor at once, and a connect
that completes afterward closes again; the FlightScope driver's (up to 5 s,
the SDK's `connect_tcp`) gives up at once on a stop. The ExPutt's
session start cannot be cut short: the TCP connect and the device's greeting
(up to 3s each), its two handshake replies (up to 3s each) and its mat search
(up to 5s).
The ExPutt driver caps its first 3 reconnect delays at 1s after a session
that was measuring, then uses the backoff. After a session lost to silence
it waits 15s before the first attempt and leaves at least 15s between
attempts until a session succeeds again; it makes one attempt at a time.

### Reconnect

The Reconnect button on a Launch monitors or Software card
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
   (`SystemState::wait_for_exit`): 3s by default; a card on a device driver
   waits its driver's `STOP_WAIT` (6s for an R10, and for a Square Golf,
   which disarms and disconnects; 1s for a trackball); 18s for an ExPutt, whose session start
   cannot be cut short; none for the web server (the save that moves it is
   answered by the server being stopped, and its successor retries the bind).
2. If the thread is still running, the actor's `replaceable_while_stopping()`
   decides: a Mevo/Xi, an Open Connect client, the mock actors and the web
   server hold nothing a new instance needs (a TCP client, nothing, or a port
   the new one retries), nor does a trackball (it never grabs the input, and
   several programs can read one input), and an Open Connect server is replaceable
   once its listen port binds again. A replaceable one is retired: taken out of the
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
3. Emit a `Starting` status for the card, clearing an earlier `error`,
   and start the new instance (`start_actor`). The outcome lists it in
   `restarted`, so the card begins a new history; it then shows the actor's
   own statuses as it connects.

A removal stops and releases the actor the same way, without starting
anything; one still closing is retired and muted at once, and its card goes
with the outcome.

### Turning a card off

A card turned off (`enabled = false`) keeps its config section and its card;
its actor does not run. `resolve_actors` leaves such a section out, so the
config reload (`apply_config_reload`) treats a save that turns it off like a
removal: the actor is stopped and released by the rules above and the
outcome lists it in `stopped`; one still closing is muted at once and is not
started when its old instance ends (`restart_closed` resolves from the
current config). A save that turns it on finds a new expected actor and
starts it like a new card (listed in `started`). A save of another setting
of a card that is off starts nothing, and a reconnect of it is refused
(`"'<id>' is off"`, `409`). At startup it is not started.

An actor that is off is no shot source: it is not running, so it sends no
shots, has no battery in the header and adds no readiness; an output routed
to it is not ready, as with a monitor that is not configured. A tournament
round never counts it: turning it off forgets it as a shot source, as a
removal does (`RoundPolicy::forget_source` in `system::finish`), the Pureplay
output's `monitor_names` leaves it out, and a mode routed to it by ID counts
for nothing (`Relay::feeds` keeps a routed ID only when it is a known
monitor), so a mock that is off does not refuse a round. The Cameras tab's live view and its per-camera
clip status show only the cameras that are on (`SettingsForm::cameras_on`).

### One card per address

Two cards conflict when they are the same type and their addresses collide
(`flighthook::FlighthookConfig::address_clash`): the same host and port
after normalizing (`localhost` reads as `127.0.0.1`, an IPv4-mapped IPv6
address as IPv4, and a blank address as the type's default), or, for a card
that listens (Open Connect Server), the same port on overlapping bind
addresses (`0.0.0.0` overlaps every address). Only cards that are on hold an
address, and only the types with an address compare: Mevo, Square Golf
Omni, ExPutt, Open Connect Server and Client, Pureplay Connect, and the
Trackball by the device it names (compared as text; a blank one is the only
trackball or mouse, so two blank cards conflict).

Duplicates may exist; at most one of them is on.

- **Creating one.** The (+) menu adds the card off (`enabled = false`) when
  an enabled card of its type has its address. No error, no alert.
- **Turning one on.** The card's switch is locked while an enabled card of its
  type has its address, with the hover "An Open Connect card already uses
  127.0.0.1:921" (the type's name and the holder's address). After its address
  is changed to a free one and saved, the switch works. A save through the API
  that turns on a card, or adds one that is on, at a held address is refused
  with `409` and the same text, and saves nothing.
- **Editing an enabled card onto a held address.** The save goes through and
  the card is written off: the system actor turns off, after applying a save,
  every enabled card whose address another enabled card of its type has. A
  card whose address or switch the save changed loses to the cards it did not
  touch, so the edited card is the one that goes off, not the holder. The
  dashboard builds its own request the same way, so it shows Disabled at
  once.
- **A config file with two enabled cards on one address.** The first in index
  order (numeric, so `2` before `10`) keeps running; each later one loads
  off, with one warning in the log naming the card, the holder and the
  address. The file is not rewritten at startup: the next save of any
  setting writes `enabled = false` for those cards with everything else.

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
| FlightScope (Mevo+, Gen2, Xi) | always: the TCP connect gives up at once on a stop, the handshake checks for one between polls, a session polls every 50 ms; closing sends nothing | the driver's `STOP_WAIT` (3 s); the driver is not exclusive, so an old instance still closing is replaced beside it (a second socket to the device can overlap the old one until it closes) |
| Open Connect client | always: 50 ms reads, 2s write timeout, the connect on its own thread; its local-client registration ends with the connection | a write in progress (2s) |
| Pureplay | always: 50 ms reads, 2s write timeout, the connect on its own thread | a write in progress (2s) |
| Open Connect server | always: 100 ms accepts and reads, 2s write timeout, the bind retry wait polls the flag; the listen port is free once it ends | a write in progress (2s) |
| Square Golf | always: allsquare's scan, connect and identity read run on a helper thread and see the stop within about a second, and a connection that comes up after a stop is disconnected; a session disarms the device, then disconnects it, with BlueZ and with btleplug | the driver's `STOP_WAIT` (6 s: a stopped connect lets go within 5 s, every BlueZ call gives up after 2.5 s; close is a disarm given up after 2 s (btleplug) or 2.5 s (BlueZ) and a disconnect of at most 3 s or 2.5 s) |
| Garmin R10 | always: tenover's scan and connect check the stop at least every 100 ms (the connect runs on a helper thread); a connect that comes up after a stop is let go | the driver's `STOP_WAIT` (6 s: one D-Bus call of at most 3 s and a disconnect bounded the same way) |
| ExPutt | discovering (250 ms slices), measuring (100 ms event wait, the session closes at once) or waiting to reconnect | the driver's `STOP_WAIT` (18 s: a session start of up to 17 s, which cannot be cut short: connect and greeting, two handshake replies, mat search; then the close) |
| Trackball | always: opening the device is a few system calls, checked for a stop before and after; a session polls every 50 ms; closing drops the device | the driver's `STOP_WAIT` (1 s: closing joins the device's reader thread, which wakes every 100 ms); not exclusive (the card never grabs the device), so an old instance still closing is replaced beside it |
| Mock monitor | always (50 ms polls; a device driver's connect checks the stop between steps) | the driver's `STOP_WAIT` (3 s) |
| Random club | always (100 ms loop) | -- |

**FlightScope card**: ironsight's device driver (`ironsight::driver::Mevo`)
on `DriverActor` (`actors::mevo::MevoHooks`: section `MevoSection`, source
`Mevo`). The driver connects over TCP to `address` (`phase: connecting`, at
most 5 s), runs the handshake (`phase: handshaking`) and configures the
model it detected in the turn's mode (`phase: configuring`); the
card is then `Connected` (`device_info` "FlightScope Mevo+", "FlightScope
Mevo Gen2" or "FlightScope Xi") with `bus.actor` device `manufacturer:
"FlightScope"`, model `Mevo+`, `Mevo Gen2` or `Xi` and the device's firmware
(version and build date), and once armed `phase: measuring`, `ready=true`.
The FRP device id is the camera Pi's WiFi name, or "FlightScope Xi
(<address>)" on an Xi (and on a Mevo whose Pi reports none). Every session
starts in the turn's mode, so a mode change during a reconnect is kept. A
mode change that reaches the device reconfigures and re-arms it (`phase:
configuring` and `ready=false`, then `measuring` and `ready=true`): at once
when the device is armed, else once the shot in progress is done; on the Xi,
Chipping ↔ Putting sends nothing. Hand and club don't apply. A Fusion camera
mode goes out after the camera's 15 s warmup (timed from the handshake),
between shots. Telemetry keys: `ready`, `tilt`, `roll`, and on a Mevo+
`battery_pct`, `temp_c` and `external_power` (a Gen2's or Xi's status has
none), each sent when it changes. A shot is `shot.trigger` (timestamped with the
host time the driver saw it, which is when it reaches the bus), `shot.launch`
and `shot.flight` as soon as the full result (D4) arrives (the launch
with total spin and axis next to back and side spin; the flight
`basis: modeled`, with the device's total and roll distance when it reports
them), `shot.club` once the shot has a ball result, and `shot.finished`
when the device is done. A launch-only full result (launch angle of 5° or
more, no spin and no flight values: very short chips on the Xi) is a launch
without spin and no flight result. With `use_estimated` on, a shot without
a full result is sent from the early result (E8). A shot the device did not
read is an alert ("Shot #n: Xi didn't track the ball (no read)") and no
shot; a message the session cannot decode is an alert naming the error,
never its bytes, and the session carries on; the device closing the
connection, an operation timing out, or 10 s without a message from the
device ends the session (the alert is the error, e.g. "connection closed by
device"). The card's hook adds one thing on a launch: a launch with no flight result, no
spin, a launch angle of 5° or more and a mode at the trigger other than Putting
gets an estimated backspin of `launch_only_spin` × ball speed (mph) and
sidespin 0. The card publishes it as its own `shot.flight` holding only
`assumed` spin, before `shot.finished` (see [Simulated values](#simulated-values)); `shot.launch` carries only what the device
measured. A launch the Xi sends with its early result's total spin
(`spin_source` `e8` or `auto`) keeps that. The mode at the trigger is the
session's mode the host handed the driver before that `poll` pass, the mode the
driver itself takes for the shot, so a mode change between the trigger and the
full result, seconds later, changes nothing.

The driver is not exclusive: a new instance connects while an old one still
closes.

**R10 card**: tenover's device driver (`tenover::driver::R10`) on
`DriverActor` (`actors::r10::R10Hooks`: section `R10Section`, source `R10`, no
processing of its own). The driver finds a paired R10 over BLE (`phase:
discovering`), runs the handshake (`phase: handshaking`, at most 10 s from its
start), then the card is `Connected` (`device_info` "Garmin R10 (<address>)")
with `bus.actor` device `manufacturer: "Garmin"`, `model: "R10"`. It sends the
card's `range` as the tee distance once the device wakes (a changed range
restarts the card). Telemetry keys: `ready`, `tilt`, `roll`, `device_state`
(`waiting` | `recording` | `processing`); a device error is an alert with its
tilt as `tilt`/`roll`. A shot is trigger, `shot.launch` (total spin and axis plus
back/side spin, all measured; a total of 0 has no axis), `shot.club` when the
club was tracked, `shot.finished`, then `ready=false`; the R10 has no wall
clock, so the trigger carries no timestamp. Commands are ignored (no mode,
hand or club on the R10). The BLE address is the FRP device id. On Windows
and macOS closing the session disconnects the R10 (at most 4 s); on Linux the
BlueZ link is left up.

**Square Golf card**: allsquare's device driver (`allsquare::driver::Omni`)
on `DriverActor` (`actors::square::SquareHooks`: section `SquareSection`,
source `Square`, no processing of its own). The driver scans for the Omni (or
the one `address` pins: advertised name, BLE address, or the macOS peripheral
UUID; blank or the all-zero placeholder discovers), `phase: discovering`,
connects without pairing and reads its identity, then the card is
`Connected` (`device_info` "Square Golf <model> <name> (<address>)", so the
address can be copied into the config) with a `bus.actor` device
`manufacturer: "Invant"`, the model the device reports and its `lm`
firmware. It arms on its first poll (`ready=false`) with the club and hand
in play: the session's turn, else the section's `club` (default `7I`)
and a right-handed player. A club or hand change re-sends one club select
carrying both, only when either differs; a club the Omni has no code for is
armed as the nearest club of the same kind, the stronger loft on a tie;
before the arm a change is only recorded; the detection mode is ignored.
Telemetry keys: `ready`, `device_state` (`waiting` | `ready` | `recording`
| `processing`), `battery_pct`, `zero_spin_discards`. A shot is trigger
(timestamped with the ball notification's arrival), `launch` (all four spin
values; a total of 0 is sent as 0 with no axis), `club_path` and
`face_impact` (per-club calibration applied) when the club was tracked,
`shot_finished`. A zero-spin read with any club but the putter is dropped
when `discard_non_putting_zero_spin` is on (default): a `warn` alert, the
`zero_spin_discards` count, `ShotDiscarded` (no shot number taken). Settings
are read leniently: an unknown club or calibration key and a negative
calibration are skipped with a warning at the first connect; the driver
refuses nothing it can read, and any change of its settings restarts the
card. The advertised name is the FRP device id. Closing disarms the Omni
and disconnects it on both backends (it takes one central at a time), so
the card is exclusive: a new instance waits for the old one to close.

**ExPutt card**: threeputt's device driver (`threeputt::driver::ExPutt`) on
`DriverActor` (`actors::exputt::ExPuttHooks`: section `ExPuttSection`, source
`ExPutt`; its `action` turns a `FindMat` for this card into the driver's
`Action::FindMat`). With no address the driver waits for the device to
announce itself on the network (`phase: discovering`, 250 ms at a time), then
connects (`phase: connecting`) and starts measuring with the turn's hand
(the session's, right-handed when unknown; `phase: measuring`, which the
`Connected` status keeps). The card is then `Connected` (`device_info`
"ExPutt RG (<ip>)", which is also the FRP device id) with a `bus.actor` device
`model: "ExPutt RG"` (and the device software version as `firmware` when it
was discovered). The device page's Find mat icon (`POST /api/find-mat`, a
`FindMat` bus command) has the device locate the mat again without
reconnecting; one pressed while the card has no session is dropped. A
handedness change (from any source, through the session's turn) starts a new round with the new hand, which
locates the mat the same way; the detection mode and the club do not apply.
Mat searches are at least 10 s apart (the one at session start counts):
requests inside that time are folded into one search with the latest hand,
run when it ends, and a hand changed back to the one in effect needs none;
`ready` is false while one waits. Telemetry keys: `ready` (true when the
device is ready for a putt, false from a strike until it is ready again, and
false when the connection is lost, before `reconnecting`), `device_state`
(`finding mat` | `mat found` | `ready` | `ball struck`; a putt result
changes neither key). After 30 s of discovery without finding the device,
one `alert` (`warn`) suggests entering its IP address on the device card
(the driver's hint, worded for the card by the card's `on_frp`). Each putt
is a shot:
trigger (timestamped with the arrival of the device's strike message when one
came since it was last ready, so the cameras date the putt at its stroke),
`launch` (ball speed, launch direction as `launch_azimuth`; no launch
elevation and no spin, which the device does not measure), `club_path` with
path and face angle when the putter was tracked, `shot_finished`. Putts are
simulated in Putting mode (the driver's fixed mode, rolled on the green)
whatever the turn's mode, and the web layer files them with club `PT` and
detection mode Putting whatever club is in play. Shot numbers continue
across reconnects. Stopping ends the match and closes the session; the
device takes one connection at a time, so the card is exclusive. A closed
connection, or no event for 90 s (120 s after a strike, until the putt's
result or the device is ready again), ends the session; a quiet spell past
10 s logs one warning and makes `ready` false until the device sends again
(then it is what the device last reported). Each failed attempt is an
`alert` (`warn`, "ExPutt error: ..."); when attempts get no answer from a
device that accepts them and the first such attempt of an outage is 2
minutes old, one `alert` (`warn`) says the ExPutt is not answering and
suggests turning it off and on again, and the per-attempt alerts stop until
a session succeeds (attempts that fail another way do not restart the
clock). The reconnect delays are in [Connection state](#connection-state) above.

An Open Connect client logs a warning at start when an ExPutt is configured
and its Putting Monitor is unset (Any).

**Trackball card** (alpha): thumbswing's device driver
(`thumbswing::driver::Thumbswing`) on `DriverActor`
(`actors::trackball::TrackballHooks`: section `TrackballSection`, source
`Trackball`, which is virtual; no hook, no action). The driver opens the
input the section's `device` names (a `/dev/input/by-id` name, a
`/dev/input` path or the name the device reports), or the only readable
trackball or mouse when it names none (`phase: connecting`), never with a
grab: the card runs the driver with its `grab` off
(`DeviceSection::driver_settings`), so the desktop pointer keeps following
the ball, and a `grab` key in the section is read without an error, ignored
and left out of the next save (the config load does not count it as a
difference, so it alone never rewrites the file). The card is then `Connected`
(`device_info` "thumbswing (<the name the device reports>)"; the FRP
device id is the input's `/dev/input/by-id` link name, else the reported
name) with a `bus.actor` device `manufacturer: "divotmaker"`, `model:
"thumbswing trackball"`, the crate version as `firmware` and `source:
virtual`. Telemetry keys: `ready`, `synthetic` (`true`), `input` (the
reported name), `cpi`, `profile` (the club in play) and `handed` (`rh` |
`lh`). The club and the hand start as the section's `club` and `hand` and
follow the session's turn (a club the turn names replaces the
setting); Putting mode selects the putter whatever the club. Each swing (a
pull back, then a push) is a shot: `shot.trigger` (timestamped with the end of the
push), `shot.launch` (speed, launch angle and direction, both spin pairs),
`shot.club` (club speed, path, face angle, attack angle, dynamic loft,
smash factor, swing plane), `shot.finished`. There is
no flight result (the flight model flies it) and no raw frames. Everything is
synthesized, so the source is virtual: during a tournament round the driver
is told to hold shots, and a swing then is dropped with an info log line and takes no shot
number. When the input goes away the session stays up: `ready` turns false,
the driver reopens the device by itself (with no `device` named it chooses
again, so a device back under another node is found) and `ready` turns true.
A device that cannot be opened is a `warn` alert saying what to do (name
one with the `device` setting when there are several, which is alerted
once until the device opens; join the `input` group), never a device path, and a retry with the backoff; the card shows the
driver's error as its `error` row meanwhile. Off
Linux the driver gives up with an `error` alert ("trackball input is not
supported on this platform (Linux only)"), and the card ends Disconnected
with that error; `GET /api/status` says `trackball_supported` only on
Linux (`thumbswing::device::SUPPORTED`), and the UI offers the Trackball
card only then. The club, the hand, the thresholds and the tuning apply to
the next swing; `device`, `cpi` and `report_limit` restart the card. The
card is not exclusive: a new instance does not wait for the old one to let
the device go. Its shots cut no camera clip: the type has no
impact lead (`impact_lead_default_s("trackball")` is `None`).

**Open Connect client actor** (GSPro, Pureplay, Muni Golf, ...): emits `Starting` before
first TCP connect attempt, `Connected` on success. Connection loss after a
successful session emits `Reconnecting`; failure before ever connecting stays
at `Starting`. A refused, failed or closed connection is shown only through
this status and the log, with no alert: a simulator not running is normal when
another one is in use.

**Open Connect server (input)** is a launch monitor actor with source `measured`
(another vendor's companion software counts as measured). It publishes
`shot.*` from the Open Connect shots, and its own flight with `basis: modeled`
unless it knows the values were tracked. The player information it sends back
in its `201` (hand, club) comes from flighthook's `golfer.turn`; because the turn
always has a hand, `Handed` is always sent (`RH` until a hand is chosen). It does
not re-assert.

**Open Connect self-loop guard**: an Open Connect client pointed at this
process's own Open Connect server (e.g. client `127.0.0.1:921`, server bind
`0.0.0.0:921`, GSPro closed) would send every shot client -> server -> bus ->
client, forever. `SystemState.local_clients` (`state/local_clients.rs`) holds
the local socket address of every connected Open Connect client, registered
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
  after the server's name: "Refused Open Connect Client "<client name>" from
  <peer>: it is this flighthook's own client, which would loop shots back.
  Point it at the simulator's port.";
- the client sets its telemetry `error` key, shown after the client's name in
  the banner: "Refused by this flighthook's own Open Connect Server "<server
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

**Open Connect client shot delivery**: the bridge drains the bus the whole time
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

**Integration readiness**: the Open Connect client tracks the routed launch
monitor's readiness from its `bus.actor` (no blockers means ready; under "Any",
any launch monitor that may feed a simulator) and uses it to set both
`launch_monitor_is_ready` and `launch_monitor_ball_detected` in heartbeats
and shot messages.

**Open Connect client on FRP.** The client composes each shot with an SDK
aggregator fed only flighthook's own turn and sends a finished shot when its
launch monitor is the one routed for the mode of flighthook's turn as the shot
finishes (routing names launch monitors by actor id; Auto-Chip always passes; a
game's launch monitor, the Pureplay Accessory card, never does). The launch it
sends is the measured launch with a missing value filled from the launch
monitor's own flight's `assumed` (the FlightScope launch-only spin); the carry
and other results come from the launch monitor's own flight only, never the
flight model's, and an Auto-Chip's launch goes alone. Whenever the simulator's
player information arrives, the card sends one complete `golfer.select` (club,
hand and, with club lists set, the mode) and re-asserts it by the rule in
[Games and simulators re-assert](#games-and-simulators-re-assert). A routing
that names an actor no card holds shows "Missing monitor" and routes nothing
for that shot type until another is picked.

**Simulator sources feed no simulator output.** A simulator output (the Open
Connect client, the Pureplay Connect output) never takes shots, readiness or
a place among a round's feeds from a simulator source, under "Any" and by ID,
with no address comparison: `Feed::feeds_simulators(actor)` (`simout.rs`) is false
for an actor whose registered `ShotSource::is_simulator()` (the Pureplay
card) or whose ID starts `pureplay_accessory.` (before it registers). It is
the one predicate behind `Feed::passes`, the outputs' readiness (`Feed::ready`)
and the monitors they report (ready, connection state, battery), `monitor_names`
and `Relay::feeds`. The
Open Connect client's `Relay` holds a `gspro::Registry` (the `RoundPolicy`
clone, for the source types, and the send registry) for it. A save that
routes an output to a `pureplay_accessory.N` ID is refused with `400` ("An
output can't take shots from Pureplay", `FlighthookConfig::simulator_routes`
in `invalid_setting`); a config file with one loads with a warning, and the
route passes nothing. The UI's routing pickers leave the card out
(`devices::routing_monitors`). Every other consumer (Shots tab, cameras,
exports, `/frp`) takes the card's shots like any shot. 

**Send registry.** `SystemState.sent_shots` (`state/sent_shots.rs`,
`SentShots`) is how the Pureplay Accessory card tells Pureplay's return of a shot an
output sent from a shot of its own. Each simulator output records
`(ShotKey, SimAddr, Instant)` just before it writes a shot (only shots it
writes: not one strict mode keeps back or one dropped as stale), with the
key the bus carried (the shot's own number, before the Open Connect client
raises it). A write that fails takes its record back (`SentShots::forget`;
for Pureplay Connect, a failed BallData write). `SimAddr::of(local, peer)`
is fixed per connection: the simulator's address and whether it is this
computer (loopback, unspecified, or the connection's two ends share an
address); the card makes one for its own connection the same way. At most
64 records are kept; records older than `ECHO_WINDOW` (5 s) expire.
`take_echo(card's SimAddr, now)` consumes the oldest unexpired record and
every other record of the same shot (a second output's send of it, so it
cannot become the next shot's echo); when records to more than one
simulator are unexpired, the oldest to the card's host is preferred (the
same address, or both this computer). The address only breaks ties: a lone
record is the echo whatever its address, since the card on the LAN address
and an output on `127.0.0.1` reach the same Pureplay. Records are kept for
sends to every simulator, GSPro and Muni Golf included: a lone record of a
GSPro send matches a Pureplay shot inside the window in Auto. A send
Pureplay never plays (it was not ready) stays until it expires.

#### Open Connect client telemetry

The Open Connect client (GSPro, Pureplay, Muni Golf or a generic simulator;
one actor)
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
client sends them as a `golfer.select` and shows them on its card. Until the 1 s refusal verdict the keys wait
and go out with `Connected`; after it, with a `Connected` status as they
change. When the connection ends, the status that says so removes the
connection's keys and `club` and `handed` (empty values). The first status of
a new instance sets `address` and removes every other key an earlier
instance left. Because a `Starting` or a lost connection drops the card's
telemetry, every `Starting` and `Reconnecting` status carries `address`,
`last_shot` and a held refusal `error` again, so a reconnecting card still
shows where it dials. Nothing here adds traffic to the simulator.

#### Pureplay output (alpha)

`actors/pureplay/` (actor `pureplay.N`, config `[pureplay.N]`,
`PureplaySection`, UI type label "Pureplay Connect") sends shots to Pureplay
over Pureplay Connect v1, Pureplay's published protocol: Pureplay is the host
and listens on TCP 9760, flighthook is the device and connects. The user
guide is [outputs/pureplay.md](outputs/pureplay.md), which also covers the
Pureplay (Open Connect) preset, the Open Connect client card on 921.

- `codec.rs` is the wire format, pure and unit-tested: frames (`"PL"`,
  version 1, reserved 0, type, u16 LE length, payload, CRC32C LE over header
  and payload; a table-driven CRC32C, no crate), `FrameReader` (splits frames
  out of the TCP stream; on a bad magic, a header with version 0, or a CRC
  mismatch it skips one byte and looks again, all skips of one call leaving
  the buffer in one drain, and hands back the skipped bytes for the log; a
  frame is read whatever its reserved byte, which the link logs once per link
  when it is not 0), and every v1 message type both ways.
  Payloads decode their known prefix and ignore any appended bytes;
  enumerated fields stay raw bytes, so an enumerator added later passes
  through. The spec's worked examples are golden tests, byte for byte.
- `mapper.rs` maps a finished shot (`map_shot`) to BallData and, when
  any club value or face impact was measured, ClubData with the same ShotId.
  A ValidMask bit is set only for a value present (and finite) in the shot's
  launch monitor's own `launch`, `club` and `impact` (a missing launch value
  may be filled from its flight's `assumed` outside strict mode); a flight
  result is never read. In strict mode the values go out as the launch monitor
  sent them.
  A total spin with no axis and no components also goes out as BackSpin
  (`flighthook::output_backspin`). Nothing is computed (no total/axis from
  the components, no smash factor):
  Pureplay fills those gaps. Speeds in m/s, impact in mm via
  `Distance::as_millimeters()`. Signs pass through unchanged for both hands:
  HLA, SpinAxis, FaceAngle and ClubPath are target-absolute on the bus and in
  Pureplay; face impact is golfer-relative (+ toe, + high) on both. Pureplay
  Connect v1 leaves SideSpin's sign undefined; it goes out with the bus's
  (+ curves right) through `mapper::side_spin` (`SIDE_SPIN_SIGN`), the one
  place to change it, to be confirmed against Pureplay. Pureplay's club numbers map to flighthook's clubs in
  `club_from_pureplay` (0 driver, 1-8 woods 2-9, 9-16 hybrids 2-9, 17-25
  irons 1-9, 26-29 PW/GW/SW/LW, 30 putter).
- `mod.rs` runs the link like the Open Connect client runs its connection:
  the mode is flighthook's turn mode when a shot finishes, the bus is drained
  the whole time (shots still unfinished 45 s after their trigger are let go,
  the shared `ShotAggregator` rule), the same backoff (1 s, +1 s, cap 3 s,
  reset after a 10 s link), the same per-mode routing by launch monitor actor
  id (`GsProRouting`, `Feed::passes`, Auto-Chip always
  accepted), shots queued
  only while the link is up and dropped when older than 5 s. A link: TCP
  connect (5 s), Hello (ProtocolVersion 1; DeviceCaps BatteryPowered when the
  monitor that stands for the device has reported `battery_pct`;
  DeviceName `flighthook (<monitor name>)` or `flighthook`; Firmware the app
  version), then HelloAck within 5 s, else the handshake fails with `error`
  telemetry. A HelloAck choosing any version but 1 fails it too. Then the
  status goes `Connected`, the ReadyState is pushed and every change after
  it, and each routed shot goes out as BallData (+ ClubData), ShotId
  counting from 1 on every link.
- The device's state comes from the routed launch monitors' `bus.actor`
  (blockers, battery) and card status (connection, `error`), and
  `shot.trigger`/`shot.finished`. ReadyState: NotReady while the monitor is
  not Connected or has blockers, Processing from a trigger until its shot
  finishes (at most 10 s), Ready otherwise; never BallDetected.
  ConnectionState: Connected, Connecting (`Starting`), Reconnecting, Error
  (an `error` key while not connected) or Disconnected. The monitor read is
  the one routed for the current mode; with Any, the best state of every
  monitor, and the battery and name of the only configured monitor, if there
  is one. During a round only the monitors the policy counts (measured ones)
  make the ReadyState and ConnectionState.
- Host commands: Ping → Heartbeat (ConnectionState, ReadyState, battery
  0-100 or 0xFF). SetClubInfo → one complete `golfer.select` (club and hand) on the bus, as a
  simulator's player information (unknown values logged and ignored), which the
  card re-asserts when flighthook's turn differs; the card shows the values as
  `club`/`handed`. SetShotContext: the putter selects putting, a distance under
  30 yards chipping, anything else full, sent as the mode in the select.
  SetDetect and SetBallType are logged only. SetStrictMode
  asks the round policy for a tournament round, below. Strict mode starts
  off on every link.
  Error from the host → a Warn alert, one per burst of Errors less than 30 s
  apart across links (not the StrictModeRequired that follows a refusal on
  the same link). Any
  other type, including a device-to-host one, or a payload too short for its
  type → Error code 0, and the link carries on.
- Audit log (`audit` target): every frame sent and received as its type and
  full frame hex, then the decoded message; bytes skipped while
  resynchronizing, as hex.

Strict mode on the link. SetStrictMode(1) asks the round policy for a
round (`RoundPolicy::begin_round`) with the holder's card name ("Pureplay
Connect" when the card has none, so the round never reads as the Pureplay
Accessory card's) and the launch monitors routed to the output
(`GsProRouting::feeds`: each mode's monitor; "Any" = every configured one
from the live config plus every one seen on the bus; a simulator source
never) and acks the answer: StrictModeAck(1, "") with the
`RoundGuard` held by the link, or StrictModeAck(0, reason) with strict mode
off and a Warn alert ("Pureplay strict mode refused: `<reason>`. Tournament
rounds take only shots a launch monitor measured."), one
per 30 s burst (Pureplay closes the link and every reconnect asks again).
SetStrictMode(0) → StrictModeAck(1), off. The guard drops with
SetStrictMode(0), a refusal, the end of the link or the actor's stop. While
it is held, a shot of a launch monitor that is not measured is not sent and
takes no ShotId (an info log line once per source per link), and every
`shot.flight` is ignored, so an estimated spin is never sent and nothing
waits for it.

#### Pureplay Accessory card (beta)

`actors/pureplay_accessory/` (actor `pureplay_accessory.N`, config
`[pureplay_accessory.N]`, `PureplayAccessorySection` = `name`, `enabled`,
`address`, `shots`, `impact_lead_s`; UI type label and card name
"Pureplay Accessory", under Software, (+) entry "Pureplay Accessory", on the
welcome screen among the launch monitors, not offered by the outputs'
routing dropdowns) follows a round in
Pureplay
through its Accessory API, Pureplay's published WebSocket API (version 1,
early access): `ws://<address>/` (default `127.0.0.1:9770`), subprotocol
`binary`. It is a public, documented API, like Pureplay Connect, so the card
needs no driver crate. The user guide is
[devices/pureplay.md](devices/pureplay.md).

- `protocol.rs` reads the messages (`hello`, `pairing.pending`, `paired`,
  `response`, `event` and the payloads the card uses; unknown fields,
  message types and events ignored; names compared without regard to case;
  `null` read as absent, a `null` list or flag as empty or `false`) and
  builds `pair`, `subscribe` and `state.get`.
  Pureplay's 31 club types map to flighthook clubs (`club`; flighthook's MD
  and 1H have none), `RH`/`LH` to the hand (`Unknown` to none). Every JSON
  example of Pureplay's document is a parser fixture (`fixtures.rs`).
- `token.rs`: the pairing token is kept in `pureplay-tokens.json` in
  flighthook's data folder (`SystemState::pureplay_tokens`, a `TokenStore`;
  tests use a fresh temporary file per state), never in the config file.
  Entries `{ token, clientId, paired_at }` are keyed by the card's address
  normalized (`flighthook::address_key`), so changing the address pairs
  again. Written whole and atomically (a temporary file, then a rename),
  readable by its owner only (0600 on Unix). A file that is not a token file
  reads as empty (warned once) and is copied to `pureplay-tokens.json.bad`
  before the next write replaces it. `Token`'s `Debug` prints `<redacted>`,
  and every frame's text has each token seen (as written and JSON-escaped)
  replaced by `<redacted>` before it is logged or put on the bus (`redact`),
  the rest byte for byte the wire text. A `paired` frame's token is learned
  from the frame's JSON before the typed parse, so a `paired` that does not
  parse is still logged redacted; a frame that is not JSON is logged by its
  length only. Parse errors and response texts are redacted too. The app's
  log filter holds tungstenite at `warn` (its trace lines print whole
  frames).
- `model.rs` keeps the round: per player `isLocal`, the profile slot, name,
  handedness, held club, bag, ball and distance to the hole, the active player
  (`turn.activeShooterId`), the hole and the readiness, from the snapshot and
  the `player.*`, `turn.changed`, `hole.started`, `shot.ready_changed` and
  `context.invalidated` events. For the **active local player** only (a
  remote player's club, hand and turn give nothing) it holds the turn the card
  wants: the player's golfer id, club, hand and mode, sent whole in one
  `golfer.select` when Pureplay reports a change (snapshot, `turn.changed`,
  `player.changed`, the player's `player.club_changed` whatever its `reason`, a
  `shot.initialized` distance that changes the mode) and flighthook's turn
  differs from it, and again whenever flighthook's turn differs from it, unless
  a game or simulator card caused that turn. The golfer differs when the turn's
  golfer and the player's id do not resolve to the same golfer (the lookup
  rule). The mode is the shared club + distance rule: the putter putts, a
  distance to the hole under 30 yards chips, anything else is a full swing.
  `player.changed` clears, on an explicit `null`, only the identity fields it
  reports (profile, name, hand, team, handicap). A snapshot of the results
  screen (`matchActive` false for the match that just ended) brings no players
  and no shooter. While a club pick is outstanding (`Model::hold_club`) the
  card neither sends nor compares the club; while a `golfer.select` awaits its
  reply the turn on the bus is not final and nothing is compared with it. A new
  `serverInstanceId` (Pureplay restarted) drops the model.
- `game.rs` publishes the round on the bus as the `game` actor: every player as
  `golfer.profile` `pureplay.<name>` (a player without a name as
  `pureplay.player-<playerId>`; name, hand, bag), the match as `game.round`,
  the ball about to be played as `game.ball` and the scores as `game.scores`.
  A player who leaves, `match.ended` and `context.invalidated` remove what they
  published. `game.aim` is answered `unsupported`.
- `mod.rs` runs the connection on the actor's thread with tungstenite's
  blocking client over `connect_tcp` (5 s), a 100 ms read timeout so the
  bus is drained throughout, the WebSocket handshake included (resumed
  after each poll, 2.5 s at most). It waits for `hello` (5 s), checks
  `protocol` = `ppaccessory` and `version` = 1, sends `pair` (`client`
  "flighthook", `version` the app version, the saved token or `null`) as its
  first message, then `subscribe` (the events of its list that
  `hello.capabilities.events` names, all of them when it names none; a list
  naming none of them is an `error`, "Pureplay offers none of the events
  flighthook follows", retried every 30 s) with `snapshot: true`, and is
  Connected once the snapshot is applied. A gap in the events' `seq` is
  counted (`missed_events`) and, like `context.invalidated`,
  `match.started` or an event naming a player the card does not hold
  (`turn.changed`, `shot.initialized`, `shot.launched`,
  `player.club_changed`), asks for `state.get`: one unanswered at a time
  (answered by the snapshot with its `requestId`), at least 1 s apart. A
  player a snapshot asked for did not bring is not asked for again until
  `match.started`, `context.invalidated`, `player.joined` or a reconnect. A
  request that gets no snapshot (no answer in 5 s, or an error response to
  its id such as `rate_limited`) marks nobody: it is sent again after 5 s,
  10 s, then every 30 s (or the response's `retryAfterMs` when longer) until
  a snapshot comes, asking for its players and any named meanwhile; a player
  named while a request is out is asked for by the next one. A `seq` lower than
  the last is logged. With nothing received for 20 s the card sends a
  WebSocket ping; after two unanswered (60 s quiet) the link is lost (`PING`
  switches this off). It keeps `hello.capabilities.commands` and the
  pairing's scopes (both logged). It sends text frames and reads binary and
  text (`SEND_BINARY` switches what it sends). Its only commands are club
  out's.
- Club out (`club_out.rs`, `ClubOut`): a `golfer.select` carrying a club that
  the card drains while following the round is a pick when its sender is
  neither the card nor another game or simulator card (the header, the API,
  random club, a launch monitor or a third-party tool; a simulator's club is
  Pureplay's own coming back; `SystemState::is_game_card`); the latest pick
  of a drain is taken. Usable (`club_out` `on`) with `control` in the
  pairing's scopes (Pureplay's prompt grants `read` for **Read Only** and
  `read` + `control` for **Aim and Club**; `scopes: null` grants nothing),
  `club.set`, `club.next` or `club.prev` in `hello.capabilities.commands`,
  the master switch not known off (`controls_disabled` in the readiness,
  or `forbidden_scope` on a command until a later readiness is
  `controllable` without `controls_disabled`), and, while a local player is
  up, a bag for that player (none or empty: `not supported`). Each pairing
  sets the grant again (`ClubOut::link`), so a new pairing with another
  grant changes `club_out` without a restart. A pick is for the active
  local player (none up: ignored, nothing put back) and resolves to the
  first bag entry of its type, looked up when the command goes; a type not
  in the bag drops it. While the readiness is not `controllable` or names
  no player or another player, the pick is pending (`PICKS_WAIT`, the one
  switch: `false` drops it). A readiness that is `controllable` and names
  the player but does not list the command drops it at once ("Pureplay
  does not take club changes here"). When the command could go, Pureplay
  holding the pick's type already sends nothing. `club.set { playerId,
  clubId }`, or without `club.set` `club.next`/`club.prev { playerId }` steps
  toward the type (direction by bag position, one step in flight, at most
  the bag's length, each reply naming the club reached; a reply naming the
  club held before the step (by id, else by type) did not move: the
  direction turns once when the other command is offered, else, or the
  second time, it gives up; a newer pick takes over at the next reply, from
  the club reached). Ids `c-N`, one command in flight; picks meanwhile
  replace the pending one. The pending pick is dropped on `turn.changed`
  to another player, `context.invalidated`, the connection's end (so a new
  `serverInstanceId`), a snapshot naming another active player, and the
  player's `user_selected` change; a `turn.changed` naming its own player
  (a new hole in a one-player round) keeps it for the new turn, a command in
  flight included. A turn change while a command from an earlier turn is in
  flight drops nothing. An `auto` change keeps the pick, and an `auto`
  change for the shot (readiness `shotId` at the send) a `club.set` just
  succeeded for re-sends the pick once (a second is accepted), unless a
  newer pick or the player's `user_selected` change came since; an `auto`
  change seen while a command is in flight came before that command and is
  not answered. Replies (matched by `id`): success updates the model with
  the reply's `club`/`clubId`; `not_ready` → pending again, sent at the next
  readiness (`shot.ready_changed` or a snapshot) or after 1 s
  (`NOT_READY_RETRY`), whichever comes first, at most three times
  (`NOT_READY_RETRIES`), then dropped; `rate_limited` → one retry after
  `retryAfterMs` (100 ms when it names none); any other refusal, or no reply
  in 5 s (`COMMAND_TIMEOUT`), drops it, and a reply after that is ignored.
  While a pick is pending or in flight for the turn (`ClubOut::holds_club`)
  Pureplay's club changes stay off the bus (the steps' and the command's
  own `api_command` changes included); when a pick ends (applied, refused,
  dropped, or not usable), `Action::SnapBack` puts Pureplay's club on the
  bus when flighthook holds another (`Model::sync`) as the card's complete
  select.
- Shots (`shots.rs`): the card is a virtual launch monitor and a game
  (`ShotSource::PureplayAccessory`, declared through `Actor::source()`, source
  `virtual`). It never joins a round, never blocks one (it feeds no simulator
  output) and does not ask `allows`/`send_if_allowed`. A `shot.launched` (live
  only, never from a snapshot) is a candidate when its `playerId` is a local
  player. The Shots setting, read from the config at each shot
  (`shots::decide`): Auto takes the candidate unless `sent_shots.take_echo`
  finds the send it echoes; "From Pureplay" takes every candidate and matches
  nothing (`shots::matches_echoes`, the one switch); "From flighthook" takes
  none and still matches. A taken shot gets a fresh UUID and goes on the bus as
  `shot.trigger` (no timestamp: Pureplay's clock is not flighthook's; the
  camera dates it on arrival and the impact lead, default 0.9 s, covers the
  delay), `shot.launch` (`ballSpeedMs` m/s, `launchAngleDeg`, `directionDeg` +
  right, spins rounded, `sideSpinRpm` times `shots::SIDE_SPIN_SIGN`; no flight,
  club or impact) and `shot.finished`. A matched echo puts nothing on the bus.
  A `shot.launched` whose `shotId` the card already holds is logged and not
  taken again. `ShotMap` keeps Pureplay's `shotId` against the `shot_id` of the
  shot it stands for (a taken shot or a matched echo; the newest 256), kept
  across a reconnect to the same `serverInstanceId` and cleared on a new one.
  `shot.completed` finds its shot there (never by time) and goes on the bus as
  `game.stroke` (stroke id, shot id, golfer, hole, stroke number, the launch
  values flown, a modeled flight with Pureplay's carry, total and apex, the
  outcome `in_play`, `green`, `holed`, `out_of_bounds`, `water`, `lost`,
  `unplayable`, `gimme`, `auto_putt` or `penalty_only`, the putt and penalty
  strokes, lie and distances to the hole); a Gimme, AutoPutt or PenaltyOnly has
  no shot id. `shot.cancelled` `discarded` (a scramble shot the team did not
  pick) sends no stroke and retracts none. `score.updated` is `game.scores`. A
  remote player's shot is logged and stored nowhere. The `ShotMap` and the
  card's shot count belong to the actor: the card's Reconnect, an address
  change or turning it off and on starts a new one, which forgets them.
- Readiness: the card's `bus.actor` `blockers` are Pureplay's readiness
  blockers (empty when `readiness.ready`; from the snapshot and
  `shot.ready_changed`); the first carries the device identity (manufacturer
  "Pureplay", model "Accessory API", firmware `gameVersion`). A connection that
  said ready and ends publishes a blocker again.
- Connection and pairing: refused or dropped without a close frame →
  Reconnecting, backoff 1, 2, 4 ... 30 s (reset once a connection has held
  30 s), the token kept, an `error` from Pureplay (version, events) cleared.
  `4429` before `hello` → Reconnecting, `pairing` "Pureplay has too
  many apps connected", backoff. A `hello` of another protocol or version →
  Reconnecting, `error`, retried every 30 s. Waiting in Pureplay's queue,
  then `pairing.pending` → Starting, `pairing` "Waiting for Pureplay", then
  "Approve flighthook in Pureplay (42 s)" counted down once a second from
  `expiresInSec`; only a `pair` without a token waits without limit, one
  with a token gives up after 5 s (Reconnecting). `bad_request` to a `pair`
  without a token (another app on the address is waiting) → the card
  closes, Reconnecting, retried in 10 s; the sixth in a row → Disconnected
  with Pureplay's `message` in `pairing`.
  A refusal, told apart by the response before the close and the close code:
  `forbidden_scope` + `4403` ("Turn on Ask about new apps in Pureplay's
  Settings > Accessories"), `unauthorised` + `4403` ("Pureplay turned the
  request down"), `unauthorised` + `4408` ("The request timed out in
  Pureplay") → Disconnected, no retry. `rate_limited` + `4429` (the cooldown)
  → Disconnected, "Pureplay asks to wait (1:42)" counted down from
  `retryAfterMs`, then the same attempt once more; a second cooldown answer
  counts down to "Press Reconnect to pair again.". `unauthorised` echoing the
  `pair` + `4401` with a saved token → the token is deleted and the card pairs
  once without one, when this actor start has not sent its one `pair`
  without a token yet (else "Removed in Pureplay. Press Reconnect to pair
  again."). After a removal in Pureplay that `pair` meets Pureplay's
  cooldown, so the card counts the wait down and then asks once more, which
  shows the prompt. An unprompted `unauthorised` (`id: null`) then `4401`
  while connected (removed in Pureplay) → the token deleted, Disconnected,
  "Removed in Pureplay. Press Reconnect to pair again.", no retry. When the
  socket ends without a close frame (or none comes within 1 s), the refused
  response decides: `unauthorised` to a `pair` without a token → "Pureplay
  turned the request down", with a token → the dead-token rule,
  `forbidden_scope` → the Ask-about-new-apps text, `rate_limited` → the
  cooldown, an unprompted `unauthorised` while connected → the removal. A
  `pair` without a token is sent by itself at most once per actor start;
  after that only a Reconnect (a new start) sends another.
- Telemetry (card status rows, each key only while it has a value): connected,
  `name`, `club`, `handed` (the active local player), `hole`, `blockers`
  (while the readiness is not ready: its blockers, `controls_disabled` not
  one), `club_out` (`on`, `read only`, `off in Pureplay`, `not supported`;
  see Club out above),
  `shots` (`auto`, `auto: last from Pureplay`, `auto: last from
  flighthook`, `from Pureplay`, `from flighthook`), `missed_events`,
  `pureplay` (`gameVersion`); `last_shot` (`#12 at 16:41:07`, the shot the
  card took last) for the actor; otherwise `pairing` and `error`. Every
  status change carries every row; between changes only what changed. The
  rows are checked on every pass of the follow loop (100 ms), so `shots`
  follows a Shots setting change while Pureplay is quiet.
- Forget pairing: `POST /api/actors/{id}/forget-pairing` deletes the card's
  token (on a blocking worker), then reconnects it through the reconnect
  path; a reconnect of the card already in flight answers 409 first and
  keeps the token.
- Audit log (`audit` target): every frame sent and received as its frame
  type and its JSON text, tokens redacted; close frames with their code.
  The messages a frame produces carry that text as their raw payload. Each
  shot decision is logged: taken (with the card's shot number), the echo of
  which flighthook shot, or not taken and why. Each club pick is logged: sent,
  waiting, set, refused (with the code) or dropped and why.
- The game's view of a shot attaches in the collector: a `game.stroke` joins
  the stored shot with the same `shot_id` whenever it arrives (a shot the
  history no longer holds is skipped). The Shots tab does not show it; the JSONL
  export carries it in `data.stroke`, CSV does not. `/frp` forwards it like any
  FRP message. The export's device kind is `pureplay`.

### Tournament rounds

A tournament round is system-wide policy, not an output's: `state::round`
(`SystemState::round`, a `RoundPolicy`). Pureplay Connect's strict mode asks
for one, the Tournament toggle in the header sets one by hand, and nothing in
the policy is Pureplay's.

- **Source.** A launch monitor's `source` (FRP's `device.source`) says where a
  shot comes from, nothing more: `measured` (a launch monitor measured it: a
  driver reads the hardware, or a monitor's own app sends it to the Open
  Connect input) or `virtual` (the trackball, the mock monitor, Auto-Chip and
  the Pureplay Accessory card). Every local shot source is an
  `actors::provenance::ShotSource` (`Mevo`, `R10`, `Square`, `ExPutt`,
  `Trackball`, `MockMonitor`, `OpenConnectServer`, `AutoChip`,
  `PureplayAccessory`) whose single exhaustive match gives the source;
  `is_simulator()` (the Pureplay Accessory card) keeps a source out of every
  simulator output. An actor type declares its source (`Actor::source()`;
  `None` for actors that put no shots on the bus), and `start_actor` registers
  it in the policy by actor ID. A remote launch monitor's source is read from
  its `bus.actor` (unknown counts as not measured).
- **Simulator sources.** The Pureplay Accessory card is `virtual`, so it never joins a
  round; it never blocks one either, because no simulator output counts it
  among its feeds (see the Open Connect client's routing rule), which is a
  routing fact, not a round special case; and it is not switched off
  during a round, since a simulator's own shots are not flighthook's to
  switch off.
- **Acceptance.** `begin_round(&RoundRequest { holder, feeds })` accepts
  when every launch monitor in `feeds` is measured by its registered
  source; otherwise `Refusal` names the first, in actor ID order: "`<name>`
  is a virtual launch monitor", or "`<name>` is not a launch monitor
  flighthook reads" (no registered source); a blank name gives the actor
  ID. A source removed from the config or turned off is forgotten
  (`RoundPolicy::forget_source`, from the system actor once the removal is
  done), so a monitor that is gone or off feeds no output: Pureplay drops a
  seen monitor that is not a known source, and counts a mode routed by ID
  only to a known monitor. No launch monitor at all is accepted: nothing
  unmeasured can reach the output.
- **The guard.** `RoundGuard` ends the round when it drops. Several holders
  may hold rounds at once; the round is active while any does, and the
  first still held names it.
- **Tournament mode.** `RoundPolicy::set_manual(on)` is the manual holder,
  named `"Manual"` (`round::MANUAL_HOLDER`). It feeds no output, so there is
  no acceptance check and no refusal: it only turns the virtual producers
  off. A simulator's round on top of it is named by the simulator, and its
  end falls back to the setting. The setting is `[simulation] tournament`:
  `SystemState::new` applies it at start-up, `POST /api/tournament` writes
  it through `ConfigAction::SetTournament` (applied before the outcome goes
  out), and the system actor applies the saved value again after every
  config command.
- **Which shots count.** In a Pureplay Connect strict-mode round the output
  sends only the shots of measured launch monitors and ignores every
  `shot.flight`, so it never sends a simulated or estimated value. It sends
  `launch`, `club` and `impact` as the launch monitor sent them, so the
  values a real launch monitor reports (the R10's back and side spin, the Xi's
  spin from its early result, the Omni's vertical impact, the Open Connect
  input's completed spin pair) go out.
- **Readiness.** `RoundGuard::counts(source)`: only a measured launch
  monitor counts for the holder's ready state and heartbeat, remote ones
  included.
- **Virtual producers.** Auto-Chip (`autochip::send`, the single firing
  point) and the mock monitor send a shot's trigger through
  `RoundPolicy::send_if_allowed(Source::Virtual, ..)`, which runs the
  send only while no round is active (tournament mode on counts as active) and holds the policy while it runs, so
  no round can begin between the check and the trigger. A remote launch monitor
  that declares itself virtual is held the same way
  (`RoundPolicy::admits_remote`): while any round is active, its
  `shot.trigger` and the rest of that shot are not put on the bus. Auto-Chip also asks
  `RoundPolicy::allows` before its solve, to answer at once. Refused, it
  returns `SolveError::RoundActive`, which `POST /api/auto-chip` answers
  with `409 {"error":"Auto-Chip is off during a tournament round"}`; nothing is put on the bus and no number is taken. A device
  driver of a virtual source (the mock, the trackball) asks
  `Events::shots_allowed()`, which its `DriverActor` sets from
  `RoundPolicy::allows` before every poll, before it numbers a shot: a held
  shot takes no number; the mock's fires once the round ends, a swing on
  the trackball during the round is dropped. The actor logs "<id>: no shots during the
  tournament round" once per round, and sends each trigger through
  `send_if_allowed` as the backstop: a shot that starts anyway is dropped
  whole.
- **State.** `RoundPolicy::state()` is a `flighthook::RoundState { active,
  holder, manual, held }` (`manual`: tournament mode is on; `held`: a
  simulator holds the round). `main` attaches the bus, and every change goes out as a
  local `RoundState` event; `GET /api/status` carries it as `round` (omitted
  while inactive) and `/api/ws` sends it in the snapshot and on every change.
  The UI keeps it in `FlighthookApp::round`: the header's
  Tournament icon (off, on, or held with a lock) and center title show it,
  and the Auto-Chip icon is disabled from it. A click on the icon sends
  `POST /api/tournament` while off or on, and opens the held round's card
  (no request) while a simulator holds it; the icon waits while Settings has
  unsaved edits. A dashboard connection that closes or fails resets the round, so it does
  not outlive the server.

Pureplay card rows (each key only when it changes):

| Key | Value | Example | Holds for |
|---|---|---|---|
| `address` | Pureplay's address | `127.0.0.1:9760` | the actor |
| `last_shot` | the ShotId sent last and the local time | `#4 at 16:41:07` | the actor |
| `shots_sent` | shots sent on this link | `4` | the link |
| `ready_state` | the ReadyState last sent | `ready` | the link |
| `strict_mode` | `on` / `off` | `off` | the link |
| `error` | why the handshake failed | `no reply to Hello from 127.0.0.1:9760 within 5s; ...` | until a handshake succeeds or a connect fails another way |

As for the Open Connect client, a connection refused or closed is a status,
never an `error` or an alert, and `club`/`handed` are removed when the link
ends. The state of a launch monitor is learned from its `bus.actor` (every local
actor publishes it each heartbeat), so an output that starts while a monitor is
already connected learns its state within one heartbeat.

## Device drivers

A launch monitor card can run on a flightrelay device driver
(`flightrelay::driver::DeviceDriver`): the FlightScope Mevo+, Gen2 and Xi
(`ironsight::driver::Mevo`), the R10 (`tenover::driver::R10`), the Square
Golf Omni (`allsquare::driver::Omni`), the ExPutt
(`threeputt::driver::ExPutt`), the trackball
(`thumbswing::driver::Thumbswing`) and the mock
(`flightrelay::mock::MockDriver`) do. The driver owns the device (connect,
session, shots, telemetry, raw frames); `actors::driver::DriverActor<D, H>`
is the host around it, one thread per card. What a card adds is its
`Hooks<D>`: its config section (`H::Section`, a lib `DeviceSection`), its
shot source (`H::SOURCE`) and optional processing (`on_frp`, `action`).
The FlightScope's are `actors::mevo::MevoHooks` (its `on_frp` estimates a
launch-only chip's spin and publishes it as the card's own `assumed` flight),
the R10's `actors::r10::R10Hooks`, the Square Golf's
`actors::square::SquareHooks`, the ExPutt's `actors::exputt::ExPuttHooks`
(its `action` is Find mat, its `on_frp` puts the driver's discovery hint in
the card's words), the trackball's `actors::trackball::TrackballHooks` and
the mock's `actors::mock::MockHooks` (no processing of their own).

**A card that cannot connect says why.** While connects keep failing the
card shows the latest connect error (the driver error's text) as its `error`
row, through the retry delays too, and the log gets one `cannot connect`
warning when the failures begin and another whenever the text changes, not
one per retry. The row is part of the card's status, so a UI opened later
reads it from `/api/status`; it goes when a session connects (the log then
says it connected after failed connects) or the card stops. A driver's own
alerts are logged as well (an alert that repeats the last one, as a
driver may raise on every retry, once).

**Config.** A `DeviceSection` is flighthook's own fields (`name`,
`enabled`, `impact_lead_s`) with the driver's config `#[serde(flatten)]`ed
next to them, so the file and the REST JSON hold one flat table per card;
every driver field is left out while at its default. The actor builds and updates the driver from
`DeviceSection::driver_settings()`: the section's driver config as it is,
except where the host decides a setting itself (the Trackball's grab is
always off). `FlighthookConfig::device_error()` runs every
card's `DriverConfig::validate` and names the card and the field
(`mock_monitor.0: shot_interval_s: must be 0 to 86400`, cards in number
order). A save that changes a card's driver settings into refused ones is
`400` (`changed_device_error` against the saved config); a save that keeps
a card's refused settings as they are goes through, so the card still
saves its other fields and its on/off switch. A file that fails it loads
as written, logs the error and that card does not start (its card shows the
error until it is fixed). The card form follows the same rule: the refusal
shows under the card's fields, and only a change into refused settings
disables Save.

**Rule: every field a driver's `validate()` can refuse is editable on that
card**, so a refused value can always be fixed from the UI. Each card on a
device driver is checked against it when it moves onto `DriverActor` or is
added (the trackball: every number its `validate()` checks is a field on its
card). (The mock's driver fields are file and API only.) A type error in a
driver field, which serde reads through the flattened buffer, is located by
re-reading each section into the driver's config, so the load error names
the key's line. Tests keep host keys and driver keys apart and check that a
card's source class is its driver's declared source.

**Run loop.** `D::new(config)`; a refused config sends Disconnected with
the error as the card's `error` and the actor waits for its stop. Then
Starting (once), and per attempt `connect` with a `ConnectCtx` whose host
is the actor: flighthook's own `golfer.turn` becomes the driver's `TurnHints`
(club, hand, mode), handed to a connect through `hints()` and to the session
whenever the turn changes (each turn is complete), and the card's actions are
queued (actions still queued when a connect starts were asked while no session
ran, and are dropped). A session: Connected with `device_info` (the identity's
label) and `shot_count`, then a `bus.actor` carrying the identity and
`device.source` before any shot; each pass drains the bus into the hints and
the card's `action`s, hands a live config to `apply_config`, sets `wants_raw`
(always) and `shots_allowed` (the round policy), and polls for 50 ms. A
session error sends `not_armed` and then Reconnecting;
the driver's `retry_delay` (its own alerts, an SDK `Backoff`) is slept with
the bus drained; `None` gives up with Disconnected and the error. A stop
closes the session (`close`) and ends Disconnected. A panic in the driver
(any of its calls) is caught on the actor's thread: `not_armed`, then
Disconnected with `error = "the device driver failed; press Reconnect"`,
and the thread ends, so a card never stays Connected over a dead driver. `stop_wait()` is
`D::STOP_WAIT`; a driver whose device takes one connection at a time
(`exclusive`) is not replaceable while stopping.

**Events.** Each driver event goes on the bus under the card's actor id as soon
as the driver gives it; nothing is held:

| Driver event | Bus |
|---|---|
| `shot.trigger` | `shot.trigger` (with the driver's timestamp) through `RoundPolicy::send_if_allowed`; opens the raw capture window at the trigger's time; refused, the whole shot is dropped |
| `shot.launch`, `shot.flight`, `shot.club`, `shot.impact` | the same messages, after the card's `on_frp` hook (the FlightScope hook may add its own `assumed` flight) |
| `shot.finished` | the shot's raw frames as the local `ShotCapture` (`"<kind> device has no wire protocol"` without a raw profile), then `shot.finished`, then the card's `shot_count` |
| `alert` | `bus.alert` |
| status and identity | the card's `bus.actor` (blockers, battery, attitude, telemetry, device), published when it changes |
| `raw_frame` | numbered per device id across sessions (frames of the first connect, before the id is known, number on into it), a hex line at the `audit` target, recorded for the export ([raw capture](#raw-capture-and-shot-export)); never on the bus |
| `Phase` | the card's `phase`; the Connected status removes a phase set during the connect (`phase: ""`), unless the connect's last phase was `measuring` (the session's own phase in the driver contract's list), which it keeps. A connect that fails removes its phase before the retry delay, and a stop during a connect leaves the card Disconnected without it |
| `Log` | tracing; trace lines at the `audit` target |
| `ShotDiscarded` | the pre-roll is cleared |

**Live config.** `reconfigure` reads the card's section: unchanged or a
host field only is Applied; a driver change that `restart_needed` calls
live (and that validates) is Applied and handed to the running driver;
anything else, a refused config or a removed section is RestartRequired.
The actor's thread takes an Applied section at its next pass whatever it is
doing: between session polls, before each connect, every 100 ms of a retry
delay and while it waits for its stop, so a change takes effect at the next
shot or connect (the hooks see the new section at once).

## Cameras and replay

USB cameras record a clip of every swing; the Shots tab replays it beside
the shot. Capture, the pre-roll and clip cutting are the
[stance](https://github.com/divotmaker/stance) crate's; flighthook
supplies the triggers, stores the clips and serves them. Frames never travel
on the bus: only one local `ShotVideo` event per finished clip.

### Camera actor

One `CameraActor` (`app/src/actors/camera.rs`) per `[camera.N]`, on its own
thread. It opens its source with stance's `Capture::start` (which
blocks while the camera opens; the camera then runs on a capture thread of
its own, frames going into a bounded ring) and loops:

- every waiting bus message is read first; each `shot.trigger` becomes a
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
- every 2 s, telemetry (`fps`, `dropped`, `position`) goes out in a
  card status, only when it changed; `fps` moves only when the measured
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

The Cameras tab's Live view shows each camera live through the actor's **live slot** in
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

The **Cameras tab** (`ui/src/panels/cameras/`) is the second nav icon, enabled
when the server can record from cameras (`cameras_supported`, as for the
Settings Cameras section's (+)) and the saved config has a camera
(`FlighthookApp::cameras_shown`); without one its hover text says so. The Shots
and Cameras tabs share one shots navigation (the left column: shot list, view
selector, device filter, export, Clear history, New set, New session), so the
selected shot, the scroll position, the grouping and the device filter are the
same in both. The Cameras tab has its own small toolbar: an on/off **Live**
switch (`render_live_toggle`, `theme::switch_pill`) followed by the **Tour |
Amateur** reference picker (`render_reference_picker`,
`FlighthookApp::body_reference`, kept in local UI storage under
`body_reference`). The Shots tab keeps its own Cards | Table choice. One helper
says what the tab lays out, `FlighthookApp::shots_layout` -> `ShotsLayout`
`Cards | Table | Cameras`; the panel and the card clips go by it. The Cameras
tab takes everything right of the shots navigation: the Dispersion column and
the bottom chart row are not laid out, and the card row's camera clips are
dropped. Live is part of the Cameras tab: picking a shot leaves Live, turning
Live off shows the shot the list shows, and a new shot keeps Live (the newest
shot is selected underneath).

Each time the tab comes on screen (`sync_cameras_pane`, comparing with
`cameras_pane_was_shown`, called at the start of each update), the shot focus
follows the latest shot again (any pin dropped) and Live is turned on exactly
when there is then no shot to show under the device filter. Clear history marks
the tab as not shown, so a tab on screen turns Live on.

- **Live's Last shot strip** (`panels/last_shot.rs`, alpha): while Live
  shows, `sync_last_shot` follows the newest shot under the device filter
  (`shots::latest_index`) with a `LastShot` (its own request number from
  `replay_seq`): each camera's `.swing.json` once its `video[].swing` is
  set, then its grades, again on a new `grades_key` (`Fetch::Swing` /
  `Fetch::Grades` through `net::fetch_swing` / `net::fetch_grades`; replies
  routed by number through `LiveStrip::find_mut`). It is shown once a camera
  found a swing and its grades are in (`ready`), and keeps showing until the
  next shot's is (`LiveStrip { shown, next }`); nothing while body tracking
  is Off. `render_last_shot` (mark `live_last_shot`, `LAST_SHOT_H` reserved
  under the pictures before the column width is fitted): the title
  (`Last shot · #12 · DR`), the tempo tiles and the six body tiles from the
  same `tempo_values` / `frame_tiles`, each camera at its own impact event
  (so graded), and the miss line. No clip is fetched.
- **A shot** (`render_shot_cameras`): the shot's header (number, LATEST /
  Back to latest, Replay), then the inline player (`FlighthookApp::
  shot_player`, a `replay::ReplayView` made `with_swing`, kept while the
  shown shot and its cameras stay the same): the clips side by side, each
  picture at most 62 % of the pane's height, the replay's scrubber and
  transport across them, looping from the start point with the skeleton.
  Besides the green start tick and the gold impact tick, the scrubber has a
  small gray tick for each swing event the reports found other than impact
  (`swing_markers` in `panels/replay.rs`: address, takeaway, lead arm back,
  top, lead arm down, finish),
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
  frame (none outside the series) and the centimeters from `scale`. Several
  reports of one view average; a tile with no value is a dash. Formatting is
  the metrics table's (`metric_shown`). A line under the tiles names the
  tiles off their band (see **Grades** below). Then under each column that
  camera's metrics at address, top, impact and finish, and lead arm back and
  lead arm down too in a column 560 points or wider (`table_positions`; a
  heading that doesn't fit its column takes two lines)
  (degrees whole; `torso` metrics in inches or centimeters by the Units
  switch when the report has `cm`, else as a share of the torso). The
  shifts from address show their magnitude and a direction word
  (`direction_word`, none when the value shows as 0): `hip_sway` and
  `head_lateral` (signed toward the target) from `signs::target`
  (`Toward` / `Away`), `early_extension` (signed toward the ball) from
  `signs::ball` (`Toward ball` / `Back`) and `head_vertical` (signed up)
  from `signs::rise` (`Up` / `Down`), painted after the
  value on its baseline at the Cards view's sign-word size, shrinking to the
  cards' floor before the numeral does (`worded_value`, shared by the tiles
  and the table). A report
  whose `impact_source` is `given` (no swing found near the impact) shows
  only the line "No swing found in this clip" there, in the clip status
  lines' style. Over a Golfer Front camera's numbers, when its saved
  `|yaw_deg|` is 5 or more, one muted line (mark `yaw_warning:<camera>`):
  `Camera 7° off square: one-camera turn and sway are approximate` (the yaw's
  size, rounded).
  **Grades** (alpha): once a column's swing report is in, the player
  fetches `GET /api/shots/{shot_id}/grades/{camera}` (`Fetch::Grades`,
  `ReplayReply::Grades`), and again whenever the saved Golfers card changes
  (`SettingsForm::grades_key`, `ReplayView::set_grades_key`; the grades
  shown stay until the new ones arrive); `PlayerColumn::grades`. The pane
  reads the picked reference's `ReferenceGrades` and colors by `Fit`
  (`grade_color`: `theme::GRADE_BOTH` aqua = `TEAL`, `GRADE_FADE` blue
  (120, 170, 255, apart from `HEADING`), `GRADE_DRAW` green = `ACCENT`,
  `GRADE_NEAR` yellow = `GOLD`, `GRADE_FAR` red = `DANGER`; ungraded stays
  `TEXT`; `worded_value` lays text out uncolored and the painter gives the
  color). Tiles (`Swing { report, t_ns, grades }`, `frame_tiles`,
  `tempo_values`): a frame tile is graded only when its camera's frame is a
  key event's frame of that report or one frame either side
  (`key_position`, by series frame), with that position's grade; several
  cameras' tile takes the grade they share (`shared_grade`: all graded, same
  fit), else none; the tempo tiles take the face-on cameras' shared grade,
  else the first camera's in column order (Backswing is never graded).
  `graded_shown`: a graded turn's word is `signs::turn` (Open/Closed) of the
  signed value, a shift keeps its own word, any other value off its band
  takes `signs::direction` of the grade's `direction`. The miss line
  (`miss_entries`, `miss_job`, `miss_line`, mark `frame_misses`; 13 pt, at
  most two lines) lists the tempo and frame tiles off their band (not
  `Both`): the tile's name muted, then the grade's `word` in its fit's
  color, joined by ` · `; while the shot has grades a line's height is kept
  even when it is empty, so the tables hold still as the playhead crosses a
  key frame. Table cells (`metric_rows` -> `Cell { shown, grade, worded }`)
  are colored by their position's grade; a graded cell's hover text
  (`band_tip`, `band_text`; marks `cell:<camera>:<row>:<col>`) is the
  reference and its band (fade and draw bands for the shape-linked rows,
  inches in the Units system, rounded after the conversion), the value as a
  tile reads it, and the words.
  The table has Hip turn and Shoulder turn and leaves out
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
  waits 1 s. The views exist only while Live is on screen (the Cameras tab,
  Live on, the window not minimized, not a
  hidden browser tab); otherwise they are dropped and their late replies
  ignored.
  A camera that is not Connected shows its Settings card's badge and `error`
  instead of a picture. Each picture carries the skeleton of the pose that
  came with it, and under it the posture tiles of the camera's view from the
  same `X-Pose` (golfer_front: Side bend, Shoulder tilt, Lead elbow;
  target_line: Forward bend, Lead knee flex, Trail knee flex, Trail elbow;
  none for
  golfer_back and overhead). A readout line under the row names the body
  tracking values and each camera's live rate and pose time (see
  [Body tracking](#body-tracking-alpha)).

### Frame restamping (latency)

`[camera.N] latency_s` (`Option<f64>`, 0 to 10 s, serde-omitted when unset =
0; out of range is refused with `400` on save and ignored with a warning at
load) is how long a frame is old when it reaches flighthook, for a camera
that delivers late (a buffered network stream). Every frame's host
timestamp, which the platform sets to its arrival, becomes
`arrival − latency` (`camera::Restamp`; saturating at 0, the device time
moves with it) before it reaches the `ClipRecorder`; the still slot carries
no time and takes the frame as it is. Triggers are dated in real time too
(their timestamp, or when read), so frames, triggers and impact estimates
share real time: the clip window, the
sidecar times and the replay's sync between cameras need nothing else. The
latency is read from the live config on every capture pass (`reconfigure` is
`Applied` for a latency-only change; the camera is not reopened).

### Rotation, yaw and pitch

`[camera.N] rotation_deg` (`u16`: 0, 90, 180 or 270, serde-omitted at 0),
`yaw_deg` and `pitch_deg` (`Option<f64>`, -30 to 30 degrees, serde-omitted
when unset = 0) are validated like `latency_s`: `rotation_error` /
`yaw_error` / `pitch_error`, all four reported by `CameraSection::error(id)`
and `FlighthookConfig::camera_error()` (the first camera by index), `400` on
save, not in effect (0, a warning in the log) from the file. `reconfigure`
is `Applied` for a change of any of them. Clips are stored as the camera
sends them; the rotation is metadata applied where a frame is analyzed or
drawn:

- **Clip pass**: `pose::clips::clip_config(model, rotation)` puts
  `SystemConfig::camera_rotation(actor)` (the value in effect at the time
  the clip runs) in stance's `PoseConfig::rotation`; stance turns every
  decoded frame before detection, and the track's keypoints and
  `width`/`height` are in the turned frame (the track records it).
  `ClipStore::set_pose(slot, camera, rotation)` records that rotation and
  `stance::VERSION` on the camera's `ShotVideo` (`rotation`,
  `analyzed_with`, kept in `shot.json`); `shot_pose` carries the rotation.
- **Live**: `LiveSettings.rotation` (read per frame); the worker makes a new
  tracker when it changes, built with `PoseConfig::rotation`, so stance's
  tracker turns each live image before detecting; the `X-Pose` keypoints
  and the posture's frame width are the turned frame's.
- **Display**: the JPEGs on the wire are unrotated. The UI takes each
  camera's rotation from the saved config it holds
  (`SettingsForm::saved_rotation`; a clip whose camera is not
  configured uses the rotation it was tracked with) and paints the texture
  turned about its center (`pose::paint_turned`: `egui::Image::rotate` in
  the rect of the unturned size, `turned_size` swapping width and height for
  90/270) in the live view, the replay, the inline player, the shot cards
  (frames cut to the turned card's shape first) and the device card's still
  frame (with the card's unsaved rotation). `Fit` takes the turned
  `(width, height)`, so the skeleton lands on the turned picture.

Yaw and pitch are not used by any measurement or grade. The Cameras tab
shows one line over a Golfer Front camera's swing numbers when its
`|yaw_deg|` is 5 or more (`cameras::yaw_warning`).

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
`L` short of a trigger's time, so the history (pre-roll + longest reach +
0.5 s) needs nothing for the latency.

### Triggers

A `shot.trigger` from a launch monitor is dated on the host monotonic
clock (`stance::clock::now_ns`): at its `timestamp` (UTC ms, the strike) when
it carries one (`camera::trigger_time`; a timestamp ahead of the host's UTC
clock counts as now), else when the actor reads it. The ExPutt's triggers
carry the arrival of the device's strike message and reach the bus with the
putt's result, seconds later; the Square Golf's carry the arrival of the
ball notification, which is when they reach the bus too. The actor reads the
bus at least every 10 ms while capturing, so a stamp on arrival lags the
trigger by 10 ms at most (100 ms while the camera is being reopened, when
there are no frames to cut). The
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
| `exputt` | 0 (its trigger is dated at the strike) |
| `square` | 1000 ms |
| `r10` | 2000 ms |
| `openconnect_server` | 2500 ms |
| any other launch monitor (`DEFAULT_IMPACT_LEAD_S`) | 1500 ms |
| `autochip`, `trackball` | not recorded (no swing a camera films) |

Pre- and post-roll come from the live `[replay]` section at every trigger;
the recorder's history is the pre-roll plus the longest reach in effect over
the configured launch monitors plus 0.5 s: a monitor's reach is its impact
lead (setting or default, at most 5 s) plus its type's trigger delay
(`flighthook::trigger_delay_s`: 5 s for the ExPutt, whose trigger arrives
seconds after the strike it is dated at; 0 for the others). A trigger dated
further back than the history starts its clip at the oldest frame kept. It is re-applied at every trigger, so a setting raised
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
`ShotVideo` (`keys`, `position`, `duration_ms`, `fps`, `trigger_offset_ms`,
`impact_offset_ms`, `complete`, `dropped`, from the sidecar) and, when the
prune deleted clips, `shot_video_removed { keys }`. A failed write raises a
`warn` alert and leaves no folder.

The store indexes shot id → clip. Pruning keeps the newest `keep_shots`
clip folders (read live, held to 1 to 1000) after every clip and at startup
(`main` loads the folder, indexes every folder with a `shot.json`, deletes
the rest and emits `ShotVideoRemoved` as `system`). It skips folders with
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
one `ShotVideoRemoved` from `system`. Clips in an earlier folder are never
deleted by flighthook afterward.

### Web

Each web server stamps a `ShotVideo` onto the shots it names, found by
`shot_id` whatever actor took them: a pending shot keeps it until
`shot_finished`, a stored shot gets it in `data.video` (one entry per camera,
replacing that camera's earlier one). `ShotVideoRemoved` clears `video` on
its shots, `shot_pose` sets `pose`, `rotation` and `analyzed_with` on its
camera's entry and `shot_swing` sets `swing`. Routes
(`web/routes/video.rs`):

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
  without the clip. `?force=1` first clears the clip's analysis
  (`ClipStore::clear_analysis`: deletes `<camera>.pose.json` and
  `<camera>.swing.json`, clears `pose`/`swing`/`rotation`/`analyzed_with`
  and rewrites `shot.json`; `web::forget_analysis` clears the stored rows)
  and queues it again.
- `GET /api/shots/{shot_id}/grades/{camera}`: the camera's swing numbers
  graded against the bands, computed per request (see **Grades** under
  Body tracking); `404` until its swing file is served.
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
`swing.rs` and `grades.rs`, the modules that call `stance::golf`), held in `SystemState::pose`. `[body_tracking]` is read live
through `SystemConfig::body_tracking()` (out-of-range values at their
defaults): no change restarts a camera.

| Field | Default | Range | Used by |
| --- | --- | --- | --- |
| `mode` | `"off"` | `"off"` \| `"historical"` \| `"live"` | everything below |
| `model` | `"rtmpose-m"` | `"rtmpose-m"` \| `"rtmpose-s"` | live and recorded |
| `min_score` | 0.3 | 0 to 1 | drawing (UI): keypoints below it are hidden |
| `smoothing` | 1.0 | 0 (off), or 0.1 to 10 Hz | live only: One Euro cutoff at rest |
| `detect_every` | 10 | 1 to 60 | live only: frames between detections |
| `show_skeleton` | true | true \| false | drawing (UI): the skeleton over the live views, the clips and the replay |

Out-of-range values are refused with `400` on save (`BodyTrackingSection::error`)
and replaced by their default, with a warning, from the file. `mode` is an
enum like `model`: an unknown value is an invalid body (the JSON extractor's
`4xx`) or an unparsable file. A section with `enabled` and no `mode`
(`BodyTrackingRaw`, `#[serde(from)]`) reads `false` as `off` and `true` as
`historical`; a section with neither is `off`. Only `mode` is written, so
such a file is brought up to date with a backup at load, and a config
without the section stays byte-identical. The Body
tracking card (Cameras) edits every field, saved with the global cards.

The golfer's facts are the record of the golfer the shot names, by the lookup
rule (`SystemConfig::golfer_facts`, read live; a height out of range is none): `sex` (`male` | `female`, for the
tour averages) and `height` (100 to 250 cm: the swing analysis' distances in
cm). The Golfers card (Settings, Golfers) edits both (height typed in inches
with imperial default units, centimeters with metric) and saves with
`POST /api/settings?scope=golfers` (`SetGolfers`). A `[player]` section or a
`[body_tracking] golfer_height` in an older file is moved onto the golfers at
load (see [Loading and migration](#loading-and-migration)). Turning body tracking on from
Off saves the mode at once through
`POST /api/settings?scope=body_tracking` (`SetBodyTracking`: that section
alone, so it never writes another setting).

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
honors the cancel flag and can be made to fail.

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
with the golfer up's hand from the session and `swing::mirrored`), the
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
for body tracking when it is saved and announced (`ShotVideo`), to start no
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
true`, the rotation and `analyzed_with`, kept in `shot.json`) and the queue
sends one `shot_pose { keys, rotation }` with the camera as its actor,
straight on the bus (a fact about files, sent even if the camera actor has
been restarted since). One info line per clip gives the pass's size and
wall time: `body tracking: <camera> shot <id>: <frames> frames in <s> s
(<threads> threads) · <ms> ms a frame, golfer on <n> frames` (`<id>` the
clip's first shot). A clip pruned or
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
or before the last), the golfer's height (`SystemConfig::golfer_facts`), and whether the
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

**Grades.** `GET /api/shots/{shot_id}/grades/{camera}` (`get_shot_grades`)
grades a camera's swing file per request, never stored: `pose::grades::
grade_file` reads it (`SwingReport::read`; `404` while the file isn't
served) and grades it with stance's built-in band table
(`stance::golf::norms`, `Norms::builtin`, `grade_report` + `grade_tempo`)
for the tour and the amateur reference. The context: the club class from
the stored shot's `club_selected` (`Club::class`, lib `ClubClass` mapped to
stance's; a putter or no club grades nothing), the sex from
the golfer's record, and the height: when the report has the upright
torso length, its `scale.cm_per_px` is recomputed from the golfer's
height now (none without one, so stance assumes 177 / 164 cm) and the
analysis-time `cm` dropped. A report whose impact was `given` grades only
its tempo. Each stance `Grade` becomes a lib `MetricGrade` (`grades.rs` in
lib: `BodyGrades`, `ReferenceGrades`, `ReferenceKind`, `Fit`, `Side`); its
`word` is stance's `fit_word()` without stance's own name for the metric, so
the UI puts its own label first. The hand is reported, not used: every
stance metric is target-relative.

**UI.** The Live view draws each view's pose (the one that came with the
picture on screen) into the picture's rect with `ui/src/pose.rs`: the frame
fitted to the rect at its aspect, keypoints under `min_score` hidden with
their bones, the lead side (a right-hander's left; the game's hand) in teal,
the trail side in orange, the midline light. While the models are not
ready a band at the bottom of the picture says what they are doing. The
replay and the Cameras tab's player fetch each tracked clip's
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
plays without them, as while any clip is tracked. A clip keeps the tracking
it was given. When the Cameras tab shows a shot with a clip tracked
(`pose`) at a `rotation` other than its camera's saved one
(`cameras::retrack_due`), it posts `analyze?force=1` once per (shot,
camera) per session (`FlighthookApp::retracked`) and drops that clip's pose
and analysis from the row and the player (`types::forget_analysis`,
`ReplayView::forget_analysis`) until `shot_pose` and `shot_swing` bring the
new ones.
With `show_skeleton` false no skeleton is drawn anywhere (Live, the
Cameras tab's player, the replay); the poses are still fetched and the
swing numbers shown. In Live mode a line under the live views reads out the
values (`skeleton off` while it is hidden) and each camera's measured live
rate and pose time.

## SystemActor

The `SystemActor` (`actors/system/`) is a default actor that always runs,
independent of config. It holds the sole `SessionWriter`, the practice
bookkeeping, `Arc<SystemState>` and the bus sender, so only it mutates the
session and the configuration. It is the body of the flighthook FRP actor (the
session and practice roles). It handles:

- **The session**: applies every `golfer.select` on the bus (see [The
  turn](#the-turn)), publishes `golfer.turn` when it changed (with the select's
  sender as the cause), replies to the select after the turn, republishes the
  turn and the golfers' `golfer.profile`s on `bus.sync`, and resets a mode left
  unheld to Full once when the Shot Type control disappears (`ShotTypeReset`).
- **Practice**: follows the turn, every shot's trigger, data and finish and
  `practice.new_block`, and publishes the `practice.block`s (see
  [Sets](#sets)).
- **Remote actors**: announces an actor that missed its heartbeats as
  `ActorGone` (once a second it looks), and tells the round policy.
- **Config mutations**: subscribes to `ConfigCommand` events (from the REST
  API). Applies the mutation to `SystemConfig`, calls `apply_config_reload()` to
  reconcile actors, and emits a `ConfigOutcome` once the actors it stopped have
  ended. All config mutations are processed one at a time on the SystemActor
  thread; the stop waits run on helper threads, so selects and further commands
  are handled meanwhile.
- **Clear history**: on `ClearShotHistory` it deletes every replay clip
  (`SystemState::clips`), once for the process, whichever web server sent it,
  and starts a new session.
- **Clip folder**: after a config command that moves `[replay] clip_dir`, it
  reindexes the clip store (see [Clip store](#clip-store)).

Created in `main()` before config-driven actors, registered with ID `"system"`.
Skipped by `apply_config_reload()` (not config-driven). The reconciliation
function lives in `actors/system/reload.rs`.

The web layer's `state_updater` task changes none of this. It keeps the bus
state (every actor's FRP state), the card status cache and the shot store for
the dashboard and the API, and handles `ConfigOutcome` events to refresh actor
name caches.

## Component Identity

Cards are identified by type-prefixed card keys: `mevo.0`, `r10.0`,
`openconnect_client.0`, `mock_monitor.0`, `random_club.0`, `camera.0`,
`webserver.0`. The `system` card has a fixed key of `"system"`. The type
prefix encodes the component type; the index is the key within that type's
config section. `autochip` is the one key with no index and no config section:
the virtual source of Auto-Chip shots (`POST /api/auto-chip`).

On the bus, an FRP message's `from` is the sender's actor id. `SystemState`
keeps the map between card keys and actor ids (with the card type), so the
dashboard, the export, the outputs and the re-assert rule can name a local
card. Inner message types carry no name fields; identity lives on `from` and
in the actor's `bus.actor`.

## SystemState

Root entry point for all managed application state, passed as
`Arc<SystemState>` to all actors and the web layer.

```rust
pub struct SystemState {
    pub system: SystemConfig,     // cached config (RwLock + disk persistence + reload lock)
    pub session: Session,         // read side of the turn (golfer, club, hand, mode)
    pub beats: Beats,             // the newest bus.actor of every local actor, re-sent each heartbeat
    pub local_clients: Arc<LocalClients>,  // this process's Open Connect clients
    pub sent_shots: Arc<SentShots>,        // the shots the simulator outputs wrote (5 s)
    pub round: RoundPolicy,       // tournament rounds (strict mode, manual tournament mode)
    pub clips: ClipStore,         // replay clips on disk (beta)
    pub cameras: Cameras,         // camera actors' latest frames + the camera list (beta)
    pub pose: BodyTracking,       // body tracking models + the recorded-clip queue (alpha)
    pub pureplay_tokens: Arc<TokenStore>,  // Pureplay Accessory pairing tokens
    actors: RwLock<HashMap<String, Registered>>,  // the actor + its ActorHandle
}
```

`system` provides cached read/write access to the persisted TOML config.
Config mutations are serialized through the bus (SystemActor processes
`ConfigCommand` events one at a time on its thread), so no external lock
is needed. `session` is a read-only handle for the turn; mutations go through
the `SessionWriter` held by `SystemActor`. `actors` is a
registry of all running actors, keyed by card key. Each entry stores the
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

The Log tab in the dashboard streams the messages the `/api/ws` socket carries
(FRP messages, local events and the store's shots) in real time, with
per-message-type filter checkboxes and a one-line summary per message. Raw wire
data of a protocol is logged to the `audit` tracing target (hex first; raw JSON
for JSON protocols). The log retains the last 500 events. Protocol-level
tracing is always emitted to the `audit` tracing target (filtered by
`RUST_LOG`, not shown by default). `ShotVideo` and `ShotVideoRemoved` sit in
their own Camera filter group, each with a one-line summary (the clip's shots,
camera, length, frame rate, completeness and dropped frames; how many shots'
clips were removed).

## The FRP hub and the dashboard socket

**`/frp`** is a plain FRP 0.3.0 hub (see [Hub and remote
actors](#hub-and-remote-actors)); there is no handshake and no version
negotiation. The binding's greeting (every actor's `bus.actor` and state, then
a `bus.sync`) is the first thing a peer receives; every later bus message goes
to it unless it sent it or it is `shot.raw`. What a peer sends is checked
against the own-id guard and put on the bus. A message from a remote virtual
launch monitor during a tournament round is held (see [Tournament
rounds](#tournament-rounds)). External programs use the flightrelay SDK
(`FrpEndpoint`) to connect.

**`/api/ws`** is the dashboard's private socket (the dashboard is part of the
flighthook actor). On connect the server sends one snapshot (flighthook's actor
id, the card keys by actor id, every FRP state on the bus, each local card's
status, the round, the configuration generation), then every bus message in
order as `{"frp": ...}` or `{"local": ...}`, and `{"shot": ...}` each time the
store finishes or updates a shot (see [API.md](API.md) for the wire).
The dashboard sends FRP command bodies (`golfer.select`, `practice.new_block`)
with its own `ref`; the server prefixes the `ref` with a per-connection tag
(two windows share the flighthook actor), puts the command on the bus as the
flighthook actor and returns the matching `bus.reply` to that connection only.
Local commands: `grouping` and `clear_history`. Each has a REST twin
(`POST /api/select`, `/api/blocks`, `/api/shots/clear`, `/api/shots/grouping`).
The dashboard keeps a plain state map keyed by FRP state key; the server does
the expiry (the SDK's `BusState` uses `Instant`, which does not run in the
browser).

## Threading Model

```
main thread (eframe)       std::thread per actor              tokio runtime (background)
+-----------------+        +------------------------+        +------------------------+
| eframe::run_    |        | actors::system         |        | axum web server        |
|   native() or   |        |   (session, practice)  |        | state_updater task     |
| ctrl-c (headless|        | launch monitor cards   |        |   (/frp, /api/ws)      |
+-----------------+        | actors::flight_model   |        | drain subscriber       |
                           | actors::camera (beta)  |        +------------------------+
  GUI --ehttp/ws---->      | outputs (Open Connect, |
  (to local web server)    |   Pureplay, Accessory) |                ^
                           +------------------------+                |
                                     ^                               |
                                     +--- single broadcast<BusMsg>(1024) ---+
```

**Bus-based delivery**: actor threads poll the bus via `BusReceiver::poll()` in
their event loop. Launch monitor cards read flighthook's own `golfer.turn` and
hand their driver the hints; outputs read the shots, readiness and the turn.
There is no per-device `mpsc` channel and no centralized router task.

**Actor trait and bus wrappers**: all actors implement the `Actor` trait
(`start()` + `stop()` + `reconfigure()`) and receive a `BusSender`/`BusReceiver`
pair on startup. `BusSender` wraps `broadcast::Sender` and auto-stamps the
actor's id on every outbound FRP message. `BusSender` provides
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
registry. Launch monitor actors (the `DriverActor` cards) run event loops
polling the bus. Output actors (the Open Connect client in `actors::gspro`, the
Pureplay output in `actors::pureplay`, the Accessory card, random club)
subscribe to the bus and send `golfer.select` (for club and hand changes) and
card status (for connection status).

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
  `ReplaceGlobal`: units, default view, simulation,
  replay and body tracking from the body, every actor section of the
  live config kept.
- **Body tracking** -- `POST /api/settings?scope=body_tracking` emits
  `SetBodyTracking`: the body's `[body_tracking]` alone.
- **Golfers** -- `POST /api/settings?scope=golfers` emits `SetGolfers`: the
  body's `[[golfers]]` alone, no actor reconciled.
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

`apply_config_reload()` (in `actors/system/reload.rs`) orchestrates actor lifecycle:

1. Snapshot the new config and resolve it into the expected actors (`resolve_actors`)
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
card titles, the Shots navigation's device filter and labels, and alert banners.

`ReconfigureOutcome` enum:

- `Applied` -- config unchanged or applied without restart (the default)
- `RestartRequired` -- address/routing/settings changed; must stop and recreate

## UI Zoom

The UI (native window and browser) zooms itself every frame to fit the
window (`ui/src/scale.rs`). The layout is designed for windows from 1024×768
to 1920×1080 points. With `w` × `h` the window's size in points at zoom 1
(physical pixels over the screen's native pixels per point, so the display
scale or device pixel ratio counts), the zoom is `min(w / 1920, h / 1080)`
(at most 3.0) when that is above 1, else `min(w / 1024, h / 768)` (at least
0.8) when that is below 1, else 1. Both are rounded down to a multiple of 1/32, so resizing a window changes
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

A window under 600 points wide at zoom 1 and taller than wide (a phone held
upright, or a narrow desktop or native window) gets the portrait layout
(`scale::portrait`): zoom 1, no browser cap on the pixel ratio, one header
row and a tab bar at the bottom (see [Header and navigation](#header-and-navigation)),
the shot list on top of the Shots tab with the shot's data under it (Cards
shows its charts under the cards), Settings as a list that opens one page at
a time, controls 44 points tall, and the Log's lines wrapped at the screen's
edge. On a touch screen a tap on a disabled control, or a long press on an
icon, shows its hover text until the next tap.

## Header and navigation

The header is one 52 px row of icon buttons (custom icons, beta; plain line
glyphs at rest, brand-style on hover or selection) with no text except the
center title. Every button is 40 px with a 6 px gap and no dividers.
In the portrait layout the header is one row of the right-hand icons (and
the title while it fits), and the tabs move to a 52 px bar of four equal tabs
at the bottom of the window.

```
  [Shots][Cameras][Settings][Log]    FLIGHTHOOK v0.2.1    [Energy][Tournament][Follow] [Auto-Chip][Shot Type] [Club][Golfer]
   nav icons (the tabs)               title, when it fits  status/mode            actions              selections
```

**Left, the tabs:** Shots, Cameras, Settings, Log. The app opens on Shots (the
first-run welcome screen comes first). Cameras is disabled while no camera is
configured. A red dot on Settings (and on the Settings section that holds the
card) shows while any card lost its connection less than 10 minutes ago, see
[Connection state](#connection-state). The nav icons are disabled while Settings
has unsaved edits.

**Center title.** `FLIGHTHOOK v<version>` is drawn at the true center of the bar
only while it keeps 24 px clear of both icon groups; otherwise it is not drawn.
While a tournament round is on, the title reads `TOURNAMENT · <holder>`
(`Manual` when the toggle alone holds it; the simulator's card name when a
simulator holds it), shortening to `TOURNAMENT` and then to nothing as room
shrinks; the Tournament icon carries the signal then.

**Right, by role,** left to right:

| Icon | Shown when | What it does |
|---|---|---|
| Energy | a device reports a battery charge | Opens the "Batteries" list: one row per device with its charge. The icon turns yellow when any device is below 20 % and red below 10 % |
| Tournament | always | A toggle for tournament mode (`POST /api/tournament`). On, it applies the narrow policy: Auto-Chip is off and the virtual launch monitors (Trackball, Mock, and remote ones that declare themselves virtual) hold their shots. A simulator's round (Pureplay strict mode) shows the icon locked in gold: it can't be toggled off, and clicking it opens a card naming the holder |
| Follow turns | two or more golfers are known | A toggle (on by default, for the session only). On, the view follows every change of the golfer up; off, the view stays put and a pick in the Golfer card changes only the view |
| Auto-Chip | `[simulation] auto_chip` is on | Opens the "Auto-Chip" menu (`1 Yard`, `3 Yard`, `5 Yard`); disabled, with the reason as hover text, during a round, while no simulator is connected and while a chip is in flight |
| Shot Type | an enabled launch monitor uses the hint, or an output routes Chipping or Putting to a specific monitor (see [Shot Type visibility](#games-and-simulators-re-assert)) | Opens "Shot Type": Full, Chip, Putt, sent as a `golfer.select` carrying `mode`. A tip says some launch monitors use it as a hint for the shot to expect |
| Club | always | Shows the club in play (the club's shape with its code; an iron with "-" when none). Opens the club picker filtered to the golfer up's bag, the club in play always shown; a pick sends the club only |
| Golfer | always | Shows the viewed golfer's initial in an avatar circle (an outline when there is no golfer). Opens the Golfer card |

Every picker is always editable: a pick is a `golfer.select`, and a connected
simulator that holds something else sends its own back, so the header shows the
result. The hand control lives in the **Golfer card**, with the viewed golfer's
full name, a settings icon that opens Settings, Golfers, and one row per known
golfer (saved golfers, a game's current players, and golfers with shots this
run who are not saved or in a game any more, plus a "No golfer" row while unassigned shots
exist). With Follow on, a pick makes that golfer the golfer up; with Follow off,
or for a golfer who can't be made the golfer up, it changes the view only.

**The golfer view.** The viewed golfer decides whose shots and Sets the Shots and
Cameras tabs show (there is no separate golfer filter); with Follow on it is the
golfer up. Club, Shot Type and Auto-Chip act on the golfer up and the devices,
not the view. Export is unchanged: every golfer's shots, with `golfer_id`.
Clear history clears every golfer's shots.

An error banner above the tabs shows a card's `error` (and a failed Auto-Chip or
tournament request) until the card's next status or the next press.

## Settings

Settings is the second-level page on the Settings tab: one list on the left
(`settings_nav.rs`) and the selected item's page filling the rest. The list
has a heading per section, with its (+) where it has one, and a row per item
(the shot list's look), each with its status dot: the color of the device's
stripe, the red of an alert on a red device; plain forms have none. The item
last shown is kept for the session (`settings_section`, `settings_item`); one
no longer listed gives way to its section's first item, and Settings first
opens on the first launch monitor. A deep link into Settings (the Golfer
card's gear) picks a section and shows its first item.

On a phone held upright (`scale::portrait`) the list is the Settings page,
its rows touch-sized; a row opens its page full width under a back icon,
which returns to the list (disabled while the page has unsaved edits). When
the open item goes (Cancel on a new card, Remove), the list shows again. The
page's header puts the back icon, title and on/off switch on one row and the
rest on a second row, or on the first when it fits there.

| Section | Rows, in order | (+) offers |
|---|---|---|
| General | Global (units, default shot view, API Docs), Web Server, Simulation (flight model and Auto-Chip) | -- (no (+)) |
| Devices | `mevo`, `r10`, `square`, `exputt`, `trackball`, `mock_monitor`, `openconnect_server`, then remote launch monitors (read-only, beta) | Mevo, R10, Square Golf Omni, ExPutt, Trackball (only while `/api/status` says `trackball_supported`), Open Connect Server |
| Software | `openconnect_client`, `pureplay`, `pureplay_accessory` (beta), `random_club`, then other remote actors (read-only, beta) | GSPro (Open Connect), Pureplay (Open Connect), Muni Golf (Open Connect), Pureplay Accessory (at `127.0.0.1:9770`) (no entry adds a `pureplay` card; one comes only from the config file) |
| Cameras | `camera`, Replay (clips), Body tracking | Camera (only while `/api/status` says `cameras_supported`; without it the section shows the cameras already configured and no (+)) |
| Golfers (beta) | one row per golfer | a golfer, "Golfer N" |

When the unsaved-edits lock is on, the list's rows and (+) and the nav icons
are disabled until Save or Cancel. `ui/src/panels/devices/` renders the whole
Settings page; `ui/src/panels/settings/` holds its form.

Each (+) entry is a preset (`cards::AddPreset`): an actor type plus the new
card's name and address. The Open Connect and Pureplay presets:

| Entry | Type | Name | Address |
|---|---|---|---|
| Open Connect Server | `openconnect_server` | Open Connect Server | bind `0.0.0.0:921` |
| GSPro (Open Connect) | `openconnect_client` | Open Connect | `127.0.0.1:921` |
| Pureplay (Open Connect) | `openconnect_client` | Open Connect | `127.0.0.1:921` |
| Muni Golf (Open Connect) | `openconnect_client` | Muni Golf | `127.0.0.1:9210` |
| Pureplay Accessory | `pureplay_accessory` | Pureplay Accessory | `127.0.0.1:9770` |

The Open Connect Server entry is for launch monitor software that sends Open
Connect (its welcome-screen hover says flighthook binds port 921, so GSPro on
the same host moves to 922 with OpenAPIUseAltPort). GSPro and Pureplay both listen for Open Connect on 921, so
their two entries add the same card, named "Open Connect", which sends to
whichever simulator listens there; any other simulator is a card with its
address changed. A new Open Connect client's chipping and putting club lists
default to GW, SW, LW and PT.

One card per address may be on (see [One card per address](#one-card-per-address)).
No (+) entry is ever disabled for a duplicate address: it adds its card, and
a card added where an enabled card of its type already has the address starts
off, so it can be set up for another address.

A card's type label names the actor type, "Open Connect Server" or
"Open Connect Client", whichever preset created it, and "Pureplay Connect"
for a `pureplay` card. The Open Connect card has no tournament mode; tournament (strict) mode needs
a Pureplay Connect output, which the (+) menu does not offer.

flighthook runs one web server, the one serving this UI. Its card has no
Remove and no menu adds another; the config API refuses both (`400`, see
[API.md](API.md)). A config file that already holds several `[webserver.N]`
sections loads and runs them all, a card each, every one editable.

Within a section, cards sort by type (the order above), then by instance index
(numerically). The Open Connect server is a launch monitor: it receives shots.
Software holds every card that talks to another program: the outputs, the
Pureplay Accessory card (whose shots still count as a launch monitor's for
routing; only its place on the page is Software's) and Random Club, which
consumes shots (it picks the next club on each one). A card is every configured instance plus every running
actor the settings don't list (`device_card_order`), so two simulator
connections or two launch monitors are two cards, and the cards show
(Telemetry only) before the settings have loaded.

**Remote actors (beta).** An FRP program on the hub (see [Hub and remote
actors](#hub-and-remote-actors)) gets a read-only card: launch monitors in
Devices, the rest in Software. The card shows its name, device (maker, model,
firmware), the `ready` row (or its blockers), battery and telemetry; no
settings, no Reconnect. It appears with its first `bus.actor` and goes when the
actor expires or reports `removed`. An ownership conflict shows an error row
naming the other actor.

### Device pages

Each web server, launch monitor, camera and output has a page: its header,
then its telemetry beside its settings, both visible. The telemetry sits on
the left in a block of fixed height (240 points) that scrolls inside, so
nothing beside or under it moves as rows come and go; a camera's still frame
shows under it. The settings fill the rest of the page and scroll inside it.
On the phone the telemetry block goes over the settings. A remote actor's
page, or a running actor's the settings don't list, shows its telemetry only.
The header holds the name on the left; on the right the type tag, the
status badge and the icons:

```
[Name …] [on/off]  ........  [type tag] [STATUS BADGE] [Reconnect] [Remove]
unsaved:  [Name …] [on/off]  ........  [type tag] [STATUS BADGE] [Cancel] [Save]
```

The title is the running actor's name, the form's while it has unsaved
edits. Right of the name every launch monitor, camera and output
card has a compact on/off switch (`theme::on_off_switch`, accent with its
knob right while on, gray with its knob left while off; hover "Turn off" /
"Turn on"; mark `enabled:<id>`). The web server's card has none, nor does a
running actor the settings don't list. Flipping it saves at once, with no
Save or Cancel: `SettingsForm::begin_enable` posts the saved settings with
only that card's `enabled` changed, scoped to it (`?scope=<actor_id>`), as
the other immediate actions do. It is disabled while the tab is locked (a
card with unsaved edits, its own included) or a save is out, and, on a card
that is off, while an enabled card of its type has its address
(`SettingsForm::enable_clash`; the hover is then the clash's text, "An Open
Connect card already uses 127.0.0.1:921"). While the card
is off (`SettingsForm::turned_off`: the save that is out, else the saved
settings) its state is **Disabled**: gray `DISABLED` badge and stripe
(`theme::STATUS_OFF`), a muted "Disabled" in the Telemetry body instead of
the spinner, Reconnect disabled. It shows so as soon as the switch is
flipped, before the server has stopped the actor, and a `ConfigOutcome`
that stops a card turned off leaves it Disabled rather than dropping it.
Disabled is never an alert: no red, no dot on the Settings icon, no error row
or banner. Its settings stay editable.

```
off:     [Name …] [o  ]  ........  [type tag] [DISABLED] [Reconnect] [Remove]
```

Hovering the name or the tag says what the type is and how it connects. The
tag is left out when the name reads the same as the type, ignoring case: the
web server at its default name "Web Server", or an "Open Connect Server"
card at its default name. The icons, Save, Cancel and the status badge keep
their size; a long name is cut short with "…" first, then the type tag,
which is left out once it would be cut to a stub.

- **Telemetry**: the connection state badge
  (`CONNECTING` yellow, `CONNECTED` green, `RECONNECTING` red for 10 minutes
  after the connection was lost, then yellow, `DISABLED` gray; see
  [Connection state](#connection-state)), then its
  telemetry key/value
  pairs one per row, sorted by key. Detection mode, battery, tilt, roll,
  temperature, club, handed and the rest come from the actor's telemetry map.
  The key column is as wide as the longest key, up to 45 % of the card; a key
  or value too long for its column is cut short with "…" and shown in full
  on hover (a Square Golf Omni's `device_info`, for one). With no row to
  show, the body shows the actor's state in its middle instead, 48 points
  across: a spinner (a faint ring with an arc going round it) in the card's
  status color while the actor is starting, connecting or reconnecting, and
  a check in a ring in the connected green once it is connected
  (`monitoring::no_telemetry`, `theme::loading_ring` / `connected_ring`). It
  goes as soon as a row arrives; a camera's still frame takes its place too.
  Only a spinner on screen asks for repaints (about 30 a second). Every launch
  monitor and output card (Mevo/Xi, R10, Square Golf, ExPutt, Trackball, the
  Open Connect server, the mock monitor, the Open Connect clients, Random club
  and any other output) has the **Reconnect** icon (Codicons `refresh`) in
  its header: it stops the
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
  next press. An ExPutt's **Find mat** icon (Codicons `search`) sits left of it (enabled while the ExPutt is
  connected), and a camera has the **Still frame** icon (Codicons `device-camera`) instead, its frame shown under
  the telemetry. Each header icon shows its name as its hover (or touch) tip. The plain forms and the
  cameras have no Reconnect.
- **Settings**: the instance's fields in a label column. The **Remove**
  icon (Codicons `trash`; every card but the web server's) asks for
  confirmation in a modal dialog and then saves the removal
  (`?scope=<actor_id>`, the section left out); the page goes when the
  server confirms it. A Pureplay Accessory card has the **Forget pairing** icon (Codicons `debug-disconnect`) left of the others
  (`POST /api/actors/{id}/forget-pairing`, sent and tracked like Reconnect:
  disabled while one is in flight, while the card is off and while the tab
  is locked).

#### Page height

Every page is as tall as Settings. Telemetry that arrives, an edit, a
validation message, an error line or a still frame never changes its
height. Inside a page the header stays at the top, with the "Save or cancel
your changes" hint and a failed save's line under it; the settings (or a
plain form's rows) scroll inside the page when they do not fit, with egui's
thin scroll bar shown only then, each keeping its own scroll position, keyed
by its ID, for the session.

Long names in the header and in text fields, and long entries in a dropdown,
are cut short inside the page rather than widening it.

The colored stripe down the left edge of a page is the color of the actor's
connection state (the badge's), dim while the actor is not running (unsaved,
or not started yet). The plain forms' stripe is the heading blue.

#### Plain forms

Global, Simulation, Replay, Body tracking and each golfer have a page with
their title (and tag), Save and Cancel while they have unsaved edits, and
their settings as grouped rows (`settings::form_rows`): one row per setting
across the form, at most 900 points wide, the label on the left (45 % of the
row), the control on the right, a rule between rows. A golfer's page has a
trash icon that removes the golfer from the form (saved with Save).

### Unsaved changes

When a card's settings have unsaved changes, its icons are replaced by
**Cancel** and **Save** (Save accent-filled, disabled while that card's
fields are invalid; each reason shows under its field), with the hint "Save
or cancel your changes" under them. Save
persists only that card (scoped request, below); Cancel puts the card back to
the settings last loaded or saved.

The Global and Simulation cards share one save request (`build_global_request`).
Each shows the pair while it has unsaved edits, and an edit on either card
leaves both editable: Save on either saves both, Cancel on either reverts
both. The units are the Global card's **Units** value
(`FlighthookApp::units` reads `SettingsForm::default_units`, the persisted
`default_units`; there is no separate units toggle), so they follow an edit
at once and Cancel puts the saved value back.

While anything is unsaved the page is **locked** to it: the nav icons, the
list, the other cards' switches and edit controls, and the (+) buttons
are disabled (the Club, Shot Type and Golfer pickers in the header stay live). Escape doesn't
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

A (+) after the Launch monitors, Cameras and Software headings (Codicons
`add`, muted, accent on hover; hover text "Add a launch monitor" / "Add a
camera" / "Add software") opens a
menu of that group's types. The new card's page opens, unsaved, with Save
and Cancel; Cancel removes it. The (+) after Golfers ("Add a golfer") adds a
golfer and opens its page. The (+) is disabled until the settings have
loaded.

### Fields

Config sections are type-prefixed; their keys are integer indices (never
displayed).

- Name is **required** and serves as the rename mechanism. Changing a device name
  updates the name cache in WebState, so subsequent status responses and shot
  data use the new name.
- FlightScope cards show name, Address (`ip:port`; blank is the driver's
  default, `192.168.2.1:5100`, shown as the hint), Ball Type, Tee Height,
  Monitor-to-Ball, Surface Height, Track %, Use Estimated Shots, Environment
  (Limited flight (sim/indoor) / Outdoor (full flight)), Spin source (Xi),
  Launch-only spin, Camera Mode (Standard / Fusion / Raw Fusion) and Camera
  timing. Every driver setting is on the card (`DeviceFormEntry::mevo_config`
  builds the driver's config over the one loaded, so a field still showing
  its loaded value keeps it, set or unset); the driver's refusal (track %
  outside 0 to 100, a negative distance, an address that is not `ip:port`)
  and a number field that is not a number show under the fields. Spin source
  and Launch-only spin apply live; any other change restarts the card
- R10 cards show name, Monitor-to-Ball (the tee distance sent to the device)
  and Aim offset (degrees, `°`, blank is 0; BLE auto-discovery; no address or
  radar fields)
- Open Connect Server cards show name and Bind Address, the `ip:port` the
  server listens on; blank means `0.0.0.0:921`, anything else must parse as
  `ip:port`. A bind change restarts the actor
- Open Connect Client cards show name, Address (`ip:port` of the simulator),
  the three routing dropdowns (launch monitors by name; a stored id that names
  no card reads "Missing monitor") and the Chipping Clubs and Putting Clubs
  toggles (the clubs the simulator holds for a chip and a putt)
- Pureplay Connect cards show the same: name, Address (`ip:port` of
  Pureplay) and the three routing dropdowns
- Pureplay Accessory cards (`pureplay_accessory`, beta) show name, Address (the
  `ip:port` of Pureplay's Accessory API; blank means `127.0.0.1:9770`,
  anything else must parse as `ip:port`), Shots (Auto / From Pureplay /
  From flighthook) and Camera timing. An address change restarts the card
  and pairs again; the other two apply without a restart
- The Web Server card shows name and Bind Address, the `ip:port` it listens
  on (`0.0.0.0` accepts other computers, `127.0.0.1` only this one). A bind
  change restarts it
- Camera cards (beta) show name, Camera (a dropdown of
  [`GET /api/cameras`](API.md#get-apicameras), fetched the first time a
  camera card shows its settings, with a refresh icon; a camera another card
  uses reads "(in use by camera.N)", a saved camera that is not connected
  "(not connected)"), Position, Video mode (Auto or one of the picked
  camera's modes, `1280×720 @ 120`; a saved mode the list lacks stays
  selectable), in this order: Camera, Rotation (`0° | 90° | 180° | 270°`
  pills, marks `camera_rotation:<id>` and `camera_rotation:<id>:<deg>`;
  right under the camera, so it shows without scrolling), Position, Video mode, Latency, Yaw (`camera_yaw:<id>`, with the
  sign in words under it) and Pitch (`camera_pitch:<id>`), the angles' range
  under them while out of range. The body scrolls to Yaw and Pitch. Save needs a camera, a mode
  and every value in range. They are left out of the
  Open Connect clients' routing dropdowns. Its page's header holds
  the **Still frame** icon, enabled while the camera is connected: it fetches
  [`GET /api/cameras/{actor}/still`](API.md#get-apicamerasactorstill) and
  shows the frame under the telemetry, at its width (decoded like the replay's frames),
  turned by the card's rotation
- The Body tracking card (alpha, Cameras, after Replay) holds every
  `[body_tracking]` field, in this order: Mode (`mode`, Off | Historical |
  Live, pills like Model's), Model (`model`, RTMPose-m | RTMPose-s), Show
  skeleton (`show_skeleton`, a checkbox), Minimum score (`min_score`),
  Smoothing (`smoothing`, Hz) and Detect every (`detect_every`, frames).
  Every field
  shows in every mode; each number is checked like the server's
  (`BodyTrackingSection::error`, one field at a time) with its range under
  it while out of range, and a field left as loaded saves its loaded value
  exactly. It saves and cancels with the other global cards. Picking
  Historical or Live
  while the mode in effect is Off is not a form edit: it goes through the
  model download prompt (`panels/body_models.rs`, below), which saves the
  mode itself
- The model download prompt (`panels/body_models.rs`, `ModelsPrompt`): the
  card's Off → on first asks `GET /api/body_tracking/models`. `ready`: the
  mode is saved at once through the tuning saves (`Tuning`,
  `?scope=body_tracking`, one at a time, the newest winning) and shown on the
  card. `missing` or `failed`: an `egui::Modal` dialog lists the missing
  files (label, MB, host), the total in whole MB, the models folder and that
  body tracking runs locally afterward, with **Download** and **Cancel**
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
  clips in the Cameras tab, Live in Live mode, Replay); its Cancel leaves
  the mode as it is. No hover text
- The Golfers card (beta, Golfers) lists the golfer records, each with a
  name, bag, Gender (Male | Female pills), Hand (Right-handed | Left-handed
  pills, `handed`; Right-handed while unset), Height (inches with imperial default
  units, centimeters with metric, converted when the units change; empty is
  none; its range under it while out of range, checked by
  `PlayerSection::error`) and the game ids linked to it, each with a Remove
  button, under them a picker of the game profiles on the bus that no golfer
  is linked to (`golfer.profile` under ids other than `flighthook.*`, shown
  as "Pureplay: name", the same way a linked id reads, with the id's
  own part when the player's name is not known; disabled with a hover reason
  while there is none). The card has its own Save (`scope=golfers`); a saved
  hand for the golfer up reaches the turn at once.
- The Replay card (Cameras) holds `[replay]`: Keep clips, Pre-roll,
  Post-roll and Start before impact in seconds, a **Show video on shot
  cards** checkbox (`shot_cards`), and the clip folder,
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

## Shots Tab

The Shots and Cameras tabs share one **shots navigation** on the left (the
header row with the shot count, the export icon and the Clear history trash;
the `By set | By club | None` selector; the device filter; the shot list; the
New set and New session foot). The Shots tab's own toolbar to the right of it
holds `Cards | Table` and, in Table, the "italic = simulated" legend.

```
  SHOTS 42 [export] [trash]   [Cards|Table]
  [By set|By club|None]       (Table: italic = simulated)
  DEVICE [All v]  (2+ devices)

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

The Shots toolbar (the Cards | Table switch, and in Table the "italic = simulated"
legend, since only Table draws simulated values in italics:
`ShotsView::shows_simulated_legend`) sits above both views. The chosen view
(`ShotsView` in `panels/shots/views.rs`) lasts for the session and is not saved.
The viewed golfer decides which golfer's shots, Sets and numbers are shown (see
[Header and navigation](#header-and-navigation)); there is no golfer filter.

The **device filter** (`DEVICE [All v]`, a compact dropdown in the shots
navigation) shows in both tabs, in every grouping, while the
session's shots come from two or more devices (`device_options`); each
device is labeled by `chart::device_label`. It lasts for the session
(`FlighthookApp::device_filter`) and falls back to All when its device
leaves the history. One filter applies to everything on the Shots and Cameras tabs,
through one predicate (`ShotFilter::passes`): the shot list and its section
counts, the shot on show, the Table's rows, the view's members (range bars,
stats and face impact cards, Flight, Dispersion) and the charts' faint
shots. An Auto-Chip is no device, so a filter hides it like any other
device's shot. With All, the bars, Flight and Dispersion keep their device
colors and sub-bands. The JSONL/CSV export and the server are not
filtered: the export is always the whole session. Every cache that depends
on the filter holds it in its key: the members (`chart::MembersKey`, and so
the chart scenes) and the Table's rows (`FlighthookApp::filtered_rows`, on
the shots revision and the filter).

Both views always show their charts, in one fixed layout per window size
(`chart_slots` in `panels/shots/views.rs`); nothing is toggled or dragged, and
every size follows from the window's, so the layout holds still until the
window changes.
With the window at least 1200 px wide, **Dispersion** fills a full-height
column on the right, 24 % of the window's width (260-420 px), in both views,
and Cards has **Flight** along the bottom of its center column (between the
shot list and the Dispersion column), 30 % of the window's height (180-320
px). Narrower, Cards puts Dispersion in the bottom row beside Flight (62 % of
the row for Flight, 38 % for Dispersion, widened to at least 312 px, up to
half the row, so its header keeps to one row), so the cards keep the
center's width; the row is 27 % of the window's height (210-320 px). Narrow
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
view falls back to the latest shot (`ShotFocus` in `panels/shots/selection.rs`).

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
applies it at once and saves it as the default view, over `/api/ws`, else
`POST /api/shots/grouping`. The view only changes what the tab shows: Sets are
recorded the same way in every view (see [Sets](#sets)). The list scrolls between the
selector and its **foot**, pinned at the bottom of the column: a hairline, a
one-line hint when there is one, then **New set** (a `practice.new_block` for the
viewed golfer; disabled while their open Set has no shots) and **New session**
(`practice.new_block { new_session }`, closing every golfer's open Set; disabled
while no Set is open). The hint reads "Next shot starts a new set (DR)" in By set when the
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
first, under a header named by its club (`club_name` in `ui/src/groups.rs`):
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

The dashboard learns the Sets from `practice.block` messages and the store's
block records (`GET /api/blocks`), counts the session up on every new session
and clear it sees, and never holds it below a listed Set's `session`
(`ShotGroups::session`). Header counts are the server's, so they cover shots
older than the list holds. Rows without data (a shot in flight, a dead
trigger) never break a run. The rows before the first row of a known Set (a
shot in flight, rows with data whose Set is not known yet) come first, under a
dim `NOT IN A SET` header when a header is above them. Later rows in no known
Set get the same header: in place in By set, gathered in one last section in By
club.

**Export** is an icon immediately left of the trash in the shots navigation's
header row (`export_menu` in `panels/shots/panel.rs`: `theme::icon` with `Icon::Export`, Codicons `export`
U+EBAC, the same height as the trash, muted at rest and accent on hover,
hover text "Export session"). A click opens an `egui::Popup::menu` listing
**JSONL** and **CSV** (`EXPORT_FORMATS`); a pick opens
`GET /api/shots/export?format=…&units=…` in the shown units, the same URLs a
link would use. It is always enabled (an empty session exports an empty
file).

**Clear history** is the trash icon at the right end of the shots navigation's header row: a Codicons glyph
(`theme::icon` with `Icon::Trash`; the
font is `ui/assets/fonts/codicon.ttf`, compiled in with `include_bytes!` and
registered as its own font family by `theme::install`), as tall as the
export icon, muted at rest and alert red on hover. It is disabled while
there are no shots. A click opens a modal: "Delete all N shots and sets?
This can't be undone. Export first if you want to keep them." with Cancel
and a red Delete; Escape or a click outside cancels. Delete sends
`clear_history` over `/api/ws`, else `POST /api/shots/clear`. Every
dashboard that sees the clear on `/api/ws` clears at once
(`FlighthookApp::clear_shot_history`): shots, Sets (the session counts up),
the shown shot, the device filter, the caches and an open confirmation. The
Shots tab then shows its empty state.

**Replay** (beta): a shot whose row has `video` gets a **Replay** chip (a
video icon) on the Cards view's shot header line, after LATEST or Back to
latest, and the Table view a **Replay** column after Club, shown only while
some row has clips. Rows take `video` from `/api/shots`, from each
`ShotVideo` (every row whose `shot_id` is in its `keys`, whatever device
took it, before or after `shot_finished`; a camera's newer clip replaces its
older one) and lose it on `ShotVideoRemoved`.

The chip opens the replay, an `egui::Modal` over the Shots tab
(`ui/src/panels/replay.rs`, logic in `ui/src/replay.rs`): the shot's cameras
side by side, each labeled with its card's name and position and followed
by a download icon (Codicons `download`, hover "Download"; the `.avi`); a scrubber with the start point as a green tick
and the estimated impact as a gold tick; play/pause; previous/next frame;
speed 1× · ½ · ¼ · ⅛; a Loop toggle (on whenever a replay opens, UI state only). Close, Escape
or the backdrop closes it, and so do Clear history and a
`ShotVideoRemoved` naming its shot. Each camera's track
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

<a id="auto-chip-beta"></a>**Auto-Chip** (beta) is a header icon (`panels/auto_chip.rs`; it is
not part of the Shots tab, which shows live and historical data rather than
controls). It shows while `[simulation] auto_chip` (default `false`,
serde-omitted at the default; the Simulation card's **Auto-Chip** checkbox;
`SimulationSection::auto_chip`) is on in the *saved* settings, and not before
they have loaded. It is off by default; turn it on in Settings → General →
Simulation (useful with radar launch monitors that miss short chips). With it
off the icon is not drawn and `POST /api/auto-chip` answers `409`. Its menu,
titled "Auto-Chip", lists `1 Yard`, `3 Yard`, `5 Yard`
(`flighthook::AUTO_CHIP_YARDS`); a pick sends `POST /api/auto-chip`
(`net::post_auto_chip`). The icon is disabled, with the reason on hover, while
a round is active (tournament mode on, or a simulator's round: the server's
`RoundState`, `FlighthookApp::round`; the server fires no chip then), while no
simulator card is connected (an Open Connect client or a Pureplay Connect
output) and while a chip is waiting for its reply (`auto_chip_in_flight`). A
failed reply shows in the error banner above the tabs (`Auto-Chip: <status>
<error>`) until the next pick. Auto-Chip shots already recorded display as
before when the setting is turned off.

On the server (`app/src/autochip.rs`), `solve_speed` bisects the ball speed
(0.1-60 mph, to 1e-4 mph) at which dimple, with the `[simulation]`
environment and surface and `BallModel::TOUR`, rests at the chosen total
for a straight 45° launch with backspin `round(SPIN_PER_MPH (300) ×
mph)` and no sidespin; the total rises with speed. `send` publishes the
Auto-Chip launch monitor's `bus.actor` (source `virtual`), `shot.trigger`,
`shot.launch`, a modeled `shot.flight` (simulated whatever `[simulation]
mode`, `off` included, in Chipping mode), a local `ShotCapture` and
`shot.finished`, all under the Auto-Chip actor id (`[frp] auto_chip_actor_id`),
keyed by a fresh UUID and the next `WebState::auto_chip_shots`, from
`spawn_blocking`; the counter moves only once the solve has succeeded. The
stored shot's mode is Chipping, whatever the turn's. The dashboard labels the
actor `Auto-Chip` (`chart::device_label`); it has no config section, so it gets
no Settings card. The Open Connect client's routing check
(`Feed::passes`) takes Auto-Chip shots in every mode whatever monitor is
routed, and sends its launch alone: the chip stands in for a read the routed
monitor missed. The client's `Relay::next_shot` sends `max(shot_number, last
sent + 1)` as the `ShotNumber`, so device and Auto-Chip numbers never repeat.
Auto-Chip is enabled while an Open Connect client or Pureplay Connect card is
connected.

The server sets count an Auto-Chip like any shot. The UI keeps it out of
every statistic through one predicate, `chart::is_auto_chip`: it is never a
view member (`chart::members_for`), so the range bars, stats and face impact cards,
dispersion ellipse and flight set overlay leave it out; it is not in
`chart::device_ids`, so it has no device filter option, band or color; and
list and range-bar headers count a set as `chart::set_shot_count` (the
set's `shots` less its Auto-Chip rows). It stays in the shot list under its
set, in the Table and on the Cards; shown, it is the charts' gold shot
(`flight::drawn_sets`) but not part of the ellipse.

The dashboard gets the shots from `/api/ws`: the snapshot on connect, then a
`shot` message each time the store finishes or updates a shot; it loads
`GET /api/shots?limit=1000` (`net::SHOTS_LIMIT`, the server's whole history) and
`GET /api/blocks` on connect and whenever its Sets fall out of step. Each fetch
is tagged with the UI's history generation (`FlighthookApp::history_generation`),
which counts up on every clear and new session. A reply requested before a Clear
history is dropped when it lands, so a fetch still in flight can't bring the
cleared rows back; a Sets reply requested before a new session is dropped and
fetched again, so it can't wind the session back.

The cards show a header (shot number, device name, LATEST or Back to
latest), a headline row of five large cards, then the Ball, Flight and Club
groups of smaller cards. The groups sit side by side in two rows when each
card gets at least 96 px; otherwise in two columns two cards wide (Ball over
Flight, then Club) when that leaves 96 px; otherwise they stack with their
cards wrapping. Detail cards stay shorter than the headline cards.

The center column always tries to show everything above the panel under
it (`cards_layout`, from the height between the toolbar and that panel,
every frame). With room to spare, the cards grow with the height up to a
limit and the spare height is split above and below. Short of room, every
height in the column (the header line, headline and detail cards, the
range bar rows, label lines and gaps) shrinks toward a compact size by
the same share, just enough to fit; card padding and labels shrink with
the cards. The compact sizes keep the numerals readable: headline cards
68 px tall with numerals of at least 28 px, detail cards 52 px with at least
18 px, bar rows 26 px. At 1920×1080, 1600×900, 1440×900, 1280×800 and
1024×768 every card fits with no scrolling, with the shot list at its
default width or at the widest it opens at. Only a shorter center (e.g.
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
shots navigation, above) applies before the view; with All and two or more devices,
each bar's band splits into one sub-band per device, colored by device. The
members are cached on `chart::MembersKey` (shot and set revisions, view,
device filter, shown shot, club in play) by
`FlighthookApp::refresh_members`, which the bars, the stats and face impact
cards, Flight and Dispersion all take them from. Each bar is
drawn on a track, painted in this order: the set's range as a translucent
accent band with its min and max beside it; the median as a hollow white
ring sitting on the track's center line, its radius just inside the track's
height; the average as a filled gold dot (full-strength `theme::GOLD`),
sized to sit inside the ring (filling it when the two coincide), with a thin
dark halo so it reads against the band; and, on top of everything, the
shown shot as a thin gold line (`theme::GOLD`, with a thin dark halo of its
own) that sticks out past the track, top and bottom. A value shared by
every marker therefore reads as a white ring filled with a gold dot, with a
gold line through it poking out above and below. Total and Carry share one
domain, the set's extent padded by 15 % of its range and at least 10 yd (9 m) a side,
so roll reads as the offset between the bands; the track's ends carry no
numbers (only the band's own min and max are labeled). Lateral is symmetric
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
Connected, in Settings card order (below). **Face impact**, while any shot in
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

The **stats card** (`stats_card` in `panels/shots/range.rs`) reads on its own,
wherever it sits in the row. `STATS` sits in its top-left in the headline
cards' label style. Under it, a table: column headers `AVG`, `MED`, `SD` in
small muted capitals, then one row per bar in bar order, named like the bars
(`TOTAL`, `CARRY`, `LATERAL`) with the unit in small muted text. The values
are printed like the bars' (`StatsRow`): average and median as whole yards
or meters, lateral to a tenth with its side (`3.2 L`), the standard
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
frame is cut to the card body's aspect about its center (no letterbox) and
scaled by box averaging (nearest when scaling up) to the body's pixel size
(`ctx.pixels_per_point()`, each side rounded up to an even number). Nothing else is drawn on the
picture. Without a clip of the shown shot from that camera (still recording,
an older shot, a discarded recording) the card shows its name over an empty
body and fetches nothing; the clip starts when it arrives (`ShotVideo`, the
row's `video`). It fetches like the replay view: the sidecar, then range
windows around the playhead and 300 ms ahead (from the start point again
past the end), two requests at most, into a 24 MiB byte cache. A card keeps
every k-th frame of its loop, from the start point's frame on, with k the
smallest stride whose frames fit 48 MiB of textures at the card's pixel size
(every frame when the whole loop fits; a 556-frame loop at 182x144 px keeps
every other frame). Each kept frame is decoded once and kept as a texture
while the card shows that shot's clip. The camera cards share one decode per
screen update between them (`FlighthookApp::decode_card_frame`: the card
that has waited longest for one goes first). A card decodes the first kept
frame not decoded yet from the playhead on, in loop order, so its loop fills
in ahead of the playhead. It shows the decoded kept frame at or nearest
before the playhead, so a clip whose playhead moves several frames per
screen update still plays while its loop fills in. A new shown
shot, another camera on the card, Clear history or a new card size drops
what it holds (a new size also the picture on screen). The card row holds
clips only while it is on screen: the Table view, the view None, another tab
and the replay view of the shown shot drop every card's clip, and the cards
fetch and decode again when the row returns. While it plays a card asks for
a repaint when its next kept frame is due (`request_repaint_after`), never sooner
than 1/60 s, so a faster camera skips frames to keep its time; it asks none
while the replay view is open. A click cycles the card; the shot header's
Replay chip opens the replay. With `[replay] shot_cards` false in the saved
settings (`SettingsForm::shot_cards_enabled`, true until they load) the row's
sources hold no cameras, so it lays out as with no camera connected, and
`drop_hidden_card_clips` drops every card's clip; recording, the Cameras tab
and the replay view are unchanged. Turned on again, the cards return for the
shown shot.

**Face impact** (`ui/src/panels/impact.rs`) is a source of the card row. It draws the shown
shot's club face, face-on as the ball sees it: a right-hander's toe on the
left and hosel on the right, a left-hander's mirrored (the shot's
handedness, else the game state's, else right). Face impact itself is
golfer-relative and never mirrored. The outline is one of six generic
families by club (driver and mini driver; fairway woods; hybrids; irons
1I-9I, PW and an unknown club; wedges GW, SW, LW; putter), each a closed
chain of cubic Béziers in face millimeters with the origin at the face
center (the midpoint of both chords through it). Each family is scaled on
its own to fill the panel (`FaceView::fit`, the one place the scale is set:
the whole silhouette, hosel included, as large as fits 94 % of the plot's
width and 86 % of its height, centered), so the families are not drawn to
one scale: a wood and a hybrid look about the same size. The cubics are
flattened in screen space by Wang's formula to within 0.1 pt, and the face
is filled with a triangle mesh (ear clipping, built once per family and
scale, since egui fills only convex paths) with the outline stroked over it.
Inside: a dashed crosshair at the face center; the view's other members
(the bars' members, after the device filter) as small dots, accent or in
device colors by the same rule as the Flight and Dispersion charts; the
shown shot as a gold dot, or a gold line across the face when only one
axis was read. The shown shot
is marked only when it is a member, as on the bars; otherwise there is no
gold mark and the readout is `-`.
A mark off the face is drawn as a ring, pulled into the panel when it is
off it. The readout above the face reads `4 toe · 2 low` in whole
millimeters (`-` when the shown shot has no reading), 12.5 pt, shrinking to
10 pt in a narrow panel and cut short with an ellipsis only if it is still
too wide there; TOE, HEEL and mm sit under
the face's ends. Hovering a dot shows its shot (`#5282 · 7I`, the device
with two or more) and `4 mm toe · 2 mm low`; a click shows that shot, as
on the charts.

**Flight** (beta, `ui/src/panels/flight.rs`) is a side view of the ball
flights in the Cards view's bottom row (see the layout above). The chart is
painted with egui's painter (no plotting crate). It plots each shot's
simulated path (`path`): horizontal
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
  carry and total are labeled at 14 pt on a background box, each centered
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
  club: that club's shots in its session), in accent, or colored by device
  (`theme::DEVICE_COLORS`) when the filter is on All and there are two or
  more devices, with their landing and rest dots.
- **Faint**: the other shots of the current session, from the filtered
  device, newest 200, at 14 % alpha, for context. They do not set the
  scale and are clipped at the plot's edge.
- **None** draws the shown shot alone.

A ground ball (`flight::rolls`: a path whose flight is the tee alone and
whose ground segment is the roll) is drawn as a line along the ground from
the tee to rest, with the flight's stroke and color rules (gold for the
shown shot, bright, faint), a rest dot but no landing dot, and, in gold, only
its total labeled; it is hovered and clicked like any path and draws on
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
the tee at the bottom center, downrange up and right to the right
(target-relative for both hands, like offline), in yd or m; distance
rings about the tee every 25 (thinned below 60 px) are labeled where they
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
members, in accent or colored by device when the filter is on All with two
or more devices; **faint** the rest of the current session from the
filtered device, newest 200, at 18 % alpha, clipped and not setting the
scale; **None** draws the shown shot alone. In Table view the panel is
built around the latest shot of the filtered device (its rows cannot be
picked). One covariance ellipse covers the
bright and gold points (three or more), or with the filter on All and two
or more devices one per device in its color: centered on the mean, axes
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
millimeters with a side word (`4 toe`, `6 heel`, `2 high`, `5 low`; `0`
under half a millimeter; `-` without a reading), the same in both unit
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
dash. Backspin and Sidespin (card and column) always show the `launch` value,
styled as simulated (`SIM` tag, italics) when the spin came from an `assumed`
block. The simulated value wins when both are present: the flight model sets
a field the launch monitor also reported only when the simulated value is the
one to show (a `force` shot, a measured 0 carry or flight time on a shot that
flies, a ground ball's 0 carry, apex and flight time). One helper, `pick` in
`ui/src/panels/shots/mod.rs`, makes this choice for the cards, the shot list, the
table, the range bars and the Flight and Dispersion charts. Both views follow the
Imperial/Metric toggle. When the
device reports no smash factor, the Smash card and table column show ball
speed over club speed when both are known; this is display only, and the bus,
Open Connect output and export carry the device's value. When a new shot arrives the
cards glow with the accent color, a few pixels wide and following their
rounded corners, and the table's latest row gets an accent outline; both fade
over 1.5 s.

The look is defined once in `ui/src/theme.rs` (palette, egui style, pill,
badge and card helpers) and installed at startup for both the web and native
UI; the dashboard is always dark.
