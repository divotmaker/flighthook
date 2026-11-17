use super::*;
use crate::TempDir;
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
        hand: None,
        golfer: None,
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
        pose: false,
        swing: false,
        rotation: 0,
        analyzed_with: None,
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

/// A clip records its shot's golfer (the turn's raw id at the trigger)
/// in `shot.json`, and a reload reads it back.
#[test]
fn a_clip_keeps_its_golfer_across_a_reload() {
    let dir = temp();
    let store = ClipStore::default();
    let mut s = shot("a");
    s.golfer = Some("pureplay.local-3".into());
    let slot = store
        .begin(&dir.0, &[s], store.generation())
        .expect("begin");
    std::fs::write(slot.dir.join("camera.0.avi"), b"RIFF").expect("avi");
    std::fs::write(slot.dir.join("camera.0.json"), b"{}").expect("json");
    assert!(store.finish(&slot, video("camera.0"), 5).kept);
    let file = std::fs::read_to_string(slot.dir.join("shot.json")).expect("shot.json");
    assert!(file.contains("\"golfer\": \"pureplay.local-3\""), "{file}");
    let reloaded = ClipStore::default();
    reloaded.load(&dir.0, 5);
    let clip = reloaded.camera_clip("a", "camera.0").expect("clip");
    assert_eq!(clip.shots[0].golfer.as_deref(), Some("pureplay.local-3"));
    // A shot with no golfer writes none.
    let none = serde_json::to_string(&shot("b")).expect("json");
    assert!(!none.contains("golfer"), "{none}");
}

