# flighthook

Acts as a bridge between golf launch monitors and simulation software.
Connects to launch monitors, decodes shot data, and forwards it to integrations like GSPro.
Provides a REST and WebSocket API for custom integrations to participate on the central event bus.

## Status

> **Alpha (0.0.x)** — This project is in early development. The API, configuration format, and internal interfaces may change at any time. No semver compatibility guarantees are provided until a stable 1.0 release.

## Features

- Multi-device support
- Automatic detection mode switching based on club selection (full / chipping / putting)
- Dual UI: native desktop window (eframe/egui) and browser dashboard (WASM, same codebase)
- Configurable via TOML file with live settings updates
- REST + WebSocket API for external consumers — subscribe to shot data, device telemetry, and raw audit events in real time. Build custom shot triggers, data loggers, or alternative integrations without touching the core.

## Architecture

A single `broadcast<FlighthookMessage>` bus connects all components.
Each message carries a UTC timestamp, a typed event, and an optional raw payload (for debugging).
Session threads, integration bridges, and the web layer all subscribe to the same bus.

## Quick Start

### Prerequisites

- Rust toolchain
- [Trunk](https://trunkrs.dev/) (for WASM UI build)
- `wasm32-unknown-unknown` target (`rustup target add wasm32-unknown-unknown`)
- For Windows cross-compile: `mingw-w64` (`sudo apt install mingw-w64`)

### Build and Run

```bash
# Build everything (UI WASM + native binary)
make build

# Run with ~/.config/flighthook/config.toml
make run

# Run with a specific config
make run config=mock.toml

# Headless (web dashboard only, no native window)
make run headless=true
```

### Cross-Compile and Deploy to Windows

```bash
make deploy host=golfpc dir=Documents
```

### CLI

```
flighthook [--config PATH] [--headless]
```

| Flag         | Default                         | Description                          |
| ------------ | ------------------------------- | ------------------------------------ |
| `--config`   | platform config dir (see below) | Config file path                     |
| `--headless` | off                             | Web dashboard only, no native window |

To run with a mock device, point `--config` at a TOML file with
`[mock_monitor.0]` sections instead of `[mevo.0]`.

## Configuration

TOML file auto-created on first run at the platform config directory:

- **Linux**: `~/.config/flighthook/config.toml`
- **Windows**: `%APPDATA%\flighthook\config.toml`
- **macOS**: `~/Library/Application Support/flighthook/config.toml`

```toml
[webserver.0]
name = "Web Server"
bind = "0.0.0.0:3030"

[mevo.0]
name = "My Mevo+"
address = "192.168.2.1:5100"
ball_type = 0                  # 0 = RCT, 1 = Standard
tee_height = "1.5in"
range = "8ft"
surface_height = "0in"
track_pct = 80.0
use_partial = "chipping_only"  # never | chipping_only | always

[gspro.0]
name = "Local GSPro"
address = "127.0.0.1:921"
```

Section prefixes encode component type: `mevo`, `mock_monitor`, `gspro`,
`random_club`. The index after the dot (`0`, `1`, ...) identifies the
instance. Settings can also be edited live from the Settings tab in the UI.

## Crates

| Crate            | Path   | Description                                                                |
| ---------------- | ------ | -------------------------------------------------------------------------- |
| `flighthook`     | `lib/` | Shared type definitions (config, bus events, API types, game state)        |
| `flighthook-app` | `app/` | Bridge application binary — device sessions, GSPro integration, web server |
| `flighthook-ui`  | `ui/`  | egui dashboard (WASM + native)                                             |

See **[lib/README.md](lib/README.md)** for the full type reference.

## Documentation

- **[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)** — Multi-device config model,
  unified bus, threading, state machine, settings UI design.
- **[docs/API.md](docs/API.md)** — REST and WebSocket API reference for the web
  dashboard (`/api/status`, `/api/shots`, `/api/ws`, etc.).

## Shot Data Flow

```
Launch Monitor
  │
  ▼
ironsight        decode D4/E8/ED/EF → typed structs
  │
  ▼
ShotAccumulator  collect burst, prefer D4, fallback E8
  │
  ▼
mapper.rs        m/s → mph, m → yd, spin decomposition
  │
  ▼
GSPro            TCP 921, Open Connect V1 JSON
```

The accumulator collects all shot messages (flight result, club result, spin
result) from a single shot burst and emits a unified `ShotData` on the
"PROCESSED" text marker. If the device sends only an E8 (early distance-model
result) without a full D4, the accumulator can synthesize from E8 — controlled
by the `use_partial` setting.
