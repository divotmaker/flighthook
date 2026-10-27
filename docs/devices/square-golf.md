# Square Golf Omni

**Status: Supported** — decoded by
[allsquare](https://crates.io/crates/allsquare) over BLE (GATT). No pairing
required.

The original **Square / Square Home is not supported** — it uses a different
club-code scheme.

## Face impact (beta)

flighthook forwards the Omni's face impact location to GSPro as
`HorizontalFaceImpact` / `VerticalFaceImpact`, in millimetres from face centre.

The calibration comes from [allsquare](https://crates.io/crates/allsquare) and
is in beta. The Omni measures vertical impact from the bottom edge of the club
sticker's dot, so allsquare adds the selected club's distance from the bottom of
the dot down to face centre. The defaults assume the sticker is in its recommended spot, with the dot centre about 5 mm
below the top of the club. A sticker placed higher or lower shifts vertical
impact by the same amount. See allsquare's README for the default table and
caveats, and [divotmaker/allsquare#1](https://github.com/divotmaker/allsquare/issues/1)
to help tune it.

Keep the club selected in the simulator in sync with the club in hand: the
vertical offset follows it. The putter has no vertical estimate, so only
horizontal impact is sent for putts.

The Omni has no code for MD, 2W, 4W, 6W, 8W, 9W, 1H, 2H, 6H, 7H, 8H, 9H, 1I or
2I. When the simulator selects one of these, flighthook sets the device to the
nearest club of the same kind it does have (MD as DR, 2W and 4W as 3W, 6W as
5W, 8W and 9W as 7W, 1H and 2H as 3H, 6H, 7H, 8H and 9H as 5H, 1I and 2I as
3I), and that club's calibration applies.

Each tracked shot logs the reading:

```
  impact (beta): toe=3.2mm up=-1.5mm
```

Dynamic loft and smash factor are reported normally.

### Per-club calibration

The defaults assume a sticker in its recommended spot. Measuring your own clubs
— from the bottom edge of the sticker's dot straight down to face centre, in
millimetres — gives better results than the defaults, and is the only way to
get a vertical estimate for the putter, which has none built in.

Set overrides in config, keyed the same way the `club` field is:

```toml
[square.0.dot_bottom_to_face_centre_mm]
DR = 15.0
7i = 18.5
```

Or on the Devices tab: flip a Square Golf card to Settings; it has a
collapsible "Face impact calibration (beta)" section with one field per club. Leave a field
blank to use the default for that club; a value overrides it. Changing a
calibration restarts the device actor.

## Spin

The Omni reports total spin and spin axis alongside backspin and sidespin.
flighthook sends all four, as FRP `backspin_rpm`, `sidespin_rpm`,
`total_spin_rpm` and `spin_axis`. The device's sidespin and axis are
positive for a ball curving left; both are negated, so in FRP a positive
sidespin and axis curve the ball right. A total of 0 has no axis.

## Carry and total (beta, simulated)

The Omni measures launch only: ball speed, launch angle, launch direction,
backspin and sidespin. flighthook simulates the rest — carry, apex, flight
time, descent angle, offline, roll and total — from that launch (see the
README's [Simulated values](../../README.md#simulated-values-beta) section).
They show in *italics* in the Shots tab, appear as `derived` in the API, and
are logged under the ball line:

```
  ball: 119.9mph VLA=17.0 HLA=-1.2 back=6500rpm side=-300rpm
  sim (estimated, beta): carry=170.0yd offline=-7.1yd apex=96.1ft time=5.9s descent=50.1deg roll=+5.4yd roll_offline=-0.5yd total=175.4yd total_offline=-7.7yd
```

GSPro still receives the launch data only and simulates the flight itself.

The Omni reports no apex, so a shot launched at 0.5° or less (a putt, or any
bladed shot) is a ground ball: carry, apex and flight time are 0 and the roll
is simulated from the tee, from the ball speed and direction alone, on a
green in Putting mode and on the configured surface otherwise. A shot
launched higher is flown and needs a spin reading like every other flight.

## Zero-spin rejection

A ball struck near the front edge of the Omni's detection zone can come back
with zero spin. A struck ball always spins, so that is a failed read, and a
spinless shot flies far too long in the sim. With
`discard_non_putting_zero_spin` enabled (the default) such a shot is discarded
with a warning — re-hit it.

**Putts are never discarded.** A putt has no airborne flight for the device to
measure spin over, so it reads zero every time; discarding those would make
putting impossible. Every other club is a struck shot that should show spin,
whatever the distance.

```toml
discard_non_putting_zero_spin = true    # default; written when a device is added
```

Set it to `false` to forward every shot, spin or not.

## Putting mode

The Omni has no separate putting mode. Selecting a putter in the sim puts the
device into putting mode via the normal club-forwarding path.

## Shot export

`GET /api/shots/export` (the **JSONL** link on the Shots tab) saves
each shot with the BLE traffic behind it: the notifications the Omni sent for
the shot (ball, club, and anything else in between, including the duplicate
copies the device sends), and the commands flighthook wrote, such as the club
metrics request. Each frame is hex with its first byte as `type`.

Never captured: the identity characteristics (serial, firmware, hardware and
model are read once at connect, not through the capture), anything before the
connection is set up, heartbeats and their acks, and the reply to the status
query. Zero-spin shots that are discarded are not exported. The device's BLE
name and address are not in the file.

## Configuration

```toml
[square.0]
name = "Square Golf Omni"
# address is optional — omit it to auto-discover. No pairing required.
# Pin by the advertised name flighthook logs on connect: it is the same on
# every OS. A MAC address also works, or on macOS (which hides MACs) the
# peripheral UUID.
address = "SquareGolf(CD01)"
club = "7i"                          # club selected on connect
advanced_spin = true                 # device's advanced spin measurement
discard_non_putting_zero_spin = true # drop 0-spin misreads (putts exempt)
```
