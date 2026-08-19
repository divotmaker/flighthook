# Garmin Approach R10

**Status: Alpha** — decoded by [tenover](https://crates.io/crates/tenover) over
BLE, using Garmin's GFDI framing and protobuf payloads.

## Pairing

The R10 is auto-detected from the system's connected Bluetooth devices, so the
config section carries no address field. Pair the R10 with the host operating
system first, then start flighthook.

## Configuration

```toml
[r10.0]
name = "Garmin R10"
range = "7ft"                  # distance from the device to the ball
```

### `range`

Distance from the R10 to the ball. Garmin recommends placing the device 6-8 ft
behind the ball. flighthook sends this to the device as its tee distance once
the device wakes up, which is the same setting the Garmin Golf app exposes.

Accepts any distance unit (`"7ft"`, `"2.3yd"`, `"2.1m"`) and is converted to the
yards the R10 protocol expects.

Omit it to leave the device on whatever tee distance was last set on it. The
value is pushed once per connection, so changing it restarts the device actor.
