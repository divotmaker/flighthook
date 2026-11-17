use stance::Trigger as CutTrigger;

use super::*;
use crate::bench::{bench, out};
use crate::testing::unpaced_clip;

/// The config `toml` holds.
fn config_of(toml: &str) -> FlighthookConfig {
    toml::from_str(toml).expect("config")
}

/// A config in Live mode.
const LIVE: &str = "[body_tracking]\nmode = \"live\"\n";

fn ms(config: &FlighthookConfig, actor: &str) -> Option<u128> {
    impact_lead(config, actor).map(|d| d.as_millis())
}

/// `card`'s trigger for shot `key`, with a strike time when `dated`.
fn shot(card: &str, key: &ShotKey, dated: bool) -> Trigger {
    let prefix = card.split_once('.').map_or(card, |(p, _)| p);
    Trigger {
        card: card.to_owned(),
        device: prefix.to_owned(),
        key: key.clone(),
        timestamp: dated.then_some(0),
    }
}

/// `recording` takes `shot` at `at_ns`, as the camera runs it with the
/// start turn and the first generation.
fn trigger(config: &FlighthookConfig, recording: &mut Recording, shot: &Trigger, at_ns: u64) {
    let turn = GolferTurn::new(
        None,
        None,
        flighthook::Handedness::Right,
        flighthook::Mode::Full,
    );
    recording.trigger(config, shot, at_ns, &turn, 0);
}

#[test]
fn impact_leads_default_by_device_kind() {
    let config = config_of("");
    assert_eq!(ms(&config, "mevo.0"), Some(0));
    assert_eq!(ms(&config, "mock_monitor.3"), Some(0));
    assert_eq!(ms(&config, "square.0"), Some(1000));
    assert_eq!(ms(&config, "exputt.0"), Some(1500));
    assert_eq!(ms(&config, "r10.1"), Some(2000));
    assert_eq!(ms(&config, "openconnect_server.0"), Some(2500));
    assert_eq!(ms(&config, "pureplay_accessory.0"), Some(900));
    // A launch monitor kind without a better-known value.
    assert_eq!(ms(&config, "newmonitor.0"), Some(1500));
    assert_eq!(ms(&config, flighthook::AUTO_CHIP_ACTOR), None);
    assert_eq!(ms(&config, "trackball.0"), None, "no swing a camera films");
    for actor in ["mevo.0", "square.0", "r10.0", "openconnect_server.0"] {
        assert!(
            impact_lead(&config, actor).unwrap() <= lead(MAX_IMPACT_LEAD_S),
            "{actor}"
        );
    }
}

#[test]
fn a_configured_impact_lead_overrides_the_default() {
    let config = config_of(
        "[mevo.0]\nname = \"Mevo\"\nimpact_lead_s = 0.25\n\n\
             [square.0]\nname = \"Omni\"\nimpact_lead_s = 0\n\n\
             [square.1]\nname = \"Omni 2\"\n\n\
             [exputt.0]\nname = \"Putt\"\nimpact_lead_s = 5\n\n\
             [pureplay_accessory.0]\nname = \"Pureplay\"\nimpact_lead_s = 1.75\n",
    );
    assert_eq!(ms(&config, "mevo.0"), Some(250));
    assert_eq!(ms(&config, "pureplay_accessory.0"), Some(1750));
    assert_eq!(ms(&config, "pureplay_accessory.1"), Some(900), "no section");
    // 0 is a value, not "unset".
    assert_eq!(ms(&config, "square.0"), Some(0));
    // No value: the device default.
    assert_eq!(ms(&config, "square.1"), Some(1000));
    assert_eq!(ms(&config, "exputt.0"), Some(5000));
    // No section: the default of its kind.
    assert_eq!(ms(&config, "mevo.7"), Some(0));
    assert_eq!(ms(&config, "r10.0"), Some(2000));
    // Auto-chip is not a swing, whatever a section says.
    assert_eq!(ms(&config, flighthook::AUTO_CHIP_ACTOR), None);
}

#[test]
fn an_invalid_impact_lead_in_the_config_falls_back_to_the_default() {
    let mut config = config_of("[square.0]\nname = \"Omni\"\nimpact_lead_s = 9\n");
    assert_eq!(ms(&config, "square.0"), Some(1000));
    config.square.get_mut("0").unwrap().impact_lead_s = Some(f64::NAN);
    assert_eq!(ms(&config, "square.0"), Some(1000));
    config.square.get_mut("0").unwrap().impact_lead_s = Some(-0.1);
    assert_eq!(ms(&config, "square.0"), Some(1000));
}

