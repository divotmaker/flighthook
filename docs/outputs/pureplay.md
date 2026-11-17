# Pureplay

flighthook sends shots to [Pureplay](https://github.com/PureplayGolf/Pureplay-Connect)
over GSPro Open Connect. Pureplay accepts Open Connect on TCP port 921, the
same port as GSPro.

## Setup

1. Start Pureplay.
2. In flighthook's Settings > Software, open the **+** and pick
   **Pureplay (Open Connect)**. It adds the same card as GSPro (Open
   Connect): an Open Connect card at `127.0.0.1:921`, which sends to
   whichever simulator listens on that port. If that card exists, the entry
   adds a second one that starts off (only one card per address can be on);
   the card you have already reaches Pureplay.
3. The card turns green (**CONNECTED**) once Pureplay accepts the
   connection.

Shots, club and hand work as with GSPro; see the
[Open Connect client](open-connect.md), including routing and the club lists
that set the mode.

To follow the round in Pureplay as well (Pureplay's club and hand in
flighthook, and Pureplay's result for each shot kept on it), add the
[Pureplay Accessory card](../devices/pureplay.md) from the same Software
(+) menu: **Pureplay Accessory**. Pureplay reports each shot this card sends
back to the Pureplay Accessory card, which does not take it again. The
Pureplay Accessory card never feeds an output: its own shots and readiness
never go to Pureplay, GSPro or any other simulator, and an output's launch
monitor pickers do not list it.

## Pureplay Connect card (alpha)

The **Pureplay Connect** card sends shots to Pureplay over Pureplay Connect,
Pureplay's own protocol: Pureplay listens on TCP port 9760 and flighthook
connects to it. It takes Pureplay's club and hand, and its `strict_mode`
row reads `on` while a tournament round is in progress. The Software (+)
menu does not offer it; add a `[pureplay.N]` section to the config file.

Driver: built into flighthook.

### Settings

| Field | What it does |
| --- | --- |
| **Name** | The card's name in the UI and the logs. |
| **Address** | Pureplay's address and port, `ip:port`, where shots are sent over Pureplay Connect. Default `127.0.0.1:9760`, Pureplay on this computer. Change it when Pureplay runs on another computer. |
| **Full Monitor** | Which launch monitor's full swings the card sends. Default **Any**: every launch monitor's. Routing works as on the [Open Connect client](open-connect.md#routing). |
| **Chipping Monitor** | Which launch monitor's chips the card sends. Default **Any**. |
| **Putting Monitor** | Which launch monitor's putts the card sends. Default **Any**. |

```toml
[pureplay.0]
name = "Pureplay Connect"
address = "127.0.0.1:9760"   # default
full_monitor = "mevo.0"      # optional routing, as on the Open Connect client
```
