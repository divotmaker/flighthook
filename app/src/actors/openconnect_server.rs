//! The Open Connect server card (`openconnect_server.N`): the Open Connect
//! server driver ([`flighthook_openconnect::server`]) on [`DriverActor`].
//!
//! It is a launch monitor card, not an output: launch monitor software that
//! sends Open Connect connects to it, and its shots go on the bus as a
//! measured launch monitor's, which anything downstream (an Open Connect
//! client card, for one) takes unchanged.
//!
//! ```text
//!   launch monitor software        --TCP 921-->  openconnect_server.0
//!                                                        |
//!                                                        v  bus
//!                                       openconnect_client.0  --TCP 922-->  GSPro
//! ```
//!
//! GSPro listens on 921 as well, but its port is movable: setting
//! `<OpenAPIUseAltPort>true</OpenAPIUseAltPort>` in
//! `C:\GSPro\GSPC\GSPconnect.exe.config` starts GSPConnect on 922 and frees
//! 921 for this card, so both can share one host. Splitting across two hosts
//! works too, but only if the monitor's connector can target a non-localhost
//! address; Open Connect's documentation specifies `127.0.0.1`.
//!
//! The card adds its section ([`OpenConnectServerSection`]) and its shot
//! source, and names the client it refuses: the driver reports a client of
//! this process by its actor id, and the card shows the name of its card.
//! A change of `bind` restarts the card.

use flighthook::OpenConnectServerSection;
use flighthook_openconnect::server::Server;
use flightrelay::FrpBody;

use super::actor_names;
use super::driver::{DriverActor, HookCtx, Hooks, Pass};
use super::provenance::ShotSource;
use crate::state::SystemState;

/// flighthook's side of an Open Connect server card.
#[derive(Debug, Default)]
pub struct OpenConnectServerHooks;

impl Hooks<Server> for OpenConnectServerHooks {
    type Section = OpenConnectServerSection;

    /// A measured source: a launch monitor's own app sends it the shots
    /// that monitor measured.
    const SOURCE: Option<ShotSource> = Some(ShotSource::OpenConnectServer);

    /// An alert that names one of this process's clients by its actor id
    /// shows its card's name instead.
    fn on_frp(&mut self, body: &mut FrpBody, ctx: &mut HookCtx<'_, Self::Section>) -> Pass {
        if let FrpBody::BusAlert(alert) = body {
            alert.message = named(&alert.message, ctx.state);
        }
        Pass::Send
    }
}

/// The Open Connect server actor (`openconnect_server.N`).
pub type OpenConnectServerActor = DriverActor<Server, OpenConnectServerHooks>;

