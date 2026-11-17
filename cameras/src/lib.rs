//! Swing cameras for flighthook (beta): the clip store, the camera frame
//! slots and body tracking (alpha).
//!
//! [`Cameras`] is everything the cameras share, one per process. The host
//! builds it over its live config and its [`Host`], and hears from it
//! through an [`Out`].
//!
//! - [`clips`]: the replay clip store, one folder per recorded swing.
//! - [`frames`]: what the cameras share with the web layer, the newest
//!   still and live frames and the camera list.
//! - [`pose`]: the golfer's keypoints on the cameras' pictures, and the
//!   swing numbers and grades computed from them.
//! - [`settings`]: what the cameras read from the live config.

pub mod clips;
pub mod frames;
pub mod pose;
pub mod settings;

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use flighthook::{ActorStatus, CameraPosition, FlighthookConfig, GolferTurn, ShotKey};

use clips::ClipStore;
use frames::Frames;
use pose::BodyTracking;
use settings::Settings;

/// Everything the cameras share; one per process.
pub struct Cameras {
    /// The replay clips on disk.
    pub clips: ClipStore,
    /// The cameras' newest frames and the camera list.
    pub frames: Frames,
    /// Body tracking's models and recorded-clip queue (alpha).
    pub pose: BodyTracking,
    /// The live config, read at every use.
    settings: Settings,
    host: Arc<dyn Host>,
}

impl Cameras {
    /// The cameras over the host's live `config`, with body tracking
    /// `pose`.
    pub fn new(
        config: Arc<RwLock<FlighthookConfig>>,
        host: Arc<dyn Host>,
        pose: BodyTracking,
    ) -> Arc<Self> {
        Arc::new(Self {
            clips: ClipStore::default(),
            frames: Frames::default(),
            pose,
            settings: Settings::new(config),
            host,
        })
    }

    /// The host's turn now.
    pub fn turn(&self) -> GolferTurn {
        self.host.turn()
    }

    /// The live config the cameras read.
    pub fn settings(&self) -> &Settings {
        &self.settings
    }
}

/// What the cameras need from the host.
pub trait Host: Send + Sync + 'static {
    /// The turn now, read at each trigger (the clip's hand and golfer) and
    /// by the live view (the posture's hand).
    fn turn(&self) -> GolferTurn;
}

/// What a camera tells the host.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The camera's status and card rows.
    Status {
        status: ActorStatus,
        telemetry: HashMap<String, String>,
    },
    /// A warning for the user.
    Alert(String),
    /// A clip of these shots was saved.
    Video {
        keys: Vec<ShotKey>,
        position: CameraPosition,
        duration_ms: u64,
        fps: f64,
        trigger_offset_ms: i64,
        impact_offset_ms: i64,
        complete: bool,
        dropped: u32,
    },
    /// The clips of these shots were deleted.
    VideoRemoved { keys: Vec<ShotKey> },
    /// Body tracking of the camera's clip of these shots is done (alpha).
    Pose { keys: Vec<ShotKey>, rotation: u16 },
    /// The swing analysis of the camera's clip of these shots is done
    /// (alpha).
    Swing { keys: Vec<ShotKey> },
}

/// Where a camera's events go, as that camera's.
pub type Out = Arc<dyn Fn(Event) + Send + Sync>;

/// Removes the directory on drop, so a failing assertion doesn't leak it.
#[cfg(any(test, feature = "testing"))]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct TempDir(pub(crate) std::path::PathBuf);

#[cfg(any(test, feature = "testing"))]
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// How long a test waits for a thread's answer before it gives up.
#[cfg(any(test, feature = "testing"))]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const GIVE_UP: std::time::Duration = std::time::Duration::from_secs(60);

/// The crate's test bench: [`Cameras`] over a scratch folder and a config
/// of its own, and an [`Out`] that keeps every event.
#[cfg(test)]
pub(crate) mod bench {
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex, RwLock};

    use flighthook::{FlighthookConfig, GolferTurn};

    use super::{Cameras, Event, Host, Out, TempDir};

    /// A host whose turn is always the start turn: no golfer, no club,
    /// right-handed, full.
    struct FakeHost;

    impl Host for FakeHost {
        fn turn(&self) -> GolferTurn {
            GolferTurn::new(
                None,
                None,
                flighthook::Handedness::Right,
                flighthook::Mode::Full,
            )
        }
    }

    /// A bench: the cameras, their config, and the scratch folder (gone
    /// on drop).
    pub struct Bench {
        pub dir: TempDir,
        pub config: Arc<RwLock<FlighthookConfig>>,
        pub cameras: Arc<Cameras>,
    }

    impl Bench {
        /// Change the config.
        pub fn update(&self, f: impl FnOnce(&mut FlighthookConfig)) {
            f(&mut self.config.write().unwrap());
        }
    }

    /// A bench over `config`, its models in an empty folder of its own
    /// with a stand-in source (`models::fake`).
    pub fn bench(config: FlighthookConfig) -> Bench {
        let dir = TempDir(std::env::temp_dir().join(format!(
            "flighthook-cameras-bench-{}",
            uuid::Uuid::new_v4().simple()
        )));
        std::fs::create_dir_all(&dir.0).expect("dir");
        let config = Arc::new(RwLock::new(config));
        let pose = crate::pose::BodyTracking::new(
            dir.0.join("models"),
            crate::pose::models::fake::Fake::new(40, std::time::Duration::from_millis(10)),
        );
        let cameras = Cameras::new(Arc::clone(&config), Arc::new(FakeHost), pose);
        Bench {
            dir,
            config,
            cameras,
        }
    }

    /// An [`Out`] and the events sent on it, in order.
    pub fn out() -> (Out, mpsc::Receiver<Event>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let out: Out = Arc::new(move |e| {
            let _ = tx.lock().unwrap().send(e);
        });
        (out, rx)
    }
}
