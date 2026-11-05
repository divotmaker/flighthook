//! Body tracking (alpha): the golfer's keypoints on the cameras' pictures,
//! found on this computer's CPU with stance's `pose` feature (RTMPose and a
//! YOLOX person detector).
//!
//! - [`models`]: the models, downloaded into the models folder only when
//!   the user confirms it, loaded once and shared.
//! - [`live`]: a camera's live view worker, which puts each live frame in
//!   its slot together with the pose found on it.
//! - [`clips`]: the queue that tracks every recorded swing and writes its
//!   keypoints, then its swing analysis, beside the clip.
//! - [`swing`]: swing events, tempo and body metrics of a recorded swing,
//!   and the live posture (stance's golf analysis).
//! - [`priority`]: below-normal CPU priority for the clip pass.
//!
//! `[body_tracking]` is read live everywhere: a change never restarts a
//! camera. Its `mode` decides what runs:
//!
//! - Off (the default): nothing. No model is loaded or downloaded.
//! - Historical: a clip is tracked the first time it is viewed
//!   ([`clips::request`]); the models load with the first one.
//! - Live: the live views while watched, and every clip as it is recorded;
//!   the models load when a camera starts.
//!
//! [`BodyTracking::follow`] applies a mode change: leaving Live stops the
//! live workers (each camera's route sees the mode on its next frame) and
//! the recorded clips still waiting; Off also drops every clip waiting,
//! lets the models go and stops a model download.
//!
//! No mode downloads anything: a model whose files are missing waits
//! ([`models::ModelState::Missing`]) until the user confirms the download
//! in the UI ([`models::Models::download`]).

pub mod clips;
pub mod live;
pub mod models;
pub mod priority;
pub mod swing;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use flighthook::{BodyTrackingMode, BodyTrackingSection};
use tracing::info;

/// Everything body tracking holds for the app: the models, the queue of
/// recorded clips, and the cameras whose live tracking has failed.
pub struct BodyTracking {
    pub models: models::Models,
    pub clips: clips::ClipQueue,
    /// Why each camera's live tracking stopped, by camera actor.
    live_failed: Mutex<HashMap<String, String>>,
    /// The mode [`BodyTracking::follow`] last saw.
    mode: Mutex<Option<BodyTrackingMode>>,
}

impl BodyTracking {
    /// Body tracking with its models in `dir`, checked and downloaded
    /// through `source`.
    pub fn new(dir: PathBuf, source: Arc<dyn models::Source>) -> Self {
        Self {
            models: models::Models::new(dir, source),
            clips: clips::ClipQueue::default(),
            live_failed: Mutex::default(),
            mode: Mutex::default(),
        }
    }

    /// Apply the settings' mode after a config change: when it turned Off,
    /// drop every clip waiting, let the models go, stop a model download
    /// and forget the cameras' live failures. A pass already running
    /// finishes; its models go when it ends.
    pub fn follow(&self, settings: &BodyTrackingSection) {
        let mode = settings.mode;
        let before = self
            .mode
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replace(mode);
        if before == Some(mode) {
            return;
        }
        if before.is_some() {
            info!("body tracking: {}", mode.label());
        }
        if mode == BodyTrackingMode::Off {
            let dropped = self.clips.clear();
            if dropped > 0 {
                info!("body tracking off: {dropped} clip(s) left untracked");
            }
            self.models.unload();
            self.models.cancel_download();
            self.live_failed
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
        }
    }

    /// Record why camera `actor`'s live tracking stopped, or clear it
    /// (`None`).
    pub fn set_live_failed(&self, actor: &str, error: Option<String>) {
        let mut failed = self.live_failed.lock().unwrap_or_else(|e| e.into_inner());
        match error {
            Some(error) => failed.insert(actor.to_owned(), error),
            None => failed.remove(actor),
        };
    }

