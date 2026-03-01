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
bind = "0.0.0.0:3030"

[mevo.0]
name = "Mevo WiFi"
address = "192.168.2.1:5100"
ball_type = 0
track_pct = 80.0
use_partial = "chipping_only"
tee_height = "1.5in"
range = "8ft"
surface_height = "0in"

[gspro.0]
name = "Local GSPro"
address = "127.0.0.1:921"
# full_monitor = "mevo.0"       # optional: route full-swing shots from specific monitor
# chipping_monitor = "mevo.0"   # optional: route chipping shots
# putting_monitor = "mevo.0"    # optional: route putting shots
```

- `[mevo.<idx>]` -- Mevo/Mevo+ device instance
- `[mock_monitor.<idx>]` -- mock launch monitor instance
- `[gspro.<idx>]` -- GSPro integration instance
- `[random_club.<idx>]` -- random club cycling integration instance
- `[webserver.<idx>]` -- web server instance
- `name` is **required** -- the user-visible name, editable (rename) in settings UI
- Radar settings (ball_type, tee_height, etc.) are per-mevo only
- Mock sections show only name (no address or radar fields)
- Global IDs = `"{type_prefix}.{index}"` (e.g. `mevo.0`, `gspro.0`)
- WebSocket source IDs = `"ws.{8-hex-chars}"`

### Rust types

**schemas/src/config.rs** (shared config types, used by both app and UI):

```rust
pub struct FlighthookConfig {
    pub chipping_clubs: Vec<Club>,
    pub putting_clubs: Vec<Club>,
    pub webserver: HashMap<String, WebserverSection>,
    pub mevo: HashMap<String, MevoSection>,
    pub mock_monitor: HashMap<String, MockMonitorSection>,
    pub gspro: HashMap<String, GsProSection>,
    pub random_club: HashMap<String, RandomClubSection>,
}

pub struct WebserverSection { pub name: String, pub bind: String }
pub struct MevoSection { pub name: String, pub address: Option<String>, pub ball_type: Option<u8>, pub tee_height: Option<Distance>, pub range: Option<Distance>, pub surface_height: Option<Distance>, pub track_pct: Option<f64>, pub use_partial: Option<PartialMode> }
pub struct MockMonitorSection { pub name: String }
pub struct GsProSection { pub name: String, pub address: Option<String>, pub full_monitor: Option<String>, pub chipping_monitor: Option<String>, pub putting_monitor: Option<String> }
pub struct RandomClubSection { pub name: String }
```

**schemas/src/api.rs** (shared REST API types, used by both app and UI):

```rust
pub struct StatusResponse { pub actors: HashMap<String, ActorStatusResponse>, pub mode: Option<ShotDetectionMode> }
pub struct ActorStatusResponse { pub name: String, pub status: ActorStatus, pub telemetry: HashMap<String, String> }
pub struct PostSettingsResponse { pub restart_required: bool, pub restarted: Vec<String>, pub stopped: Vec<String> }
pub struct ModeRequest { pub mode: ShotDetectionMode }
```

**app/src/state/config.rs** (TOML persistence + resolved runtime config):

```rust
// Resolved at startup
pub enum ClientMode { Mevo { addr }, Mock { mode } }
pub enum IntegrationMode { GsPro { addr }, RandomClub }

pub struct ResolvedLaunchMonitorConfig {
    pub id: String,                 // global ID (e.g. "mevo.0")
    pub name: String,               // user-visible name
    pub client_mode: ClientMode,
    pub mode: ShotDetectionMode,
    pub session_config: SessionConfig,
}

pub struct ResolvedIntegrationConfig {
    pub id: String,                 // global ID (e.g. "gspro.0")
    pub name: String,               // user-visible name
    pub mode: IntegrationMode,
}