#[test]
fn the_impact_lead_is_read_live_at_every_trigger() {
    let mut config = config_of("[mevo.0]\nname = \"Mevo\"\n");
    let mut recording = Recording::new(&config);
    let key = |n: u32| ShotKey {
        shot_id: format!("s{n}"),
        shot_number: n,
    };
    assert_eq!(ms(&config, "mevo.0"), Some(0));
    trigger(&config, &mut recording, &shot("mevo.0", &key(1), false), 1);
    config.mevo.get_mut("0").unwrap().impact_lead_s = Some(0.4);
    assert_eq!(ms(&config, "mevo.0"), Some(400));
    trigger(&config, &mut recording, &shot("mevo.0", &key(2), false), 2);
    config.mevo.get_mut("0").unwrap().impact_lead_s = None;
    assert_eq!(ms(&config, "mevo.0"), Some(0));
}

#[test]
fn the_recorder_keeps_the_pre_roll_and_the_longest_lead() {
    let replay = ReplaySection {
        pre_roll_s: 4.0,
        post_roll_s: 1.5,
        ..ReplaySection::default()
    };
    let longest = Duration::from_millis(2500);
    let config = recorder_config(&replay, longest, Some(120.0));
    assert_eq!(config.pre_roll, Duration::from_secs(4));
    assert_eq!(config.post_roll, Duration::from_millis(1500));
    assert_eq!(config.buffer, Duration::from_millis(4000 + 2500 + 500));
    assert!(config.buffer_covers(longest));
    assert_eq!(config.fps, Some(120.0));
    // Out-of-range rolls from a hand-edited file are clamped.
    let wild = ReplaySection {
        pre_roll_s: f64::NAN,
        post_roll_s: 99.0,
        ..ReplaySection::default()
    };
    let config = recorder_config(&wild, longest, None);
    assert_eq!(config.pre_roll, Duration::ZERO);
    assert_eq!(config.post_roll, Duration::from_secs(10));
}

#[test]
fn the_recorder_history_covers_the_largest_lead_in_effect() {
    let mut config = config_of("[mevo.0]\nname = \"Mevo\"\n\n[square.0]\nname = \"Omni\"\n");
    // Defaults only: Square's 1 s is the longest.
    assert_eq!(longest_reach(&config), Duration::from_secs(1));
    config.mevo.get_mut("0").unwrap().impact_lead_s = Some(3.5);
    assert_eq!(longest_reach(&config), Duration::from_millis(3500));
    // A setting above the maximum is not in effect.
    config.mevo.get_mut("0").unwrap().impact_lead_s = Some(8.0);
    assert_eq!(longest_reach(&config), Duration::from_secs(1));
    // With no monitor configured there is no lead to cover.
    assert_eq!(longest_reach(&config_of("")), Duration::ZERO);

    // The recorder grows its history when a setting grows, at the next
    // trigger, and never lets a lead outrun it.
    let replay = config.replay.clone();
    let mut recording = Recording::new(&config);
    let before = recording.recorder.config().buffer;
    config.square.get_mut("0").unwrap().impact_lead_s = Some(5.0);
    let key = ShotKey {
        shot_id: "a".into(),
        shot_number: 1,
    };
    trigger(&config, &mut recording, &shot("square.0", &key, true), 1);
    let recorder = &recording.recorder;
    assert!(recorder.config().buffer > before);
    assert!(recorder.config().buffer_covers(Duration::from_secs(5)));
    assert_eq!(
        recorder.config().buffer,
        roll(replay.pre_roll_s) + Duration::from_secs(5) + BUFFER_MARGIN
    );
}

const MS: u64 = 1_000_000;

fn millis(ms: u64) -> Duration {
    Duration::from_millis(ms)
}

/// A trigger with a `timestamp` (the strike, UTC ms) is dated then on
/// the host clock; one without, when it is read; a timestamp ahead of
/// the host's UTC clock counts as now.
#[test]
fn a_trigger_is_dated_by_its_timestamp() {
    let utc_ms = || {
        u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_millis(),
        )
        .expect("ms")
    };
    let now = 100_000 * MS;
    assert_eq!(trigger_time(now, None), now);
    let at = trigger_time(now, Some(utc_ms() - 3400));
    let early = now - at;
    assert!(
        (3400 * MS..=3450 * MS).contains(&early),
        "{} ms",
        early / MS
    );
    assert_eq!(trigger_time(now, Some(utc_ms() + 60_000)), now);
    // Older than the host clock reaches: the clock's start.
    assert_eq!(trigger_time(MS, Some(utc_ms() - 3400)), 0);
}

