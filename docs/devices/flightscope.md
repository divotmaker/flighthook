# FlightScope Mevo+ / Mevo Gen2

**Status: Supported** — decoded by
[ironsight](https://crates.io/crates/ironsight) over the device's binary TCP
protocol on port 5100.

Connect over the launch monitor's WiFi network. The device reports its SSID
during the handshake, and flighthook uses that as the FRP device identity.

## Estimated shots

Shot data arrives across several messages. A full result (`D4`) is the
authoritative one; when it never arrives, the device may still have sent an
estimated flight result (`E8`).

`use_estimated` controls what happens then:

- `true` (default) — emit the estimated flight as `BallFlight`.
- `false` — skip estimated-only shots entirely.

Estimated shots may be missing club data and sidespin.

## Club data and camera mode

Ball flight needs nothing beyond the defaults. **Club data — path, face angle,
attack angle, dynamic loft, smash factor, swing planes — comes from the device's
Fusion camera processing**, which is off unless you ask for it. `camera_mode`
selects it:

| Mode | Capture | Reports |
| --- | --- | --- |
| `standard` (default) | 1024x768 | Ball flight only |
| `fusion` | 1640x1232 JPEG | Ball flight + club data |
| `raw_fusion` | 640x480 @ 180fps | Ball flight + club data |

Two things gate club data, and missing either produces ball flight alone with no
error:

- **The Pro Package must be enabled on the device**, through FlightScope's own
  app. flighthook cannot turn it on and cannot see whether it is on.
- **The Fusion variant must match the firmware.** `raw_fusion` is for BM17.04
  and newer, where the device's Pi does the processing itself; `fusion` is for
  older firmware, where capture is driven from the host. If one yields no club
  data, try the other.

A Fusion mode is not applied at connect. The device's camera boots in standard
mode and only accepts a Fusion config once that has finished, so flighthook
waits out a 15-second warmup, applies the config, and re-arms. Expect club data
from roughly the second shot of a session rather than the first, and a
`camera warmup complete` line in the log when it lands.

Face impact is not reported in any mode.

## Configuration

```toml
[mevo.0]
name = "My Mevo+"
address = "192.168.2.1:5100"
ball_type = 0                  # 0 = RCT, 1 = Standard
tee_height = "1.5in"
range = "8ft"
surface_height = "0in"
track_pct = 80.0
camera_mode = "standard"       # standard | fusion | raw_fusion
```

Distances accept a unit suffix — `in`, `ft`, `m`, `cm`, `yd`, `mm` — and are
converted to the wire format on demand. `track_pct` is 0–100.

Changing the address, the camera mode, or any session setting restarts the
actor.
