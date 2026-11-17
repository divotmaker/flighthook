//! Camera actor (beta): one `[camera.N]` on [`flighthook_cameras::Camera`].
//!
//! The actor is the bus side of the camera: its thread runs
//! [`Camera::run`], the camera's events go out as the card's local events on
//! the actor's muted sender (nothing goes out once a new instance has
//! replaced it) and those of its clip jobs on the unmuted one, and every
//! launch monitor `shot.trigger` it reads on the bus is handed over as an
//! [`Input::Trigger`].

use std::sync::Arc;

use flighthook::CameraSection;
use flighthook_cameras::{Camera, Input, Out, Trigger, restart_needed};

use crate::actors::web::device_kind;
use crate::actors::{Actor, ReconfigureOutcome};
use crate::bus::{BusReceiver, BusSender, PollError, camera_event, camera_raw};
use crate::state::SystemState;

/// Camera actor: the camera it records from and how.
pub struct CameraActor {
    pub section: CameraSection,
}

impl Actor for CameraActor {
    fn start(
        &self,
        state: Arc<SystemState>,
        sender: BusSender,
        mut receiver: BusReceiver,
    ) -> std::thread::JoinHandle<()> {
        let section = self.section.clone();
        let thread_name = format!("device:{}", sender.actor_id());
        std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                let actor = sender.actor_id().to_owned();
                let muted = sender.clone();
                let out: Out = Arc::new(move |event| muted.local(camera_event(event)));
                let raw = camera_raw(sender.raw_sender(), &actor);
                Camera::run(&state.cameras, &actor, &section, out, raw, || {
                    next_input(&state, &mut receiver)
                });
            })
            .expect("failed to spawn camera thread")
    }

    /// The camera and the video mode need it reopened; the card name, the
    /// camera's own name and the position are read live.
    fn reconfigure(&self, state: &Arc<SystemState>, sender: &BusSender) -> ReconfigureOutcome {
        let index = sender.actor_id().strip_prefix("camera.").unwrap_or("");
        if restart_needed(&self.section, state.system.camera(index).as_ref()) {
            ReconfigureOutcome::RestartRequired
        } else {
            ReconfigureOutcome::Applied
        }
    }
}

