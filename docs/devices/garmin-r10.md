# Garmin Approach R10

**Status: Alpha** — decoded by [tenover](https://crates.io/crates/tenover) over
BLE, using Garmin's GFDI framing and protobuf payloads.

## Pairing

The R10 is auto-detected from the system's connected Bluetooth devices, so the
config section carries only a name — there is no address field and no radar
settings to configure. Pair the R10 with the host operating system first, then
start flighthook.

## Configuration

```toml
[r10.0]
name = "Garmin R10"
```
