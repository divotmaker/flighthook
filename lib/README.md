# flighthook

Shared type definitions for the flighthook launch monitor bridge. Used by both
the application crate (`flighthook-app`) and the UI crate (`flighthook-ui`).

## Configuration types

Types for the TOML config file and unit-aware value handling.

| Type                 | Description                                                                                         |
| -------------------- | --------------------------------------------------------------------------------------------------- |
| `FlighthookConfig`   | Top-level config with per-section `HashMap`s (webserver, mevo, gspro, mock_monitor, random_club)    |
| `WebserverSection`   | Web server instance (name + bind address)                                                           |
| `MevoSection`        | Mevo device instance (address, ball type, tee height, range, surface height, track %)               |
| `GsProSection`       | GSPro integration instance (address, per-mode monitor routing, estimated mode)                      |
| `MockMonitorSection` | Mock launch monitor instance                                                                        |
| `RandomClubSection`  | Random club cycling integration instance                                                            |
| `ShotDetectionMode`  | `Full` / `Putting` / `Chipping`                                                                     |
| `EstimatedMode`      | Estimated ball flight forwarding policy: `Never` / `ChippingOnly` / `Always`                        |
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

All variants are flat struct variants with named fields (no wrapper types).

| Variant                | Description                                              |
| ---------------------- | -------------------------------------------------------- |
| `ShotTrigger`          | Ball strike detected (carries `ShotKey`)                 |
| `BallFlight`           | Ball flight data (key, ball data, estimated flag)        |
| `ClubPath`             | Club path data (key, club data)                          |
| `ShotFinished`         | Shot sequence complete (key)                             |
| `LaunchMonitorState`   | Armed/ball-detected state from a launch monitor          |
| `PlayerInfo`           | Player handedness update                                 |
| `ClubInfo`             | Club selection update                                    |
| `ShotDetectionMode`    | Global detection mode change                             |
| `ConfigCommand`        | Config mutation request (from POST handler)              |
| `ConfigOutcome`        | Mutation acknowledgment (from SystemActor)               |
| `ActorStatus`          | Actor lifecycle + telemetry                              |
| `Alert`                | User-visible warn/error                                  |

### Shot data

| Type              | Description                                                                                        |
| ----------------- | -------------------------------------------------------------------------------------------------- |
| `ShotKey`         | Shot correlation: UUID v4 `shot_id` (String) + `shot_number` (i32)                                 |
| `ShotData`        | Complete shot: source, shot number, ball flight, optional club, estimated flag                      |
| `ShotAccumulator` | Collects shot lifecycle events into a `ShotData`                                                   |
| `BallFlight`      | Launch speed, elevation, azimuth, carry/total distance, max height, flight time, backspin/sidespin  |
| `ClubData`        | Club speed, path, attack angle, face angle, dynamic loft, smash factor, swing plane, offset/height |

### Actor status

| Type              | Description                                                                 |
| ----------------- | --------------------------------------------------------------------------- |
| `ActorStatus`     | Lifecycle enum: `Starting` / `Disconnected` / `Connected` / `Reconnecting`  |

### Commands and outcomes

| Type                | Description                                                                      |
| ------------------- | -------------------------------------------------------------------------------- |
| `ConfigAction`      | `ReplaceAll` / `UpsertWebserver` / `UpsertMevo` / `UpsertGsPro` / `Remove` / ... |
| `AlertLevel`        | `Warn` / `Error`                                                                 |

## API types

REST request/response types shared between the web server and the UI.

| Type                   | Description                                              |
| ---------------------- | -------------------------------------------------------- |
| `StatusResponse`       | `GET /api/status` -- actor map + current mode            |
| `ActorStatusResponse`  | Per-actor: name, status, telemetry map                   |
| `ModeRequest`          | `POST /api/mode` -- target detection mode                |
| `PostSettingsResponse` | `POST /api/settings` -- lists of restarted/stopped actors |

## Game state types

| Type                | Description                                                                                                                   |
| ------------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| `Club`              | 19-variant enum (Driver through Putter). Serializes to GSPro codes (`"DR"`, `"7I"`, `"PT"`). `mode()` maps to detection mode. |
| `ClubInfo`          | Current club selection                                                                                                        |
| `PlayerInfo`        | Player handedness                                                                                                             |
| `GameStateSnapshot` | Immutable snapshot: player info, club info, current mode                                                                      |
