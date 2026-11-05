//! A recorded clip played as a camera, for development and tests only:
//! this module exists in debug builds alone.
//!
//! A camera section whose `camera` is `clip:<path to an .avi>` plays that
//! stance clip (its sidecar beside it) in a loop at its own pace, every
//! frame stamped afresh on the host clock as it is delivered, so the actor
//! records, goes live and tracks bodies on it as on a real camera.

use std::path::{Path, PathBuf};
use std::time::Duration;

use stance::{AviFile, CameraInfo, Format, Frame, Source};

/// The `camera` prefix that names a clip.
pub const PREFIX: &str = "clip:";

/// A stance clip played in a loop in real time.
pub struct LoopingClip {
    path: PathBuf,
    file: AviFile,
    info: CameraInfo,
    format: Format,
    /// Host time of the last frame delivered.
    last_ns: Option<u64>,
}

impl LoopingClip {
    /// Open `path` (an MJPEG AVI) to play from now.
    ///
    /// # Errors
    ///
    /// As [`AviFile::open`]; an empty clip is [`stance::Error::Ended`].
    pub fn open(path: &Path) -> stance::Result<Self> {
        let file = AviFile::open(path, true)?;
        if file.is_empty() {
            return Err(stance::Error::Ended);
        }
        let mut info = file.info().clone();
        info.id = format!("{PREFIX}{}", path.display());
        let format = file.format().clone();
        Ok(Self {
            path: path.to_owned(),
            file,
            info,
            format,
            last_ns: None,
        })
    }
}

impl Source for LoopingClip {
    fn next_frame(&mut self, timeout: Duration) -> stance::Result<Option<Frame>> {
        match self.file.next_frame(timeout) {
            Err(stance::Error::Ended) => {
                // Start over one frame period after the last frame.
                let period = self.format.frame_period_ns().max(1);
                let origin = self
                    .last_ns
                    .map_or_else(stance::clock::now_ns, |t| t.saturating_add(period));
                self.file = AviFile::open(&self.path, true)?.with_origin(origin);
                self.file.next_frame(timeout).map(|f| self.seen(f))
            }
            result => result.map(|f| self.seen(f)),
        }
    }

    fn info(&self) -> &CameraInfo {
        &self.info
    }

    fn format(&self) -> &Format {
        &self.format
    }
}

impl LoopingClip {
    fn seen(&mut self, frame: Option<Frame>) -> Option<Frame> {
        if let Some(f) = &frame {
            self.last_ns = Some(f.host_ns);
        }
        frame
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actors::web::tests::TempDir;
    use stance::{ClipRecorder, RecorderConfig, RecorderEvent, TestPattern, TestPatternConfig};

    /// A 10-frame, 100 fps test-pattern clip written by stance.
    fn clip(dir: &Path) -> PathBuf {
        let mut pattern = TestPattern::new(TestPatternConfig {
            width: 64,
            height: 48,
            fps: 100,
            paced: false,
            ..TestPatternConfig::default()
        })
        .expect("pattern");
        let mut recorder: ClipRecorder<u32> = ClipRecorder::new(RecorderConfig {
            pre_roll: Duration::from_millis(40),
            post_roll: Duration::from_millis(50),
            ..RecorderConfig::default()
        });
        let mut clip = None;
        for i in 0..40u64 {
            let mut frame = pattern.next_frame(Duration::ZERO).unwrap().unwrap();
            frame.host_ns = 1_000_000_000 + i * 10_000_000;
            if i == 5 {
                recorder.trigger(stance::Trigger::new(frame.host_ns, "m", 1));
            }
            for event in recorder.push(frame) {
                if let RecorderEvent::Finished(c) = event {
                    clip = Some(c);
                }
            }
        }
        let files = stance::write_clip(dir, "camera.0", &clip.expect("clip")).expect("write");
        files.avi
    }

    #[test]
    fn a_clip_plays_in_a_loop_with_fresh_rising_stamps() {
        let dir = TempDir(std::env::temp_dir().join(format!(
            "flighthook-clip-source-{}",
            uuid::Uuid::new_v4().simple()
        )));
        std::fs::create_dir_all(&dir.0).unwrap();
        let avi = clip(&dir.0);
        let mut source = LoopingClip::open(&avi).expect("open");
        let n = AviFile::open(&avi, false).unwrap().len();
        assert!(n >= 5, "{n} frames");
        assert!(source.info().id.starts_with(PREFIX));
        let start = stance::clock::now_ns();
        let mut stamps = Vec::new();
        while stamps.len() < n * 2 + 2 {
            if let Some(f) = source.next_frame(Duration::from_secs(1)).unwrap() {
                stamps.push(f.host_ns);
            }
        }
        assert!(stamps[0] >= start.saturating_sub(1_000_000), "stamped now");
        assert!(stamps.windows(2).all(|w| w[1] > w[0]), "{stamps:?}");
        // Played at its own pace: about 10 ms a frame, across the loop too.
        let span = stamps.last().unwrap() - stamps[0];
        let per = span / (stamps.len() as u64 - 1);
        assert!((9_000_000..=11_000_000).contains(&per), "{per} ns a frame");
        assert!(stance::clock::now_ns() >= *stamps.last().unwrap(), "paced");
    }
}
