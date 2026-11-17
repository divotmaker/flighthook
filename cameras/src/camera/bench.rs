//! The camera's core on one thread: a [`Recording`] and a [`Pass`] fed by
//! the unpaced test pattern at explicit times, every finished clip saved
//! at once into [`Cameras`] over a scratch folder, and the events kept in
//! order. No capture thread, no host, no sleeps.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use flighthook::{BodyTrackingMode, CameraPosition, CameraSection, FlighthookConfig, ShotKey};
use stance::{RecorderEvent, Source, TestPattern, TestPatternConfig};

use super::{Pass, Recording, Tag, Trigger, save};
use crate::{Cameras, Event, Out, settings};

const MS: u64 = 1_000_000;

const CAMERA: &str = "camera.0";

/// One test-pattern camera at 100 fps, its frames dated from 0, read one
/// at a time at their own host time.
struct Bench {
    base: crate::bench::Bench,
    cameras: Arc<Cameras>,
    /// Every event, in order.
    events: Arc<Mutex<Vec<Event>>>,
    out: Out,
    recording: Recording,
    pass: Pass,
    pattern: TestPattern,
    /// The instant host time 0 stands for.
    t0: Instant,
    /// Host time of the newest frame read: the bench's clock.
    now_ns: u64,
    /// Clips the recorder discarded.
    discarded: usize,
}

impl Bench {
    /// A 0.4 s pre-roll and a 0.6 s post-roll, keeping `keep` shots, body
    /// tracking in `mode`.
    fn new(keep: u32, mode: BodyTrackingMode) -> Self {
        let mut config = FlighthookConfig::default();
        config.replay.keep_shots = keep;
        config.replay.pre_roll_s = 0.4;
        config.replay.post_roll_s = 0.6;
        config.camera.insert(
            "0".into(),
            CameraSection {
                name: "Face".into(),
                camera: CameraSection::TEST_PATTERN.into(),
                position: CameraPosition::GolferFront,
                ..CameraSection::default()
            },
        );
        config.body_tracking.mode = mode;
        let mut recording = Recording::new(&config);
        recording.set_fps(100.0);
        let base = crate::bench::bench(config);
        base.update(|c| {
            c.replay.clip_dir = Some(base.dir.0.join("clips").display().to_string());
        });
        let cameras = Arc::clone(&base.cameras);
        let events: Arc<Mutex<Vec<Event>>> = Arc::default();
        let sink = Arc::clone(&events);
        let out: Out = Arc::new(move |e| sink.lock().unwrap().push(e));
        let pattern = TestPattern::new(TestPatternConfig {
            width: 64,
            height: 48,
            fps: 100,
            paced: false,
            origin_ns: Some(0),
            ..TestPatternConfig::default()
        })
        .expect("pattern");
        let t0 = Instant::now();
        Self {
            base,
            cameras,
            events,
            out,
            recording,
            pass: Pass::new(t0),
            pattern,
            t0,
            now_ns: 0,
            discarded: 0,
        }
    }

    /// The next frame, read at its host time with the latency in effect,
    /// then the recorder polled: what the shell does in a pass with one
    /// frame. Returns the time the frame was recorded at, `None` when it
    /// was dropped.
    fn frame(&mut self) -> Option<u64> {
        let frame = self
            .pattern
            .next_frame(Duration::ZERO)
            .expect("frame")
            .expect("frame");
        self.now_ns = frame.host_ns;
        let lag = settings::latency(&self.cameras.settings().read(), CAMERA);
        let now = self.t0 + Duration::from_nanos(self.now_ns);
        let (_, frame) = self.pass.frame(frame, lag, now);
        let recorded = frame.map(|frame| {
            let at = frame.host_ns;
            let events = self.recording.push(frame);
            self.handle(events);
            at
        });
        let events = self.recording.poll(self.now_ns, lag);
        self.handle(events);
        recorded
    }

    /// Frames until host time `ms`.
    fn run_to(&mut self, ms: u64) {
        while self.now_ns < ms * MS {
            self.frame();
        }
    }

    /// Finished clips saved, discarded ones counted.
    fn handle(&mut self, events: Vec<RecorderEvent<Tag>>) {
        for event in events {
            match event {
                RecorderEvent::Finished(clip) => {
                    save(&self.cameras, CAMERA, &self.out, &self.out, &clip);
                }
                RecorderEvent::Discarded(_) => self.discarded += 1,
            }
        }
    }

    /// `card`'s trigger for shot `n`, read now, without a strike time.
    fn trigger(&mut self, card: &str, n: u32) -> ShotKey {
        let key = ShotKey {
            shot_id: format!("{card}-{n}"),
            shot_number: n,
        };
        let prefix = card.split_once('.').map_or(card, |(p, _)| p);
        let shot = Trigger {
            card: card.to_owned(),
            device: prefix.to_owned(),
            key: key.clone(),
            timestamp: None,
        };
        let turn = self.cameras.turn();
        let generation = self.cameras.clips.generation();
        self.recording.trigger(
            &self.cameras.settings().read(),
            &shot,
            self.now_ns,
            &turn,
            generation,
        );
        key
    }

    fn events(&self) -> Vec<Event> {
        self.events.lock().unwrap().clone()
    }

    /// Each `Video` event's shots, and whether it was complete.
    fn videos(&self) -> Vec<(Vec<ShotKey>, bool)> {
        self.events()
            .into_iter()
            .filter_map(|e| match e {
                Event::Video { keys, complete, .. } => Some((keys, complete)),
                _ => None,
            })
            .collect()
    }

