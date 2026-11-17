//! The replay clip store: one folder per recorded swing, shared by every
//! camera (beta).
//!
//! A clip folder is `<clip-id>/` under the clip folder
//! (`[replay] clip_dir`, default `flighthook/clips` in the user's local data
//! folder). Each camera writes `<camera>.avi` and `<camera>.json`
//! (stance's MJPEG AVI and sidecar, sidecar last), and the store
//! writes `shot.json`: the shots the swing belongs to, the launch monitor
//! kind behind each, and every camera's clip. Clip ids start with the UTC
//! time the clip was opened (`20261102-153012-345-1a2b3c4d`), so they sort
//! oldest first; folders not named that way are never touched.
//!
//! The store keeps an index from shot id to clip, prunes to `[replay]
//! keep_shots` clip folders after every clip and at startup, and deletes
//! everything on Clear history. Pruning never deletes a folder a camera is
//! still writing into, nor the clip just finished: those go at a later
//! prune. A folder that can't be deleted (a file still open, on Windows) is
//! retried at the next clip and at the next startup; no failure here panics.
//!
//! When the clip folder setting changes, the store forgets the clips of the
//! old folder (they stay on disk, untouched) and indexes the new one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};

use flighthook::{Handedness, ReplaySection, ShotKey, ShotVideo};

/// The flighthook index file in each clip folder.
pub const SHOT_FILE: &str = "shot.json";

/// Version of [`SHOT_FILE`]'s format.
const SHOT_FILE_VERSION: u32 = 1;

/// The default clip folder: `flighthook/clips` in the user's local data
/// folder (`%LOCALAPPDATA%\flighthook\clips`, `~/.local/share/flighthook/clips`).
pub fn default_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("flighthook")
        .join("clips")
}

/// The clip folder `replay` names: its `clip_dir`, else [`default_dir`].
pub fn clip_dir(replay: &ReplaySection) -> PathBuf {
    match replay.clip_dir.as_deref().map(str::trim) {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => default_dir(),
    }
}

/// A shot a clip belongs to, as `shot.json` records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipShot {
    pub key: ShotKey,
    /// The kind of launch monitor that reported it (`flightscope`,
    /// `garmin_r10`, ...).
    pub device: String,
    /// How long before its trigger the impact was estimated, ms.
    pub impact_lead_ms: u64,
    /// The player's hand when it triggered, for the swing analysis.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hand: Option<Handedness>,
    /// The turn's golfer id when it triggered (raw, as the turn named it),
    /// whose facts the swing analysis and grades read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub golfer: Option<String>,
}

/// `shot.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct ShotFile {
    version: u32,
    shots: Vec<ClipShot>,
    cameras: Vec<ShotVideo>,
}

/// The clips `replay` keeps: its `keep_shots`, at least 1 and at most
/// [`ReplaySection::MAX_KEEP_SHOTS`] whatever a hand-edited file says.
pub fn keep(replay: &ReplaySection) -> u32 {
    replay.keep_shots.clamp(1, ReplaySection::MAX_KEEP_SHOTS)
}

/// A clip's estimated impact for its swing analysis, ns after its first
/// frame: `None` when it falls outside the clip's frames (before the first,
/// or after the last of a clip cut short).
#[must_use]
pub fn clip_impact(sidecar: &stance::sidecar::Sidecar) -> Option<u64> {
    let last = sidecar.frames.last()?.t_ns;
    u64::try_from(sidecar.impact_ns).ok().filter(|t| *t <= last)
}

/// One clip folder.
#[derive(Debug, Clone)]
struct StoredClip {
    id: String,
    dir: PathBuf,
    shots: Vec<ClipShot>,
    /// Cameras whose files are complete on disk.
    cameras: Vec<ShotVideo>,
    /// Cameras writing into the folder now (begun, not finished or
    /// abandoned). Pruning skips the clip while there are any.
    writers: usize,
}

#[derive(Debug, Default)]
struct Inner {
    /// Oldest first.
    clips: Vec<StoredClip>,
    /// Shot id -> clip id.
    index: HashMap<String, String>,
    /// Counted up by every clear: a clip begun before one is not kept.
    generation: u64,
    /// Folders whose deletion failed, retried later.
    retry: Vec<PathBuf>,
}