#[test]
fn clip_ids_sort_by_time_and_are_recognized() {
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

/// Begin, write and finish camera.0's clip of shot `id`, then its pose
/// file, as the body tracking queue does. Returns the slot.
fn record_with_pose(store: &ClipStore, dir: &Path, id: &str, keep: u32) -> ClipSlot {
    let slot = store
        .begin(dir, &[shot(id)], store.generation())
        .expect("begin");
    std::fs::write(slot.dir.join("camera.0.avi"), b"RIFF").expect("avi");
    std::fs::write(slot.dir.join("camera.0.json"), b"{}").expect("json");
    assert!(store.finish(&slot, video("camera.0"), keep).kept);
    assert!(store.file(id, "camera.0", "pose.json").is_none(), "not yet");
    std::fs::write(slot.dir.join("camera.0.pose.json"), b"{}").expect("pose");
    assert!(store.set_pose(&slot, "camera.0", 0));
    slot
}

#[test]
fn a_clips_pose_file_is_served_once_done_and_survives_a_restart() {
    let tmp = temp();
    let store = ClipStore::default();
    let slot = record_with_pose(&store, &tmp.0, "a", 20);
    let pose = store.file("a", "camera.0", "pose.json").expect("served");
    assert_eq!(pose, slot.dir.join("camera.0.pose.json"));
    assert!(store.videos("a")[0].pose);
    assert!(!store.set_pose(&slot, "camera.7", 0), "no such camera clip");
    // shot.json carries it into the next run.
    let again = ClipStore::default();
    assert!(again.load(&tmp.0, 20).is_empty());
    assert!(again.videos("a")[0].pose);
    assert!(again.file("a", "camera.0", "pose.json").is_some());
}

#[test]
fn a_clips_swing_file_is_served_once_done_and_goes_with_the_clip() {
    let tmp = temp();
    let store = ClipStore::default();
    let first = record_with_pose(&store, &tmp.0, "a", 1);
    assert!(
        store.file("a", "camera.0", "swing.json").is_none(),
        "not yet"
    );
    std::fs::write(first.dir.join("camera.0.swing.json"), b"{}").expect("swing");
    assert!(store.set_swing(&first, "camera.0"));
    let swing = store.file("a", "camera.0", "swing.json").expect("served");
    assert_eq!(swing, first.dir.join("camera.0.swing.json"));
    assert!(store.videos("a")[0].swing);
    assert!(!store.set_swing(&first, "camera.7"), "no such camera clip");
    // shot.json carries it into the next run.
    let again = ClipStore::default();
    again.load(&tmp.0, 20);
    assert!(again.videos("a")[0].swing);
    // Pruned with its clip, and a late one is not kept.
    let second = record_with_pose(&store, &tmp.0, "b", 1);
    assert!(!swing.exists());
    assert!(store.file("a", "camera.0", "swing.json").is_none());
    assert!(!store.set_swing(&first, "camera.0"));
    std::fs::write(second.dir.join("camera.0.swing.json"), b"{}").expect("swing");
    assert!(store.set_swing(&second, "camera.0"));
    store.clear();
    assert!(!second.dir.join("camera.0.swing.json").exists(), "cleared");
    assert!(!store.set_swing(&second, "camera.0"));
}

#[test]
fn pruning_and_clear_history_delete_the_pose_file_with_the_clip() {
    let tmp = temp();
    let store = ClipStore::default();
    let first = record_with_pose(&store, &tmp.0, "a", 1);
    let pose = first.dir.join("camera.0.pose.json");
    assert!(pose.exists());
    let second = record_with_pose(&store, &tmp.0, "b", 1);
    assert!(!pose.exists(), "pruned with its clip");
    assert!(store.file("a", "camera.0", "pose.json").is_none());
    // A pose finished after its clip was pruned is not kept.
    assert!(!store.set_pose(&first, "camera.0", 0));
    let pose = second.dir.join("camera.0.pose.json");
    assert!(store.clear().contains(&key("b")));
    assert!(!pose.exists(), "cleared with its clip");
    assert!(!store.set_pose(&second, "camera.0", 0), "cleared since");
}

#[test]
fn a_pose_records_its_rotation_and_the_stance_version() {
    let tmp = temp();
    let store = ClipStore::default();
    let slot = record_with_pose(&store, &tmp.0, "a", 20);
    let video = &store.videos("a")[0];
    assert_eq!(video.rotation, 0);
    assert_eq!(video.analyzed_with.as_deref(), Some(stance::VERSION));
    assert!(store.set_pose(&slot, "camera.0", 270));
    let video = &store.videos("a")[0];
    assert_eq!(video.rotation, 270);
    // shot.json carries both into the next run.
    let again = ClipStore::default();
    again.load(&tmp.0, 20);
    let video = &again.videos("a")[0];
    assert_eq!(video.rotation, 270);
    assert_eq!(video.analyzed_with.as_deref(), Some(stance::VERSION));
    let text = std::fs::read_to_string(slot.dir.join(SHOT_FILE)).expect("shot.json");
    assert!(text.contains(r#""rotation": 270"#), "{text}");
}

#[test]
fn an_old_shot_file_reads_as_rotation_zero_and_no_version() {
    let tmp = temp();
    let store = ClipStore::default();
    let slot = record_with_pose(&store, &tmp.0, "a", 20);
    let path = slot.dir.join(SHOT_FILE);
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
    let camera = v["cameras"][0].as_object_mut().expect("camera");
    camera.remove("rotation");
    camera.remove("analyzed_with");
    std::fs::write(&path, serde_json::to_vec(&v).expect("ser")).expect("write");
    let again = ClipStore::default();
    again.load(&tmp.0, 20);
    let video = &again.videos("a")[0];
    assert!(video.pose);
    assert_eq!(video.rotation, 0);
    assert_eq!(video.analyzed_with, None);
}

#[test]
fn clearing_the_analysis_deletes_its_files_and_flags() {
    let tmp = temp();
    let store = ClipStore::default();
    let slot = record_with_pose(&store, &tmp.0, "a", 20);
    assert!(store.set_pose(&slot, "camera.0", 90));
    std::fs::write(slot.dir.join("camera.0.swing.json"), b"{}").expect("swing");
    assert!(store.set_swing(&slot, "camera.0"));
    // Another camera's files are left alone.
    std::fs::write(slot.dir.join("camera.1.pose.json"), b"{}").expect("pose");
    assert!(store.clear_analysis(&slot, "camera.0"));
    assert!(!slot.dir.join("camera.0.pose.json").exists());
    assert!(!slot.dir.join("camera.0.swing.json").exists());
    assert!(slot.dir.join("camera.0.avi").exists(), "the clip stays");
    assert!(slot.dir.join("camera.1.pose.json").exists());
    let video = &store.videos("a")[0];
    assert!(!video.pose && !video.swing);
    assert_eq!(video.rotation, 0);
    assert_eq!(video.analyzed_with, None);
    assert!(store.file("a", "camera.0", "pose.json").is_none());
    assert!(store.file("a", "camera.0", "swing.json").is_none());
    // shot.json is rewritten.
    let again = ClipStore::default();
    again.load(&tmp.0, 20);
    assert!(!again.videos("a")[0].pose);
    // Clearing again finds nothing to delete and still succeeds; a
    // camera with no clip, or a cleared history, does not.
    assert!(store.clear_analysis(&slot, "camera.0"));
    assert!(!store.clear_analysis(&slot, "camera.7"));
    store.clear();
    assert!(!store.clear_analysis(&slot, "camera.0"));
}

#[test]
fn an_impact_outside_the_clip_is_no_prior() {
    use stance::sidecar::{Sidecar, SidecarFps, SidecarFrame};
    let frames = (0..10u64)
        .map(|i| SidecarFrame {
            t_ns: i * 100_000_000,
            offset: 0,
            len: 0,
        })
        .collect();
    let mut sidecar = Sidecar {
        version: stance::sidecar::SIDECAR_VERSION,
        fourcc: "MJPG".into(),
        width: 64,
        height: 48,
        fps: SidecarFps {
            nominal: Some(10.0),
            measured: 10.0,
        },
        start_ns: 0,
        frames,
        trigger_index: 9,
        impact_index: 5,
        trigger_ns: 900_000_000,
        impact_ns: 500_000_000,
        complete: true,
        dropped: 0,
        truncated: false,
    };
    assert_eq!(clip_impact(&sidecar), Some(500_000_000));
    sidecar.impact_ns = 900_000_000;
    assert_eq!(clip_impact(&sidecar), Some(900_000_000), "the last frame");
    // A clip cut short before the impact.
    sidecar.impact_ns = 950_000_000;
    assert_eq!(clip_impact(&sidecar), None);
    sidecar.impact_ns = -1;
    assert_eq!(clip_impact(&sidecar), None);
}