#[test]
fn a_clip_short_of_its_pre_roll_is_told() {
    let pre = Duration::from_secs(4);
    assert_eq!(pre_roll_shortfall(6000 * MS, 10_000 * MS, pre), None);
    assert_eq!(pre_roll_shortfall(6090 * MS, 10_000 * MS, pre), None);
    assert_eq!(
        pre_roll_shortfall(7500 * MS, 10_000 * MS, pre),
        Some(millis(1500))
    );
    // An impact earlier than the pre-roll reaches.
    assert_eq!(pre_roll_shortfall(0, 3000 * MS, pre), None);
}
#[test]
fn the_latency_in_effect_is_read_live_and_clamped() {
    let mut config = config_of(
        "[camera.0]\nname = \"Net\"\ncamera = \"test-pattern\"\nlatency_s = 1.5\n\n\
             [camera.1]\nname = \"USB\"\ncamera = \"test-pattern\"\n\n\
             [camera.2]\nname = \"Bad\"\ncamera = \"test-pattern\"\nlatency_s = 11\n",
    );
    let latency = |config: &FlighthookConfig, actor| settings::latency(config, actor);
    assert_eq!(latency(&config, "camera.0"), millis(1500));
    // Absent is 0, and 0 is a value.
    assert_eq!(latency(&config, "camera.1"), Duration::ZERO);
    // Out of range from a hand-edited file: not in effect.
    assert_eq!(latency(&config, "camera.2"), Duration::ZERO);
    assert_eq!(latency(&config, "camera.9"), Duration::ZERO);
    config.camera.get_mut("1").unwrap().latency_s = Some(10.0);
    config.camera.get_mut("0").unwrap().latency_s = Some(f64::NAN);
    assert_eq!(latency(&config, "camera.1"), millis(10_000));
    assert_eq!(latency(&config, "camera.0"), Duration::ZERO);
}

fn at(host_ns: u64, device_ns: Option<u64>) -> Frame {
    Frame {
        jpeg: Arc::from(vec![0u8; 8]),
        host_ns,
        device_ns,
        seq: host_ns / (10 * MS),
        width: 8,
        height: 8,
    }
}

/// The capture's stats at `fps`.
fn stats(fps: f64) -> CaptureStats {
    let mut stats = CaptureStats::default();
    stats.fps = fps;
    stats
}

#[test]
fn the_still_is_put_with_the_first_frame_and_then_every_half_second() {
    let t0 = Instant::now();
    let at_ms = |ms: u64| t0 + Duration::from_millis(ms);
    let mut pass = Pass::new(t0);
    let still =
        |pass: &mut Pass, ns: u64, ms: u64| pass.frame(at(ns, None), Duration::ZERO, at_ms(ms)).0;
    assert!(still(&mut pass, 1, 0).is_some());
    assert!(still(&mut pass, 2, 499).is_none());
    assert!(still(&mut pass, 3, 500).is_some());
    assert!(still(&mut pass, 4, 999).is_none());
}

#[test]
fn telemetry_is_due_every_two_seconds() {
    let t0 = Instant::now();
    let at_ms = |ms: u64| t0 + Duration::from_millis(ms);
    let mut pass = Pass::new(t0);
    assert!(!pass.telemetry_due(at_ms(1999)));
    assert!(pass.telemetry_due(at_ms(2000)));
    assert!(!pass.telemetry_due(at_ms(3999)));
    assert!(pass.telemetry_due(at_ms(4000)));
}

#[test]
fn telemetry_rows_go_out_only_when_they_changed() {
    let t0 = Instant::now();
    let at_ms = |ms: u64| t0 + Duration::from_millis(ms);
    let mut pass = Pass::new(t0);
    let (rows, _) = pass.tick(at_ms(2000), &stats(30.0), 30.0, "Face on".into());
    let rows = rows.expect("the first rows");
    assert_eq!(rows["fps"], "30");
    assert_eq!(rows["dropped"], "0");
    assert_eq!(rows["position"], "Face on");
    let (rows, _) = pass.tick(at_ms(4000), &stats(30.0), 30.0, "Face on".into());
    assert!(rows.is_none(), "nothing changed");
    let (rows, _) = pass.tick(at_ms(6000), &stats(30.0), 30.0, "Down the line".into());
    assert_eq!(
        rows.expect("the position changed")["position"],
        "Down the line"
    );
}

#[test]
fn a_camera_under_half_its_rate_for_ten_seconds_alerts_once_a_session() {
    let t0 = Instant::now();
    let at_ms = |ms: u64| t0 + Duration::from_millis(ms);
    let mut pass = Pass::new(t0);
    let slow = |pass: &mut Pass, ms: u64, fps: f64| {
        pass.tick(at_ms(ms), &stats(fps), 30.0, "Face on".into()).1
    };
    // Slow from the first tick, 2 s in: nothing until 10 s later.
    assert_eq!(slow(&mut pass, 2000, 10.0), None);
    assert_eq!(slow(&mut pass, 11_999, 10.0), None);
    assert_eq!(slow(&mut pass, 12_000, 10.0), Some(10.0));
    assert_eq!(slow(&mut pass, 20_000, 10.0), None, "once a session");
    // A session that recovers in time never alerts: the count starts over.
    let mut pass = Pass::new(t0);
    assert_eq!(slow(&mut pass, 2000, 10.0), None);
    assert_eq!(slow(&mut pass, 8000, 20.0), None, "back above half");
    assert_eq!(slow(&mut pass, 9000, 10.0), None);
    assert_eq!(slow(&mut pass, 18_999, 10.0), None);
    assert_eq!(slow(&mut pass, 19_000, 10.0), Some(10.0));
}

