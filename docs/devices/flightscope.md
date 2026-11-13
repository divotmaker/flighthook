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

A Mevo+ reports its battery level (`battery_pct`), temperature (`temp_c`)
and whether it runs on external power (`external_power`). While it is
connected, the header's Energy icon turns yellow under 20 % and red under 10 %; click it for each device's
charge. The Mevo Gen2 and the
Xi report none of the three. Every model reports `ready`, `tilt` and `roll`.

## Connection

The card connects over TCP to `address` (`phase` `connecting`, given up after
5 s), runs the handshake (`handshaking`) and configures the model it found
(`configuring`), then reads Connected with "FlightScope Mevo+", "FlightScope
Mevo Gen2" or "FlightScope Xi" and, once the device is armed, `measuring`. It starts in
the detection mode in effect then, also after a reconnect, so a mode change
made while the device was away is not lost. A mode change re-arms the device
(`configuring`, not ready, then `measuring` and ready again), right away when
it is armed or once the shot in progress is done.

The session ends when the device closes the connection, an operation times
out, or nothing at all arrives from the device for 10 s; a message the card
cannot decode raises an alert and the session carries on. The card then
reads Reconnecting and tries again 1 s after the first failed attempt,
growing by 1 s per attempt up to 15 s, and back to 1 s once a session lasted
10 s. Turning the card off, a restart or Reconnect ends a connect or a
session at once; a new connection can open while the old one still closes.

### Shots

