//! What the camera actors share with the web layer (beta): each actor's
//! latest frame, for the device card's still, and the camera it holds; and
//! the camera list, enumerated at most every few seconds.
//!
//! Frames never travel on the bus: an actor puts its newest JPEG here at
//! most twice a second, and `GET /api/cameras/{actor}/still` reads it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use flighthook::{CameraListEntry, CameraListResponse};
use stance::CameraInfo;

/// How long an enumeration is reused.
pub const LIST_CACHE: Duration = Duration::from_secs(3);

/// Whether this platform has a camera backend. The test picture works
/// everywhere.
pub fn supported() -> bool {
    stance::camera::SUPPORTED
}

/// One camera actor's slot.
#[derive(Debug, Default, Clone)]
struct Slot {
    /// The camera id it has open.
    camera: Option<String>,
    /// Its newest frame, as the camera sent it.
    still: Option<Arc<[u8]>>,
}

/// An enumeration: when it ran, the cameras it found, and why it failed.
type Listing = (Instant, Vec<CameraInfo>, Option<String>);

/// The camera actors' slots and the cached camera list.
#[derive(Debug, Default)]
pub struct Cameras {
    slots: RwLock<HashMap<String, Slot>>,
    list: Mutex<Option<Listing>>,
}

impl Cameras {
    /// Record that actor `actor` has camera `camera` open, or none.
    pub fn set_open(&self, actor: &str, camera: Option<&str>) {
        let mut slots = self.slots.write().unwrap_or_else(|e| e.into_inner());
        slots.entry(actor.to_owned()).or_default().camera = camera.map(str::to_owned);
    }

    /// Put actor `actor`'s newest frame.
    pub fn set_still(&self, actor: &str, jpeg: Arc<[u8]>) {
        let mut slots = self.slots.write().unwrap_or_else(|e| e.into_inner());
        slots.entry(actor.to_owned()).or_default().still = Some(jpeg);
    }

    /// Actor `actor`'s newest frame.
    pub fn still(&self, actor: &str) -> Option<Arc<[u8]>> {
        let slots = self.slots.read().unwrap_or_else(|e| e.into_inner());
        slots.get(actor).and_then(|s| s.still.clone())
    }

    /// Forget actor `actor` (it stopped).
    pub fn remove(&self, actor: &str) {
        self.slots
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(actor);
    }

    /// The actor holding camera `camera`, if one does.
    pub fn holder(&self, camera: &str) -> Option<String> {
        let slots = self.slots.read().unwrap_or_else(|e| e.into_inner());
        let mut holders: Vec<&String> = slots
            .iter()
            .filter(|(_, s)| s.camera.as_deref() == Some(camera))
            .map(|(actor, _)| actor)
            .collect();
        holders.sort();
        holders.first().map(|a| (*a).clone())
    }

    /// The camera list (`GET /api/cameras`), from an enumeration at most
    /// [`LIST_CACHE`] old; blocks while it enumerates. stance never
    /// opens a camera this process has open (an actor recording from it) to
    /// list it, on any platform: its entry is the one read when it was
    /// opened.
    pub fn list(&self) -> CameraListResponse {
        self.list_with(stance::list)
    }

    /// [`Cameras::list`] with `enumerate` in place of the platform's.
    pub fn list_with(
        &self,
        enumerate: impl FnOnce() -> stance::Result<Vec<CameraInfo>>,
    ) -> CameraListResponse {
        let (infos, error) = {
            let mut cache = self.list.lock().unwrap_or_else(|e| e.into_inner());
            match cache.as_ref() {
                Some((at, infos, error)) if at.elapsed() < LIST_CACHE => {
                    (infos.clone(), error.clone())
                }
                _ => {
                    let (infos, error) = match enumerate() {
                        Ok(infos) => (infos, None),
                        Err(stance::Error::Unsupported) => (Vec::new(), None),
                        Err(e) => (Vec::new(), Some(e.to_string())),
                    };
                    *cache = Some((Instant::now(), infos.clone(), error.clone()));
                    (infos, error)
                }
            }
        };
        if let Some(error) = error {
            tracing::warn!("cameras: listing failed: {error}");
        }
        let mut cameras: Vec<CameraListEntry> = infos.iter().map(|i| self.entry(i)).collect();
        let mut pattern = self.entry(&stance::TestPattern::camera_info());
        // Every test-pattern camera renders its own picture.
        pattern.in_use_by = None;
        cameras.push(pattern);
        CameraListResponse {
            supported: supported(),
            cameras,
        }
    }