/// `message` with the first quoted actor id of a card of this process
/// replaced by the name its card shows (its card key when that is blank).
fn named(message: &str, state: &SystemState) -> String {
    let Some(id) = message.split('"').nth(1) else {
        return message.to_owned();
    };
    let Some(card) = state.card_of_actor(id) else {
        return message.to_owned();
    };
    let names = actor_names(&state.system.snapshot());
    let name = names
        .get(&card)
        .filter(|n| !n.trim().is_empty())
        .map_or(card.as_str(), String::as_str);
    message.replacen(&format!("\"{id}\""), &format!("\"{name}\""), 1)
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::sync::Arc;

    use flighthook::{
        ActorStatus, OpenConnectClientConfig, OpenConnectClientSection, OpenConnectServerConfig,
        RawDir,
    };
    use flightrelay::{BusActor, FrpMessage};
    use tokio::sync::broadcast;

    use super::*;
    use crate::actors::outputs::OpenConnectClientActor;
    use crate::actors::scripted::recv_within;
    use crate::actors::web::tests::TempDir;
    use crate::bus::{BusMsg, LocalEvent};

    const SERVER: &str = "openconnect_server.0";
    const CLIENT: &str = "openconnect_client.0";

    const SHOT: &str = r#"{"DeviceID":"Range Bridge","Units":"Yards","ShotNumber":1,"APIversion":"1","BallData":{"Speed":100.0,"VLA":14.0,"HLA":1.0,"TotalSpin":3000.0,"SpinAxis":2.0},"ShotDataOptions":{"ContainsBallData":true}}"#;
    const READY: &str = r#"{"DeviceID":"Range Bridge","ShotNumber":0,"APIversion":"1","ShotDataOptions":{"IsHeartBeat":true,"LaunchMonitorIsReady":true}}"#;
    const LATERAL: &str = r#"{"DeviceID":"TestLM","Units":"Yards","ShotNumber":7,"APIversion":"1","BallData":{"Speed":120.0,"SpinAxis":-13.05,"TotalSpin":2878.5,"BackSpin":2800.0,"SideSpin":-650.0,"HLA":4.2,"VLA":14.0},"ClubData":{"Speed":90.0,"FaceToTarget":2.1,"Path":-3.4,"HorizontalFaceImpact":-6.5,"VerticalFaceImpact":3.0},"ShotDataOptions":{"ContainsBallData":true,"ContainsClubData":true}}"#;

    /// An address on loopback no one is listening on.
    fn free_address() -> SocketAddr {
        TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("a free port")
    }

    /// A process's worth of actors on one bus, sharing one `SystemState`,
    /// with Open Connect client 0 named "Bay GSPro" and Open Connect server
    /// 0 "Bay Bridge".
    struct Rig {
        state: Arc<SystemState>,
        tx: broadcast::Sender<BusMsg>,
        events: broadcast::Receiver<BusMsg>,
        /// Actors started as the app starts them, stopped on drop.
        started: Vec<String>,
        _dir: TempDir,
    }

    impl Rig {
        fn new() -> Self {
            let dir = TempDir(std::env::temp_dir().join(format!(
                "flighthook-oc-server-{}",
                crate::state::config::generate_id()
            )));
            let (state, _writer) = SystemState::new(dir.0.join("config.toml"));
            state.system.update(|c| {
                c.openconnect_client.insert(
                    "0".into(),
                    OpenConnectClientSection {
                        name: "Bay GSPro".into(),
                        actor_id: Some(CLIENT.into()),
                        ..Default::default()
                    },
                );
            });
            let (tx, events) = broadcast::channel(4096);
            Self {
                state: Arc::new(state),
                tx,
                events,
                started: Vec::new(),
                _dir: dir,
            }
        }

        /// Start `openconnect_server.0` as the app starts it, on a free
        /// address, once it listens.
        fn server(&mut self) -> SocketAddr {
            let bind = free_address();
            let section = OpenConnectServerSection {
                name: "Bay Bridge".into(),
                actor_id: Some(SERVER.into()),
                driver: OpenConnectServerConfig {
                    bind: Some(bind.to_string()),
                },
                ..OpenConnectServerSection::default()
            };
            self.state.system.update(|c| {
                c.openconnect_server.insert("0".into(), section.clone());
            });
            crate::actors::start_actor_as(
                SERVER.into(),
                Some(SERVER.into()),
                Box::new(OpenConnectServerActor::new(section)),
                &self.state,
                &self.tx,
            );
            self.started.push(SERVER.into());
            self.until("the server listening", |m| phase(m) == Some("listening"));
            bind
        }

        /// Start `openconnect_client.0` dialing `addr`, as the app starts it.
        fn client(&mut self, addr: SocketAddr) {
            let mut section = self
                .state
                .system
                .snapshot()
                .openconnect_client
                .get("0")
                .cloned()
                .expect("the client's section");
            section.driver = OpenConnectClientConfig {
                address: Some(addr.to_string()),
                ..OpenConnectClientConfig::default()
            };
            crate::actors::start_actor_as(
                CLIENT.into(),
                Some(CLIENT.into()),
                Box::new(OpenConnectClientActor::new(section)),
                &self.state,
                &self.tx,
            );
            self.started.push(CLIENT.into());
        }

        /// Every message until `done` holds for one; blocks on the bus.
        fn until(&mut self, what: &str, done: impl Fn(&BusMsg) -> bool) -> Vec<BusMsg> {
            let mut seen = Vec::new();
            loop {
                let msg = recv_within(&mut self.events)
                    .unwrap_or_else(|| panic!("timed out waiting for {what}"));
                let stop = done(&msg);
                seen.push(msg);
                if stop {
                    return seen;
                }
            }
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            for actor in &self.started {
                let _ = self.state.stop_and_wait(actor);
            }
        }
    }

    /// The phase `msg` says the server is in, if it says one.
    fn phase(msg: &BusMsg) -> Option<&str> {
        match msg.as_local() {
            Some((card, LocalEvent::CardStatus { telemetry, .. })) if card == SERVER => {
                telemetry.get("phase").map(String::as_str)
            }
            _ => None,
        }
    }

    /// The card status `card` reports on `msg`, if it is one.
    fn status<'a>(
        msg: &'a BusMsg,
        card: &str,
    ) -> Option<(ActorStatus, &'a std::collections::HashMap<String, String>)> {
        match msg.as_local() {
            Some((c, LocalEvent::CardStatus { status, telemetry })) if c == card => {
                Some((*status, telemetry))
            }
            _ => None,
        }
    }

    fn server_shot(msg: &BusMsg) -> bool {
        matches!(
            msg.as_frp(),
            Some(FrpMessage { from, body: FrpBody::ShotTrigger(_) }) if from == SERVER
        )
    }

    fn server_finished(msg: &BusMsg) -> bool {
        matches!(
            msg.as_frp(),
            Some(FrpMessage { from, body: FrpBody::ShotFinished(_) }) if from == SERVER
        )
    }

    /// The alerts `actor` raised: its `bus.alert`s and its local alerts.
    fn alerts(seen: &[BusMsg], actor: &str) -> Vec<String> {
        seen.iter()
            .filter_map(|m| match m {
                BusMsg::Frp {
                    msg:
                        FrpMessage {
                            from,
                            body: FrpBody::BusAlert(alert),
                        },
                    ..
                } if from == actor => Some(alert.message.clone()),
                BusMsg::Local {
                    card,
                    event: LocalEvent::Alert { message, .. },
                } if card == actor => Some(message.clone()),
                _ => None,
            })
            .collect()
    }

    fn server_bus_actor(msg: &BusMsg) -> Option<&BusActor> {
        match msg.as_frp() {
            Some(m) if m.from == SERVER => match &m.body {
                FrpBody::BusActor(a) => Some(a),
                _ => None,
            },
            _ => None,
        }
    }

    // -- Naming ------------------------------------------------------------------

    /// An alert that quotes the actor id of one of this process's cards
    /// shows the card's name; any other message is left as it is.
    #[test]
    fn an_alert_names_the_client_by_its_card() {
        let rig = Rig::new();
        let message = format!("Refused Open Connect Client \"{CLIENT}\" from 10.0.0.1:5: no");
        assert_eq!(
            named(&message, &rig.state),
            "Refused Open Connect Client \"Bay GSPro\" from 10.0.0.1:5: no"
        );
        for other in [
            "Refused Open Connect Client \"nobody\" from 10.0.0.1:5: no",
            "no quotes here",
            "",
        ] {
            assert_eq!(named(other, &rig.state), other);
        }
        // A blank name shows the card's key.
        rig.state.system.update(|c| {
            if let Some(s) = c.openconnect_client.get_mut("0") {
                s.name = " ".into();
            }
        });
        assert!(named(&message, &rig.state).contains(CLIENT), "{message}");
    }

    // -- The card on the driver host -----------------------------------------------

    /// A monitor that is not one of ours is served: its shot reaches the bus
    /// with the request and the reply as the shot's two raw frames, with no
    /// alert, and the card shows the monitor as its device.
    #[test]
    fn a_foreign_peer_is_served() {
        let mut rig = Rig::new();
        let bind = rig.server();
        let mut monitor = TcpStream::connect(bind).expect("monitor connects");
        let local = monitor.local_addr().expect("monitor address").to_string();
        monitor.write_all(SHOT.as_bytes()).expect("send shot");
        let seen = rig.until("the shot finished", server_finished);
        assert!(alerts(&seen, SERVER).is_empty());
        assert!(seen.iter().any(server_shot));
        assert!(
            seen.iter().any(|m| matches!(
                status(m, SERVER),
                Some((ActorStatus::Connected, t))
                    if t.get("device_info").is_some_and(|d| *d == format!("Range Bridge ({local})"))
            )),
            "the card shows the monitor"
        );
        let (raw, scrub) = seen
            .iter()
            .rev()
            .find_map(|m| match m.as_local() {
                Some((_, LocalEvent::ShotCapture { raw, scrub, .. })) => {
                    Some(((**raw).clone(), scrub.clone()))
                }
                _ => None,
            })
            .expect("the shot's capture");
        // The monitor's address is scrubbed from the export.
        assert_eq!(scrub, std::slice::from_ref(&local));
        assert_eq!(raw.transport, "openconnect-json");
        let frames: Vec<_> = raw
            .frames
            .iter()
            .map(|f| (f.dir, f.text.clone().unwrap_or_default()))
            .collect();
        assert_eq!(
            frames,
            [
                (RawDir::Rx, SHOT.to_owned()),
                (
                    RawDir::Tx,
                    r#"{"Code":200,"Message":"Shot received"}"#.to_owned()
                )
            ]
        );
        let mut reply = [0u8; 64];
        let n = monitor.read(&mut reply).expect("the 200");
        assert_eq!(&reply[..n], br#"{"Code":200,"Message":"Shot received"}"#);
    }

    /// A monitor that said it is ready and then disconnects leaves the
    /// server's input not ready, and the card waits for the next monitor on
    /// the same port.
    #[test]
    fn the_input_is_not_ready_once_its_monitor_disconnects() {
        let mut rig = Rig::new();
        let bind = rig.server();
        let mut monitor = TcpStream::connect(bind).expect("monitor connects");
        monitor.write_all(READY.as_bytes()).expect("send ready");
        rig.until("the input ready", |m| {
            server_bus_actor(m).is_some_and(BusActor::is_clear)
        });
        drop(monitor);
        rig.until("the input not ready", |m| {
            server_bus_actor(m).is_some_and(|a| !a.is_clear())
        });
        let mut next = TcpStream::connect(bind).expect("the next monitor connects");
        next.write_all(SHOT.as_bytes()).expect("send shot");
        rig.until("the next monitor's shot", server_shot);
    }

    /// The card shows the monitor while it is connected, and Reconnecting
    /// once it left, with no alert for a monitor that merely went away.
    #[test]
    fn the_card_shows_the_monitor_until_it_disconnects() {
        let mut rig = Rig::new();
        let bind = rig.server();
        let mut monitor = TcpStream::connect(bind).expect("monitor connects");
        let local = monitor.local_addr().expect("monitor address").to_string();
        monitor.write_all(READY.as_bytes()).expect("send ready");
        rig.until("the card connected", |m| {
            matches!(status(m, SERVER), Some((ActorStatus::Connected, t))
                if t.get("device_info").is_some_and(|d| d.ends_with(&format!("({local})"))))
        });
        drop(monitor);
        let seen = rig.until("the card reconnecting", |m| {
            matches!(status(m, SERVER), Some((ActorStatus::Reconnecting, _)))
        });
        assert!(alerts(&seen, SERVER).is_empty());
    }

    /// A client of this process pointed at the server is refused: its card
    /// shows why, the server alerts once, naming the client's card, and
    /// neither card ever shows Connected.
    #[test]
    fn a_client_pointed_at_its_own_server_is_refused_and_alerted_once() {
        let mut rig = Rig::new();
        // The product owner's setup: the server on a wide address, the
        // client on loopback, nothing else listening.
        let bind = rig.server();
        rig.client(bind);

        let client_error = |m: &BusMsg| {
            status(m, CLIENT).and_then(|(_, t)| t.get("error").filter(|e| !e.is_empty()).cloned())
        };
        let seen = rig.until("the first refusal", |m| client_error(m).is_some());
        let error = seen.iter().find_map(client_error).expect("error");
        assert_eq!(
            error,
            format!(
                "Refused by this flighthook's own Open Connect Server on {bind}: this \
                 address points at it, which would loop shots back. Use the simulator's \
                 port instead."
            )
        );
        let alerts = alerts(&seen, SERVER);
        assert_eq!(alerts.len(), 1, "{alerts:?}");
        let (start, end) = alerts[0]
            .split_once(" from 127.0.0.1:")
            .expect("the peer address");
        assert_eq!(start, "Refused Open Connect Client \"Bay GSPro\"");
        assert!(
            end.ends_with(
                ": it is this flighthook's own client, which would loop shots back. \
                 Point it at the simulator's port."
            ),
            "{end}"
        );
        // A refused connection never shows as Connected.
        for card in [CLIENT, SERVER] {
            assert!(
                !seen
                    .iter()
                    .any(|m| matches!(status(m, card), Some((ActorStatus::Connected, _)))),
                "{card} reported Connected"
            );
        }
    }

    /// Strict mode, through the real path: a monitor that sends spin
    /// components has the completed pair (total and axis) go out too.
    #[test]
    fn strict_mode_sends_the_completed_spin_pair() {
        let mut rig = Rig::new();
        let bind = rig.server();
        let mut monitor = TcpStream::connect(bind).expect("monitor connects");
        monitor.write_all(LATERAL.as_bytes()).expect("send shot");
        let mut messages = Vec::new();
        // The server's whole bus, from its `bus.actor` to the shot's end.
        messages.extend(rig.until("the shot finished", server_finished));
        let frames = crate::test_ball::strict_frames(SERVER, &messages).expect("sent");
        let spin = flighthook_pureplay::connect::codec::BallData::TOTAL_SPIN
            | flighthook_pureplay::connect::codec::BallData::BACK_SPIN
            | flighthook_pureplay::connect::codec::BallData::SIDE_SPIN
            | flighthook_pureplay::connect::codec::BallData::SPIN_AXIS;
        assert_eq!(frames.ball.valid & spin, spin);
    }
}