pub struct ResolvedConfig {
    pub launch_monitors: Vec<ResolvedLaunchMonitorConfig>,
    pub integrations: Vec<ResolvedIntegrationConfig>,
    pub webservers: Vec<(String, SocketAddr)>,
}
```

Helper functions: `global_id(prefix, index) -> String` builds `"{prefix}.{index}"`,
`load(path)` / `save_to(path, config)` for TOML I/O.

## Type Boundary Rule

**All types that cross the app<->UI boundary (REST API, WebSocket, config)
MUST live in `flighthook`.** The UI crate never defines its own
"response" mirror types. Both crates import the canonical type from schemas.
This eliminates drift between serializer (app) and deserializer (UI) —
e.g., `ActorStatus` enum vs string, `PartialMode` enum vs string.

## Unified FlighthookMessage Bus

All communication between components flows through a single
`broadcast<FlighthookMessage>(1024)` channel. Every message carries a
timestamp, optional raw payload (hex-first policy), and a typed event.
Producers create messages; consumers subscribe and filter by event kind.

```rust
pub struct FlighthookMessage {
    pub source: String,                   // global ID of originator
    pub timestamp: DateTime<Utc>,
    pub raw_payload: Option<RawPayload>,
    pub event: FlighthookEvent,
}
```

### RawPayload

Wire data attached to bus messages. Follows the hex-first policy.

```rust
pub enum RawPayload {
    Binary(Vec<u8>),    // serializes as lowercase hex string (no spaces)
    Text(String),       // serializes as-is (e.g. GSPro JSON)
}
```

### FlighthookEvent

The typed event payload. Tagged with `kind` in JSON serialization.

```rust
pub enum FlighthookEvent {
    LaunchMonitor(LaunchMonitorRecv),
    ConfigChanged(ConfigChanged),
    GameStateCommand(GameStateCommand),
    GameStateSnapshot(GameStateSnapshot),
    UserData(UserDataMessage),
    ActorStatus(ActorState),
    ConfigCommand(ConfigCommand),     // config mutation request
    ConfigOutcome(ConfigOutcome),       // config mutation acknowledgment
    Alert(AlertMessage),              // user-visible warn/error
}
```

### LaunchMonitor -- shot data from a launch monitor

Source is on `FlighthookMessage.source`, not repeated in the inner type:

```rust
pub struct LaunchMonitorRecv {
    pub event: LaunchMonitorEvent,
}

pub enum LaunchMonitorEvent {
    ShotResult { shot: Box<ShotData> },
    ReadyState { armed: bool, ball_detected: bool },
}
```

### ConfigChanged -- generic configuration update

Emitted by `reconfigure()` when an actor's settings change. Consumed by
integrations (e.g. the GSPro bridge reads `use_partial` from this event).

```rust
pub struct ConfigChanged {
    pub config: MevoConfigEvent,
}
```

### GameStateCommand -- from integrations / WS clients

```rust
pub struct GameStateCommand {
    pub event: GameStateCommandEvent,  // originator in FlighthookMessage.source
}

pub enum GameStateCommandEvent {
    SetPlayerInfo { player_info: PlayerInfo },
    SetClubInfo { club_info: ClubInfo },
    SetMode { mode: ShotDetectionMode },
}
```

The `SystemActor` auto-derives `SetMode` from `SetClubInfo` (using
`config.club_mode()` to map club to detection mode via the configurable
`chipping_clubs`/`putting_clubs` lists). Integrations only
need to emit `SetClubInfo`; the SystemActor handles mode derivation
centrally. Launch monitor actors react to `SetMode` to reconfigure the
device. Mode is global state, not per-device.

### ActorStatus -- generic actor lifecycle

All actors emit `ActorStatus` events on the bus with a generic status + key/value
state map. Replaces per-actor-type status events (`StateChanged`, `Telemetry`,
`LaunchMonitorInfo`, `ReadyStatus`, `InternalEvent`).

```rust
pub enum ActorStatus { Starting, Disconnected, Connected, Reconnecting }

pub struct ActorState {
    pub status: ActorStatus,
    pub telemetry: HashMap<String, String>,   // actor-specific k/v pairs
}
```

`ActorState` is the bus event type. `ActorStatusResponse` (in `schemas/api.rs`)
adds a `name` field and is used as the cached per-actor state in both the web
layer and the UI.

Launch monitor actors use telemetry keys:
`mode`, `armed`, `shooting`, `battery_pct`, `tilt`, `roll`, `temp_c`,
`external_power`, `device_info`. Mock launch monitors add `shot_count`
and `tracking_mode`.

Integration actors use telemetry keys:
`club`, `handed`, `error`.

### ConfigCommand / ConfigOutcome -- event-sourced config mutations

Config mutations are routed through the bus as typed events, processed
exclusively by SystemActor. This parallels the GameState pattern (sole
writer via `GameStateWriter`).

```rust
pub struct ConfigCommand {
    pub request_id: Option<String>,  // correlation ID for request-reply
    pub action: ConfigAction,
}

