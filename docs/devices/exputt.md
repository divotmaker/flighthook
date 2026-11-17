# ExPutt RG

**Status: Alpha** — read by [threeputt](https://crates.io/crates/threeputt)
over the device's WiFi connection.

Driver: [threeputt](https://github.com/divotmaker/3putt)
([docs.rs](https://docs.rs/threeputt)).

The ExPutt RG is a putting launch monitor. flighthook reaches it over the
local network: the device must be in its mobile mode, on the same network as
the flighthook computer. Its shots are putts: club `PT` and Putting mode
whatever club is in play, vertical launch 0, no spin. It follows the hand of
the golfer up.

## Setup

1. Put the ExPutt in its mobile mode and join it to your network.
2. In flighthook's Settings > Devices, open the **+** after Launch monitors
   and pick **ExPutt**.
3. The card turns green (**CONNECTED**) once flighthook has found the device.

To route the putts of an ExPutt and the full swings of another launch
monitor to one simulator, set the output card's **Putting Monitor** to the
ExPutt (see [Routing](../outputs/open-connect.md#routing)).

## Find mat

The **Find mat** icon (a magnifier) in the card's header has the ExPutt
locate its mat again, for example after it locked onto the wrong spot
because someone stood in its view when it connected. It is enabled while the
card is connected. The card then reports `ready` false and `device_state`
`finding mat`, `mat found` and `ready` as the device works.

## Settings

| Field | What it does |
| --- | --- |
| **Name** | The card's name in the UI and the logs. |
| **IP Address** | The ExPutt's IP address on your network, without a port. Blank (the default) finds the device on the network. Set it when flighthook can't find the device, or to pick one of two. Changing it restarts the card. |
| **Camera timing** | Seconds from the putt's strike to the time the ExPutt dates it, 0 to 5, which places the cameras' clips (beta). Blank uses the default, 0 s: the ExPutt dates each putt at its strike. See [Setting the camera timing](cameras.md#setting-the-camera-timing). |

```toml
[exputt.0]
name = "ExPutt"
# address is optional; omit it to find the device on the network.
address = "192.168.1.50"
```

A host name or an address with a port is refused: the card shows why and
won't save it, and a config file holding one keeps the card from starting,
with the error on the card.
