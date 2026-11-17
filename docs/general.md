# General

Settings, General holds the cards that apply to flighthook as a whole:
**Global**, **Web Server** and **Simulation**. Global and Simulation are
saved together: **Save** or **Cancel** on either one saves or cancels both.

## Global

### Settings

| Field | What it does |
| --- | --- |
| **Units** | The unit system of every Shots view and of the exports: **Imperial** (yards, feet, mph) or **Metric** (meters, m/s). Default Imperial. The Shots tab follows a change at once; the Simulation card's temperature and a golfer's height switch to °F and inches (or °C and centimeters) with it. |
| **Default shot view** | Which view the Shots tab opens in: **By set**, **By club** or **None** (one list). Default By set. Sets are always recorded; this only changes how shots are grouped on screen. |
| **Auto-Chip Helper** | **Show** puts the Auto-Chip control in the header, which sends a short straight chip to the simulator for chips your launch monitor can't read. Default Hide. The chip is simulated under the Simulation card's altitude, temperature and surface, whatever its Simulation mode. See [Auto-chip](../README.md#auto-chip-beta). |

The card's **API Docs** button opens the reference of flighthook's REST API
and sockets inside the app; the same reference is [API.md](API.md).

```toml
default_units = "imperial"   # imperial | metric
shot_grouping = "by_set"     # by_set | by_club | none

[simulation]
auto_chip = false            # the Auto-Chip Helper
```

## Web Server

flighthook runs one web server, which serves this dashboard, the REST API and
the FRP socket. It can be edited but not removed.

### Settings

| Field | What it does |
| --- | --- |
| **Name** | The card's name in the UI and the logs. |
| **Bind Address** | The address and port the web server listens on, `ip:port`. Default `0.0.0.0:5880`, which accepts connections from other computers too (a tablet or phone as a second screen). `127.0.0.1:5880` accepts them only from this computer. Change the port when another program already uses 5880. |

The dashboard has no login: anyone who can reach the address can view shots
and change settings. See
[Second screen on a tablet or phone](../README.md#second-screen-on-a-tablet-or-phone).

```toml
[webserver.0]
name = "Web Server"
bind = "0.0.0.0:5880"
```

## Simulation

When a launch monitor doesn't report carry, apex, flight time, descent,
offline, roll or total, flighthook simulates them from the measured launch.
Simulated values are tagged SIM on the Cards view and shown in italics in the
Table. They are never sent to GSPro, which simulates the flight itself. See
[Simulated values](../README.md#simulated-values).

Physics: [dimple](https://github.com/divotmaker/dimple)
([docs.rs](https://docs.rs/dimple)).

### Settings

| Field | What it does |
| --- | --- |
| **Simulation** | **Off** shows only what the launch monitor reports. **On** simulates the values the launch monitor doesn't report. **Force** replaces the carry, apex, offline, roll and total the launch monitor reports with flighthook's simulation of its launch numbers; launch numbers are never changed. Default On. |
| **Altitude** | Altitude above sea level, 0 to 5000 m (16,404 ft), in the unit picked beside it. Default 0. Thinner air carries the ball further. |
| **Temperature** | Air temperature, -30 to 50 °C (-22 to 122 °F). Default 15 °C (59 °F). |
| **Surface** | The flat ground the ball lands and rolls on: **Fairway**, **Green** or **Firm green**. Default Fairway. |

Altitude, Temperature and Surface are grayed out while Simulation is Off;
the Auto-Chip Helper's chip still uses them.

```toml
[simulation]
mode = "on"            # off | on | force
altitude = "0ft"
temperature_c = 15.0
surface = "fairway"    # fairway | green | firm_green
```
