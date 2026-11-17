# Mock launch monitor

A virtual launch monitor for trying flighthook without a device: it arms,
reports a ball ready and makes a shot at a fixed interval, with values for
the detection mode in play. Its shots are virtual, so a tournament round
holds them like the Trackball's.

Driver: the mock driver of the [flightrelay](https://github.com/flightrelay/sdk-rust)
SDK ([docs.rs](https://docs.rs/flightrelay)).

The Launch monitors (+) menu does not offer it. Add a `[mock_monitor.N]`
section to the config file, or run flighthook with a config file that holds
one (see [CLI](../../README.md#cli)).

## Settings

| Field | What it does |
| --- | --- |
| **Name** | The card's name in the UI and the logs. |
| **Camera timing** | Seconds from the shot's impact to its report, 0 to 5, which places the cameras' clips (beta). Blank uses the default, 0 s. See [Setting the camera timing](cameras.md#setting-the-camera-timing). |

The pace of its shots is set in the config file only, each left out while
at its default:

```toml
[mock_monitor.0]
name = "Mock"
shot_interval_s = 30      # from the ball being ready to the shot
arm_delay_s = 2           # from connecting or a shot's end to armed
ball_ready_delay_s = 1    # from armed to the ball being ready
post_shot_delay_s = 1     # from a shot to re-arming
serial = "0001"           # the serial it reports
```

Each delay is 0 to 86400 seconds. A value out of range keeps the card from
starting, with the error on the card. A change restarts the card.