Shots use the device's full flight result (`D4`), so FRP consumers and the
simulator outputs receive the device's spin and sidespin, plus its radar club
data (see [Club data and camera mode](#club-data-and-camera-mode)), on every
shot. A shot with only an estimate is skipped unless `use_estimated` is `true`
(see [Estimated shots](#estimated-shots)); `environment` picks the detection
mode (see [Environment](#environment)).

## Environment

`environment` sets how the device detects a full swing. FlightScope's own
app calls the two modes "Limited flight" and "Outdoor":

| Value | Use it for |
| --- | --- |
| `limited_flight` (default) | Nets and simulator screens |
| `outdoor` (beta) | Real outdoor use with full ball flight |

Use `limited_flight` unless the ball flies freely. `outdoor` tracks the whole
flight and needs about 40 yd of unobstructed flight; a shot into a net or
screen does not read in that mode. The device still sends its estimated result
but reports the full result with zero launch speed, so flighthook drops that
full result and the shot counts as estimated-only (see below).

`outdoor` is beta: it has had less real-world testing than `limited_flight`,
and the settings flighthook sends for it may still be incomplete.

The environment applies to full-swing mode only; chipping and putting modes
are unchanged. On the Xi, chipping and putting use the device's pitching shot type,
which is always limited flight (see [FlightScope Xi](#flightscope-xi)).

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
- `true` — emit the estimate as the shot's `shot.launch` and `shot.flight`.

Estimated shots may be missing club data and sidespin. Set
`use_estimated = true` in the device section (or tick **Use Estimated Shots**
in the settings UI) to keep them.

### Shots the device didn't read

Sometimes the device triggers but tracks no ball. It then sends an estimate
with zero ball speed and no full result, and club data with zero club speed.
flighthook treats that as a missed read, not a 0 mph shot:

- No ball flight and no club data are sent for the shot, so it does not
  reach GSPro, FRP consumers get no `shot.launch`, `shot.flight` or `shot.club`
  for it, and
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
hands), in the shot's `shot.flight` (`basis` `"modeled"`: the device's own
flight model) with its carry, apex and flight time. A full result (`D4`)
also reports the ball's velocity at landing,
which gives `descent_angle`, the angle below horizontal at landing. An
estimate (`E8`) has no landing velocity, so no descent angle. Neither is sent
for a ball that did not fly (a carry, flight time or apex of 0). With the
carry offline flighthook computes the shot's curve (see
[Shot metrics](../API.md#shot-metrics)).

The device reports spin as backspin and sidespin; the shot's launch carries
the total spin and spin axis worked out from them next to the pair.

## Simulated values

Carry, apex and flight time come from the device, except that a carry or
flight time of 0 on a shot that flies is not a reading and is simulated like a
missing value. So do the carry offline and descent angle above. flighthook
simulates what the device doesn't report — roll and total, their offlines,
and the descent angle of an estimated shot — and marks them as simulated
(italics, `derived` in the API); total is the measured carry plus the
simulated roll, and the simulated path lands on the measured carry and
offline. When the device reports a total distance itself, the total is the
device's and the roll is total minus carry. A
shot with an apex of 0 (a putt, or any bladed shot) is a ground ball: carry,
apex and flight time are 0 and the roll is simulated from the tee, on a green
when the shot was triggered in Putting mode (a mode change before its result
arrives doesn't count). Estimated (`E8`) shots carry no sidespin, so they are
not flown; one with an apex of 0 is still rolled as a ground ball. A full
result with launch data only is flown with an estimated spin instead; see
[Launch-only chips](#launch-only-chips-beta). See the README's
[Simulated values](../../README.md#simulated-values) section.

### Launch-only chips (beta)

On very short chips (under about 25 mph of ball speed) the Xi can send a full
result (`D4`) with launch data (ball speed, launch angle and direction) but
no spin and no carry, apex or flight time (all three 0). When such a result
was launched at 5° or more, the shot's launch carries no spin and no flight
values, and the card adds its own flight result to the shot, before the shot
finishes, that holds an assumed backspin of `launch_only_spin` × ball speed in
mph, with no sidespin. The default is 100 rpm per mph: about 1500 rpm for a
15 mph chip. The simulation then computes carry, apex, flight time and descent
(and roll and total) from the launch and that spin, and the simulator outputs
(GSPro, Pureplay) get the estimated spin with a spin axis of 0. The log shows
`shot #n: launch only, estimated backspin <rpm> rpm`. The dashboard shows the
spin as simulated (a `SIM` tag on the Backspin and Sidespin cards, italics in
the table), and the API and export flag it `spin_estimated`. On the FRP stream
(`/frp`) the shot's `shot.launch` carries no spin (it was not measured) and the
card's own `shot.flight` message carries the estimated spin in `assumed`, marked as
modeled (see the [API reference](../API.md)).

- `launch_only_spin = 0` turns it off. Such a shot then goes out with its
  launch alone (no spin, no carry, apex or flight time), and is not
  simulated.
- Not used for a shot triggered in Putting mode (a mode change before its
  result arrives doesn't count): it goes out as with the setting at 0.
- Applies to any FlightScope model, and only to a result with no spin and
  none of carry, apex or flight time. A result with spin (including an Xi
  total taken from `spin_source`), with any of the three, or launched below
  5° (a putt, a bladed shot) keeps what the device sent; one with an apex of
  0 is rolled as a ground ball.

In the settings UI this is **Launch-only spin** on the device. A change
applies from the next shot, without reconnecting.

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
waits out a 15-second warmup (counted from the end of the handshake),
applies the config between shots, and re-arms. Expect Fusion
club data from roughly the second shot of a session rather than the first, and a
`camera warmup complete` line in the log when it lands.

If the Mevo+ doesn't finish a Fusion camera setup (the Pro Package is off, or the
other Fusion mode fits its firmware), flighthook shows an alert, keeps the camera
in standard mode and arms the device anyway, so ball flight keeps working without
Fusion club data.

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
launch_only_spin = 100.0       # rpm per mph for launch-only chips; 0 = off
```

Distances accept a unit suffix — `in`, `ft`, `m`, `cm`, `yd`, `mm` — and are
converted to the wire format on demand. `track_pct` is 0–100, distances are
0 or more, `ball_type` is 0 or 1 and `address` is an IP address and port:
the settings page shows any other value under the card's fields and won't
save it, `POST /api/settings` answers `400`, and a config file holding one
keeps the card from starting, with the error on the card. A setting left
out takes the default shown above (the address `192.168.2.1:5100`).
`launch_only_spin` is 0 to 1000. A value outside that range (or not a
number) in the config file turns it off, with a warning in the log; the
settings page and `POST /api/settings` refuse to save one.

Changing the address, the environment, the camera mode, or any other device
setting restarts the card. `spin_source` and `launch_only_spin` apply from
the next shot without a restart.

## Shot export

`GET /api/shots/export` (the export icon's **JSONL** item on the Shots tab) saves
each shot with the device frames behind it: up to 8 frames before the shot's
trigger, then every frame through the device re-armed, in both directions,
as hex, each labeled with its message type and direction: the shot results
and their data, the Xi's other per-shot messages, and the acknowledgments and
re-arm commands flighthook sends. Shots that produced no usable result
(estimated-only, or a zero-speed full result) are included too.

Never captured: status (the keepalive, which carries hardware and
application IDs), device and product info (serial numbers), the DSP hardware
query, network config and WiFi scans, factory calibration, the sensor
activation exchange, time sync and the AVR config query: everything ironsight
classifies as able to identify the unit, its network or the host; the
device's free text and debug output; and everything before the handshake
finished. Those reach the log only, as hex lines at trace level (`audit`
target); the frames in the file are logged as hex lines at debug level. The
device's network name and address are not in the file.

**Xi owners:** if shots go missing or read wrong, attach a JSONL export of a
short session to the issue — it holds exactly what the device sent for each
shot.

See [docs/API.md](../API.md#get-apishotsexport) for the format.

## FlightScope Xi

Connects and reports shots. Feedback welcome on
[issue #5](https://github.com/divotmaker/flighthook/issues/5).

Add it as a normal `[mevo.N]` device; the Xi uses the same address and port
(`192.168.2.1:5100`). flighthook recognizes it during the handshake and shows
it as **FlightScope Xi**. The Xi does not report its SSID over its protocol, so
its device identity is `FlightScope Xi (<address>)`.

What differs from the Mevo+:

- **Configured through ironsight.** flighthook builds the Xi's settings from
  the device card (`range`, `surface_height`, `environment` and the detection
  mode) and hands them to ironsight, which sends the Xi its full
  configuration: parameters, radar calibration and detection mode (see
  ironsight's docs). The detection mode selects the Xi's shot type:

  | flighthook mode | Xi shot type | Detection mode |
  | --- | --- | --- |
  | Full | Full swing | from `environment`: `limited_flight` or `outdoor` |
  | Chipping | Pitching | limited flight |
  | Putting | Pitching | limited flight |

  flighthook has no putting-specific setup for the Xi, so putting uses
  Pitching too. A mode change disarms the Xi,
  reconfigures it and re-arms it; a switch between Chipping and Putting
  changes nothing on the Xi. `outdoor`
  sends the same configuration with the outdoor detection mode and has not been
  verified outdoors, so use it only for real outdoor use.
- **The Xi is set to normal power mode at every connection.** Each
  time flighthook connects, ironsight sets the Xi to normal power mode, so it
  wakes even if another app left it in standby. If the Xi does not answer
  within 2 seconds, flighthook carries on connecting. Right
  after power-on the Xi takes a few seconds before it answers; flighthook
  keeps asking about once a second and then connects as normal.
- **A refused setting is logged and skipped.** If the Xi refuses one of the
  configuration writes, flighthook logs a warning with the device's reply in
  hex and carries on with the rest, rather than waiting for a timeout and
  reconnecting.
- **Flight results are acknowledged.** ironsight acknowledges each estimated
  and full result from the Xi as it arrives. The acknowledgments
  appear in the shot export as sent frames. The Mevo+ and Mevo Gen2 need
  none.
- **`range` and `surface_height` are sent** as the radar calibration (the
  distance from the Xi to the ball and the height of the hitting surface),
  once per connection (defaults: 8 ft and 0 in). `ball_type`, `tee_height` and `track_pct` are not sent to the
  Xi.
- **No camera.** `camera_mode` does not apply.
- **No battery level.** The Xi's status does not carry a decoded battery
  level, so `battery_pct` is not reported.
- **Two spin values.** The Xi sends two backspin values per shot, and
  `spin_source` picks which one flighthook reports. The default is `d4`, the
  value FS Golf displays. See [Spin on the Xi](#spin-on-the-xi).
- **Very short chips** can arrive with launch data but no spin or carry;
  flighthook estimates their spin. See
  [Launch-only chips](#launch-only-chips-beta).

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
| `e8` | E8's backspin; D4's when the shot has no E8 spin | From D4 when the shot has club data (club speed plus attack angle or path), and the total is sent split into backspin and sidespin by it; otherwise none: the total is sent alone, with no back/side components. D4's when the shot has no E8 spin |
| `auto` | E8 when flighthook is in Full mode, D4 otherwise | Same as whichever of the above applies |

`d4` matches FS Golf. `e8` is steadier on full swings. `auto` gets `e8`'s
behavior on full swings and `d4`'s elsewhere, without having to switch the
setting by hand.

**Caveat for `auto`:** flighthook decides E8 vs. D4 from its own detection
mode, not the shot itself. A chip hit while flighthook is still in Full mode
gets E8's speed-scaled value, same as choosing `e8` directly on that shot. Add
the club you chip with to the simulator card's Chipping Clubs (so the simulator's club switches the mode
automatically, see [Open Connect](../outputs/open-connect.md#modes-and-clubs)); with no simulator card connected,
pick the mode with Shot Type in the header before chipping.

A change applies from the next shot, without reconnecting. In the settings UI
this is the **Spin source (Xi)** dropdown on the device. The setting has no
effect on a Mevo+ or Mevo Gen2, which report a single spin value.

**Chips and pitches:** in Chipping (and Putting) mode flighthook sets the Xi
to its **Pitching** shot type. To compare short shots with FS Golf, use its
Pitching mode.
