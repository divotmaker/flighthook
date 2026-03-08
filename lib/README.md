# flighthook

Shared type definitions for the flighthook launch monitor bridge. Used by both
the application crate (`flighthook-app`) and the UI crate (`flighthook-ui`).

## Configuration types

Types for the TOML config file and unit-aware value handling.

| Type                 | Description                                                                                      |
| -------------------- | ------------------------------------------------------------------------------------------------ |
| `FlighthookConfig`   | Top-level config with per-section `HashMap`s (webserver, mevo, gspro, mock_monitor, random_club) |
| `WebserverSection`   | Web server instance (name + bind address)                                                        |
| `MevoSection`        | Mevo device instance (address, ball type, tee height, range, surface height, track %)            |
| `GsProSection`       | GSPro integration instance (address, per-mode monitor routing)                                   |
| `MockMonitorSection` | Mock launch monitor instance                                                                     |
| `RandomClubSection`  | Random club cycling integration instance                                                         |
| `ShotDetectionMode`  | `Full` / `Putting` / `Chipping`                                                                  |
| `UnitSystem`         | `Imperial` / `Metric`                                                                            |
| `Distance`           | Unit-aware distance (ft, in, m, cm, yd, mm). Re-exported from `flightrelay`. Serializes as suffix string: `"1.5in"`, `"8ft"` |
| `Velocity`           | Unit-aware velocity (mph, mps, kph, fps). Re-exported from `flightrelay`. Serializes as suffix string: `"90.3mph"` |
| `DistanceExt`        | Extension trait for `Distance` — `unit_key()`, `from_value_and_unit()`, `to_mm()`                |
| `VelocityExt`        | Extension trait for `Velocity` — `unit_key()`, `from_value_and_unit()`                           |

## Bus event types

All inter-component communication flows through a single
`broadcast<FlighthookMessage>` channel.

### Message envelope

| Type                | Description                                                                           |
| ------------------- | ------------------------------------------------------------------------------------- |
| `FlighthookMessage` | Bus message: source ID, optional device ID (FRP), optional `RawPayload`, typed `FlighthookEvent` |
| `RawPayload`        | `Binary(Vec<u8>)` (serializes as hex) or `Text(String)`                               |

### Event variants (`FlighthookEvent`)

All variants are flat struct variants with named fields (no wrapper types).

| Variant              | Description                                                        |
| -------------------- | ------------------------------------------------------------------ |
| `ShotTrigger`        | Ball strike detected (carries `ShotKey`) — FRP                     |
| `BallFlight`         | Ball flight data (key, ball data) — FRP                            |
| `ClubPath`           | Club path data (key, club data) — FRP                              |
| `FaceImpact`         | Face impact location (key, impact data) — FRP                      |
| `ShotFinished`       | Shot sequence complete (key) — FRP                                 |
| `DeviceInfo`         | Device identification (manufacturer, model, firmware) — FRP        |
| `Alert`              | User-visible warn/error/critical (severity + message) — FRP        |
| `PlayerInfo`         | Player handedness update                                           |
| `ClubInfo`           | Club selection update                                              |
| `SetDetectionMode`   | Detection mode change command (full/chipping/putting)              |
| `ConfigCommand`      | Config mutation request (from POST handler)                        |
| `ConfigOutcome`      | Mutation acknowledgment (from SystemActor)                         |
| `ActorStatus`        | Actor lifecycle + telemetry                                        |

### Shot data

| Type              | Description                                                                                        |
| ----------------- | -------------------------------------------------------------------------------------------------- |
| `ShotKey`         | Shot correlation: UUID v4 `shot_id` (String) + `shot_number` (u32). Re-exported from `flightrelay` |
| `ShotData`        | Complete shot: source, shot number, optional ball flight, optional club, optional face impact       |
| `ShotAccumulator` | Low-level: collects individual shot lifecycle events into a `ShotData`                              |
| `ShotAggregator`  | High-level: feed `FlighthookMessage`s, get complete `ShotData` back when shots finish               |
| `BallFlight`      | All fields `Option`. Re-exported from `flightrelay`. Launch speed, elevation, azimuth, carry/total distance, max height, flight time, backspin/sidespin |
| `ClubData`        | All fields `Option`. Re-exported from `flightrelay`. Club speed, path, attack angle, face angle, dynamic loft, smash factor, swing plane, offset/height |
| `FaceImpact`      | All fields `Option`. Re-exported from `flightrelay`. Face impact location data                     |

### Actor status

| Type          | Description                                                                |
| ------------- | -------------------------------------------------------------------------- |
| `ActorStatus` | Lifecycle enum: `Starting` / `Disconnected` / `Connected` / `Reconnecting` |

### Commands and outcomes

| Type           | Description                                                                      |
| -------------- | -------------------------------------------------------------------------------- |
| `ConfigAction` | `ReplaceAll` / `UpsertWebserver` / `UpsertMevo` / `UpsertGsPro` / `Remove` / ... |
| `Severity`     | `Warn` / `Error` / `Critical`. Re-exported from `flightrelay`                    |

## API types

REST request/response types shared between the web server and the UI.

| Type                   | Description                                               |
| ---------------------- | --------------------------------------------------------- |
| `StatusResponse`       | `GET /api/status` -- actor map + current mode             |
| `ActorStatusResponse`  | Per-actor: name, status, telemetry map                    |
| `ModeRequest`          | `POST /api/mode` -- target detection mode                 |
| `PostSettingsResponse` | `POST /api/settings` -- lists of restarted/stopped actors |

## WebSocket client (`client` feature)

A synchronous WebSocket client for connecting to a running flighthook server.
Behind the `client` feature flag (adds `tungstenite`). Not included by default
so UI/WASM builds are unaffected.

```toml
[dependencies]
flighthook = { version = "0.0.6", features = ["client"] }
```

| Type               | Description                                              |
| ------------------ | -------------------------------------------------------- |
| `FlighthookClient` | WebSocket client. `connect`, `recv`, `try_recv`, `send`. |
| `ClientError`      | Error enum: `WebSocket`, `Json`, `Closed`                |

### Blocking

```rust
use flighthook::{FlighthookClient, ShotAggregator};

let mut client = FlighthookClient::connect("ws://localhost:5880/api/ws", "my-app")?;
let mut shots = ShotAggregator::new();

loop {
    let msg = client.recv()?;
    if let Some(shot) = shots.feed(&msg) {
        println!("shot #{}: {:?}", shot.shot_number, shot.ball.as_ref().and_then(|b| b.launch_speed));
    }
}
```

### Non-Blocking (Game Loop)

```rust
use flighthook::{FlighthookClient, ShotAggregator};

let mut client = FlighthookClient::connect("ws://localhost:5880/api/ws", "my-sim")?;
client.set_nonblocking(true)?;
let mut shots = ShotAggregator::new();

loop {
    // Drain all pending messages
    while let Ok(Some(msg)) = client.try_recv() {
        if let Some(shot) = shots.feed(&msg) {
            println!("shot #{}: {:?}", shot.shot_number, shot.ball.as_ref().and_then(|b| b.launch_speed));
        }
    }
    // ... render frame, physics tick, etc.
}
```

## Game state types

| Type                | Description                                                                                                                   |
| ------------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| `Club`              | 19-variant enum (Driver through Putter). Serializes to GSPro codes (`"DR"`, `"7I"`, `"PT"`). `mode()` maps to detection mode. |
| `ClubInfo`          | Current club selection                                                                                                        |
| `PlayerInfo`        | Player handedness                                                                                                             |
| `GameStateSnapshot` | Immutable snapshot: player info, club info, current mode                                                                      |
