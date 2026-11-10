# Pureplay (beta)

flighthook sends shots to [Pureplay](https://github.com/PureplayGolf/Pureplay-Connect)
over **Pureplay Connect**, Pureplay's open launch monitor protocol. Pureplay
listens on TCP port 9760 and flighthook connects to it, so to Pureplay
flighthook is the launch monitor. Any launch monitor flighthook supports can
then play in Pureplay.

This output is **beta**: it follows the published protocol (version 1) and
is tested against a protocol-level mock of Pureplay, not yet against
Pureplay itself.

## Setup

1. Start Pureplay.
2. In flighthook's Devices tab, open the **+** after Outputs and pick
   **Pureplay**. The card starts with Pureplay's default address,
   `127.0.0.1:9760` (Pureplay on the same computer). Change the address if
   Pureplay runs on another computer, pick the launch monitor for each mode
   if you have more than one (see [Routing](#routing)), and **Save**.
3. The card turns green (**CONNECTED**) once Pureplay has answered
   flighthook's handshake. If Pureplay is not running yet the card stays
   yellow and flighthook keeps trying every few seconds.

In the config file:

```toml
[pureplay.0]
name = "Pureplay"
address = "127.0.0.1:9760"
# full_monitor = "mevo.0"       # optional: route full-swing shots from one monitor
# chipping_monitor = "mevo.0"   # optional: route chipping shots
# putting_monitor = "exputt.0"  # optional: route putting shots
```

## What Pureplay gets

- **Device name**: `flighthook (<monitor name>)`, the name of the launch
  monitor the current mode is routed to (or of your only launch monitor), or
  `flighthook` alone when several could deliver the shot. The firmware is
  flighthook's version.
- **Battery**: when that launch monitor reports a battery (Mevo+, Square
  Golf Omni), flighthook tells Pureplay it is battery powered and reports the
  level, 0 to 100 %, in its heartbeats. A launch monitor that has not reported
  a battery by the time flighthook connects to Pureplay counts as externally
  powered for that connection.
- **Ready state**: *Ready* while the launch monitor is connected and ready
  for a shot, *Processing* from the moment it detects a strike until the shot
  is complete, *Not ready* otherwise. Every change is sent at once.
  flighthook never reports *Ball detected*: its readiness already includes
  the ball.
- **Connection state** (in heartbeats): the launch monitor's connection as
  its card shows it: connecting, connected, reconnecting, an error, or
  disconnected.
- **Shots**: every shot routed to this output as ball data, plus club data
  when the device measured any club value. Each shot is numbered from 1 on
  every new connection.

### Shot values

Only values the device measured are marked valid; the rest are sent as 0
with their valid bit clear, and Pureplay fills in what it can (total spin
and spin axis from back and side spin, smash factor from the speeds).
flighthook's own simulated values (carry, apex, roll, ...) are never sent.

| Pureplay field | flighthook value | Unit |
|---|---|---|
| BallSpeed | launch speed | m/s |
| VLA | launch elevation | degrees, + up |
| HLA | launch direction | degrees, + right of the target line |
| TotalSpin | total spin | rpm |
| BackSpin | backspin | rpm |
| SideSpin | sidespin | rpm, + curves right |
| SpinAxis | spin axis | degrees, + tilted right (fade/slice) |
| ClubHeadSpeed | club speed | m/s |
| SmashFactor | smash factor, only when the device measures it | |
| DynamicLoft | dynamic loft | degrees |
| FaceAngle | face angle | degrees, + right of the target line (open for a right-hander) |
| ClubPath | club path | degrees, + right of the target line |
| AttackAngle | attack angle | degrees, + up |
| ImpactPointX | face impact, horizontal | mm from the center, + toward the toe |
| ImpactPointY | face impact, vertical | mm from the center, + above |

Pureplay Connect version 1 does not say which way SideSpin's sign points.
flighthook sends it as above, + curving right, the same way as SpinAxis.
This is still to be confirmed against Pureplay: a launch monitor that
reports back and side spin but no spin axis (Mevo+, Xi) depends on it,
because Pureplay works out the axis from the two.

Left-handed players: direction values are relative to the target line, the
same for both hands, and face impact is relative to the club (toe and heel),
also the same for both hands, so nothing changes with the player's hand.

## What Pureplay controls

- **Club and hand**: when Pureplay selects a club or sets the player's hand,
  flighthook follows, exactly as with a connected simulator: the club and
  hand in flighthook's header follow Pureplay and can't be changed there
  while Pureplay is connected, and the club picks the detection mode as
  usual (Chipping and Putting clubs). The Pureplay card shows the club and
  hand it set. A club number this version of the protocol does not define is
  ignored.
- **Arm / disarm, ball type and distance to the target**: received and
  logged. flighthook does not change the launch monitor's settings for them.
- **Strict mode** (an event or tournament round): see
  [Tournament (strict) mode](#tournament-strict-mode).

## Tournament (strict) mode

When Pureplay starts a round whose score is recorded (an event or a
tournament), it asks flighthook for strict mode. While strict mode is on,
flighthook sends Pureplay **only values a physical launch monitor measured
for that shot**. Anything virtual, relayed from another app, estimated,
computed, filled in or calibrated is either left out (sent as not
measured) or, when a whole launch monitor cannot meet that, strict mode is
refused. Strict mode is beta.

Every launch monitor flighthook supports falls into one of three classes:

| Class | Launch monitors | In strict mode |
|---|---|---|
| Measured | FlightScope Mevo, Mevo+ and Xi; Square Golf Omni; Garmin R10; ExPutt RG | allowed |
| Virtual | the mock launch monitor; Auto-chip | not allowed |
| Relayed | the OpenConnect input (shots another app sends to flighthook) | not allowed |

- **Accepting a round**: flighthook accepts strict mode only when every
  launch monitor this output takes shots from is a measured one: the
  monitor picked for each detection mode, or with "Any" every configured
  launch monitor. Otherwise it refuses and tells Pureplay why, naming the
  device (for example "Mock is a virtual launch monitor"); Pureplay then
  closes the connection, and flighthook shows the reason as a warning. With
  no launch monitor at all, strict mode is accepted: nothing could be sent.
  To play a round, route only measured monitors to the Pureplay card (or
  remove the others).
- **During a round**: shots from a launch monitor that is not a measured
  one (for example one that connects after the round started, under "Any")
  are not sent. Measured monitors keep working.
- **Values left out** (sent as not measured, so Pureplay fills them in or
  goes without):
  - spin flighthook estimated for a FlightScope chip that came back without
    spin;
  - the Garmin R10's back and side spin, which are worked out from the total
    spin and spin axis it measures (total spin and spin axis are sent);
  - the Xi's back and side spin when its **Spin source (Xi)** setting
    combines the E8 total with an axis (set it to D4 for rounds to keep
    spin);
  - the ExPutt's launch angle, which the device does not measure (flighthook
    uses 0);
  - the Square Golf Omni's vertical face impact, which includes the
    per-club offset from the club sticker to the face center (horizontal
    impact is sent).

  Smash factor goes out only when the launch monitor measured it, and
  flighthook's simulated values are never sent, in or out of strict mode.
- **Auto-chip is off** while Pureplay has a round in progress: it does not
  fire, and the header's Auto-chip control is shown disabled. It returns to
  your setting when the round ends or the connection to Pureplay drops.
- The card's `strict_mode` shows whether a round is in progress. Every new
  connection starts with strict mode off; Pureplay turns it on again when a
  round is still running.

## Routing

The Full Monitor, Chipping Monitor and Putting Monitor fields work as on an
OpenConnect Client card: each picks the launch monitor whose shots go to
Pureplay in that detection mode, and "Any" accepts every monitor. The
monitor picked for the current mode is also the one whose ready state,
connection and battery Pureplay sees. With "Any", Pureplay sees the best
state among all launch monitors (ready if any is ready).

## The card

| Key | Value |
|---|---|
| `address` | Pureplay's address |
| `last_shot` | the number of the shot Pureplay got last and when it was sent |
| `shots_sent` | shots sent on this connection |
| `ready_state` | the ready state last sent: `ready`, `not ready` or `processing` |
| `strict_mode` | `on` while Pureplay has a recorded round in progress |
| `club`, `handed` | the club and hand Pureplay set |
| `error` | why the handshake failed (shown in red over the tabs) |

Pureplay not running is normal and is not an error: the card just stays
yellow. `error` appears only when something answered at the address but the
handshake failed, for example another program listening on the port, or a
Pureplay that offers only a protocol version flighthook does not speak.

## Limits

- A launch monitor's state is learned from its own updates. When the
  Pureplay output starts while a launch monitor is already connected (the
  card was just added or saved), Pureplay sees *Not ready* until that monitor
  next reports that it is ready, which also counts as connected.
- Only the TCP transport is supported.