pub enum ConfigAction {
    ReplaceAll { config: FlighthookConfig },        // POST /api/settings
    UpsertMevo { index: String, section: MevoSection },
    UpsertGsPro { index: String, section: GsProSection },
    UpsertWebserver { index: String, section: WebserverSection },
    UpsertMockMonitor { index: String, section: MockMonitorSection },
    UpsertRandomClub { index: String, section: RandomClubSection },
    Remove { id: String },                          // "mevo.0", "gspro.1", "webserver.0", etc.
}

pub struct ConfigOutcome {
    pub request_id: String,
    pub restart_required: bool,
    pub restarted: Vec<String>,
    pub stopped: Vec<String>,
    pub started: Vec<String>,
}
```

The POST handler uses request-reply: emit `ConfigCommand` with a `request_id`,
subscribe to the bus, and wait for `ConfigOutcome` with the matching ID.

## GameState

`GameState` in `state/game.rs` -- read-only handle for the current round's
`PlayerInfo` (handedness), `ClubInfo` (club selection), and
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
`ActorStatus` events on the bus via `emit_device_status()` or
`emit_integration_status()`.

```
Starting -> Connected -> (error) -> Reconnecting -> Connected -> ...
   |                                     ^
   +-> Disconnected --------------------+
```

| ActorStatus  | Meaning                                   |
| ------------ | ----------------------------------------- |
| Starting     | Actor spawned, not yet connected          |
| Disconnected | No connection to device/service           |
| Connected    | Active and operational                    |
| Reconnecting | Lost connection, backing off before retry |

**Mevo actor mapping** (from internal phases):

- `Connecting | Handshaking | Configuring | Arming` -> `Starting`
- `Armed` -> `Connected` with `telemetry["armed"] = "true"`
- `Shooting` -> `Connected` with `telemetry["shooting"] = "true"`
- `Disconnected` -> `Disconnected`
- `Reconnecting` -> `Reconnecting`

Post-shot cycle: `Connected(shooting)` -> `Starting` (re-arm) -> `Connected(armed)`.

**Integration readiness**: the GSPro actor always reports `launch_monitor_is_ready`
and `launch_monitor_ball_detected` as `true` in both heartbeats and shot messages.

## SystemActor

The `SystemActor` (`actors/system.rs`) is a default actor that always runs,
independent of config. It holds the sole `GameStateWriter`, `Arc<SystemState>`,
and `broadcast::Sender`, enforcing at the type level that only `SystemActor`
can mutate game state and process config mutations. It handles:

- **Game state**: subscribes to `GameStateCommand` bus events, updates game
  state via the writer (player info, club selection, detection mode).
  **Auto-derives detection mode from club selection**: when `SetClubInfo` is
  received, calls `club_to_mode()` and emits `SetMode` on the bus. This
  centralizes mode derivation so all integrations trigger mode changes.
- **Config mutations**: subscribes to `ConfigCommand` bus events (from the
  REST API). Applies the mutation to `SystemConfig`, calls
  `apply_config_reload()` to reconcile actors, and emits a `ConfigOutcome`
  if the command had a `request_id` (request-reply pattern). This provides
  natural sequencing — all config mutations are processed one at a time on
  the SystemActor thread.
- Ensures game state and config are consistent even without the web server

Created via `SystemActor::new(writer, state, bus_tx)` in `main()` before
config-driven actors, registered with ID `"system"`. Skipped by
`apply_config_reload()` (not config-driven).

`actors/system.rs` also contains the `create_and_start_actor()` factory and
`apply_config_reload()` reconciliation function (moved from `main.rs`).

The web layer's `state_updater` task does NOT update game state — it only
caches club/handed values in the per-actor telemetry map for the UI. It
also handles `ConfigOutcome` events to refresh actor name caches.

## Component Identity

All components are identified by type-prefixed global IDs: `mevo.0`, `gspro.0`,
`mock_monitor.0`, `random_club.0`, `webserver.0`, `ws.a1b2c3d4`. The `system`
actor has a fixed ID of `"system"`. The type prefix encodes the component type;
the index is the key within that type's config section.

`FlighthookMessage.source` carries the global ID of the message originator.
Inner event types no longer carry redundant ID or name fields.

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
The log retains the last 500 events. Protocol-level tracing is always emitted
to the `audit` tracing target (filtered by `RUST_LOG`, not shown by default).

## WebSocket Init Protocol

Single WS connection (`/api/ws`), three-phase handshake:

1. Client sends: `{ "type": "start", "name": "My Dashboard" }`
2. Server responds: `{ "type": "init", "source_id": "abc123", "global_state": { ... } }`
3. Server streams `FlighthookMessage` events (serialized directly as JSON)

Bus events are serialized and forwarded as-is. The `kind` tag on
`FlighthookEvent` identifies the event type. Consumers filter by `kind` and
nested event `type` tags.

Client -> server commands:

```json
{ "cmd": "mode", "mode": "putting" }
```

Mode commands emit `GameStateCommand::SetMode` on the bus (mode is global,
not per-device). Config updates go through `POST /api/settings` →
`ConfigCommand` on the bus → SystemActor processes → `ConfigOutcome` reply.

## Threading Model

```
main thread (eframe)       std::thread per actor              tokio runtime (background)
+-----------------+        +------------------------+        +------------------------+
| eframe::run_    |        | actors::system         |        | axum web server        |
|   native() or   |        |   (GameState updates)  |        | state_updater task     |
| ctrl-c (headless|        | actors::mevo           |        | drain subscriber       |
+-----------------+        | actors::mock::launch   |        +------------------------+
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
in their event loop. They filter for `ConfigChanged` events (from
`reconfigure()`) and `GameStateCommand` events (including `SetMode` for mode
changes). The `SystemActor` auto-derives `SetMode` from `SetClubInfo` using
`Club::mode()`, so integrations only need to emit `SetClubInfo`. There is
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
construction params and returns `NoChange`, `Applied`, or `RestartRequired`.
Actors are registered in `SystemState.actors` for dynamic
start/stop/reconfigure.

**Per-actor spawning**: the `SystemActor` is created first (always-on, not
config-driven). Then each config section constructs an actor struct and calls
`actor.start(state, sender, receiver)`. Each actor gets a per-actor
`Arc<AtomicBool>` shutdown flag shared between the `BusReceiver` and the
registry. Launch monitor actors (mevo, mock::launch) run event loops polling
the bus. Integration actors (gspro, mock::randomclub) subscribe to the bus
and emit `GameStateCommand` (for club/player changes) and `ActorStatus`
(for connection status) back onto the bus.

**Drain subscriber**: a tokio task that consumes all bus messages to keep the
broadcast channel healthy when no other subscriber is active.

## Config Reload

Config reloads are event-sourced through the bus. All config mutations flow
as `ConfigCommand` events, processed exclusively by `SystemActor`:

- **REST API** -- `POST /api/settings` emits `ConfigCommand::ReplaceAll` on
  the bus with a `request_id`, then awaits the matching `ConfigOutcome`
  (request-reply pattern, 10s timeout).
- **Per-section upserts** -- `POST /api/settings?scope=<id>` emits a scoped
  `ConfigAction` variant (e.g. `UpsertMevo`).

`SystemActor::handle_config_command()`:

1. Applies the `ConfigAction` mutation to `SystemConfig` (replace, reload,
   upsert, or remove)
2. Calls `apply_config_reload()` to reconcile actors
3. Detects webserver bind changes (old vs new config snapshot)
4. Emits `ConfigOutcome` on the bus if `request_id` was present

`apply_config_reload()` (in `actors/system.rs`) orchestrates actor lifecycle:

1. Snapshot the new config and build a `ResolvedConfig`
2. Compute current vs expected actor IDs
3. **Deleted actors** (current but not expected): stop + remove
4. **Existing actors**: call `reconfigure()` on each
   - `Applied`: config sent to running actor via bus (e.g. radar settings)
   - `RestartRequired`: stop old actor, create and start new one
   - `NoChange`: skip
5. **New actors** (expected but not current): create via `create_and_start_actor()`

The web layer's `state_updater` handles `ConfigOutcome` events to refresh
actor name caches and remove stopped actors.

`create_and_start_actor()` is a factory function that constructs the right actor
struct, creates a per-actor `Arc<AtomicBool>` shutdown flag, calls `start()`,
and registers the actor + flag in `SystemState`.

`ReconfigureOutcome` enum:

- `NoChange` -- default for mock actors
- `Applied` -- MevoActor: session config differs, emitted `ConfigChanged` on bus
- `RestartRequired` -- address changed or section removed; must stop and recreate

## Settings Panel

Type-prefixed sections. Keys are integer indices (never displayed); `name` is
the required user-visible name shown in headers and editable via the "Name"
field. Each section has its own Save button (red when dirty, gray when clean).
Saving one section only persists and reconfigures that section -- other sections
with unsaved changes keep their dirty state.

```
Global
  Default Units: [Imperial v]                     [Save] [API Docs]

  Web Server             [Web]    [Remove] [Save]
    Name: [Web Server]
    Address: [0.0.0.0:3030]

  Mevo WiFi              [Mevo]   [Remove] [Save]
    Name: [Mevo WiFi]
    Address: [192.168.2.1:5100]
    Ball Type: [RCT v]
    Tee Height: [1.5] [in v]
    Monitor-to-Ball: [8.0] [ft v]
    Surface Height: [0.0] [in v]
    Track %: [80] %
    Partial: [Chipping only v]

  Local GSPro            [GSPro]  [Remove] [Save]
    Name: [Local GSPro]
    Address: [127.0.0.1:921]

  [+ Add v]  (dropdown: Mevo, GSPro, Web Server)
```

- Name is **required** and serves as the rename mechanism. Changing a device name
  updates the name cache in WebState, so subsequent status responses and shot
  data use the new name.
- Type shown as a badge label next to the heading
- Mock launch monitor and Random Club are developer-only types (must be added
  manually to the config TOML; they do not appear in the Add dropdown)
- Add/remove modify the form Vec, save writes back to TOML
- New entries get sequential integer indices (0, 1, 2...) and default names
- Per-section save: each Save button builds a scoped config (original config +
  only that section's changes) and passes `?scope=<actor_id>` to the backend.
  The backend only reconfigures the target actor. The UI stores the original
  config from the last load/save and updates it per-section on successful save.
- Config changes auto-reconfigure actors (address changes restart, radar settings apply in-place)
- Webserver bind changes trigger a restart (same as address changes for other actors)

### API

`GET/POST /api/settings` returns/accepts the full `FlighthookConfig` shape
(type-prefixed maps + webserver section). `POST` emits a `ConfigCommand::ReplaceAll`
on the bus and waits for a `ConfigOutcome` reply.

## Telemetry Tab

Unified actor status panel. All actors (launch monitors and integrations)
are rendered uniformly from the `actors` map. Telemetry key/value pairs are
rendered one per row, sorted alphabetically, indented under the actor header.

```
  [Telemetry] [Shots] [Audit] [Settings]  |  [Full] [Chipping] [Putting]  FLIGHTHOOK

  Mevo WiFi              [CONNECTED]
    battery_pct: 85
    roll: -0.2
    temp_c: 32
    tilt: 0.5

  Local GSPro            [CONNECTED]
    club: 7I
    handed: RH

  Random Club            [CONNECTED]
    club: DR
    handed: LH
```

Detection mode is **global state** (not per-device). The mode selector
buttons are in the tab bar, between the tab selectors and the title.
Clicking a mode button emits `GameStateCommand::SetMode` on the bus.
Launch monitor actors react to `SetMode` to reconfigure the device.

Each actor shows a name and status badge (green=connected, yellow=starting,
red=disconnected/reconnecting). Telemetry (battery, tilt, roll, temp)
and club/handed are read from the actor's telemetry map.

The web layer caches telemetry from `ActorStatus` events plus club/handed from
`GameStateCommand` events so reconnecting clients recover latest values
immediately. If any actor has status `disconnected` or `reconnecting`, the
Telemetry tab label turns red.
