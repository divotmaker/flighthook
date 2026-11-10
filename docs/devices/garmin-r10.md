# Garmin Approach R10

**Status: Alpha** — decoded by [tenover](https://crates.io/crates/tenover) over
BLE, using Garmin's GFDI framing and protobuf payloads.

## Pairing

The R10 is auto-detected from the system's connected Bluetooth devices, so the
config section carries no address field. Pair the R10 with the host operating
system first, then start flighthook.

## Connection

While it connects the card shows `phase` `discovering` (finding the R10),
then `handshaking`; the handshake must finish within 10 s. When the R10
sleeps or goes out of range the card reads Reconnecting and flighthook looks
for it again, 1 s after the first failed attempt, growing by 1 s per attempt
up to 15 s, and back to 1 s once a session lasted 10 s. Turning the card off,
a restart or Reconnect stops a search or a connect within a few seconds. On
Windows and macOS stopping the card disconnects the R10; on Linux the
Bluetooth link is left to the system.

## Spin

The R10 measures total spin and spin axis. flighthook sends both, as FRP
`total_spin_rpm` and `spin_axis` (+ = tilted right, curves the ball right),
along with the same spin as backspin and sidespin components. All four are
the R10's reading, so a tournament round keeps them. A total of 0 has no
axis.

## Carry and total (beta, simulated)

The R10 reports launch data, not carry. flighthook simulates carry, apex,
flight time, descent angle, offline, roll and total from the measured launch
and shows them in *italics*, as `derived` in the API, and in a
`sim (estimated, beta)` log line; see the README's
[Simulated values](../../README.md#simulated-values-beta) section. GSPro still
receives the launch data only.

## Shot export

`GET /api/shots/export` (the **JSONL** link on the Shots tab) saves
each shot with the protocol messages behind it, one record per message: up
to 256 messages before the shot event (the shot message among them), as hex,
each labeled by the driver with its direction and message type
(`notify <type>` from the device, `write <type>` to it). The same messages
are written to the log as hex lines at debug level (`audit` target).

Never captured: anything before the handshake completes, which is where the
device's unit ID travels; after it, any message carrying the device's
identity or keys, or one that does not decode. Those reach the log only, as
hex lines at trace level. The device's BLE address is not in the file.

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

It must be greater than 0. The card's Settings side shows why a value is
refused and won't save it; a config file holding one loads, and that card does
not start and shows the error until the value is fixed.

Omit it to leave the device on whatever tee distance was last set on it. The
value is pushed once per connection, so changing it restarts the card.