/// Where a camera writes one clip: the folder, for the store's
/// [`ClipStore::finish`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipSlot {
    pub id: String,
    pub dir: PathBuf,
    generation: u64,
}

/// One camera's finished clip, as [`ClipStore::camera_clip`] finds it.
#[derive(Debug, Clone)]
pub struct CameraClip {
    /// Its folder.
    pub slot: ClipSlot,
    /// Its AVI, the sidecar beside it.
    pub avi: PathBuf,
    /// The shots the clip belongs to.
    pub shots: Vec<ClipShot>,
    /// Its listing (`pose` and `swing` say what is done).
    pub video: ShotVideo,
}

/// What [`ClipStore::finish`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Finished {
    /// The clip is kept and listed.
    pub kept: bool,
    /// Shots whose clips were deleted to keep the newest `keep_shots`.
    pub removed: Vec<ShotKey>,
}

/// The clip folders on disk and the shots they belong to.
#[derive(Debug, Default)]
pub struct ClipStore {
    inner: Mutex<Inner>,
}

impl ClipStore {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Read the clip folders under `dir` (at startup): every folder named
    /// like a clip id is indexed from its `shot.json`, and all but the
    /// newest `keep` are deleted, along with those without a `shot.json`
    /// (no camera finished writing them). Returns the shots whose clips
    /// were deleted.
    pub fn load(&self, dir: &Path, keep: u32) -> Vec<ShotKey> {
        let mut found: Vec<(String, PathBuf)> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                (is_clip_id(&name) && e.path().is_dir()).then(|| (name, e.path()))
            })
            .collect();
        found.sort();
        let mut inner = self.lock();
        retry_deletes(&mut inner);
        let mut removed = Vec::new();
        for (id, path) in found {
            if inner.clips.iter().any(|c| c.id == id) {
                continue;
            }
            match read_shot_file(&path) {
                Some(file) => {
                    for shot in &file.shots {
                        inner.index.insert(shot.key.shot_id.clone(), id.clone());
                    }
                    inner.clips.push(StoredClip {
                        id,
                        dir: path,
                        shots: file.shots,
                        cameras: file.cameras,
                        writers: 0,
                    });
                }
                None => delete_dir(&mut inner, path),
            }
        }
        inner.clips.sort_by(|a, b| a.id.cmp(&b.id));
        removed.extend(prune(&mut inner, keep, None));
        removed
    }

    /// Switch to clip folder `dir` (the setting changed): forget the clips
    /// of any other folder, leaving their files as they are, then index
    /// `dir` as [`ClipStore::load`] does. A clip a camera is still writing
    /// stays indexed until it finishes. Returns the shots whose clips are no
    /// longer served: those forgotten and those pruned.
    pub fn reindex(&self, dir: &Path, keep: u32) -> Vec<ShotKey> {
        let mut forgotten = Vec::new();
        {
            let mut inner = self.lock();
            let elsewhere: Vec<String> = inner
                .clips
                .iter()
                .filter(|c| c.writers == 0 && c.dir.parent() != Some(dir))
                .map(|c| c.id.clone())
                .collect();
            for id in elsewhere {
                if let Some(pos) = inner.clips.iter().position(|c| c.id == id) {
                    let clip = inner.clips.remove(pos);
                    inner.index.retain(|_, clip_id| *clip_id != id);
                    forgotten.extend(clip.shots.into_iter().map(|s| s.key));
                }
            }
        }
        forgotten.extend(self.load(dir, keep));
        forgotten
    }

    /// The store's generation, counted up by every Clear history. A camera
    /// reads it when a shot triggers and hands it to [`ClipStore::begin`].
    pub fn generation(&self) -> u64 {
        self.lock().generation
    }

    /// Open (or join) the clip folder for a swing with these shots, under
    /// `dir`: the folder of a clip that already holds one of them (another
    /// camera's clip of the same swing), else a new one. The folder is
    /// created. `generation` is the store's [`generation`](Self::generation)
    /// when the shots triggered: a slot for shots a Clear history has
    /// removed since is returned without a folder and is never
    /// [current](Self::is_current).
    ///
    /// # Errors
    ///
    /// When the folder can't be created.
    pub fn begin(
        &self,
        dir: &Path,
        shots: &[ClipShot],
        generation: u64,
    ) -> std::io::Result<ClipSlot> {
        let mut inner = self.lock();
        if generation != inner.generation {
            return Ok(ClipSlot {
                id: String::new(),
                dir: PathBuf::new(),
                generation,
            });
        }
        let existing = shots
            .iter()
            .find_map(|s| inner.index.get(&s.key.shot_id).cloned());
        let (id, path) = match existing
            .and_then(|id| inner.clips.iter().find(|c| c.id == id))
            .map(|c| (c.id.clone(), c.dir.clone()))
        {
            Some(found) => found,
            None => {
                let id = new_clip_id();
                let path = dir.join(&id);
                inner.clips.push(StoredClip {
                    id: id.clone(),
                    dir: path.clone(),
                    shots: Vec::new(),
                    cameras: Vec::new(),
                    writers: 0,
                });
                (id, path)
            }
        };
        if let Some(clip) = inner.clips.iter_mut().find(|c| c.id == id) {
            clip.writers += 1;
            for shot in shots {
                if !clip.shots.iter().any(|s| s.key.shot_id == shot.key.shot_id) {
                    clip.shots.push(shot.clone());
                }
            }
        }
        for shot in shots {
            inner.index.insert(shot.key.shot_id.clone(), id.clone());
        }
        drop(inner);
        std::fs::create_dir_all(&path)?;
        Ok(ClipSlot {
            id,
            dir: path,
            generation,
        })
    }

    /// Whether `slot` can still be kept: no Clear history since it was
    /// begun. A camera checks before writing, so a clip cleared meanwhile
    /// costs no write.
    pub fn is_current(&self, slot: &ClipSlot) -> bool {
        let inner = self.lock();
        slot.generation == inner.generation && inner.clips.iter().any(|c| c.id == slot.id)
    }

    /// Record a camera's finished clip in `slot` (its files are written),
    /// rewrite `shot.json`, then delete all but the newest `keep` clip
    /// folders (at least 1), skipping folders still being written and this
    /// clip. A slot begun before a Clear history is not kept: its folder is
    /// deleted.
    pub fn finish(&self, slot: &ClipSlot, video: ShotVideo, keep: u32) -> Finished {
        let mut inner = self.lock();
        retry_deletes(&mut inner);
        let current = slot.generation == inner.generation;
        let Some(clip) = inner
            .clips
            .iter_mut()
            .find(|c| c.id == slot.id)
            .filter(|_| current)
        else {
            if !inner.clips.iter().any(|c| c.dir == slot.dir) {
                delete_dir(&mut inner, slot.dir.clone());
            }
            return Finished::default();
        };
        clip.writers = clip.writers.saturating_sub(1);
        clip.cameras.retain(|c| c.camera != video.camera);
        clip.cameras.push(video);
        clip.cameras.sort_by(|a, b| a.camera.cmp(&b.camera));
        let file = ShotFile {
            version: SHOT_FILE_VERSION,
            shots: clip.shots.clone(),
            cameras: clip.cameras.clone(),
        };
        if let Err(e) = write_shot_file(&clip.dir, &file) {
            tracing::warn!("clips: writing {}: {e}", clip.dir.join(SHOT_FILE).display());
        }
        let removed = prune(&mut inner, keep, Some(&slot.id));
        Finished {
            kept: true,
            removed,
        }
    }

    /// Forget a slot whose camera wrote nothing (the write failed, or the
    /// clip was cleared before it was written): its folder goes if no other
    /// camera finished a clip in it or is writing one. A slot begun before a
    /// Clear history leaves no folder behind.
    pub fn abandon(&self, slot: &ClipSlot) {
        let mut inner = self.lock();
        if slot.generation != inner.generation {
            if !slot.dir.as_os_str().is_empty() && !inner.clips.iter().any(|c| c.dir == slot.dir) {
                delete_dir(&mut inner, slot.dir.clone());
            }
            return;
        }
        let Some(clip) = inner.clips.iter_mut().find(|c| c.id == slot.id) else {
            return;
        };
        clip.writers = clip.writers.saturating_sub(1);
        if clip.cameras.is_empty() && clip.writers == 0 {
            remove_clip(&mut inner, &slot.id);
        }
    }

    /// Delete every clip (Clear history). A clip being written now is not
    /// kept when its camera finishes. Returns the shots that had clips.
    pub fn clear(&self) -> Vec<ShotKey> {
        let mut inner = self.lock();
        inner.generation += 1;
        retry_deletes(&mut inner);
        let ids: Vec<String> = inner.clips.iter().map(|c| c.id.clone()).collect();
        let mut removed = Vec::new();
        for id in ids {
            removed.extend(remove_clip(&mut inner, &id));
        }
        removed
    }

    /// Camera `camera`'s finished clip of shot `shot_id`: where it is, the
    /// clip's shots and its listing.
    pub fn camera_clip(&self, shot_id: &str, camera: &str) -> Option<CameraClip> {
        let inner = self.lock();
        let clip = clip_of(&inner, shot_id)?;
        let video = clip.cameras.iter().find(|c| c.camera == camera)?.clone();
        Some(CameraClip {
            slot: ClipSlot {
                id: clip.id.clone(),
                dir: clip.dir.clone(),
                generation: inner.generation,
            },
            avi: clip.dir.join(format!("{camera}.avi")),
            shots: clip.shots.clone(),
            video,
        })
    }

    /// The finished camera clips of shot `shot_id`, by camera.
    pub fn videos(&self, shot_id: &str) -> Vec<ShotVideo> {
        let inner = self.lock();
        clip_of(&inner, shot_id).map_or_else(Vec::new, |c| c.cameras.clone())
    }

    /// The file `<camera>.<ext>` of shot `shot_id`'s clip, when that
    /// camera's clip is finished (`ext` is `avi` or `json`), or its body
    /// tracking is (`ext` is `pose.json`), or its swing analysis is (`ext`
    /// is `swing.json`).
    pub fn file(&self, shot_id: &str, camera: &str, ext: &str) -> Option<PathBuf> {
        let done: fn(&ShotVideo) -> bool = match ext {
            "avi" | "json" => |_| true,
            "pose.json" => |c| c.pose,
            "swing.json" => |c| c.swing,
            _ => return None,
        };
        let inner = self.lock();
        let clip = clip_of(&inner, shot_id)?;
        clip.cameras
            .iter()
            .any(|c| c.camera == camera && done(c))
            .then(|| clip.dir.join(format!("{camera}.{ext}")))
    }

    /// Record that body tracking of `camera`'s clip in `slot` is done (its
    /// `<camera>.pose.json` is written) with the camera turned by
    /// `rotation` clockwise degrees, and rewrite `shot.json` with the
    /// rotation and [`stance::VERSION`]. False when the clip is gone
    /// (pruned, or a Clear history since it was begun): then the file is
    /// not served, and the caller deletes it if its folder is still there.
    pub fn set_pose(&self, slot: &ClipSlot, camera: &str, rotation: u16) -> bool {
        self.mark(slot, camera, |video| {
            video.pose = true;
            video.rotation = rotation;
            video.analyzed_with = Some(stance::VERSION.to_owned());
        })
    }

    /// Forget the body tracking and swing analysis of `camera`'s clip in
    /// `slot`: delete its `<camera>.pose.json` and `<camera>.swing.json`,
    /// clear what its listing says about them, and rewrite `shot.json`.
    /// False when the clip is gone.
    pub fn clear_analysis(&self, slot: &ClipSlot, camera: &str) -> bool {
        let dir = slot.dir.clone();
        let camera_files = camera.to_owned();
        self.mark(slot, camera, move |video| {
            for ext in ["pose.json", "swing.json"] {
                let path = dir.join(format!("{camera_files}.{ext}"));
                match std::fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => tracing::warn!("clips: deleting {}: {e}", path.display()),
                }
            }
            video.pose = false;
            video.swing = false;
            video.rotation = 0;
            video.analyzed_with = None;
        })
    }

    /// Record that the swing analysis of `camera`'s clip in `slot` is done
    /// (its `<camera>.swing.json` is written), as [`ClipStore::set_pose`].
    pub fn set_swing(&self, slot: &ClipSlot, camera: &str) -> bool {
        self.mark(slot, camera, |video| video.swing = true)
    }

    /// Apply `set` to `camera`'s clip in `slot` and rewrite `shot.json`.
    /// False when the clip is gone.
    fn mark(&self, slot: &ClipSlot, camera: &str, set: impl FnOnce(&mut ShotVideo)) -> bool {
        let mut inner = self.lock();
        if slot.generation != inner.generation {
            return false;
        }
        let Some(clip) = inner.clips.iter_mut().find(|c| c.id == slot.id) else {
            return false;
        };
        let Some(video) = clip.cameras.iter_mut().find(|c| c.camera == camera) else {
            return false;
        };
        set(video);
        let file = ShotFile {
            version: SHOT_FILE_VERSION,
            shots: clip.shots.clone(),
            cameras: clip.cameras.clone(),
        };
        if let Err(e) = write_shot_file(&clip.dir, &file) {
            tracing::warn!("clips: writing {}: {e}", clip.dir.join(SHOT_FILE).display());
        }
        true
    }

    /// Number of clip folders held.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.lock().clips.len()
    }
}

