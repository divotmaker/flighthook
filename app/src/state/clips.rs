//! The replay clip store: one folder per recorded swing, shared by every
//! camera actor (beta).
//!
//! A clip folder is `<clip-id>/` under the clip folder
//! (`[replay] clip_dir`, default `flighthook/clips` in the user's local data
//! folder). Each camera writes `<camera-actor>.avi` and `<camera-actor>.json`
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

use flighthook::{ReplaySection, ShotKey, ShotVideo};

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

    /// The finished camera clips of shot `shot_id`, by camera.
    pub fn videos(&self, shot_id: &str) -> Vec<ShotVideo> {
        let inner = self.lock();
        clip_of(&inner, shot_id).map_or_else(Vec::new, |c| c.cameras.clone())
    }

    /// The file `<camera>.<ext>` of shot `shot_id`'s clip, when that
    /// camera's clip is finished (`ext` is `avi` or `json`).
    pub fn file(&self, shot_id: &str, camera: &str, ext: &str) -> Option<PathBuf> {
        if !matches!(ext, "avi" | "json") {
            return None;
        }
        let inner = self.lock();
        let clip = clip_of(&inner, shot_id)?;
        clip.cameras
            .iter()
            .any(|c| c.camera == camera)
            .then(|| clip.dir.join(format!("{camera}.{ext}")))
    }

    /// Number of clip folders held.
    #[cfg(test)]
    pub fn len(&self) -> usize {
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
mod tests {
    use super::*;
    use crate::actors::web::tests::TempDir;
    use flighthook::CameraPosition;

    fn temp() -> TempDir {
        TempDir(std::env::temp_dir().join(format!(
            "flighthook-clips-test-{}",
            uuid::Uuid::new_v4().simple()
        )))
    }

    fn key(id: &str) -> ShotKey {
        ShotKey {
            shot_id: id.into(),
            shot_number: 1,
        }
    }

    fn shot(id: &str) -> ClipShot {
        ClipShot {
            key: key(id),
            device: "mock".into(),
            impact_lead_ms: 0,
        }
    }

    fn video(camera: &str) -> ShotVideo {
        ShotVideo {
            camera: camera.into(),
            position: CameraPosition::GolferFront,
            duration_ms: 5500,
            fps: 30.0,
            trigger_offset_ms: 4000,
            impact_offset_ms: 4000,
            complete: true,
            dropped: 0,
        }
    }

    /// Begin, write a camera's files, and finish a clip of `ids`.
    fn record(store: &ClipStore, dir: &Path, ids: &[&str], camera: &str, keep: u32) -> Finished {
        let shots: Vec<ClipShot> = ids.iter().map(|id| shot(id)).collect();
        let slot = store.begin(dir, &shots, store.generation()).expect("begin");
        std::fs::write(slot.dir.join(format!("{camera}.avi")), b"RIFF").expect("avi");
        std::fs::write(slot.dir.join(format!("{camera}.json")), b"{}").expect("json");
        store.finish(&slot, video(camera), keep)
    }

    #[test]
    fn clip_ids_sort_by_time_and_are_recognised() {
        let a = new_clip_id();
        assert!(is_clip_id(&a), "{a}");
        assert!(!is_clip_id("photos"));
        assert!(!is_clip_id("20261102-153012-345-1a2b3c4G"));
        assert!(!is_clip_id(&format!("{a}x")));
    }

    #[test]
    fn cameras_of_one_swing_share_a_folder_and_every_key_finds_it() {
        let tmp = temp();
        let store = ClipStore::default();
        assert!(record(&store, &tmp.0, &["a"], "camera.0", 20).kept);
        // The second camera merged a second monitor's trigger into its clip.
        assert!(record(&store, &tmp.0, &["a", "b"], "camera.1", 20).kept);
        assert_eq!(store.len(), 1);
        let cams: Vec<String> = store.videos("b").into_iter().map(|v| v.camera).collect();
        assert_eq!(cams, ["camera.0", "camera.1"]);
        assert_eq!(store.videos("a").len(), 2);
        let avi = store.file("b", "camera.0", "avi").expect("served");
        assert!(avi.exists());
        assert!(store.file("b", "camera.9", "avi").is_none());
        assert!(store.file("b", "camera.0", "txt").is_none());
        let file = read_shot_file(avi.parent().unwrap()).expect("shot.json");
        assert_eq!(file.shots.len(), 2);
        assert_eq!(file.cameras.len(), 2);
    }

    #[test]
    fn a_camera_still_writing_is_not_served() {
        let tmp = temp();
        let store = ClipStore::default();
        let slot = store
            .begin(&tmp.0, &[shot("a")], store.generation())
            .expect("begin");
        assert!(slot.dir.is_dir());
        assert!(store.videos("a").is_empty());
        assert!(store.file("a", "camera.0", "avi").is_none());
    }

    #[test]
    fn prunes_to_keep_oldest_first_and_reports_the_shots() {
        let tmp = temp();
        let store = ClipStore::default();
        for (i, id) in ["a", "b", "c"].iter().enumerate() {
            let done = record(&store, &tmp.0, &[id], "camera.0", 2);
            assert!(done.kept);
            if i < 2 {
                assert!(done.removed.is_empty());
            } else {
                assert_eq!(done.removed, [key("a")]);
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(store.len(), 2);
        assert!(store.videos("a").is_empty());
        assert_eq!(store.videos("c").len(), 1);
        let folders = std::fs::read_dir(&tmp.0).expect("dir").count();
        assert_eq!(folders, 2);
    }

    #[test]
    fn startup_reads_the_folders_prunes_and_ignores_foreign_ones() {
        let tmp = temp();
        let store = ClipStore::default();
        for id in ["a", "b", "c"] {
            record(&store, &tmp.0, &[id], "camera.0", 20);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        // A folder with no shot.json (a crash mid-write) and one not ours.
        std::fs::create_dir_all(tmp.0.join(new_clip_id())).expect("orphan");
        std::fs::create_dir_all(tmp.0.join("holiday photos")).expect("foreign");

        let fresh = ClipStore::default();
        let removed = fresh.load(&tmp.0, 2);
        assert_eq!(removed, [key("a")]);
        assert_eq!(fresh.len(), 2);
        assert_eq!(fresh.videos("c")[0].camera, "camera.0");
        assert!(tmp.0.join("holiday photos").is_dir());
        let ours = std::fs::read_dir(&tmp.0)
            .expect("dir")
            .flatten()
            .filter(|e| is_clip_id(&e.file_name().to_string_lossy()))
            .count();
        assert_eq!(ours, 2);
    }

    #[test]
    fn clear_deletes_everything_and_drops_clips_still_being_written() {
        let tmp = temp();
        let store = ClipStore::default();
        record(&store, &tmp.0, &["a"], "camera.0", 20);
        let slot = store
            .begin(&tmp.0, &[shot("b")], store.generation())
            .expect("begin");
        let mut removed = store.clear();
        removed.sort_by(|x, y| x.shot_id.cmp(&y.shot_id));
        assert_eq!(removed, [key("a"), key("b")]);
        assert_eq!(store.len(), 0);
        // The camera writing "b" finishes after the clear: not kept.
        std::fs::create_dir_all(&slot.dir).expect("rewrite");
        std::fs::write(slot.dir.join("camera.0.avi"), b"RIFF").expect("avi");
        assert!(!store.finish(&slot, video("camera.0"), 20).kept);
        assert!(!slot.dir.exists());
        assert!(store.videos("b").is_empty());
        assert_eq!(std::fs::read_dir(&tmp.0).expect("dir").count(), 0);
    }

    #[test]
    fn an_abandoned_slot_leaves_no_folder() {
        let tmp = temp();
        let store = ClipStore::default();
        let slot = store
            .begin(&tmp.0, &[shot("a")], store.generation())
            .expect("begin");
        store.abandon(&slot);
        assert_eq!(store.len(), 0);
        assert!(!slot.dir.exists());
    }

    #[test]
    fn pruning_skips_a_clip_another_camera_is_still_writing() {
        let tmp = temp();
        let store = ClipStore::default();
        // Camera 1 begins swing A and is still writing when camera 0
        // finishes swing B with keep 1.
        let a = store.begin(&tmp.0, &[shot("a")], 0).expect("begin a");
        std::thread::sleep(std::time::Duration::from_millis(2));
        let done = record(&store, &tmp.0, &["b"], "camera.0", 1);
        assert!(done.kept);
        assert!(done.removed.is_empty(), "a is being written");
        assert!(a.dir.is_dir());
        assert!(store.is_current(&a));
        // Camera 1 finishes A: it is kept (just written) and B is newer, so
        // both stay over the count until the next clip.
        std::fs::write(a.dir.join("camera.1.avi"), b"RIFF").expect("avi");
        let done = store.finish(&a, video("camera.1"), 1);
        assert!(done.kept);
        assert!(done.removed.is_empty());
        assert_eq!(store.videos("a").len(), 1);
        std::thread::sleep(std::time::Duration::from_millis(2));
        let done = record(&store, &tmp.0, &["c"], "camera.0", 1);
        let mut removed: Vec<String> = done.removed.into_iter().map(|k| k.shot_id).collect();
        removed.sort();
        assert_eq!(removed, ["a", "b"]);
        assert_eq!(store.len(), 1);
        assert!(!a.dir.exists());
    }

    #[test]
    fn a_zero_keep_still_keeps_the_clip_just_written() {
        let tmp = temp();
        let store = ClipStore::default();
        record(&store, &tmp.0, &["a"], "camera.0", 20);
        std::thread::sleep(std::time::Duration::from_millis(2));
        let done = record(&store, &tmp.0, &["b"], "camera.0", 0);
        assert!(done.kept);
        assert_eq!(done.removed, [key("a")]);
        assert_eq!(store.videos("b").len(), 1);
        let replay = ReplaySection {
            keep_shots: 0,
            ..ReplaySection::default()
        };
        assert_eq!(keep(&replay), 1);
        let replay = ReplaySection {
            keep_shots: u32::MAX,
            ..ReplaySection::default()
        };
        assert_eq!(keep(&replay), ReplaySection::MAX_KEEP_SHOTS);
    }

    #[test]
    fn a_slot_cleared_before_its_write_is_not_current_and_leaves_nothing() {
        let tmp = temp();
        let store = ClipStore::default();
        let slot = store
            .begin(&tmp.0, &[shot("a")], store.generation())
            .expect("begin");
        assert!(store.is_current(&slot));
        store.clear();
        assert!(!store.is_current(&slot));
        store.abandon(&slot);
        assert!(!slot.dir.exists());
        assert_eq!(store.len(), 0);
        // Shots that triggered before the clear get a slot that is never
        // current and has no folder.
        let stale = store.begin(&tmp.0, &[shot("x")], 0).expect("begin");
        assert!(!store.is_current(&stale));
        store.abandon(&stale);
        assert_eq!(store.len(), 0);
        assert!(store.videos("x").is_empty());
        // A second camera's failed write leaves the first camera's clip.
        let first = store
            .begin(&tmp.0, &[shot("b")], store.generation())
            .expect("begin");
        let second = store
            .begin(&tmp.0, &[shot("b")], store.generation())
            .expect("join");
        assert_eq!(first.dir, second.dir);
        store.abandon(&second);
        assert!(first.dir.is_dir(), "camera 0 is still writing");
        std::fs::write(first.dir.join("camera.0.avi"), b"RIFF").expect("avi");
        assert!(store.finish(&first, video("camera.0"), 20).kept);
        assert_eq!(store.videos("b").len(), 1);
    }

    #[test]
    fn a_new_clip_folder_is_indexed_and_the_old_one_left_alone() {
        let old = temp();
        let new = temp();
        let store = ClipStore::default();
        record(&store, &old.0, &["a"], "camera.0", 20);
        // A camera still writing into the old folder keeps its clip.
        let writing = store.begin(&old.0, &[shot("w")], 0).expect("begin");
        // The new folder already holds a clip from an earlier run.
        let earlier = ClipStore::default();
        record(&earlier, &new.0, &["n"], "camera.0", 20);

        let gone = store.reindex(&new.0, 20);
        assert_eq!(gone, [key("a")]);
        assert!(store.videos("a").is_empty());
        assert_eq!(store.videos("n").len(), 1);
        assert_eq!(
            std::fs::read_dir(&old.0).expect("old").count(),
            2,
            "nothing deleted in the old folder"
        );
        std::fs::write(writing.dir.join("camera.0.avi"), b"RIFF").expect("avi");
        assert!(store.finish(&writing, video("camera.0"), 20).kept);
        assert_eq!(store.videos("w").len(), 1);
        // Once it finished, the next reindex forgets it too; then nothing
        // changes.
        assert_eq!(store.reindex(&new.0, 20), [key("w")]);
        assert!(store.reindex(&new.0, 20).is_empty());
        assert_eq!(store.videos("n").len(), 1);
    }

    #[test]
    fn the_clip_folder_follows_the_replay_setting() {
        let mut replay = ReplaySection::default();
        assert!(clip_dir(&replay).ends_with(Path::new("flighthook").join("clips")));
        replay.clip_dir = Some("/tmp/swings".into());
        assert_eq!(clip_dir(&replay), PathBuf::from("/tmp/swings"));
        replay.clip_dir = Some("  ".into());
        assert_eq!(clip_dir(&replay), default_dir());
    }
}