    fn entry(&self, info: &CameraInfo) -> CameraListEntry {
        let mut modes: Vec<&stance::Format> = info.mjpeg_formats().collect();
        modes.sort_by(|a, b| b.fps().total_cmp(&a.fps()).then(b.area().cmp(&a.area())));
        let mut names: Vec<String> = modes
            .iter()
            .map(|f| {
                stance::FormatRequest::Mode {
                    width: f.width,
                    height: f.height,
                    fps: f.fps(),
                }
                .to_string()
            })
            .collect();
        names.dedup();
        CameraListEntry {
            id: info.id.clone(),
            name: info.name.clone(),
            usb: info.usb.map(|u| u.to_string()),
            modes: names,
            in_use_by: self.holder(&info.id),
            error: info.probe_error.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_hold_the_still_and_the_camera() {
        let cams = Cameras::default();
        assert!(cams.still("camera.0").is_none());
        cams.set_open("camera.0", Some("usb-1"));
        cams.set_still("camera.0", Arc::from(vec![0xFF, 0xD8].as_slice()));
        assert_eq!(cams.still("camera.0").as_deref(), Some(&[0xFF, 0xD8][..]));
        assert_eq!(cams.holder("usb-1").as_deref(), Some("camera.0"));
        assert_eq!(cams.holder("usb-2"), None);
        cams.remove("camera.0");
        assert!(cams.still("camera.0").is_none());
        assert_eq!(cams.holder("usb-1"), None);
    }

    fn info(id: &str, formats: Vec<stance::Format>) -> CameraInfo {
        CameraInfo {
            id: id.into(),
            name: "HD Pro Webcam C920".into(),
            usb: Some(stance::camera::UsbId {
                vid: 0x046d,
                pid: 0x082d,
            }),
            formats,
            controls: Vec::new(),
            exposure_unit: None,
            probe_error: None,
        }
    }

    #[test]
    fn the_list_names_modes_fastest_first_marks_held_cameras_and_ends_with_the_test_pattern() {
        use stance::Format;
        let cams = Cameras::default();
        cams.set_open("camera.1", Some("usb-1"));
        let yuyv = Format {
            fourcc: "YUY2".into(),
            ..Format::mjpeg(640, 480, 30)
        };
        let list = cams.list_with(|| {
            Ok(vec![
                info(
                    "usb-1",
                    vec![
                        Format::mjpeg(1920, 1080, 30),
                        yuyv,
                        Format::mjpeg(1280, 720, 60),
                        Format::mjpeg(640, 480, 60),
                    ],
                ),
                info("usb-2", Vec::new()),
            ])
        });
        assert_eq!(list.supported, supported());
        assert_eq!(list.cameras.len(), 3);
        let first = &list.cameras[0];
        assert_eq!(first.modes, ["1280x720@60", "640x480@60", "1920x1080@30"]);
        assert_eq!(first.usb.as_deref(), Some("046d:082d"));
        assert_eq!(first.in_use_by.as_deref(), Some("camera.1"));
        assert_eq!(list.cameras[1].in_use_by, None);
        let last = list.cameras.last().expect("test pattern");
        assert_eq!(last.id, flighthook::CameraSection::TEST_PATTERN);
        assert_eq!(last.id, stance::TEST_PATTERN_ID);
        assert_eq!(last.name, stance::TEST_PATTERN_NAME);
        assert_eq!(last.modes, ["1280x720@60", "640x360@30"]);
        // Reused within the cache time: no second enumeration.
        let again = cams.list_with(|| panic!("enumerated twice"));
        assert_eq!(again.cameras.len(), 3);
    }
}