fn clip_of<'a>(inner: &'a Inner, shot_id: &str) -> Option<&'a StoredClip> {
    let id = inner.index.get(shot_id)?;
    inner.clips.iter().find(|c| c.id == *id)
}

/// Delete all but the newest `keep` clips (at least 1), except clips a
/// camera is still writing into and clip `spare` (the one just finished):
/// those stay over the count until a later prune. Returns the deleted
/// clips' shots.
fn prune(inner: &mut Inner, keep: u32, spare: Option<&str>) -> Vec<ShotKey> {
    let keep = usize::try_from(keep.max(1)).unwrap_or(usize::MAX);
    let excess = inner.clips.len().saturating_sub(keep);
    let doomed: Vec<String> = inner.clips[..excess]
        .iter()
        .filter(|c| c.writers == 0 && Some(c.id.as_str()) != spare)
        .map(|c| c.id.clone())
        .collect();
    let mut removed = Vec::new();
    for id in doomed {
        removed.extend(remove_clip(inner, &id));
    }
    removed
}

/// Drop clip `id` from the store and delete its folder. Returns its shots.
fn remove_clip(inner: &mut Inner, id: &str) -> Vec<ShotKey> {
    let Some(pos) = inner.clips.iter().position(|c| c.id == id) else {
        return Vec::new();
    };
    let clip = inner.clips.remove(pos);
    inner.index.retain(|_, clip_id| clip_id != id);
    delete_dir(inner, clip.dir);
    clip.shots.into_iter().map(|s| s.key).collect()
}