/// The next launch monitor trigger on the bus, `Idle` when none is waiting,
/// `Stop` once the actor is to stop.
fn next_input(state: &SystemState, rx: &mut BusReceiver) -> Input {
    loop {
        match rx.poll() {
            Err(PollError::Shutdown) => return Input::Stop,
            Ok(None) => return Input::Idle,
            Ok(Some(msg)) => {
                if let Some(flightrelay::FrpMessage {
                    from,
                    body:
                        flightrelay::FrpBody::ShotTrigger(flightrelay::ShotTrigger { key, timestamp }),
                }) = msg.as_frp()
                {
                    // The card of a local launch monitor; a remote one by
                    // its actor id.
                    let card = state.card_of_actor(from).unwrap_or_else(|| from.clone());
                    let prefix = card.split_once('.').map_or(card.as_str(), |(p, _)| p);
                    return Input::Trigger(Trigger {
                        device: device_kind(prefix).to_owned(),
                        card,
                        key: key.clone(),
                        timestamp: *timestamp,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::SocketAddr;
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};

    use tokio::sync::broadcast;

    use super::*;
    use crate::actors::start_actor;
    use crate::actors::web::WebActor;
    use crate::actors::web::tests::TempDir;
    use crate::bus::{BusMsg, LocalEvent};
    use flighthook::{ActorStatus, Launch, ShotKey, Velocity};

    /// A local event seen on the bus, with its card.
    struct Seen {
        actor: String,
        event: LocalEvent,
    }

    /// A running system, web server and test-pattern camera.
    struct Rig {
        dir: TempDir,
        state: Arc<SystemState>,
        bus: broadcast::Sender<BusMsg>,
        rx: broadcast::Receiver<BusMsg>,
        web: WebActor,
        addr: SocketAddr,
    }

    impl Rig {
        /// Start with `[replay] keep_shots = 20`, short rolls, and one
        /// 30 fps test-pattern camera, waiting until it captures and its
        /// pre-roll has filled (the paced pattern keeps real time, skipping
        /// frames when it falls behind, so a slow machine only waits longer).
        fn start() -> Self {
            let dir = TempDir(std::env::temp_dir().join(format!(
                "flighthook-camera-test-{}",
                uuid::Uuid::new_v4().simple()
            )));
            std::fs::create_dir_all(&dir.0).expect("dir");
            let clips = dir.0.join("clips");
            let config = format!(
                "[replay]\nkeep_shots = 20\npre_roll_s = 0.4\npost_roll_s = 0.6\nclip_dir = {:?}\n\n\
                 [camera.0]\nname = \"Face\"\ncamera = \"test-pattern\"\nposition = \"golfer_front\"\nmode = \"160x120@30\"\n",
                clips.display().to_string(),
            );
            std::fs::write(dir.0.join("config.toml"), config).expect("config");
            let (state, writer) = SystemState::new(dir.0.join("config.toml"));
            let state = Arc::new(state);
            let (bus, rx) = broadcast::channel(1024);
            crate::actors::system::start_for_test(
                writer,
                &state,
                &bus,
                Arc::new(AtomicBool::new(false)),
            );
            let addr = std::net::TcpListener::bind("127.0.0.1:0")
                .and_then(|l| l.local_addr())
                .expect("free port");
            let web = WebActor::new(addr);
            let web_sender = BusSender::new(
                "webserver.0".into(),
                bus.clone(),
                Arc::new(AtomicBool::new(false)),
            );
            web.start(
                Arc::clone(&state),
                web_sender.clone(),
                web_sender.subscribe(),
            );
            let section = state.system.camera("0").expect("camera.0");
            start_actor(
                "camera.0".into(),
                Box::new(CameraActor { section }),
                &state,
                &bus,
            );
            let mut rig = Self {
                dir,
                state,
                bus,
                rx,
                web,
                addr,
            };
            rig.wait_for(Duration::from_secs(10), |m| {
                m.actor == "camera.0"
                    && matches!(
                        m.event,
                        LocalEvent::CardStatus {
                            status: ActorStatus::Connected,
                            ..
                        }
                    )
            })
            .expect("camera connected");
            std::thread::sleep(Duration::from_millis(600));
            rig
        }

        fn clips_dir(&self) -> std::path::PathBuf {
            self.dir.0.join("clips")
        }

        /// Clip folders on disk.
        fn folders(&self) -> usize {
            std::fs::read_dir(self.clips_dir())
                .map(|d| d.flatten().filter(|e| e.path().is_dir()).count())
                .unwrap_or(0)
        }

        fn sender(&self, actor: &str) -> BusSender {
            BusSender::new(
                actor.into(),
                self.bus.clone(),
                Arc::new(AtomicBool::new(false)),
            )
        }

        /// A whole shot from `actor`: trigger, ball, finished.
        fn shot(&self, actor: &str, n: u32) -> ShotKey {
            let key = ShotKey {
                shot_id: uuid::Uuid::new_v4().to_string(),
                shot_number: n,
            };
            // The card's FRP actor id (its card key when it has none).
            let frp_id = self
                .state
                .actor_of_card(actor)
                .unwrap_or_else(|| actor.into());
            let monitor = self.sender(actor).with_frp_id(Some(frp_id));
            monitor.frp(flightrelay::ShotTrigger::new(key.clone()));
            monitor.frp(flightrelay::ShotLaunch {
                key: key.clone(),
                launch: Launch {
                    launch_speed: Some(Velocity::MilesPerHour(100.0 + f64::from(n))),
                    ..Launch::default()
                },
            });
            monitor.frp(flightrelay::ShotFinished { key: key.clone() });
            key
        }

        /// The next bus message matching `want` within `timeout`.
        fn wait_for(
            &mut self,
            timeout: Duration,
            mut want: impl FnMut(&Seen) -> bool,
        ) -> Option<Seen> {
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                match self.rx.try_recv() {
                    Ok(BusMsg::Local { card, event }) => {
                        let m = Seen { actor: card, event };
                        if want(&m) {
                            return Some(m);
                        }
                    }
                    Ok(_) | Err(broadcast::error::TryRecvError::Lagged(_)) => {}
                    Err(_) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
            None
        }

        /// The next `shot_video`'s keys.
        fn next_video(&mut self) -> Option<(Vec<ShotKey>, bool)> {
            self.wait_for(Duration::from_secs(10), |m| {
                matches!(m.event, LocalEvent::ShotVideo { .. })
            })
            .map(|m| match m.event {
                LocalEvent::ShotVideo { keys, complete, .. } => (keys, complete),
                _ => unreachable!(),
            })
        }

        /// GET `path` with `headers`: status code, headers (lower-case
        /// names), body.
        fn get(
            &self,
            path: &str,
            headers: &[(&str, &str)],
        ) -> (u16, HashMap<String, String>, Vec<u8>) {
            self.call("GET", path, headers)
        }

        /// `method` `path` with `headers`, as [`Rig::get`].
        fn call(
            &self,
            method: &str,
            path: &str,
            headers: &[(&str, &str)],
        ) -> (u16, HashMap<String, String>, Vec<u8>) {
            let mut stream = std::net::TcpStream::connect(self.addr).expect("connect");
            let mut req = format!(
                "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n",
                self.addr
            );
            for (k, v) in headers {
                req.push_str(&format!("{k}: {v}\r\n"));
            }
            req.push_str("\r\n");
            stream.write_all(req.as_bytes()).expect("request");
            let mut response = Vec::new();
            stream.read_to_end(&mut response).expect("response");
            let split = response
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .expect("head");
            let head = String::from_utf8_lossy(&response[..split]).to_string();
            let body = response[split + 4..].to_vec();
            let mut lines = head.lines();
            let code = lines
                .next()
                .and_then(|l| l.split(' ').nth(1))
                .and_then(|c| c.parse().ok())
                .expect("status");
            let headers = lines
                .filter_map(|l| l.split_once(':'))
                .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_owned()))
                .collect();
            (code, headers, body)
        }

        fn json(&self, path: &str) -> serde_json::Value {
            let (code, _, body) = self.get(path, &[]);
            assert_eq!(code, 200, "{path}");
            serde_json::from_slice(&body).expect("json")
        }

        /// The `/api/shots` row of `key`, once its video count is `videos`.
        fn row(&self, key: &ShotKey, videos: usize) -> serde_json::Value {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let row = self
                    .json("/api/shots")
                    .as_array()
                    .and_then(|rows| {
                        rows.iter()
                            .find(|r| r["shot_id"] == key.shot_id.as_str())
                            .cloned()
                    })
                    .unwrap_or_default();
                let n = row
                    .get("video")
                    .and_then(serde_json::Value::as_array)
                    .map_or(0, Vec::len);
                if n == videos || Instant::now() > deadline {
                    return row;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            for id in self.state.actor_ids() {
                self.state.stop_actor(&id);
            }
            self.web.stop();
            // The camera releases its capture before the folder goes.
            self.wait_for(Duration::from_secs(5), |m| {
                m.actor == "camera.0"
                    && matches!(
                        m.event,
                        LocalEvent::CardStatus {
                            status: ActorStatus::Disconnected,
                            ..
                        }
                    )
            });
        }
    }

    #[test]
    fn a_mock_shot_records_a_clip_that_is_served_with_the_shot() {
        let mut rig = Rig::start();
        let key = rig.shot("mock_monitor.0", 1);
        let (keys, _) = rig.next_video().expect("shot_video");
        assert_eq!(keys, std::slice::from_ref(&key));
        assert_eq!(rig.folders(), 1);

        // On disk: the camera's AVI and sidecar, and flighthook's shot.json.
        let folder = std::fs::read_dir(rig.clips_dir())
            .expect("clips")
            .flatten()
            .next()
            .expect("folder")
            .path();
        for file in ["camera.0.avi", "camera.0.json", "shot.json"] {
            assert!(folder.join(file).is_file(), "{file}");
        }
        let shot_file: serde_json::Value =
            serde_json::from_slice(&std::fs::read(folder.join("shot.json")).expect("read"))
                .expect("shot.json");
        assert_eq!(
            shot_file["shots"][0]["key"]["shot_id"],
            key.shot_id.as_str()
        );
        assert_eq!(shot_file["shots"][0]["device"], "mock");
        assert_eq!(shot_file["cameras"][0]["position"], "golfer_front");

        // The /api/shots row carries it.
        let row = rig.row(&key, 1);
        let video = &row["video"][0];
        assert_eq!(video["camera"], "camera.0");
        assert_eq!(video["position"], "golfer_front");
        assert!(video["complete"].is_boolean());
        // The trigger and duration are measured on the camera's clock; the
        // camera's own tests cover their windows.
        assert!(video["fps"].is_number() && video["duration_ms"].is_number());
        assert_eq!(video["impact_offset_ms"], video["trigger_offset_ms"]);

        // The clip list and its files.
        let id = &key.shot_id;
        let list = rig.json(&format!("/api/shots/{id}/video"));
        assert_eq!(list.as_array().map(Vec::len), Some(1));
        assert_eq!(list[0]["camera"], "camera.0");
        let (code, headers, body) = rig.get(&format!("/api/shots/{id}/video/camera.0.json"), &[]);
        assert_eq!(code, 200);
        assert!(headers["content-type"].contains("json"), "{headers:?}");
        assert_eq!(headers["cache-control"], "private, no-cache");
        let sidecar =
            stance::sidecar::Sidecar::from_json(std::str::from_utf8(&body).expect("utf-8"))
                .expect("sidecar");
        let frame = sidecar.impact_frame().expect("impact frame");

        // The AVI in byte ranges: its header, then one frame.
        let avi = format!("/api/shots/{id}/video/camera.0.avi");
        let (code, headers, body) = rig.get(&avi, &[("Range", "bytes=0-11")]);
        assert_eq!(code, 206);
        assert_eq!(body.len(), 12);
        assert_eq!(&body[..4], b"RIFF");
        assert_eq!(&body[8..12], b"AVI ");
        assert!(
            headers["content-range"].starts_with("bytes 0-11/"),
            "{headers:?}"
        );
        let range = format!("bytes={}-{}", frame.offset, frame.end() - 1);
        let (code, _, body) = rig.get(&avi, &[("Range", &range)]);
        assert_eq!(code, 206);
        assert_eq!(body.len(), frame.len as usize);
        assert_eq!(&body[..2], &[0xFF, 0xD8]);
        assert_eq!(headers["cache-control"], "private, no-cache");
        let (code, headers, body) = rig.get(&avi, &[]);
        assert_eq!(code, 200);
        assert_eq!(&body[..4], b"RIFF");
        assert_eq!(headers["cache-control"], "private, no-cache");
        for missing in [
            format!("/api/shots/{id}/video/camera.1.avi"),
            format!("/api/shots/{id}/video/camera.0.txt"),
            format!("/api/shots/{id}/video/..%2Fshot.json"),
            "/api/shots/nope/video/camera.0.avi".to_owned(),
        ] {
            assert_eq!(rig.get(&missing, &[]).0, 404, "{missing}");
        }

        // The still: the newest frame, never cached.
        let (code, headers, body) = rig.get("/api/cameras/camera.0/still", &[]);
        assert_eq!(code, 200);
        assert_eq!(headers["content-type"], "image/jpeg");
        assert_eq!(headers["cache-control"], "no-store");
        assert_eq!(&body[..2], &[0xFF, 0xD8]);
        assert_eq!(rig.get("/api/cameras/camera.9/still", &[]).0, 404);

        // The status says where clips go. (`/api/cameras` is left out: it
        // enumerates the machine's real cameras.)
        let status = rig.json("/api/status");
        assert_eq!(
            status["clip_dir"],
            rig.clips_dir().display().to_string().as_str()
        );

        // The live view: a viewer asks, and the picture comes alone (body
        // tracking is off).
        // `204` means no newer frame yet: ask again, as a viewer does, until
        // the first one arrives.
        let (code, headers, body) = (0..60)
            .map(|_| rig.get("/api/cameras/camera.0/live?after=0", &[]))
            .find(|answer| answer.0 != 204)
            .expect("a live frame");
        assert_eq!(code, 200);
        assert_eq!(headers["content-type"], "image/jpeg");
        assert_eq!(&body[..2], &[0xFF, 0xD8]);
        assert!(!headers.contains_key("x-pose"), "{headers:?}");
        assert_eq!(headers["x-pose-state"], "off");

        // Never in the export.
        let (code, _, body) = rig.get("/api/shots/export?format=jsonl", &[]);
        assert_eq!(code, 200);
        let text = String::from_utf8(body).expect("utf-8");
        assert!(text.contains(&key.shot_id), "{text}");
        assert!(!text.contains("\"video\""), "{text}");
        assert!(!text.contains("camera.0"), "{text}");
    }
}
