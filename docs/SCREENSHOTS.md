# README screenshots

The images in `screens/` are generated. `make screens` rebuilds flighthook,
runs one scene per image against a headless flighthook and the web UI, and
writes `screens/<name>.png`. Two runs from the same tree give the same
pictures, and an image whose picture is unchanged is not rewritten, so after a
change `git status` shows exactly the screenshots whose picture changed.

```bash
make screens                     # regenerate every screenshot
make screens only=shots,cameras  # just these scenes (or image names)
make screens-check               # regenerate into target/screenshots/check and
                                 # exit 1, listing the pictures that differ from screens/
```

`node scripts/screens/run.js` takes the same options directly (`--only`,
`--check`), plus `--out DIR` (write elsewhere), `--keep` (keep each scene's
scratch folder: config, data, flighthook log) and `--no-build`.

## Requirements

- What `make build` needs: Rust, Trunk and the `wasm32-unknown-unknown` target.
- Node.js 18 or newer with npm. `make screens` installs the pinned Playwright
  (`scripts/screens/package.json`) with `npm ci` the first time. Its Chromium
  is installed once with `cd scripts/screens && npx playwright install chromium`
  (on Linux, `npx playwright install-deps chromium` adds the system libraries
  it needs).
- ffmpeg 5.1 or newer on the `PATH` (or `FFMPEG=/path/to/ffmpeg`), for the
  Cameras scene's footage.
- A network connection the first time: the swing footage (18 MB) and the body
  tracking models (about 75 MB, through flighthook's own model download). Both
  are cached in `target/screenshots/cache`.
- Linux or macOS. The Shots, Cameras and Devices scenes show cameras, so they
  run only where flighthook supports cameras (64-bit Linux); elsewhere they are
  reported as skipped.
- Ports 5971 to 5975 free on localhost: the Devices and routing scenes show
  their ports, so they always use these. Every other port is picked free.

`SCREENS_NODE` names the command that runs the Playwright script (default
`node`), for a machine whose browser runs somewhere else, e.g. in the
`mcr.microsoft.com/playwright:v<version>-noble` image on the host network with
the repository mounted at the same path.

## How it works

1. **Build.** The web UI is built with Trunk into `target/screenshots/dist`
   and copied into `ui/dist` only where a file changed, then flighthook is
   built in the `screens` cargo profile: release, plus debug assertions, which
   compile in the test seams below. A warm build with no change takes a few
   seconds.
2. **Footage**, for the Cameras scene, in parallel with the build: a CC0
   slow-motion swing video from Wikimedia Commons (page 197476291) is
   downloaded, checked against a pinned SHA-256 and cut by ffmpeg into two
   stance clips (face-on and down-the-line, `lib/footage.js`).
3. **Scenes**, all at once (`scripts/screens/scenes/*.js`). Each one:
   - writes a config into a scratch folder (`target/screenshots/run/<scene>`)
     and starts `flighthook --headless` there with its own home folder;
   - drives it to the scene's state: shots from an Open Connect launch monitor,
     a GSPro stand-in for flighthook's output (both in Node,
     `lib/openconnect.js`) and the REST API, waiting on flighthook's replies
     and state rather than fixed delays;
   - runs `scripts/screens/snap.js` in Playwright Chromium: open the UI, click
     through the scene's steps, wait until the picture stops changing, and
     write the PNG;
   - stops everything it started, on failure and Ctrl-C too. A failed scene
     keeps its scratch folder and says where.

| Scene | Images | What it shows |
| --- | --- | --- |
| `shots.js` | `shots.png` | Shots tab, Cards view: a 12-shot 7-iron session from one Open Connect monitor, GSPro connected, a camera recording with video off on the shot cards |
| `cameras.js` | `cameras.png` | Shots tab, Cameras pane: two cameras playing the swing clips, body tracking Live, shot #2 paused on the impact mark |
| `devices.js` | `devices.png`, `devices-settings.png` | Devices tab with a Mock and an Open Connect monitor, a test-pattern camera and GSPro; then the camera card's Settings side |
| `multi-monitor-routing.js` | `multi-monitor-routing.png` | A GSPro output's card routing full swings, chips and putts to different monitors |

## What keeps the pictures the same

- **Test seams** in flighthook, compiled only in debug-assertions builds and
  read from the environment:
  - `FLIGHTHOOK_SCREENS_GRID_MS`: a time grid. A camera playing a clip puts the
    clip's labelled impact on a multiple of it, and every shot trigger is
    stamped at the next multiple, so a shot's clips hold the same frames and
    impact whenever the shot arrives.
  - `FLIGHTHOOK_SCREENS_CLOCK`: the time of day telemetry shows (GSPro's last
    shot).
- **The scenes**: fixed ports where a card shows them, including the launch
  monitor's client port; a relative clip folder; no HTTP request the Web
  Server card would count beyond a fixed one; a settle across the web server's
  5 s telemetry update; the Mock's first shot (33 s after it starts) comes
  after the Devices screenshots, and the scene checks it did.
- **The page** (`snap.js`): fetches run one at a time in the order the UI
  makes them; a frame runs only once the network is quiet; the page clock
  moves one 60 Hz frame per frame. The UI then draws the same sequence of
  pictures on every run, which also fills egui's font atlas in the same order
  (its layout shows in the last bit of the text's anti-aliasing). UTC, en-US,
  the pointer parked away from anything with a hover state.
- **The PNG** is Chromium's encoder output: no time or text chunks.
- **The comparison** (`lib/png.js`): two images show the same picture when
  at most 16 pixels differ, each by at most 8 in any channel. Text
  anti-aliasing can still move a pixel by a step between runs; anything a
  reader could see is a difference.

The pictures are the same for one machine and toolchain. Another CPU,
ffmpeg or Chromium version can move pixels (video decoding, body tracking
maths, rasterising), so regenerate and commit from one machine rather than
comparing across machines.

## Adding a scene

Add `scripts/screens/scenes/<name>.js` exporting:

- `name`, and `snap()` returning `{ viewport, images: [{ name, steps, crop }] }`
  (steps: `click`, `move`, `settle`, `waitPixel`, `clickPixel`; see the top
  of `snap.js`);
- `ports`: `{ web, ... }`, each `'free'` or a fixed number (for ports on screen);
- `config(ctx)`: the TOML, using `ctx.ports`;
- optionally `needs` (`footage`, `models`, `cameras`), `env` (seams),
  `before(ctx)` (before flighthook starts), `drive(ctx)`, `quietMs` (how long
  the last picture must hold still) and `verify(ctx)`.

## Git hook (optional)

`scripts/screens/install-hook.sh` installs a pre-push hook into your clone
(`--remove` takes it out). When the commits being pushed touch `ui/`, `app/`,
`lib/`, `Cargo.lock`, `scripts/screens/` or `screens/`, it runs
`make screens-check` and stops the push if a screenshot is out of date. A
check takes about 40 s with a warm build, too long for every commit, so it
runs before a push. `git push --no-verify` skips it once.