/// Delete a clip folder, or queue it for a retry.
fn delete_dir(inner: &mut Inner, dir: PathBuf) {
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            tracing::debug!(
                "clips: deleting {} failed ({e}), retrying later",
                dir.display()
            );
            if !inner.retry.contains(&dir) {
                inner.retry.push(dir);
            }
        }
    }
}

/// Try the deletions that failed before again.
fn retry_deletes(inner: &mut Inner) {
    let pending = std::mem::take(&mut inner.retry);
    for dir in pending {
        delete_dir(inner, dir);
    }
}

/// A new clip id: the UTC time to the millisecond and 8 random hex digits.
fn new_clip_id() -> String {
    let now = chrono::Utc::now().format("%Y%m%d-%H%M%S-%3f");
    let random = uuid::Uuid::new_v4().simple().to_string();
    format!("{now}-{}", &random[..8])
}

/// Whether `name` is a clip id ([`new_clip_id`]): only such folders are
/// read or deleted.
fn is_clip_id(name: &str) -> bool {
    let b = name.as_bytes();
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    let hex = |r: std::ops::Range<usize>| {
        b[r].iter()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c))
    };
    b.len() == 28
        && digits(0..8)
        && b[8] == b'-'
        && digits(9..15)
        && b[15] == b'-'
        && digits(16..19)
        && b[19] == b'-'
        && hex(20..28)
}

fn read_shot_file(dir: &Path) -> Option<ShotFile> {
    let text = std::fs::read_to_string(dir.join(SHOT_FILE)).ok()?;
    serde_json::from_str::<ShotFile>(&text)
        .ok()
        .filter(|f| f.version >= 1)
}

/// Write `shot.json` through a temporary file, so a reader never sees half
/// of one.
fn write_shot_file(dir: &Path, file: &ShotFile) -> std::io::Result<()> {
    let json = serde_json::to_vec_pretty(file).map_err(std::io::Error::other)?;
    let part = dir.join(format!("{SHOT_FILE}.part"));
    std::fs::write(&part, json)?;
    std::fs::rename(&part, dir.join(SHOT_FILE))
}

#[cfg(test)]
mod tests;
