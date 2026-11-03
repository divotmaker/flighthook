# Cameras

**Status: Beta** — swing replay: USB cameras record a clip of every shot,
and the Shots tab plays it back beside the shot's numbers. No live video and
no body tracking yet.

flighthook keeps the last few seconds from each camera in memory. When a
launch monitor reports a shot, it cuts a clip from a few seconds before the
estimated impact to a moment after the report, saves it, and marks the shot
with a replay button. Capture uses the
[stance](https://github.com/divotmaker/stance) crate.

## What you need

- A USB (UVC) camera with an MJPEG mode: most webcams (a Logitech C920 at
  1080p30), and machine-vision boards (720p at 120 fps) for slow motion.
  Frames are stored as the camera compressed them, so recording costs
  almost no CPU.
- Windows or Linux. **macOS is not supported yet**: the camera type is not
  offered there, and a camera card configured by hand shows
  "cameras are not supported on macOS yet".
- Disk space: see [Disk and memory](#disk-and-memory).

## Setup

1. Devices tab → **Cameras (+)** → **Camera**.
2. On the new card's Settings side, pick the **Camera** from the list
   (**Test pattern** is a generated picture for trying it out), its
   **Position**, and the **Video mode**: **Auto** takes the highest frame
   rate, then the largest size; or pick one of the camera's modes.
3. **Save**. The card turns green once frames arrive; its telemetry shows
   the resolution, the frame rate achieved and dropped frames.
4. **Still frame** on the Telemetry side shows the camera's current picture,
   for aiming it.

Add one card per camera. Clip settings are on the **Replay** card under
General.

```toml
[camera.0]
name = "Golfer Front"
camera = "/dev/v4l/by-id/usb-046d_HD_Pro_Webcam_C920_8A2B3C4D-video-index0"
camera_name = "HD Pro Webcam C920"
position = "golfer_front"  # target_line | golfer_front | golfer_back | overhead
mode = "auto"              # or e.g. "1280x720@120"

# optional; shown at its defaults
[replay]
keep_shots = 20          # clips kept on disk, newest first
pre_roll_s = 4.0         # seconds before the estimated impact, 0 to 10
post_roll_s = 1.5        # seconds after the shot is reported, 0 to 10
start_before_impact_s = 2.0  # where a replay starts, before the estimated impact, 0 to 10
# clip_dir = "D:/swings" # default: flighthook/clips in your local data folder
```

`camera` is the camera's stable id: on Windows its device path, on Linux its
`/dev/v4l/by-id` link (or its USB port and name, for identical cameras
without serial numbers). It stays the same when the camera is unplugged and
plugged into the same USB port, and across reboots. If the id is not
connected, flighthook looks for a camera named `camera_name` and logs a
warning when it uses one.

## Positions

| Position | Where the camera stands |
| --- | --- |
| Target Line | behind the golfer's hands, looking down the target line |
| Golfer Front | facing the golfer's chest |
| Golfer Back | looking at the golfer's back |
| Overhead | above the golfer, looking down |

The position labels the camera in the replay and in each clip's `shot.json`.

## Replay

Shots with clips get a **Replay** button: on the shot's header in the Cards
view, and in a column of the Table view. Replay opens over the Shots tab with the
cameras side by side, synced on time, and a scrubber with the estimated
impact marked in gold. It opens already playing, looping, once the frames at its
start point have loaded. Pause, slow motion (1× · ½ · ¼ · ⅛), step frame by
frame, or drag the scrubber. A replay opens 2 s before the estimated impact
(`start_before_impact_s`, 0 to 10 s on the Replay card): a backswing and
downswing take 1.2 to 1.5 s, with a buffer. A green line on the scrubber
marks that point. **Loop** is on by default: playback that reaches the end of
the clip goes on from the start point. With Loop off it stops at the end, and
play restarts from the start point; drag the scrubber to reach the start of
the clip. Each camera has a download link for its clip.

In the Cards view, each connected camera can also have a card beside the
range bars, when the window has room (click a card to cycle what it shows).
It loops the shown shot's clip from that camera at 1×, cropped to fill the
card, from the same start point to the end of the clip.

The browser fetches only the frames around the playhead, so a phone can
replay a long 120 fps clip. At 1× a phone may not decode every frame of a
120 fps clip in time: it skips frames and keeps the clip's pace. ½ and
slower show every frame.

## When a clip starts and ends

A clip runs from `pre_roll_s` before the **estimated impact** to
`post_roll_s` after the launch monitor reported the shot.

Only a FlightScope (Mevo+, Gen2, Xi) reports the shot at impact. The others
report once the shot is measured, so flighthook estimates the impact a fixed
time before their report. These are **beta estimates**, to be measured with
a camera:

| Launch monitor | Impact before the report |
| --- | --- |
| FlightScope Mevo+ / Gen2 / Xi | 0 s |
| Square Golf Omni | 1 s |
| ExPutt | 1.5 s |
| Garmin R10 | 2 s |
| Open Connect (Uneekor, ...) | 2.5 s |

With the default 4 s pre-roll the backswing is in the clip even when an
estimate is off by a second.

- Two launch monitors seeing one swing (reports from different monitors whose
  impact estimates are within 0.5 s) make **one clip**, shown on both shots.
- The same monitor reporting again ends the clip at that moment and starts a
  new one, so quick putts each get their own clip.
- **Auto-chip** shots are never recorded.
- If a camera stops during a clip, the clip is kept when it reached the
  impact (marked incomplete), and dropped when it didn't. The card shows
  RECONNECTING and the camera is reopened with the usual backoff.
- A camera delivering under half its frame rate for 10 s raises one warning:
  usually two cameras sharing a USB controller, or a long exposure in low
  light.

Cameras are synced on when each frame reached the computer, which includes
each camera's driver delay: expect up to a few tens of milliseconds between
two cameras. Each camera notes a launch monitor's report within 10 ms of
its arrival, so the impact mark can sit a frame apart between cameras.

## Disk and memory

Clips are stored as recorded (MJPEG): about 10 MB per second at 1280×720 and
120 fps, so a 5.5 s clip is about 55 MB per camera; a 1080p30 webcam is
about 20 MB. **Two cameras × 55 MB × 20 clips ≈ 2 GB** at the defaults.
`keep_shots` (1 to 1000) counts clips, not shots: a swing two monitors saw
is one clip, and a trigger that read nothing records one too. Older clips
are deleted when a new one is saved, and when flighthook starts; a clip
another camera is still writing waits for the next one. A value out of
range in the config file is held to 1 to 1000, with a warning in the log.

Each camera keeps the pre-roll plus 3 s in memory: about 70 MB at 720p120
with the default 4 s pre-roll.

Each clip is a folder in the clip folder (shown on the Replay card): one
`<camera>.avi` (plays in VLC, Kinovea, Windows Media Player) and
`<camera>.json` (frame times) per camera, and `shot.json` (the shots, launch
monitor kinds and cameras). Only folders flighthook named are ever deleted.
**Clear history** on the Shots tab deletes every clip.

Changing the clip folder (`clip_dir`) takes effect with the next clip. The
clips in the previous folder are left as they are, and flighthook does not
list or delete them: move or delete them yourself. Clips already in the
new folder are listed and count toward `keep_shots`.

Clips are never in the shot export.

## Privacy and your network

Swing video shows you. The web server has no password: anyone on your
network who can open the dashboard can watch your clips and the cameras'
stills. On a shared network, set the Web Server card's Bind Address to
`127.0.0.1:5880`, which serves this computer only.
