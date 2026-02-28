# flighthook

Shared type definitions for the flighthook launch monitor bridge. Used by both
the application crate (`flighthook-app`) and the UI crate (`flighthook-ui`).

## Configuration types

Types for the TOML config file and unit-aware value handling.

| Type                 | Description                                                                                         |
| -------------------- | --------------------------------------------------------------------------------------------------- |
| `FlighthookConfig`   | Top-level config with per-section `HashMap`s (webserver, mevo, gspro, mock_monitor, random_club)    |
| `WebserverSection`   | Web server instance (name + bind address)                                                           |
| `MevoSection`        | Mevo device instance (address, ball type, tee height, range, surface height, track %, partial mode) |
| `GsProSection`       | GSPro integration instance (address + per-mode monitor routing)                                     |
| `MockMonitorSection` | Mock launch monitor instance                                                                        |
| `RandomClubSection`  | Random club cycling integration instance                                                            |
| `ShotDetectionMode`  | `Full` / `Putting` / `Chipping`                                                                     |
| `PartialMode`        | E8 fallback policy: `Never` / `ChippingOnly` / `Always`                                             |
| `UnitSystem`         | `Imperial` / `Metric`                                                                               |
| `Distance`           | Unit-aware distance (ft, in, m, cm, yd). Serializes as suffix string: `"1.5in"`, `"8ft"`            |
| `Velocity`           | Unit-aware velocity (mph, mps, kph, fps). Serializes as suffix string: `"90.3mph"`                  |

## Bus event types

All inter-component communication flows through a single
`broadcast<FlighthookMessage>` channel.

### Message envelope

| Type                | Description                                                                           |
| ------------------- | ------------------------------------------------------------------------------------- |
| `FlighthookMessage` | Bus message: source ID, UTC timestamp, optional `RawPayload`, typed `FlighthookEvent` |
| `RawPayload`        | `Binary(Vec<u8>)` (serializes as hex) or `Text(String)`                               |

### Event variants (`FlighthookEvent`)

| Variant             | Payload             | Description                                     |
| ------------------- | ------------------- | ----------------------------------------------- |
| `LaunchMonitor`     | `LaunchMonitorRecv` | Shot data or ready-state from a launch monitor  |
| `ConfigChanged`     | `ConfigChanged`     | Actor settings changed (emitted by reconfigure) |
| `GameStateCommand`  | `GameStateCommand`  | Global state mutation (club, player, mode)      |
| `GameStateSnapshot` | `GameStateSnapshot` | Full state snapshot after mutation              |
| `UserData`          | `UserDataMessage`   | Opaque data from third-party WS clients         |
| `ActorStatus`       | `ActorState`        | Actor lifecycle + telemetry                     |
| `ConfigCommand`     | `ConfigCommand`     | Config mutation request (from POST handler)     |
| `ConfigOutcome`     | `ConfigOutcome`     | Mutation acknowledgment (from SystemActor)      |
| `Alert`             | `AlertMessage`      | User-visible warn/error                         |

### Shot data

| Type         | Description                                                                                        |
| ------------ | -------------------------------------------------------------------------------------------------- |
| `ShotData`   | Complete shot: source, shot number, ball flight, optional club + spin, estimated flag              |
| `BallFlight` | Launch speed, elevation, azimuth, carry/total distance, max height, flight time, backspin/sidespin |
| `ClubData`   | Club speed, path, attack angle, face angle, dynamic loft, smash factor, swing plane, offset/height |
| `SpinData`   | Total spin (RPM) and spin axis (degrees)                                                           |

### Actor status

| Type              | Description                                                                 |
| ----------------- | --------------------------------------------------------------------------- |
| `ActorStatus`     | Lifecycle enum: `Starting` / `Disconnected` / `Connected` / `Reconnecting`  |
| `ActorState`      | Status + key-value telemetry map (battery, tilt, club, etc.)                |
| `MevoConfigEvent` | Mevo device settings snapshot (ball type, distances, track %, partial mode) |

### Commands and outcomes

| Type                    | Description                                                                      |
| ----------------------- | -------------------------------------------------------------------------------- |
| `LaunchMonitorEvent`    | `ShotResult { shot }` or `ReadyState { armed, ball_detected }`                   |
| `GameStateCommandEvent` | `SetPlayerInfo` / `SetClubInfo` / `SetMode`                                      |
| `ConfigCommand`         | Request ID + `ConfigAction`                                                      |
| `ConfigAction`          | `ReplaceAll` / `UpsertWebserver` / `UpsertMevo` / `UpsertGsPro` / `Remove` / ... |
| `ConfigOutcome`         | Request ID + lists of restarted/stopped/started actor IDs                        |
| `AlertMessage`          | `AlertLevel` (Warn/Error) + message string                                       |

## API types

REST request/response types shared between the web server and the UI.

| Type                   | Description                                              |
| ---------------------- | -------------------------------------------------------- |
| `StatusResponse`       | `GET /api/status` — actor map + current mode             |
| `ActorStatusResponse`  | Per-actor: name, status, telemetry map                   |
| `ModeRequest`          | `POST /api/mode` — target detection mode                 |
| `PostSettingsResponse` | `POST /api/settings` — lists of restarted/stopped actors |

## Game state types

| Type                | Description                                                                                                                   |
| ------------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| `Club`              | 19-variant enum (Driver through Putter). Serializes to GSPro codes (`"DR"`, `"7I"`, `"PT"`). `mode()` maps to detection mode. |
| `ClubInfo`          | Current club selection                                                                                                        |
| `PlayerInfo`        | Player handedness                                                                                                             |
| `GameStateSnapshot` | Immutable snapshot: player info, club info, current mode                                                                      |
