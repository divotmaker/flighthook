# Open Connect client

The **Open Connect Client** card sends every shot to a simulator that accepts
[GSPro Open Connect V1](https://gsprogolf.com/GSProConnectV1.html): GSPro and
[Pureplay](pureplay.md) on port 921, Muni Golf on port 9210. It reads the
simulator's player information (club and hand) back and makes it flighthook's.
Launch monitor software that sends Open Connect to flighthook is the other
direction, see [Open Connect Server](../devices/open-connect.md).

Driver: built into flighthook.

## Setup

1. Start the simulator.
2. In flighthook's Settings, open the **+** after Software and pick
   **GSPro (Open Connect)**, **Pureplay (Open Connect)** or **Muni Golf (Open
   Connect)**. The first two add a card at `127.0.0.1:921`, Muni Golf one at
   `127.0.0.1:9210`; for another simulator, change the card's address. Only
   one card per address can be on.
3. The card turns green (**CONNECTED**) once the simulator accepts the
   connection.

```toml
[openconnect_client.0]
name = "GSPro"
address = "127.0.0.1:921"        # default
full_monitor = "mevo.0"          # optional routing, see below
chipping_clubs = ["GW", "SW", "LW"]   # default
putting_clubs = ["PT"]                # default
```

`address` is the simulator's `ip:port`. A card's `full_monitor`,
`chipping_monitor` and `putting_monitor` are saved as the launch monitor
card's actor id (`actor_id` in its section); the file may name a card by its key
(`mevo.0`) and flighthook rewrites it as the actor id at the next start.

## Routing

Each shot type (Full, Chipping, Putting) has a **Monitor** pick on the card:
**Any** takes the shots of every launch monitor, or one launch monitor. This is
how a Mevo+ can serve full swings and an ExPutt the putts. The pick is saved by
the launch monitor card's actor id. Removing a launch monitor card and adding it
again gives it a new id, so a routing that named the old one reads **Missing
monitor** until you pick the monitor again. A shot from a launch monitor that
is not routed for the shot's type is not sent. The card also takes remote
launch monitors (beta, programs that connect to flighthook's `/frp` socket) in its list.

The Pureplay Accessory card can't be picked: its shots never go to a
simulator.

## Modes and clubs

The simulator's club and hand are flighthook's, and flighthook's mode follows
the club through two lists on the card:

| Setting | Meaning | Default |
|---|---|---|
| **Chipping Clubs** | clubs of the simulator that set Chipping | GW, SW, LW |
| **Putting Clubs** | clubs of the simulator that set Putting | PT |

Any other club is Full. With both lists empty the card sends no mode: the
mode stays whatever the Shot Type pick in the header says. The lists belong to
each card, since a simulator knows its own clubs.

The card holds the simulator's club, hand and mode as one complete pick. The
header's pickers stay editable, but while the card is connected a pick that
differs from the simulator's is set back to it at once, and so is a change by
other software. A change by another simulator or game card (the Pureplay
Accessory) is the exception: the last of them to change it
wins.

## Auto-chip

[Auto-chip](../../README.md#auto-chip-beta) sends its chip to every connected
Open Connect client, whatever the routing says, and is offered while a client
is connected.

## Settings

| Field | What it does |
| --- | --- |
| **Name** | The card's name in the UI and the logs. |
| **Address** | The simulator's address and port, `ip:port`, where shots are sent. GSPro and Pureplay listen on `127.0.0.1:921` when they run on this computer, Muni Golf on `127.0.0.1:9210`. Change it for a simulator on another computer or port. |
| **Full Monitor** | Which launch monitor's full swings the card sends. Default **Any**: every launch monitor's. See [Routing](#routing). |
| **Chipping Monitor** | Which launch monitor's chips the card sends. Default **Any**. |
| **Putting Monitor** | Which launch monitor's putts the card sends. Default **Any**. Pick a putting monitor here to send its putts and another monitor's full swings. |
| **Chipping Clubs** | The simulator's clubs that set Chipping when the simulator selects them. Default GW, SW and LW. Click a club to add it or take it out. See [Modes and clubs](#modes-and-clubs). |
| **Putting Clubs** | The simulator's clubs that set Putting. Default PT. With both lists empty the card sets no mode. |

## The card

Connected, the card shows the simulator's address (`device_info`), the
simulator's `club` and `handed`, and for this connection `last_shot` (the
shot number the simulator got last and when), `shots_sent`, `monitor_ready`
and `response` (the simulator's last code and message). A shot that finishes
while the simulator is not connected is dropped. While the card cannot
connect, `error` says why. Connected, `error` also says so when a shot gets no
`200` reply within a second ("the simulator isn't reading shots; restart its
Open Connect window"), until a reply comes.

## GSPro takes one connection per launch

GSPro's Open Connect window accepts one connection each time it starts. Once
flighthook disconnects from it, for any reason (the card's **Reconnect**,
turning the card off and on, changing its address, or closing flighthook),
GSPro stops taking shots until you restart its Open Connect window. The card
may still connect and show **CONNECTED**; the first shot that goes unanswered
sets `error` as described above. After you restart GSPro's Open Connect window,
the card connects to the new window. Pureplay isn't affected.
