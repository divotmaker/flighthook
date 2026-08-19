# FlightScope Mevo+ / Mevo Gen2

**Status: Supported** — decoded by
[ironsight](https://crates.io/crates/ironsight) over the device's binary TCP
protocol on port 5100.

Connect over the launch monitor's WiFi network. The device reports its SSID
during the handshake, and flighthook uses that as the FRP device identity.

## Estimated shots

Shot data arrives across several messages. A full result (`D4`) is the
authoritative one; when it never arrives, the device may still have sent an
estimated flight result (`E8`).

`use_estimated` controls what happens then:

- `true` (default) — emit the estimated flight as `BallFlight`.
- `false` — skip estimated-only shots entirely.

Estimated shots may be missing club data and sidespin.

## Configuration

```toml
[mevo.0]
name = "My Mevo+"
address = "192.168.2.1:5100"
ball_type = 0                  # 0 = RCT, 1 = Standard
tee_height = "1.5in"
range = "8ft"
surface_height = "0in"
track_pct = 80.0
```

Distances accept a unit suffix — `in`, `ft`, `m`, `cm`, `yd`, `mm` — and are
converted to the wire format on demand. `track_pct` is 0–100.

Changing the address or any session setting restarts the actor.