#[test]
fn the_models_are_asked_for_in_live_mode_only() {
    for (mode, wanted) in [
        (BodyTrackingMode::Off, false),
        (BodyTrackingMode::Historical, false),
        (BodyTrackingMode::Live, true),
    ] {
        let mut config = FlighthookConfig::default();
        config.body_tracking.mode = mode;
        let bench = bench(config);
        want_models(&bench.cameras);
        let model = bench.cameras.settings().body_tracking().model;
        assert_eq!(
            bench.cameras.pose.models.state(model).is_some(),
            wanted,
            "{mode:?}"
        );
    }
}

#[test]
fn a_frame_is_dated_its_latency_before_it_arrived() {
    let mut restamp = Restamp::default();
    let f = restamp
        .apply(at(5000 * MS, Some(4990 * MS)), millis(1500))
        .expect("frame");
    assert_eq!(f.host_ns, 3500 * MS);
    // The device time moves with it, so the two stay apart as before.
    assert_eq!(f.device_ns, Some(3490 * MS));
    // The rest of the frame is untouched.
    assert_eq!((f.seq, f.width, f.height), (500, 8, 8));
    assert_eq!(f.jpeg.len(), 8);
}

#[test]
fn no_latency_leaves_every_frame_as_it_arrived() {
    let mut restamp = Restamp::default();
    for i in 1..=5u64 {
        let f = at(i * 10 * MS, None);
        assert_eq!(restamp.apply(f.clone(), Duration::ZERO), Some(f));
    }
}

#[test]
fn a_date_before_the_clocks_zero_is_zero() {
    let mut restamp = Restamp::default();
    let f = restamp
        .apply(at(300 * MS, Some(200 * MS)), millis(2000))
        .expect("first");
    assert_eq!((f.host_ns, f.device_ns), (0, Some(0)));
    // The next one is not after it: dropped until the dating moves on.
    assert!(restamp.apply(at(310 * MS, None), millis(2000)).is_none());
    let f = restamp
        .apply(at(2010 * MS, None), millis(2000))
        .expect("next");
    assert_eq!(f.host_ns, 10 * MS);
}

#[test]
fn dated_frames_stay_strictly_increasing_when_the_latency_changes() {
    let mut restamp = Restamp::default();
    let mut passed = Vec::new();
    let mut feed = |arrival_ms: u64, latency_ms: u64| {
        if let Some(f) = restamp.apply(at(arrival_ms * MS, None), millis(latency_ms)) {
            passed.push(f.host_ns / MS);
        }
    };
    // 100 fps at latency 0.
    for i in 1..=10 {
        feed(i * 10, 0);
    }
    // Raised to 50 ms: the next four frames (dated 60..90 ms) are not
    // newer than the 100 ms already passed on.
    for i in 11..=14 {
        feed(i * 10, 50);
    }
    for i in 15..=17 {
        feed(i * 10, 50);
    }
    // Lowered to 0: the dating jumps forward, nothing is dropped.
    for i in 18..=20 {
        feed(i * 10, 0);
    }
    assert_eq!(
        passed,
        [
            10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, 180, 190, 200
        ]
    );
    assert!(passed.windows(2).all(|w| w[0] < w[1]));
}

/// Recorder settings of the simulations below: a 200 ms pre-roll and a
/// 100 ms post-roll at 100 fps, finished 300 ms after the window.
fn sim_config() -> RecorderConfig {
    let mut config = RecorderConfig::default();
    config.pre_roll = millis(200);
    config.post_roll = millis(100);
    config.merge_window = millis(500);
    config.buffer = millis(5000);
    config.max_bytes = usize::MAX;
    config.stall = millis(300);
    config.fps = Some(100.0);
    config
}

