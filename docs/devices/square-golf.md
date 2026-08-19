# Square Golf Omni

**Status: Beta** — decoded by
[allsquare](https://crates.io/crates/allsquare) over BLE (GATT). No pairing
required.

The original **Square / Square Home is not supported** — it uses a different
club-code scheme.

## Face impact

The Omni is the first supported device to report **face impact location** on the
wire, so it is currently the only one that forwards `VerticalFaceImpact` /
`HorizontalFaceImpact` to GSPro. It also reports dynamic loft and smash factor.

Polarity is verified device-side only: negative is toe / low, confirmed against
the launch monitor's own impact display. GSPro renders these as bare numeric
stats with no face diagram, so its interpretation of the sign could not be
confirmed.

## Zero-spin rejection

A ball struck near the front edge of the Omni's detection zone can come back
with zero spin. A spinless shot flies far too long in the sim, so flighthook
discards any zero-spin reading at or above `reject_zero_spin_above_mph`
(default 60 mph) and warns instead — re-hit the shot.

Slower shots are still forwarded, since putts and soft chips can legitimately
read zero. Set the value to `0` to forward every shot.

## Putting mode

The Omni has no separate putting mode. Selecting a putter in the sim puts the
device into putting mode via the normal club-forwarding path.

## Configuration

```toml
[square.0]
name = "Square Golf Omni"
# address is optional — omit it to auto-discover by name. No pairing required.
address = "DC:0D:30:62:54:E4"
club = "7i"                          # club selected on connect
advanced_spin = true                 # device's advanced spin measurement
reject_zero_spin_above_mph = 60.0    # discard 0-spin reads above this ball speed
```
