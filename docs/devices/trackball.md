# Trackball

**Status: Alpha, Linux only** — a trackball or mouse turned into a launch
monitor by [thumbswing](https://crates.io/crates/thumbswing): pull the ball
back, push it forward, and the swing becomes a full shot (club speed, path,
face, ball speed, launch angle and direction, backspin and sidespin), the way
an arcade-style trackball golf game reads a swing.

It is meant for testing and for practice without a launch monitor: every
value is synthesized from the swing, nothing is measured from a real ball.
The swing model and its defaults are a first fit, so expect to tune the feel
with your hands on the ball. On Windows and macOS the card is not offered;
a `[trackball.N]` section copied there shows "trackball input is not
supported on this platform (Linux only)" on its card.

## Setup

Add it from the Launch monitors (+) menu: **Trackball**. Any device with
relative X/Y axes works: a large arcade trackball, a thumb-ball trackball
mouse, an ordinary mouse.

Input devices are `/dev/input/event*`, readable by group `input`. Join the
group, then log out and back in:

```sh
sudo usermod -aG input $USER
```

or give the logged-in user access with a udev rule:

```sh
echo 'KERNEL=="event*", SUBSYSTEM=="input", TAG+="uaccess"' | sudo tee /etc/udev/rules.d/70-thumbswing.rules
sudo udevadm control --reload && sudo udevadm trigger
```

**Device** names the trackball: its `/dev/input/by-id` name (stable across
replugs), a `/dev/input` path, or the name the device reports. Leave it blank
to use the only trackball or mouse there is; with more than one, the card
alerts once and asks you to name it. Only one card per device can be on: two
cards naming the same device, or two leaving it blank, conflict, and the
second one stays off ("A Trackball card already uses the only trackball or
mouse").

The card reads the trackball beside the desktop: the pointer keeps following
the ball while the card runs. Another program that grabs the trackball for
itself keeps every swing from the card while it holds it.

## Connection

While it opens the device the card shows `phase` `connecting`, then
Connected, "thumbswing (<the name the device reports>)", and `ready` true:
pull back, then push. When the device is unplugged the card stays Connected
with `ready` false, opens it again by itself, and is ready again when it is
back (with no device named, it finds the device again even under a new
node). A device that cannot be opened is an alert saying what to do (name
the device, fix the permissions) and a retry, 1 s after the first failed
attempt, growing by 1 s per attempt up to 15 s. Alerts never show a device
path. Turning the card off, a restart or Reconnect closes the device within
a second.

Telemetry: `ready`, `synthetic` (`true`), `input` (the name the device
reports), `cpi`, `profile` (the club in play) and `handed` (`rh` or `lh`).

## The swing

- **Pull back.** The direction of the backswing sets how the shot curves.
- **Push forward.** The direction of the push sets the start line, its peak
  speed the club speed. A forward roll with no backswing does nothing.
- **The club caps the speed.** Full power is **Max club speed** (100 mph for
  the driver) times the club's speed ratio, so no swing hits a 7-iron faster
  than 80 mph; **Full power speed** (15 in/s) is how fast a push must be for
  100 %.
- **Curve.** Arcade (the default): push toward where the ball should start
  and pull back from where it should finish; the ball curves toward the
  backswing line. Physical: the backswing line is the club path and the ball
  curves away from it. Either way the club path and face sent on are
  consistent with the flight, so face-to-path has the sign of the curve.
- **Putter.** The pull does nothing; the push's speed sets the putt's speed
  (**Max putt speed**, 11 mph at full power) and its direction the line.

The formulas, the club table and the defaults are in thumbswing's
[README](https://crates.io/crates/thumbswing).

## Club and hand

The card starts with its **Starting club** (7-iron) and **Starting hand**
(right). A club or hand the game names (GSPro, the header's club picker, the
hand control) replaces them from then on, and Putting mode selects the
putter whatever the club. The trackball sits where it sits whichever hand
plays, so the strokes are not mirrored for a left-handed player unless
**Mirror for left-handed players** is on.

## Shots

Each swing is a shot: `launch` (speed, launch angle and direction, backspin
and sidespin, with total spin and axis), then `club_path` (club speed, path,
face angle, attack angle, dynamic loft, smash factor). There is no flight
result: flighthook simulates carry, roll and total from the launch, and GSPro
flies it itself. A putt launches at 0.3° with no spin and rolls on the green
in Putting mode. A swing that hits the device's per-report limit is logged as
saturated: lower **Full power speed**, or set **Report limit** to the
device's real limit. In the Shots tab its shots look like any other launch
monitor's.

The Trackball is a **virtual** launch monitor: during a tournament round it
sends no shots (a swing then is dropped and takes no shot number), and a
round that a Trackball feeds is refused. Its shots cut no camera clip.

## Shot export

`GET /api/shots/export` saves each shot with `device` kind `trackball`,
maker `divotmaker`, model `thumbswing trackball` and the driver's version.
There are no raw frames (`raw.unavailable` says the device has no wire
protocol). The device's name (its `/dev/input/by-id` name can carry a serial
number) and the `device` setting are not in the file.

## Settings

Every setting is a field on the card; changes to Device, CPI and Report
limit restart the card, the rest applies to the next swing. A blank number
is the default. A value out of range is refused: the card shows why and
won't save it; a config file holding one loads, and that card does not start
and shows the error until it is fixed.

```toml
[trackball.0]
name = "Trackball"
device = "usb-Example_Trackball-event-mouse"   # blank or absent: the only one
club = "DR"                    # until the game names a club (default 7I)
hand = "rh"                    # until the game names a hand: rh | lh
cpi = 800                      # counts per inch of ball travel (default 400)

[trackball.0.tuning]
full_power_speed = 40          # in/s for 100 % (default 15)
curve_sense = "physical"       # arcade (default) | physical

[trackball.0.thresholds]
coil_timeout_s = 20            # how long the top of the swing waits (default 10)
```

| Key | Card field | Default | What it sets |
|---|---|---|---|
| `device` | Device | unset | the input; unset: the only trackball or mouse |
| `club` | Starting club | `7I` | the club until the game names one (an FRP club code) |
| `hand` | Starting hand | `rh` | the hand until the game names one |
| `cpi` | CPI | 400 | counts per inch of ball travel |
| `report_limit` | Report limit | 127 | largest movement one report carries (127, 2047 or 32767) |
| `tuning.full_power_speed` | Full power speed | 15 in/s | push speed for 100 % power |
| `tuning.max_club_speed` | Max club speed | 100 mph | driver club speed at full power |
| `tuning.max_putt_speed` | Max putt speed | 11 mph | putter speed at full power |
| `tuning.angle_gain` | Face gain | 0.25 | face angle per degree of push direction |
| `tuning.putt_angle_gain` | Putt line gain | 1 | putt line per degree of push direction |
| `tuning.curve_gain` | Curve gain | 0.2 | face-to-path per degree between backswing line and push |
| `tuning.curve_sense` | Curve | `arcade` | `arcade` or `physical` |
| `tuning.backswing_power` | Backswing power | 0 | share of the power the backswing length sets, 0 to 1 |
| `tuning.power_gamma` | Power curve | 1 | exponent of the power curve |
| `tuning.mirror_for_left_hand` | Mirror for left-handed players | false | mirror the strokes while the player is left-handed |
| `tuning.face_max` | Max face angle | 20° | largest face angle |
| `tuning.face_to_path_max` | Max face-to-path | 25° | largest face-to-path |
| `thresholds.*` | Swing detection | | how swings are found in the movement: `speed_window_ms` (24), `gap_ms` (50), `v_start` (0.067), `v_stop` (0.033, below `v_start`), `v_min_swing` (0.133), `dwell_min_ms` (40), `back_min` (0.13), `full_cock` (0.67), `fwd_min` (0.17), `back_max_s` (2), `coil_timeout_s` (10), `fwd_window_ms` (300), `idle_settle_ms` (250), `cooldown_max_s` (2), `pull_cone_deg` (75) |

Speeds in the thresholds are shares of the full power speed and lengths
shares of a full stroke (the ball's travel in 100 ms at full power), so
setting **Full power speed** to how hard you want 100 % to be adapts them all
to the device.
