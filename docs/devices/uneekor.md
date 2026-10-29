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
components, or a total plus a spin axis. Both are handled, and both FRP pairs
go onto the bus: the one the monitor sent, and the other computed from it (a
total and axis decomposed into back/side, or back/side combined into a total
and axis). Spin axis follows the same handedness conversion as sidespin.

Values the monitor doesn't send (apex, flight time, descent, offline, roll,
total, and carry when absent) are simulated from the launch, as for every
device, and marked as simulated; see the README's
[Simulated values](../../README.md#simulated-values-beta) section. When the
monitor sends carry, total is that carry plus the simulated roll. Launch
direction and sidespin arrive golf-semantically (golfer-relative, like GSPro)
and are converted to flighthook's absolute sign convention using the
handedness GSPro reports, so the simulated offlines follow the same
convention as every other device. Set handedness the same in both Uneekor's
software and GSPro, since flighthook takes handedness from GSPro.

## Licensing

Third-party output is licensed separately by Uneekor and it varies by model:

- **EYE MINI family** — needs the Pro subscription (~$199/yr).
- **Legacy QED and EYE XO** — many units carry a *perpetual* third-party
  licence tied to the device serial number.

Check what your unit has before buying anything.

## Configuration

Add it from the Devices tab: Launch monitors (+) → **Uneekor (OpenConnect)**
(or the generic **OpenConnect Server**, for any other Open Connect monitor),
which creates this section:

```toml
[openconnect_server.0]
name = "Uneekor"
bind = "0.0.0.0:921"
```

To listen on another port, flip the card to Settings and edit **Bind
Address** (`ip:port`; blank means `0.0.0.0:921`). Saving restarts the server
on the new address.

The actor accepts one monitor at a time — Open Connect is a single-peer
protocol. A second monitor should be a second actor on its own port.

## Shot export

`GET /api/shots/export` (the **JSONL** link on the Shots tab) saves
each shot with the Open Connect JSON behind it: the message the monitor sent
and flighthook's reply, verbatim apart from the peer and listen addresses,
which are replaced with `<redacted>`. The monitor's `DeviceID` is a product
label (e.g. `"Uneekor EYE XO"`) and is kept, as the device model. A monitor
could in principle put a serial number in a JSON field, so check the file
before posting it publicly.

## Sharing port 921 with GSPro

GSPro listens on 921 too, but its port is movable. Edit
`C:\GSPro\GSPC\GSPconnect.exe.config` and set

```xml
<OpenAPIUseAltPort>true</OpenAPIUseAltPort>
```

to start GSPConnect on **922**, which frees 921 for flighthook and lets
everything run on one machine. flighthook's OpenConnect server takes the
Uneekor's shots on 921 and its OpenConnect client passes them on to GSPro on
922:

```text
  Uneekor software  --:921-->  flighthook  --:922-->  GSPro     (all one host)
```

```toml
[openconnect_server.0]
name = "Uneekor"
bind = "0.0.0.0:921"

[openconnect_client.0]
name = "GSPro"
address = "127.0.0.1:922"   # GSPro moved aside via OpenAPIUseAltPort
```

Note this is a boolean toggle (921 ↔ 922), not an arbitrary port.

### Don't point the client at the server

The client's address must be GSPro's port (922 here), not the server's. A
client aimed at flighthook's own server (e.g. `127.0.0.1:921` with the
server bound to `0.0.0.0:921`) would send every shot straight back into
flighthook, round and round. flighthook refuses that connection. The server
raises an error alert in the Log, "Refused OpenConnect Client "<name>" from
<address>: it is this flighthook's own client, which would loop shots
back…". The client's card and the banner above the tabs say "Refused by this
flighthook's own OpenConnect Server "<name>": this address points at it…",
and the card stays at CONNECTING. Fix the client's address; the error
clears once it connects elsewhere.

The check is exact: the server refuses only a connection it can match to one
of this flighthook's own clients. The Uneekor software connecting from the
same computer is served as normal.

Splitting across two hosts also works, but only if your monitor's connector lets
you aim it at a non-localhost address — Open Connect's own documentation
specifies `127.0.0.1`, and not every connector exposes the setting.
