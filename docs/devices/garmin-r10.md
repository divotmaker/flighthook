# Garmin Approach R10

**Status: Alpha** — decoded by [tenover](https://crates.io/crates/tenover) over
BLE, using Garmin's GFDI framing and protobuf payloads.

## Pairing

The R10 is auto-detected from the system's connected Bluetooth devices, so the
config section carries no address field. Pair the R10 with the host operating
system first, then start flighthook.

## Spin

The R10 measures total spin and spin axis. flighthook sends both, as FRP
`total_spin_rpm` and `spin_axis` (+ = tilted right, curves the ball right),
along with the backspin and sidespin components computed from them. A total
of 0 has no axis.

## Carry and total (beta, simulated)

The R10 reports launch data, not carry. flighthook simulates carry, apex,
flight time, descent angle, offline, roll and total from the measured launch
and shows them in *italics*, as `derived` in the API, and in a
`sim (estimated, beta)` log line; see the README's
[Simulated values](../../README.md#simulated-values-beta) section. GSPro still
receives the launch data only.

## Shot export

`GET /api/shots/export` (the **JSONL** link on the Shots tab) saves
each shot with the BLE chunks behind it: up to 256 chunks before the shot
event (the shot message and its replays), as hex with the MultiLink handle
byte, labelled `notify`, `write` or `register`. Chunks are recorded as they
arrived, not reassembled.

Never captured: anything before the handshake completes, which is where the
device's unit ID travels; after it, any chunk carrying part of a device
information, FIT, file transfer or authentication message (or an ACK of one),
or of a frame that does not decode. The device's BLE address is not in the
file.

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