    /// Camera `actor`'s live route `X-Pose-State` for `settings`: `off`
    /// unless the mode is Live, `failed` with why when its live tracking
    /// stopped, `downloading` while a model download runs, or where its
    /// model is (`loading` before it was first asked for, `missing` while
    /// its files are not downloaded), and the error of a failed load.
    pub fn live_state(
        &self,
        settings: &BodyTrackingSection,
        actor: &str,
    ) -> (&'static str, Option<String>) {
        if settings.mode != BodyTrackingMode::Live {
            return ("off", None);
        }
        let failed = self.live_failed.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(error) = failed.get(actor) {
            return ("failed", Some(error.clone()));
        }
        drop(failed);
        if self.models.downloading() {
            return ("downloading", None);
        }
        match self.models.state(settings.model) {
            None => ("loading", None),
            Some(models::ModelState::Failed(e)) => ("failed", Some(e)),
            Some(state) => (state.key(), None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_live_state_names_off_and_where_the_model_is() {
        let pose = BodyTracking::new(
            std::env::temp_dir().join("flighthook-no-models-here"),
            models::fake::Fake::new(1, std::time::Duration::ZERO),
        );
        let mut s = BodyTrackingSection {
            mode: BodyTrackingMode::Live,
            ..BodyTrackingSection::default()
        };
        assert_eq!(
            pose.live_state(&s, "camera.0"),
            ("loading", None),
            "not asked yet"
        );
        for mode in [BodyTrackingMode::Off, BodyTrackingMode::Historical] {
            s.mode = mode;
            assert_eq!(pose.live_state(&s, "camera.0"), ("off", None), "{mode:?}");
        }
        s.mode = BodyTrackingMode::Live;
        pose.models.want(s.model);
        for _ in 0..500 {
            if pose.live_state(&s, "camera.0").0 == "missing" {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(pose.live_state(&s, "camera.0"), ("missing", None));
        pose.models
            .set_state(s.model, models::ModelState::Failed("broken".into()));
        assert_eq!(
            pose.live_state(&s, "camera.0"),
            ("failed", Some("broken".into()))
        );
    }

    #[test]
    fn the_live_state_is_downloading_while_a_download_runs() {
        let dir = std::env::temp_dir().join(format!(
            "flighthook-live-download-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let pose = BodyTracking::new(
            dir.clone(),
            models::fake::Fake::new(1000, std::time::Duration::from_millis(5)),
        );
        let s = BodyTrackingSection {
            mode: BodyTrackingMode::Live,
            ..BodyTrackingSection::default()
        };
        pose.models.download(s.model);
        assert_eq!(pose.live_state(&s, "camera.0"), ("downloading", None));
        pose.models.cancel_download();
        assert_eq!(pose.live_state(&s, "camera.0"), ("loading", None));
        std::thread::sleep(std::time::Duration::from_millis(50));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn turning_it_off_drops_the_waiting_clips_and_lets_the_models_go() {
        let dir = std::env::temp_dir().join(format!(
            "flighthook-off-download-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let pose = BodyTracking::new(
            dir.clone(),
            models::fake::Fake::new(1000, std::time::Duration::from_millis(5)),
        );
        let at = |mode| BodyTrackingSection {
            mode,
            ..BodyTrackingSection::default()
        };
        let m = flighthook::PoseModel::RtmposeM;
        pose.follow(&at(BodyTrackingMode::Live));
        pose.models.set_state(m, models::ModelState::Loading);
        pose.set_live_failed("camera.0", Some("stopped".into()));
        // Live to Historical: the models stay for the clips viewed.
        pose.follow(&at(BodyTrackingMode::Historical));
        assert!(pose.models.state(m).is_some());
        // Off: let go, failures forgotten, a download stopped.
        assert_eq!(
            pose.models.download(m),
            models::DownloadStart::Started,
            "the fake folder is empty"
        );
        pose.follow(&at(BodyTrackingMode::Off));
        assert!(pose.models.state(m).is_none());
        assert!(!pose.models.downloading());
        assert_eq!(
            pose.live_state(&at(BodyTrackingMode::Live), "camera.0"),
            ("loading", None)
        );
        // Following the same mode again does nothing.
        pose.models.set_state(m, models::ModelState::Loading);
        pose.follow(&at(BodyTrackingMode::Off));
        assert!(pose.models.state(m).is_some());
        std::thread::sleep(std::time::Duration::from_millis(50));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cameras_failed_live_tracking_is_its_state_until_cleared() {
        let pose = BodyTracking::new(
            std::env::temp_dir().join("flighthook-no-models-here"),
            models::fake::Fake::new(1, std::time::Duration::ZERO),
        );
        let s = BodyTrackingSection {
            mode: BodyTrackingMode::Live,
            ..BodyTrackingSection::default()
        };
        pose.set_live_failed("camera.1", Some("stopped".into()));
        assert_eq!(
            pose.live_state(&s, "camera.1"),
            ("failed", Some("stopped".into()))
        );
        assert_eq!(pose.live_state(&s, "camera.0"), ("loading", None));
        let off = BodyTrackingSection {
            mode: BodyTrackingMode::Off,
            ..s.clone()
        };
        assert_eq!(pose.live_state(&off, "camera.1"), ("off", None));
        pose.set_live_failed("camera.1", None);
        assert_eq!(pose.live_state(&s, "camera.1"), ("loading", None));
    }
}
