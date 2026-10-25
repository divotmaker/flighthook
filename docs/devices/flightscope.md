# FlightScope Mevo+ / Mevo Gen2 / Xi

**Status: Supported** (Mevo+, Mevo Gen2) / **Alpha** (Xi) — decoded by
[ironsight](https://crates.io/crates/ironsight) over the device's binary TCP
protocol on port 5100.

All three use the same `[mevo.N]` device. flighthook detects which one it is
talking to when it connects. See [FlightScope Xi](#flightscope-xi-alpha) for
how the Xi differs.

Connect over the launch monitor's WiFi network. A Mevo+ or Mevo Gen2 reports
its SSID during the handshake, and flighthook uses that as the FRP device
identity.

### Upgrade notes

For anyone updating from flighthook 0.1.x with a Mevo+ or Mevo Gen2:

- **Shots use the device's full flight result** (`D4`) rather than its early
  estimate (`E8`), so GSPro and FRP consumers receive the device's spin and
  sidespin, plus its radar club data (see
  [Club data and camera mode](#club-data-and-camera-mode)), on every shot.
  Speed, launch angles and carry are unchanged; spin and spin axis will differ
  from earlier releases, which sent the estimate with zero sidespin and no
  club data.
- **`use_estimated` defaults to `false`.** A shot with only an estimate is
  skipped unless you set it to `true` (see [Estimated shots](#estimated-shots)).
- **New `environment` setting** (see [Environment](#environment)); the default,
  `limited_flight`, is the detection mode used before.

## Environment

`environment` sets the detection mode for full-swing shots. FlightScope's own
app calls the two modes "Limited flight" and "Outdoor":

| Value | Detection mode | Use it for |
| --- | --- | --- |
| `limited_flight` (default) | 9 | Nets and simulator screens |
| `outdoor` (beta) | 8 | Real outdoor use with full ball flight |

Use `limited_flight` unless the ball flies freely. `outdoor` tracks the whole
flight and needs about 40 yd of unobstructed flight; a shot into a net or
screen does not read in that mode. The device still sends its estimated result
but reports the full result with zero launch speed, so flighthook drops that
full result and the shot counts as estimated-only (see below).

`outdoor` is beta: flighthook sends the outdoor detection mode alone, and
FlightScope's app also changes a few radar settings when it switches to
Outdoor, which flighthook does not.

On a Mevo+ or Mevo Gen2 the environment applies to full-swing mode only;
chipping and putting modes are unchanged. On the Xi it sets the only mode used.

In the settings UI this is the **Environment** dropdown on the device.

## Estimated shots

Shot data arrives across several messages. A full result (`D4`) is the
authoritative one; when it never arrives, the device may still have sent an
estimated flight result (`E8`). A full result with zero launch speed (the
device's distance model failed, as in `outdoor` into a screen) is dropped and
logged with its hex, and the shot is handled as if no full result arrived.

`use_estimated` controls what happens then:

- `false` (default) — skip estimated-only shots. A shot with only an estimate,
  such as a flight too short to read in `outdoor` mode or a very short chip,
  does not reach GSPro or the shot list. It is still in the
  [shot export](#shot-export) with its raw frames.
- `true` — emit the estimated flight as `BallFlight`.

Estimated shots may be missing club data and sidespin. Set
`use_estimated = true` in the device section (or tick **Use Estimated Shots**
in the settings UI) to keep them.

## Simulated values (beta)

Carry, apex and flight time come from the device. flighthook simulates what it
doesn't report — offline, descent angle, roll and total — and marks them as
simulated (italics, `derived` in the API); total is the measured carry plus the
simulated roll. Estimated (`E8`) shots carry no sidespin, so they are not
simulated. See the README's
[Simulated values](../../README.md#simulated-values-beta) section.

## Club data and camera mode

Ball flight and radar club data need nothing beyond the defaults. **Every Mevo
shot carries the device's radar club data** — club speed, attack angle and
path, plus face angle and dynamic loft where the device provides them — in any
camera mode, and flighthook sends it to GSPro with the shot.

**The full club data set — path, face angle, attack angle, dynamic loft, smash
factor, swing planes — comes from the device's Fusion camera processing**,
which is off unless you ask for it. `camera_mode` selects it:

| Mode | Capture | Reports |
| --- | --- | --- |
| `standard` (default) | 1024x768 | Ball flight + radar club data |
| `fusion` | 1640x1232 JPEG | Ball flight + full club data |
| `raw_fusion` | 640x480 @ 180fps | Ball flight + full club data |

Two things gate the Fusion club data, and missing either produces ball flight
and radar club data alone with no error:

- **The Pro Package must be enabled on the device**, through FlightScope's own
  app. flighthook cannot turn it on and cannot see whether it is on.
- **The Fusion variant must match the firmware.** `raw_fusion` is for BM17.04
  and newer, where the device's Pi does the processing itself; `fusion` is for
  older firmware, where capture is driven from the host. If one yields no
  Fusion club data, try the other.

A Fusion mode is not applied at connect. The device's camera boots in standard
mode and only accepts a Fusion config once that has finished, so flighthook
waits out a 15-second warmup, applies the config, and re-arms. Expect Fusion
club data from roughly the second shot of a session rather than the first, and a
`camera warmup complete` line in the log when it lands.

Face impact is not reported in any mode.

## Configuration

```toml
[mevo.0]
name = "My Mevo+"
address = "192.168.2.1:5100"
ball_type = 1                  # 0 = Standard, 1 = RCT
tee_height = "1.5in"
range = "8ft"
surface_height = "0in"
track_pct = 80.0
environment = "limited_flight" # limited_flight | outdoor
camera_mode = "standard"       # standard | fusion | raw_fusion
use_estimated = false          # true keeps estimated-only (E8) shots
```

Distances accept a unit suffix — `in`, `ft`, `m`, `cm`, `yd`, `mm` — and are
converted to the wire format on demand. `track_pct` is 0–100.

Changing the address, the environment, the camera mode, or any session
setting restarts the actor.

## Shot export

`GET /api/shots/export` (the **JSONL (with raw)** link on the Shots tab) saves
each shot with the device frames behind it: every frame from just before the
BALL TRIGGER through the device re-armed, in both directions, as hex with its
type and bus addresses — the shot texts, `E8`, `D4`, `ED`, `EF`, PRC and speed
profile data, the Xi's other per-shot messages, and the acknowledgements and
re-arm commands flighthook sends. Shots that produced no usable result
(estimated-only, or a zero-speed full result) are included too.

Never captured: status (the keepalive, which carries hardware and
application IDs), device and product info (serial numbers), the DSP hardware
query, network config and WiFi scans, factory calibration, time sync and the
AVR config query. The device's network name and address are not in the file.

**Xi owners:** if shots go missing or read wrong, attach a JSONL export of a
short session to the issue — it holds exactly what the device sent for each
shot.

See [docs/API.md](../API.md#get-apishotsexport) for the format.

## FlightScope Xi (alpha)

**Status: Alpha** — connects and reports shots. Feedback welcome on
[issue #5](https://github.com/divotmaker/flighthook/issues/5).

Add it as a normal `[mevo.N]` device; the Xi uses the same address and port
(`192.168.2.1:5100`). flighthook recognises it during the handshake and shows
it as **FlightScope Xi**. The Xi does not report its SSID over its protocol, so
its device identity is `FlightScope Xi (<address>)`.

What differs from the Mevo+:

- **Detection mode comes from `environment`** (limited flight by default).
  Only limited flight has been seen working on an Xi so far; use `outdoor`
  only for real outdoor use. Mode changes from
  club selection are logged and not applied, so **putting and chipping modes
  are not yet supported** on the Xi.
- **Radar settings are not sent.** `ball_type`, `tee_height`, `range`,
  `surface_height` and `track_pct` are ignored; the Xi uses the values stored
  on the device.
- **No camera.** `camera_mode` does not apply.
- **No battery level.** The Xi's status does not carry a decoded battery
  level, so `battery_pct` is not reported.
- **Spin uses the Xi's estimated spin** (from its `E8` result) as total spin.
  When the shot also has club data (club speed plus attack angle or path), the
  spin axis comes from the full result; otherwise the axis is 0 and all of the
  spin is backspin. If the estimate is missing, the full result's spin is used.

Launch data (speed, VLA, HLA, carry, apex, flight time) and club data come
from the device as for the Mevo+. Estimated-only shots follow `use_estimated`
as above.
