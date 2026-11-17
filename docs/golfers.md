# Golfers (beta)

Settings, Golfers lists the golfers flighthook knows, one page per golfer.
The **+** after Golfers adds one; the trash icon on a golfer's page removes
it. The golfer up owns the header's Club and Shot Type and the hand, their
shots carry their name, and every golfer has their own sets, last club and
grades. Pick the golfer up from the header's Golfer card. See
[Golfers](../README.md#golfers-beta) for how golfers, games and the header
work together.

## Settings

| Field | What it does |
| --- | --- |
| **Name** | The golfer's name, shown in the header, on their shots and in the exports. A golfer without one shows as "Golfer 1", "Golfer 2" and so on. |
| **Gender** | **Male** or **Female**: whether the swing analysis compares this golfer with the men's or the women's tour averages. Default Male. |
| **Hand** | **Right-handed** or **Left-handed**. Default right-handed. A connected simulator (GSPro, Pureplay) that names the hand wins over this; the Square Golf Omni, the ExPutt and the trackball follow the hand in effect. |
| **Height** | The golfer's height, 100 to 250 cm (40 to 98 in), in the unit system of the Global card. Optional. With a height, swing measurements that are distances are shown in inches or centimeters. |
| **Linked Profiles** | Players from a game (for example a Pureplay player through the Pureplay Accessory card) that are this golfer. Pick one under **Add a game profile** to link it, **Remove** to unlink it. A linked player plays and records as this golfer. The list offers the players a game publishes now or that have shots this run; it is empty until a game has players. |
| **Bag** | The clubs the header's club picker offers for this golfer: click a club to add it or take it out. With no clubs picked, the picker offers every club. |

Golfers are saved together: **Save** or **Cancel** on a golfer's page saves
or cancels the edits to every golfer.

```toml
[[golfers]]
identifiers = ["flighthook.5b0e3c1e-7d2a-4f4b-9a63-0c1f2e8d7a10"]
name = "Alex"
handed = "rh"                  # rh | lh
bag = ["DR", "3W", "5I", "7I", "9I", "PW", "SW", "PT"]   # optional
sex = "female"                 # male (default) | female
height = 168.0                 # cm, 100 to 250; optional
```

`identifiers` holds the golfer's own id, which flighthook writes when the
golfer is created, followed by its linked game profiles.
