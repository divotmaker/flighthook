# Square Golf Omni

**Status: Beta** — decoded by
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

Or from the Settings tab: each Square Golf device has a collapsible "Face
impact calibration (beta)" section with one field per club. Leave a field
blank to use the default for that club; a value overrides it. Changing a
calibration restarts the device actor.

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

## Configuration

```toml
[square.0]
name = "Square Golf Omni"
# address is optional — omit it to auto-discover. No pairing required.
# Pin by the advertised name flighthook logs on connect: it is the same on
# every OS. A MAC address also works, or on macOS (which hides MACs) the
# peripheral UUID.
address = "SquareGolf(54E4)"
club = "7i"                          # club selected on connect
advanced_spin = true                 # device's advanced spin measurement
discard_non_putting_zero_spin = true # drop 0-spin misreads (putts exempt)
```
