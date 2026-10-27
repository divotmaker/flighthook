# FlightScope Mevo+ / Mevo Gen2 / Xi

**Status: Supported** (Mevo+, Mevo Gen2, Xi) — decoded by
[ironsight](https://crates.io/crates/ironsight) over the device's binary TCP
protocol on port 5100.

All three use the same `[mevo.N]` device. flighthook detects which one it is
talking to when it connects. See [FlightScope Xi](#flightscope-xi) for
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
  Speed, launch angles and carry match the estimate; spin and spin axis are
  the full result's, sidespin included.
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
  sends no ball flight. With no club data either, it does not reach GSPro or
  `/api/shots`, and the dashboard's shot list shows it as a dimmed entry
  reading **No read**, the same as a
  [shot the device didn't read](#shots-the-device-didnt-read). It is still in
  the [shot export](#shot-export) with its raw frames.
- `true` — emit the estimated flight as `BallFlight`.

Estimated shots may be missing club data and sidespin. Set
`use_estimated = true` in the device section (or tick **Use Estimated Shots**
in the settings UI) to keep them.

### Shots the device didn't read

Sometimes the device triggers but tracks no ball. It then sends an estimate
with zero ball speed and no full result, and club data with zero club speed.
flighthook treats that as a missed read, not a 0 mph shot:

- No ball flight and no club data are sent for the shot, so it does not
  reach GSPro, FRP consumers get no `ball_flight` or `club_path` for it, and
  it is left out of `/api/shots` and the shot sets.
- A warning alert is raised, e.g. `Shot #3: Xi didn't track the ball (no
  read)` (`Mevo` on a Mevo+ or Mevo Gen2), and the estimate is logged with
  its hex.
- The dashboard's shot list shows the shot as a dimmed entry reading
  **No read**.
- The shot is in the [shot export](#shot-export) with its raw frames.

This applies whatever `use_estimated` is set to.

## Landing offline and descent angle

Both flight results report where the ball landed (the estimate's landing is
an early estimate, and can differ from the full result's), and flighthook
sends it as FRP `carry_offline`: the landing point's distance right (+) or left (−) of
the target line, absolute (the same sense as `launch_azimuth`, for both
hands). A full result (`D4`) also reports the ball's velocity at landing,
which gives `descent_angle`, the angle below horizontal at landing. An
estimate (`E8`) has no landing velocity, so no descent angle. Neither is sent
for a ball that did not fly (a carry, flight time or apex of 0). With the
carry offline flighthook computes the shot's curve (see
[Shot metrics](../API.md#shot-metrics)).

The Mevo reports spin as backspin and sidespin. flighthook computes total
spin and spin axis from them in the shot metrics.

## Simulated values (beta)

Carry, apex and flight time come from the device, except that a carry or
flight time of 0 on a shot that flies is not a reading and is simulated like a
missing value. So do the carry offline and descent angle above. flighthook
simulates what the device doesn't report — roll and total, their offlines,
and the descent angle of an estimated shot — and marks them as simulated
(italics, `derived` in the API); total is the measured carry plus the
simulated roll, and the simulated path lands on the measured carry and
offline. A
shot with an apex of 0 (a putt, or any bladed shot) is a ground ball: carry,
apex and flight time are 0 and the roll is simulated from the tee, on a green
when the shot was triggered in Putting mode (a mode change before its result
arrives doesn't count). Estimated (`E8`) shots carry no sidespin, so they are
not flown; one with an apex of 0 is still rolled as a ground ball. See the
README's
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
spin_source = "d4"             # d4 | e8 | auto (FlightScope Xi only)
```

Distances accept a unit suffix — `in`, `ft`, `m`, `cm`, `yd`, `mm` — and are
converted to the wire format on demand. `track_pct` is 0–100.

Changing the address, the environment, the camera mode, or any session
setting restarts the actor. `spin_source` applies from the next shot without a
restart.

## Shot export

`GET /api/shots/export` (the **JSONL** link on the Shots tab) saves
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

## FlightScope Xi

Connects and reports shots. Feedback welcome on
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
- **Two spin values.** The Xi sends two backspin values per shot, and
  `spin_source` picks which one flighthook reports. The default is `d4`, the
  value FS Golf displays. See [Spin on the Xi](#spin-on-the-xi).

Launch data (speed, VLA, HLA, carry, apex, flight time) and club data come
from the device as for the Mevo+. Estimated-only shots follow `use_estimated`
as above.

### Spin on the Xi

The Xi sends two backspin values for every shot:

- **D4**, its final spin. This is the spin FlightScope's FS Golf app displays.
- **E8**, a second value, often much higher than D4 on full swings.

On chips and pitches E8 isn't measured; it scales with ball speed instead, so
choosing `e8` there means choosing that value knowingly.

`spin_source` on the device picks which one flighthook reports:

| Value | Total spin | Spin axis |
| --- | --- | --- |
| `d4` (default) | D4's backspin, as reported | As reported |
| `e8` | E8's backspin; D4's when the shot has no E8 spin | From D4 when the shot has club data (club speed plus attack angle or path), otherwise 0; D4's when the shot has no E8 spin |
| `auto` | E8 when flighthook is in Full mode, D4 otherwise | Same as whichever of the above applies |

`d4` matches FS Golf. `e8` is steadier on full swings. `auto` gets `e8`'s
behaviour on full swings and `d4`'s elsewhere, without having to switch the
setting by hand.

**Caveat for `auto`:** flighthook decides E8 vs. D4 from its own detection
mode, not the shot itself. A chip hit while flighthook is still in Full mode
gets E8's speed-scaled value, same as choosing `e8` directly on that shot. Set
chipping clubs (so the mode switches automatically) or switch the mode by hand
before chipping.

A change applies from the next shot, without reconnecting. In the settings UI
this is the **Spin source (Xi)** dropdown on the device. The setting has no
effect on a Mevo+ or Mevo Gen2, which report a single spin value.

**Chips and pitches:** FS Golf's **Pitching** mode uses the limited-flight
detection mode, the one flighthook selects, together with short-game setup:
the distance from the Xi to the ball, the surface height, and a trigger
setting for short shots. flighthook sends only the detection mode, not that
setup (its `range` and `surface_height` are not sent either), so the Xi uses
the setup it has stored. The Xi may keep the setup an app sends until it is
switched off; this is not confirmed yet. If chips don't read, it may help to
run FS Golf in **Pitching** first, close it without switching the Xi off,
then start flighthook. To compare short shots with FS Golf, use its Pitching
mode.