/// A camera `latency_ms` behind real time, shooting 100 fps, and a launch
/// monitor that triggers at impact at real time 2500 ms. Frame `seq`
/// shows real time `seq * 10` ms and arrives `latency_ms` later, stamped
/// with its arrival. The wall clock runs in 10 ms steps: the trigger,
/// then the frame that arrives, then the recorder's poll. Returns what
/// the recorder produced; `shifted` polls on [`frame_clock`] as the
/// the loop does, else on the plain wall clock.
fn simulate(latency_ms: u64, shifted: bool) -> Vec<RecorderEvent<&'static str>> {
    let lag = millis(latency_ms);
    let mut recorder = ClipRecorder::new(sim_config());
    let mut restamp = Restamp::default();
    let mut events = Vec::new();
    for wall_ms in (0..=6000).step_by(10) {
        let wall = wall_ms * MS;
        if wall_ms == 2500 {
            recorder.trigger(CutTrigger::new(wall, "mock_monitor.0", "shot"));
        }
        if wall_ms >= latency_ms {
            let arrival = Frame {
                seq: (wall_ms - latency_ms) / 10,
                ..at(wall, None)
            };
            if let Some(frame) = restamp.apply(arrival, lag) {
                events.extend(recorder.push(frame));
            }
        }
        let now = if shifted {
            frame_clock(wall, lag)
        } else {
            wall
        };
        events.extend(recorder.poll(now));
    }
    events
}

