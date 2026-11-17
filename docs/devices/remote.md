# Programs on other computers (beta)

Any program that speaks the [Flight Relay Protocol](https://github.com/flightrelay/spec)
(FRP) can connect to flighthook at `ws://<host>:5880/frp`, from the same
computer or another one: a launch monitor, a game, a logger. flighthook shows
each one that announces itself as a read-only card under Settings, in the
section that fits what it is (a launch monitor under Devices, a game under
Software). The program connects to flighthook; flighthook does not dial out
to it.

## The card

The card shows the program's name, its connection and the telemetry it
sends. A launch monitor's battery is in the header's energy menu, its shots
are in the Shots tab and the exports, and an output's routing can pick it
like a card flighthook runs itself. A remote launch monitor that says its
shots are virtual is held during a tournament round like the Trackball and
the Mock. The card goes after about six seconds without the program's
heartbeat.

## Settings

The card has no settings and no actions: the program configures itself.
Change its settings in the program. The protocol is in [API.md](../API.md).
