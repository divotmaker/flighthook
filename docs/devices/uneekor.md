# Uneekor

**Status: Alpha (untested)** — the actor is exercised end to end against a
simulated Open Connect client, but has not yet been run against real Uneekor
hardware.

Uneekor is supported through the **OpenConnect server** actor rather than a
native driver. Uneekor's own software already pushes shots as a
[GSPro Open Connect V1](https://gsprogolf.com/GSProConnectV1.html) client, so
flighthook listens and accepts them.

The same actor works for any monitor that emits Open Connect — Foresight,
SkyTrak, MLM2PRO and others — so it is not Uneekor-specific.

## What you get, and what you don't

This is a bridge, not a driver. Uneekor's PC software has to be running.

| | |
|---|---|
| Ball data | speed, launch angles, spin, carry |
| Club data | speed, attack angle, face to target, loft, path, face impact |
| **Not** available | Club Optix imagery, device telemetry beyond readiness, any device control (no arm/disarm, no detection-mode setting) |

Spin arrives one of two ways depending on the monitor: explicit back/side
components, or a total plus a spin axis. Both are handled — a total-and-axis
reading is decomposed into back/side on the way onto the bus.

## Licensing

Third-party output is licensed separately by Uneekor and it varies by model:

- **EYE MINI family** — needs the Pro subscription (~$199/yr).
- **Legacy QED and EYE XO** — many units carry a *perpetual* third-party
  licence tied to the device serial number.

Check what your unit has before buying anything.

## Configuration

```toml
[openconnect_server.0]
name = "Uneekor"
bind = "0.0.0.0:921"
```

The actor accepts one monitor at a time — Open Connect is a single-peer
protocol. A second monitor should be a second actor on its own port.

## Sharing port 921 with GSPro

GSPro listens on 921 too, but its port is movable. Edit
`C:\GSPro\GSPC\GSPconnect.exe.config` and set

```xml
<OpenAPIUseAltPort>true</OpenAPIUseAltPort>
```

to start GSPConnect on **922**, which frees 921 for flighthook and lets
everything run on one machine:

```text
  Uneekor software  --:921-->  flighthook  --:922-->  GSPro     (all one host)
```

```toml
[openconnect_server.0]
name = "Uneekor"
bind = "0.0.0.0:921"

[gspro.0]
name = "Local GSPro"
address = "127.0.0.1:922"   # GSPro moved aside via OpenAPIUseAltPort
```

Note this is a boolean toggle (921 ↔ 922), not an arbitrary port.

Splitting across two hosts also works, but only if your monitor's connector lets
you aim it at a non-localhost address — Open Connect's own documentation
specifies `127.0.0.1`, and not every connector exposes the setting.
