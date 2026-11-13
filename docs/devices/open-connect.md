# Open Connect Server

**Status: Alpha.** The card is exercised end to end against a simulated
Open Connect client.

The **Open Connect Server** card takes shots from launch monitor software
that sends [GSPro Open Connect V1](https://gsprogolf.com/GSProConnectV1.html).
Such software already sends its shots to a simulator as an Open Connect
client; point it at flighthook's address instead, and flighthook listens on
port 921 and accepts them.

## What you get, and what you don't

This is a bridge, not a driver: the launch monitor's own software has to be
running.

| | |
|---|---|
| Ball data | speed, launch angles, spin, carry |
| Club data | speed, attack angle, face to target, loft, path, face impact (when the software sends them) |
| **Not** available | device telemetry beyond readiness, any device control (no arm/disarm, no detection-mode setting) |

Spin arrives one of two ways depending on the software: explicit back/side
components, or a total plus a spin axis. Both are handled, and both FRP pairs
go onto the bus: the one the software sent, and the same spin in the other
pair (a total and axis decomposed into back/side, or back/side combined into
a total and axis). Both count as the monitor's reading.

Values the software doesn't send (apex, flight time, descent, offline, roll,
total, and carry when absent) are simulated from the launch, as for every
device, and marked as simulated; see the README's
[Simulated values](../../README.md#simulated-values) section. When the
software sends carry, total is that carry plus the simulated roll. Launch
direction, sidespin, spin axis, face to target and path are taken with the
software's signs: + is right of the target line for either hand, as for every
other device.

## Setup

1. In flighthook's Settings > Devices, open the **+** after Launch monitors and pick
   **Open Connect Server**. The card listens on `0.0.0.0:921`.
2. In the launch monitor's software, set its Open Connect (GSPro) output to
   flighthook's address: `127.0.0.1` when both run on one computer, else
   the flighthook computer's address, port 921.
3. The card turns green (**CONNECTED**) once the software connects.

This creates this section:

```toml
[openconnect_server.0]
name = "Open Connect Server"
bind = "0.0.0.0:921"
```

To listen on another port, flip the card to Settings and edit **Bind
Address** (`ip:port`; blank means `0.0.0.0:921`). Saving restarts the server
on the new address.

The card accepts one connection at a time: Open Connect is a single-peer
protocol. A second launch monitor needs a second card on its own port.

Like GSPro, the server tells the software the player's hand and club: once
the club is known it sends `{"Code":201,"Message":"GSPro Player
Information","Player":{"Handed":"RH","Club":"DR"}}` after the software's
first message (`Handed` is `RH` or `LH`, `Club` is GSPro's code, e.g. `W3`,
`I7`, `PW`, `PT`; the mini driver is `DR`), and again whenever either
changes. `Handed` is always sent: `RH` until a hand is chosen (by a golfer,
the header or a simulator). Nothing is sent while the club is unknown. The 201 is written at least 50 ms after a
`200`, so the two reach the software in separate reads.

## Shot export

`GET /api/shots/export` (the export icon's **JSONL** item on the Shots tab) saves
each shot with the Open Connect JSON behind it: the message the software
sent and flighthook's reply, verbatim apart from the peer and listen
addresses, which are replaced with `<redacted>`. The message's `DeviceID` is
a product label and is kept, as the device model. Software could in
principle put a serial number in a JSON field, so check the file before
posting it publicly.

## Sharing port 921 with GSPro

flighthook binds port 921, the port GSPro listens on too. GSPro's port is
movable: to run GSPro on the same computer, edit
`C:\GSPro\GSPC\GSPconnect.exe.config` and set

```xml
<OpenAPIUseAltPort>true</OpenAPIUseAltPort>
```

to start GSPConnect on **922**, which frees 921 for flighthook. flighthook's
Open Connect server takes the launch monitor's shots on 921 and its Open
Connect client passes them on to GSPro on 922:

```text
  launch monitor software  --:921-->  flighthook  --:922-->  GSPro     (all one host)
```

```toml
[openconnect_server.0]
name = "Open Connect Server"
bind = "0.0.0.0:921"

[openconnect_client.0]
name = "GSPro"
address = "127.0.0.1:922"   # GSPro moved aside via OpenAPIUseAltPort
```

This is a boolean toggle (921 or 922), not an arbitrary port.

### Don't point the client at the server

The client's address must be GSPro's port (922 here), not the server's. A
client aimed at flighthook's own server (e.g. `127.0.0.1:921` with the
server bound to `0.0.0.0:921`) would send every shot straight back into
flighthook, round and round. flighthook refuses that connection. The server
raises an error alert in the Log, "Refused Open Connect Client "<name>" from
<address>: it is this flighthook's own client, which would loop shots
back…". The client's card and an error banner say "Refused by this
flighthook's own Open Connect Server "<name>": this address points at it…",
and the card stays at CONNECTING. Fix the client's address; the error
clears once it connects elsewhere.

The check is exact: the server refuses only a connection it can match to one
of this flighthook's own clients. The launch monitor's software connecting
from the same computer is served as normal.

Splitting across two computers also works, but only if the software lets you
aim it at an address other than `127.0.0.1`: Open Connect's own
documentation specifies `127.0.0.1`, and not every program exposes the
setting.
