//! Swing cameras for flighthook (beta): the clip store, the camera frame
//! slots and body tracking (alpha).
//!
//! - [`clips`]: the replay clip store, one folder per recorded swing.
//! - [`frames`]: what the camera actors share with the web layer, the newest
//!   still and live frames and the camera list.
//! - [`pose`]: the golfer's keypoints on the cameras' pictures, and the
//!   swing numbers and grades computed from them.

pub mod clips;
pub mod frames;
pub mod pose;

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
