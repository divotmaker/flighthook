# flighthook docs

Every card under Settings has a **Docs** link in its header that opens its
guide here. Each guide's **Settings** section lists the card's fields as the
card labels them.

| Settings section | Card | Guide |
| --- | --- | --- |
| General | Global | [General](general.md#global) |
| General | Web Server | [General](general.md#web-server) |
| General | Simulation | [General](general.md#simulation) |
| Devices | FlightScope Mevo+, Mevo Gen2, Xi | [FlightScope](devices/flightscope.md) |
| Devices | Garmin R10 | [Garmin R10](devices/garmin-r10.md) |
| Devices | Square Golf Omni | [Square Golf](devices/square-golf.md) |
| Devices | ExPutt | [ExPutt RG](devices/exputt.md) |
| Devices | Trackball | [Trackball](devices/trackball.md) |
| Devices | Open Connect Server | [Open Connect Server](devices/open-connect.md) |
| Devices | Mock | [Mock launch monitor](devices/mock.md) |
| Devices, Software | A program on another computer | [Programs on other computers](devices/remote.md) |
| Software | Open Connect (GSPro, Pureplay, Muni Golf) | [Open Connect client](outputs/open-connect.md) |
| Software | Pureplay Connect | [Pureplay](outputs/pureplay.md#pureplay-connect-card-alpha) |
| Software | Pureplay Accessory | [Pureplay Accessory](devices/pureplay.md) |
| Software | Random Club | [Random Club](outputs/random-club.md) |
| Cameras | Camera | [Cameras](devices/cameras.md#camera-card) |
| Cameras | Replay | [Cameras](devices/cameras.md#replay-card) |
| Cameras | Body tracking | [Cameras](devices/cameras.md#body-tracking-settings) |
| Golfers | Golfer | [Golfers](golfers.md) |

A card's header also holds its controls: the switch next to the title turns
a device off or on, **Reconnect** (the circular arrow) closes its connection
and starts it again, the trash icon removes it, and **Save** and **Cancel**
appear while it has unsaved edits. See [Settings](../README.md#settings).

For developers:

- [API.md](API.md): the REST API, FRP on `/frp` and the dashboard's socket.
- [ARCHITECTURE.md](ARCHITECTURE.md): the config model, the internal bus and
  its actors, threading and the UI.
- [SCREENSHOTS.md](SCREENSHOTS.md): how the README's screenshots are made.