    /// Each `Video` event's shots.
    fn clips(&self) -> Vec<Vec<ShotKey>> {
        self.videos().into_iter().map(|(keys, _)| keys).collect()
    }

    /// Clip folders on disk.
    fn folders(&self) -> usize {
        std::fs::read_dir(self.base.dir.0.join("clips"))
            .map(|d| d.flatten().filter(|e| e.path().is_dir()).count())
            .unwrap_or(0)
    }

    /// The saved sidecar of shot `key`'s clip.
    fn sidecar(&self, key: &ShotKey) -> stance::sidecar::Sidecar {
        let path = self
            .cameras
            .clips
            .file(&key.shot_id, CAMERA, "json")
            .expect("sidecar");
        let text = std::fs::read_to_string(path).expect("read");
        stance::sidecar::Sidecar::from_json(&text).expect("sidecar")
    }
}

#[test]
fn a_latency_raised_between_two_frames_dates_the_next_frames_earlier() {
    let mut bench = Bench::new(20, BodyTrackingMode::Off);
    bench.run_to(500);
    assert_eq!(bench.frame(), Some(510 * MS), "dated as it arrived");
    bench
        .base
        .update(|c| c.camera.get_mut("0").unwrap().latency_s = Some(0.3));
    // Dated 300 ms earlier: the frames that arrive by 810 ms are not after
    // the one recorded at 510 ms, so they are dropped.
    while bench.now_ns < 810 * MS {
        assert_eq!(bench.frame(), None);
    }
    assert_eq!(bench.frame(), Some(520 * MS));
    // A shot still gets its whole clip.
    let key = bench.trigger("mock_monitor.0", 1);
    bench.run_to(3000);
    assert_eq!(bench.videos(), [(vec![key], true)]);
    assert_eq!(bench.discarded, 0);
}

#[test]
fn two_monitors_seeing_one_swing_share_one_clip() {
    let mut bench = Bench::new(20, BodyTrackingMode::Off);
    bench.run_to(1000);
    let a = bench.trigger("mock_monitor.0", 1);
    bench.frame();
    // 10 ms later: well inside the 500 ms merge window.
    let b = bench.trigger("mock_monitor.1", 1);
    bench.run_to(3000);
    assert_eq!(bench.clips(), [vec![a.clone(), b.clone()]]);
    // The next shot gets a clip of its own.
    let c = bench.trigger("mock_monitor.0", 2);
    bench.run_to(5000);
    assert_eq!(bench.clips(), [vec![a.clone(), b.clone()], vec![c.clone()]]);
    assert_eq!(bench.folders(), 2);
    for key in [&a, &b, &c] {
        assert_eq!(bench.cameras.clips.videos(&key.shot_id).len(), 1);
    }
}

#[test]
fn back_to_back_shots_from_one_monitor_make_two_clips() {
    let mut bench = Bench::new(20, BodyTrackingMode::Off);
    bench.run_to(1000);
    let a = bench.trigger("mock_monitor.0", 1);
    // Inside the first's 0.6 s post-roll.
    bench.run_to(1120);
    let b = bench.trigger("mock_monitor.0", 2);
    bench.run_to(4000);
    assert_eq!(bench.clips(), [vec![a.clone()], vec![b.clone()]]);
    assert_eq!(bench.folders(), 2);
    // The first was cut short at the second trigger.
    assert!(bench.sidecar(&a).truncated);
    assert!(!bench.sidecar(&b).truncated);
}

#[test]
fn an_auto_chip_records_nothing() {
    let mut bench = Bench::new(20, BodyTrackingMode::Off);
    bench.run_to(1000);
    bench.trigger(flighthook::AUTO_CHIP_ACTOR, 1);
    assert_eq!(bench.recording.recorder.open_clips(), 0);
    bench.run_to(4000);
    assert!(bench.events().is_empty());
    assert_eq!(bench.discarded, 0);
    assert!(bench.recording.flush().is_empty());
    assert_eq!(bench.folders(), 0);
}

#[test]
fn pruning_to_keep_shots_reports_the_removed_shots() {
    let mut bench = Bench::new(1, BodyTrackingMode::Off);
    bench.run_to(1000);
    let a = bench.trigger("mock_monitor.0", 1);
    bench.run_to(3000);
    assert_eq!(bench.clips(), [vec![a.clone()]]);
    let b = bench.trigger("mock_monitor.0", 2);
    bench.run_to(5000);
    assert_eq!(bench.clips(), [vec![a.clone()], vec![b.clone()]]);
    assert_eq!(
        bench.events().last(),
        Some(&Event::VideoRemoved {
            keys: vec![a.clone()]
        })
    );
    assert!(bench.cameras.clips.videos(&a.shot_id).is_empty());
    assert_eq!(bench.cameras.clips.videos(&b.shot_id).len(), 1);
    assert_eq!(bench.folders(), 1);
}

#[test]
fn a_clip_saved_in_live_mode_is_queued_for_body_tracking() {
    let mut bench = Bench::new(20, BodyTrackingMode::Live);
    // Hold the worker off to see the queue.
    bench.cameras.pose.clips.hold();
    bench.run_to(1000);
    let a = bench.trigger("mock_monitor.0", 1);
    bench.run_to(3000);
    assert_eq!(bench.clips(), [vec![a.clone()]]);
    let avi = bench
        .cameras
        .clips
        .file(&a.shot_id, CAMERA, "avi")
        .expect("avi");
    assert_eq!(bench.cameras.pose.clips.waiting(), [avi]);
}