fn clip_of(events: Vec<RecorderEvent<&'static str>>) -> stance::Clip<&'static str> {
    let mut clips = events.into_iter().map(|e| match e {
        RecorderEvent::Finished(c) => c,
        RecorderEvent::Discarded(d) => panic!("discarded: {d:?}"),
        _ => panic!("an unexpected recorder event"),
    });
    let clip = clips.next().expect("a clip");
    assert!(clips.next().is_none(), "one clip");
    clip
}

#[test]
fn a_clip_with_latency_covers_the_real_window() {
    let unlatencied = clip_of(simulate(0, true));
    // The window is [impact - 200 ms, trigger + 100 ms] = 2300..=2600 ms.
    assert_eq!(unlatencied.frames.len(), 31);
    assert_eq!(unlatencied.frames[0].host_ns, 2300 * MS);
    assert_eq!(unlatencied.frames[30].host_ns, 2600 * MS);
    assert!(unlatencied.complete);
    for latency_ms in [100, 500, 1500, 2000] {
        let clip = clip_of(simulate(latency_ms, true));
        assert!(clip.complete, "{latency_ms} ms");
        assert_eq!(clip.dropped, 0, "{latency_ms} ms");
        assert!(!clip.short_buffer, "{latency_ms} ms");
        // The same frames of the swing, at the same times.
        let times = |c: &stance::Clip<&str>| -> Vec<(u64, u64)> {
            c.frames.iter().map(|f| (f.seq, f.host_ns)).collect()
        };
        assert_eq!(times(&clip), times(&unlatencied), "{latency_ms} ms");
        // The impact mark sits on the frame that shows the impact, and
        // the trigger and impact times are the same as without latency.
        assert_eq!(clip.impact_frame().expect("impact").seq, 250);
        assert_eq!(clip.impact_index, unlatencied.impact_index);
        assert_eq!(clip.trigger_index, unlatencied.trigger_index);
        assert_eq!(clip.trigger_ns, unlatencied.trigger_ns);
        assert_eq!(clip.impact_ns, unlatencied.impact_ns);
    }
}

#[test]
fn the_shifted_clock_keeps_a_clip_open_for_late_frames() {
    // On the plain wall clock the recorder gives up on the window 300 ms
    // after it ended (3 s of real time here, before any frame of it has
    // arrived) and the clip has no impact frame.
    let events = simulate(2000, false);
    assert!(
        matches!(events.as_slice(), [RecorderEvent::Discarded(_)]),
        "{events:?}"
    );
    // A latency under the stall time survives either way, only
    // incomplete.
    let clip = clip_of(simulate(200, false));
    assert_eq!(clip.impact_frame().expect("impact").seq, 250);
    // On the shifted clock a camera 2 s behind still gets its clip.
    assert!(clip_of(simulate(2000, true)).complete);
}

#[test]
fn two_cameras_with_different_latency_cut_aligned_clips() {
    let direct = clip_of(simulate(0, true));
    let buffered = clip_of(simulate(1800, true));
    // What the sidecars would hold: times relative to the first frame.
    let relative = |c: &stance::Clip<&str>| {
        let first = c.frames[0].host_ns;
        (
            c.frames
                .iter()
                .map(|f| f.host_ns - first)
                .collect::<Vec<_>>(),
            c.trigger_ns - first,
            c.impact_ns - first,
            c.trigger_index,
            c.impact_index,
        )
    };
    assert_eq!(relative(&direct), relative(&buffered));
    assert_eq!(direct.frames[0].host_ns, buffered.frames[0].host_ns);
    // Frame for frame, the same moment of the swing.
    for (a, b) in direct.frames.iter().zip(&buffered.frames) {
        assert_eq!(a.seq, b.seq);
    }
}

#[test]
fn the_history_covers_the_pre_roll_whatever_the_latency() {
    // The ring spans the dated frames, which stop `latency` short of the
    // trigger, so it needs no more than pre-roll plus lead; the config
    // does not take the latency.
    let replay = ReplaySection::default();
    let config = recorder_config(&replay, millis(1500), None);
    assert_eq!(
        config.buffer,
        roll(replay.pre_roll_s) + millis(1500) + BUFFER_MARGIN
    );
    // A 10 s latency camera, impact lead 1.5 s, default pre-roll: the
    // window start is in the ring when the trigger arrives.
    let mut config = RecorderConfig::default();
    config.pre_roll = millis(500);
    config.post_roll = millis(100);
    config.buffer = millis(500) + millis(1500) + BUFFER_MARGIN;
    config.fps = Some(100.0);
    let mut recorder: ClipRecorder<&'static str> = ClipRecorder::new(config);
    let mut restamp = Restamp::default();
    let lag = millis(10_000);
    let mut clips = Vec::new();
    // Real time 0..30 s; frames arrive 10 s late.
    for seq in 0..3000u64 {
        let wall = seq * 10 * MS + 10_000 * MS;
        if wall == 22_000 * MS {
            // Trigger at real 22 s, impact 1.5 s earlier.
            recorder
                .trigger(CutTrigger::new(wall, "square.0", "shot").with_impact_lead(millis(1500)));
        }
        if let Some(f) = restamp.apply(
            Frame {
                seq,
                ..at(wall, None)
            },
            lag,
        ) {
            clips.extend(recorder.push(f));
        }
    }
    let clip = clip_of(clips);
    assert!(clip.complete);
    assert!(!clip.short_buffer);
    // Window: impact 20.5 s - 0.5 s ..= 22.1 s.
    assert_eq!(clip.frames[0].host_ns, 20_000 * MS);
    assert_eq!(clip.impact_frame().expect("impact").seq, 2050);
}

/// Frames admitted by a watched [`LiveGate`] from a camera at `fps`
/// over `secs` seconds.
fn admitted(fps: f64, secs: f64) -> usize {
    let t0 = Instant::now();
    let mut gate = LiveGate::default();
    let frames = (fps * secs) as u32;
    (0..frames)
        .filter(|n| gate.admit(true, t0 + Duration::from_secs_f64(f64::from(*n) / fps)))
        .count()
}

#[test]
fn the_live_slot_takes_at_most_live_fps_frames_a_second() {
    let fps = LIVE_FPS as usize;
    // Faster cameras are thinned to the live rate, a 30 fps one too
    // (its frames sit right on the schedule).
    for camera in [30.0, 31.0, 60.0, 120.0, 240.0] {
        let n = admitted(camera, 10.0);
        assert!(
            (fps * 10 - 1..=fps * 10 + 1).contains(&n),
            "{camera} fps: {n}"
        );
    }
    // A slower one keeps every frame.
    assert_eq!(admitted(10.0, 10.0), 100);
    assert_eq!(admitted(15.0, 10.0), 150);
}

#[test]
fn the_live_slot_takes_nothing_unwatched_and_starts_afresh() {
    let t0 = Instant::now();
    let mut gate = LiveGate::default();
    assert!(!gate.admit(false, t0));
    assert!(gate.admit(true, t0 + millis(10)));
    assert!(!gate.admit(true, t0 + millis(20)), "within one interval");
    assert!(!gate.admit(false, t0 + millis(200)));
    // A new viewer gets the next frame at once.
    assert!(gate.admit(true, t0 + millis(210)));
    // A stall doesn't bank frames: one, then the rate again.
    assert!(gate.admit(true, t0 + millis(2000)));
    assert!(!gate.admit(true, t0 + millis(2010)));
    assert!(gate.admit(true, t0 + millis(2000) + LIVE_EVERY));
}

#[test]
fn the_reported_frame_rate_moves_only_on_a_real_change() {
    let mut fps = FpsReport::default();
    assert!((fps.update(119.6, 120.0) - 119.6).abs() < 1e-9);
    // Hovering within 2 % of 120 fps keeps the value shown.
    for measured in [120.4, 118.0, 121.5, 119.1] {
        assert!(
            (fps.update(measured, 120.0) - 119.6).abs() < 1e-9,
            "{measured}"
        );
    }
    assert!((fps.update(100.0, 120.0) - 100.0).abs() < 1e-9);
    // At least 1 fps for slow modes.
    let mut slow = FpsReport::default();
    slow.update(30.0, 30.0);
    assert!((slow.update(29.2, 30.0) - 30.0).abs() < 1e-9);
    assert!((slow.update(28.9, 30.0) - 28.9).abs() < 1e-9);
}

#[test]
fn only_the_camera_and_the_mode_restart_the_camera() {
    let section = CameraSection {
        name: "Face".into(),
        camera: "usb-1".into(),
        camera_name: Some("HD Pro Webcam C920".into()),
        ..CameraSection::default()
    };
    let restart = |change: &dyn Fn(&mut CameraSection)| {
        let mut s = section.clone();
        change(&mut s);
        restart_needed(&section, Some(&s))
    };
    assert!(!restart(&|_| {}));
    assert!(!restart(&|s| {
        s.name = "DTL".into();
        s.camera_name = Some("Other name".into());
        s.position = flighthook::CameraPosition::TargetLine;
    }));
    // The latency is read for every frame: no reopen.
    assert!(!restart(&|s| s.latency_s = Some(2.5)));
    assert!(!restart(&|s| s.latency_s = None));
    // So are the rotation, yaw and pitch.
    assert!(!restart(&|s| {
        s.rotation_deg = 90;
        s.yaw_deg = Some(7.0);
        s.pitch_deg = Some(-4.0);
    }));
    assert!(restart(&|s| s.camera = "usb-2".into()));
    assert!(restart(&|s| s.mode = "640x480@30".into()));
    assert!(restart_needed(&section, None), "its section is gone");
}

/// A bench whose clips go to a folder of its own.
fn clips_bench() -> crate::bench::Bench {
    let mut config = FlighthookConfig::default();
    let bench = bench(config.clone());
    config.replay.clip_dir = Some(bench.dir.0.join("clips").display().to_string());
    bench.update(|c| *c = config);
    bench
}

#[test]
fn a_clip_of_shots_cleared_since_their_trigger_is_not_written() {
    let bench = clips_bench();
    let (out, rx) = out();
    let before = unpaced_clip(&[shot_key(0)], bench.cameras.clips.generation());
    bench.cameras.clips.clear();
    save(&bench.cameras, "camera.0", &out, &out, &before);
    assert!(rx.try_recv().is_err(), "no video");
    let clips = bench.dir.0.join("clips");
    assert_eq!(std::fs::read_dir(&clips).map_or(0, Iterator::count), 0);

    let generation = bench.cameras.clips.generation();
    let after = unpaced_clip(&[shot_key(generation)], generation);
    save(&bench.cameras, "camera.0", &out, &out, &after);
    assert!(matches!(rx.try_recv(), Ok(Event::Video { .. })));
    assert_eq!(
        bench
            .cameras
            .clips
            .videos(&format!("shot-{generation}"))
            .len(),
        1
    );
}

/// The shot `shot-{g}`.
fn shot_key(g: u64) -> ShotKey {
    ShotKey {
        shot_id: format!("shot-{g}"),
        shot_number: 1,
    }
}

#[test]
fn a_repeated_trigger_is_seen_once() {
    let mut seen = SeenTriggers::default();
    let key = |id: &str| ShotKey {
        shot_id: id.into(),
        shot_number: 1,
    };
    assert!(seen.first("mevo.0", &key("a")));
    assert!(!seen.first("mevo.0", &key("a")));
    assert!(seen.first("r10.0", &key("a")));
    for i in 0..SEEN_TRIGGERS {
        assert!(seen.first("mevo.0", &key(&i.to_string())));
    }
    // Forgotten after 64 others.
    assert!(seen.first("mevo.0", &key("a")));
}

static BUILDS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// A live worker on a fake tracker, counting the trackers it makes.
fn fake_worker(cameras: &Arc<Cameras>, actor: &str) -> Option<LiveWorker> {
    let settings_cameras = Arc::clone(cameras);
    let settings_actor = actor.to_owned();
    let settings: live::Settings =
        Box::new(move || live_settings(&settings_cameras, &settings_actor));
    let build: live::Build = Box::new(|s, _| {
        BUILDS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        Some(live::tests::fake_tracker(
            Duration::ZERO,
            &calls,
            true,
            s.detect_every,
        ))
    });
    let publish_cameras = Arc::clone(cameras);
    let publish_actor = actor.to_owned();
    let publish: live::Publish = Box::new(move |t, jpeg, pose| {
        publish_cameras
            .frames
            .set_live(&publish_actor, t, jpeg, pose);
    });
    LiveWorker::start(actor, settings, build, publish).ok()
}

/// The live slot's frame once it is newer than `after`.
fn live_after(cameras: &Cameras, after: u64) -> Arc<crate::frames::LiveFrame> {
    for _ in 0..400 {
        if let Some(f) = cameras.frames.live("camera.0").filter(|f| f.seq > after) {
            return f;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("no live frame after {after}");
}

#[test]
fn a_watched_camera_goes_live_with_poses_and_a_lapsed_watch_resets_the_tracker() {
    use crate::pose::live::tests::numbered_jpeg;
    let bench = bench(config_of(LIVE));
    let cameras = &bench.cameras;
    let mut route = LiveRoute {
        start: fake_worker,
        ..LiveRoute::default()
    };
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    // Watched, in Live mode: the frame goes out with its pose.
    route.route(cameras, "camera.0", true, at(0), 11, numbered_jpeg(60));
    let frame = live_after(cameras, 0);
    assert_eq!(frame.time_ns, 11);
    let pose: flighthook::LivePose =
        serde_json::from_str(frame.pose.as_deref().expect("a pose")).expect("json");
    assert_eq!(pose.keypoints.len(), 26);
    assert!((pose.keypoints[0][0] - 60.0).abs() <= 3.0, "{pose:?}");
    assert_eq!(BUILDS.load(std::sync::atomic::Ordering::SeqCst), 1);
    // The watch lapses: the worker goes, and nothing is put.
    route.route(cameras, "camera.0", false, at(100), 12, numbered_jpeg(1));
    assert!(route.worker.is_none());
    assert_eq!(
        cameras.frames.live("camera.0").map(|f| f.seq),
        Some(frame.seq),
        "the lapsed watch put no frame"
    );
    // Watched again: a new tracker, nothing remembered.
    route.route(cameras, "camera.0", true, at(200), 13, numbered_jpeg(120));
    let again = live_after(cameras, frame.seq);
    assert_eq!(again.time_ns, 13);
    assert!(again.pose.is_some());
    assert_eq!(BUILDS.load(std::sync::atomic::Ordering::SeqCst), 2);
    // Out of Live mode: frames go straight out, without a pose.
    let mut last = again.seq;
    for (n, mode) in [BodyTrackingMode::Historical, BodyTrackingMode::Off]
        .into_iter()
        .enumerate()
    {
        bench.update(|c| c.body_tracking.mode = mode);
        let ms = 300 + 100 * n as u64;
        route.route(cameras, "camera.0", true, at(ms), ms, numbered_jpeg(1));
        assert!(route.worker.is_none(), "{mode:?}");
        let plain = live_after(cameras, last);
        assert_eq!(plain.time_ns, ms);
        assert!(plain.pose.is_none());
        last = plain.seq;
    }
    assert_eq!(BUILDS.load(std::sync::atomic::Ordering::SeqCst), 2);
}

static DYING_STARTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// A live worker whose thread panics on its first frame.
fn dying_worker(cameras: &Arc<Cameras>, actor: &str) -> Option<LiveWorker> {
    DYING_STARTS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let settings_cameras = Arc::clone(cameras);
    let settings_actor = actor.to_owned();
    let settings: live::Settings =
        Box::new(move || live_settings(&settings_cameras, &settings_actor));
    let build: live::Build = Box::new(|_, _| panic!("a broken model"));
    let publish: live::Publish = Box::new(|_, _, _| {});
    LiveWorker::start(actor, settings, build, publish).ok()
}

#[test]
fn a_dead_live_worker_is_restarted_then_reported_as_failed() {
    use crate::pose::live::tests::numbered_jpeg;
    use std::sync::atomic::Ordering;
    let bench = bench(config_of(LIVE));
    let cameras = &bench.cameras;
    let mut route = LiveRoute {
        start: dying_worker,
        ..LiveRoute::default()
    };
    let settings = cameras.settings().body_tracking();
    let t0 = Instant::now();
    let mut t = 0;
    let mut frame = |route: &mut LiveRoute, wanted: bool| {
        t += 1;
        route.route(
            cameras,
            "camera.0",
            wanted,
            t0 + Duration::from_millis(t * 70),
            t,
            numbered_jpeg(1),
        );
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    // The first worker and three replacements die on their frame.
    for starts in 1..=4 {
        frame(&mut route, true);
        assert_eq!(DYING_STARTS.load(Ordering::SeqCst), starts);
        while route.worker.as_ref().is_some_and(LiveWorker::alive) {
            assert!(Instant::now() < deadline, "the worker did not end");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(cameras.pose.live_state(&settings, "camera.0").0, "loading");
    }
    // The fourth death: no more workers, frames go out without a pose,
    // and the live state says why.
    frame(&mut route, true);
    assert!(route.worker.is_none());
    assert_eq!(DYING_STARTS.load(Ordering::SeqCst), 4);
    let (code, error) = cameras.pose.live_state(&settings, "camera.0");
    assert_eq!(code, "failed");
    assert!(error.expect("why").contains("stopped"));
    let plain = cameras.frames.live("camera.0").expect("a frame");
    assert!(plain.pose.is_none());
    frame(&mut route, true);
    assert_eq!(DYING_STARTS.load(Ordering::SeqCst), 4);
    // The watch lapses: forgotten, and the next viewer gets a worker.
    frame(&mut route, false);
    assert_eq!(cameras.pose.live_state(&settings, "camera.0").0, "loading");
    frame(&mut route, true);
    assert_eq!(DYING_STARTS.load(Ordering::SeqCst), 5);
}
