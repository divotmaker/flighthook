//! Test support for the host's tests (feature `testing`): a clip in the
//! store without a camera.

use std::sync::Arc;
use std::time::Duration;

use flighthook::ShotKey;
use stance::{ClipRecorder, RecorderConfig, RecorderEvent, Source, TestPattern, TestPatternConfig};

use crate::Cameras;
use crate::camera::{Tag, save};
use crate::clips::ClipShot;

/// Put a clip of the shots `keys` in the clip store as `camera.0`'s, written
/// and announced as a camera does it: a 100 fps clip of the unpaced test
/// pattern, its trigger and impact 500 ms in, saved at once on this thread.
/// Every file is in place when it returns. In Live mode the clip is queued
/// for body tracking, as a camera's is.
pub fn store_clip(cameras: &Arc<Cameras>, keys: &[ShotKey]) {
    let clip = unpaced_clip(keys, cameras.clips.generation());
    let nothing: crate::Out = Arc::new(|_| {});
    save(cameras, "camera.0", &nothing, &nothing, &clip);
}

/// A 100 fps clip of the shots `keys` from the unpaced test pattern, dated
/// from 0: triggered at 500 ms, at generation `generation`.
pub(crate) fn unpaced_clip(keys: &[ShotKey], generation: u64) -> stance::Clip<Tag> {
    let mut config = TestPatternConfig::default();
    config.width = 64;
    config.height = 48;
    config.fps = 100;
    config.paced = false;
    config.origin_ns = Some(0);
    let mut pattern = TestPattern::new(config).expect("pattern");
    let mut config = RecorderConfig::default();
    config.pre_roll = Duration::from_millis(100);
    config.post_roll = Duration::from_millis(100);
    let mut recorder = ClipRecorder::new(config);
    for seq in 0..100 {
        if seq == 50 {
            for (i, key) in keys.iter().enumerate() {
                let tag = Tag {
                    shot: ClipShot {
                        key: key.clone(),
                        device: "mock".into(),
                        impact_lead_ms: 0,
                        hand: None,
                        golfer: None,
                    },
                    generation,
                };
                recorder.trigger(stance::Trigger::new(
                    500_000_000,
                    format!("mock_monitor.{i}"),
                    tag,
                ));
            }
        }
        let frame = pattern
            .next_frame(Duration::ZERO)
            .expect("frame")
            .expect("frame");
        for event in recorder.push(frame) {
            if let RecorderEvent::Finished(clip) = event {
                return clip;
            }
        }
    }
    panic!("no clip");
}
