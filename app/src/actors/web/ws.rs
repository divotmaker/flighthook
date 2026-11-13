//! `/frp`: a plain FRP 0.3.0 hub over WebSocket.
//!
//! No handshake. On connecting, the peer gets the binding's greeting: every
//! actor's `bus.actor` followed by its state, then a `bus.sync`. After that
//! every message on the bus goes to the peer as it is, except what the peer
//! sent itself and `shot.raw`. What a peer sends goes onto the internal
//! bus, and so to every other peer. A message whose `from` is one of this
//! process's own actors is dropped (the own-id guard): the link shows an
//! error on the Web Server card, logged once per link.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::broadcast;

use super::{WebState, emit_telemetry};
use crate::bus::{BusMsg, PeerId};
use flightrelay::{BusSync, FrpBody, FrpMessage};

/// The kind `/frp` never sends: raw device frames.
const SHOT_RAW: &str = "shot.raw";

/// GET /frp — upgrade to WebSocket.
pub async fn ws_upgrade(
    State(state): State<Arc<WebState>>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

/// The error a hub link shows when the own-id guard drops its messages.
pub(crate) const OWN_MESSAGES_BACK: &str = "Received this flighthook's own messages back: a relay \
     loop, or a second PC restored from this one's settings";

/// What goes out to the hub link `to` for a bus message: every FRP message
/// except the ones `to` sent and `shot.raw` (never the internal `cause`).
/// flighthook's own events stay.
pub(crate) fn outbound(msg: &BusMsg, to: PeerId) -> Option<&FrpMessage> {
    match msg {
        BusMsg::Frp { msg, peer, .. } if *peer != Some(to) && msg.body.kind() != SHOT_RAW => {
            Some(msg)
        }
        _ => None,
    }
}

/// What a hub link's text frame puts on the bus. A frame that is no FRP
/// message, or a `shot.raw`, puts nothing; nor does a shot the round
/// policy holds ([`crate::state::round::RoundPolicy::admits_remote`]: a
/// virtual remote launch monitor's shot in tournament mode). A message from
/// one of this process's own actors is [`Inbound::OwnBack`].
pub(crate) enum Inbound {
    Bus(Box<BusMsg>),
    OwnBack(String),
    Nothing,
}

pub(crate) fn inbound(root: &crate::state::SystemState, text: &str, peer: PeerId) -> Inbound {
    let msg: FrpMessage = match serde_json::from_str(text) {
        Ok(msg) => msg,
        Err(e) => {
            tracing::debug!("frp: not an FRP message ({e}): {text}");
            return Inbound::Nothing;
        }
    };
    if msg.body.kind() == SHOT_RAW {
        return Inbound::Nothing;
    }
    if root.card_of_actor(&msg.from).is_some() || root.is_card_key(&msg.from) {
        return Inbound::OwnBack(msg.from);
    }
    if !root.round.admits_remote(&msg.from, &msg.body) {
        return Inbound::Nothing;
    }
    Inbound::Bus(Box::new(BusMsg::from_peer(msg, peer)))
}

/// The greeting a new peer gets: each local actor's `bus.actor`, then its
/// other state, then a `bus.sync` from flighthook's actor. Remote actors'
/// state is left out: the `bus.sync` has them republish it themselves.
pub(super) fn greeting(state: &WebState) -> Vec<FrpMessage> {
    let states: Vec<FrpMessage> = state
        .bus_state
        .lock()
        .map(|bus| {
            bus.states()
                .filter(|m| state.root.card_of_actor(&m.from).is_some())
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    let mut actors: Vec<&FrpMessage> = states
        .iter()
        .filter(|m| matches!(m.body, FrpBody::BusActor(_)))
        .collect();
    actors.sort_by(|a, b| a.from.cmp(&b.from));
    let mut out = Vec::with_capacity(states.len() + 1);
    for actor in actors {
        out.push(actor.clone());
        out.extend(
            states
                .iter()
                .filter(|m| m.from == actor.from && !matches!(m.body, FrpBody::BusActor(_)))
                .cloned(),
        );
    }
    out.push(FrpMessage::new(state.root.flighthook_id(), BusSync::new()));
    out
}

async fn send_all(
    ws_tx: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    msgs: &[FrpMessage],
) -> bool {
    for msg in msgs {
        let Ok(json) = serde_json::to_string(msg) else {
            continue;
        };
        if ws_tx.send(Message::text(json)).await.is_err() {
            return false;
        }
    }
    true
}

async fn handle_ws(socket: WebSocket, state: Arc<WebState>) {
    let peer = PeerId::next();
    let (mut ws_tx, mut ws_rx) = socket.split();
    // Subscribed before the greeting is taken, so nothing falls between.
    let mut bus_rx = state.bus_tx.subscribe();
    if !send_all(&mut ws_tx, &greeting(&state)).await {
        return;
    }
    state.ws_count.fetch_add(1, Ordering::Relaxed);
    emit_telemetry(&state, &state.bus_tx);
    tracing::info!("frp: peer connected");

    let send_state = Arc::clone(&state);
    let mut send_task = tokio::spawn(async move {
        loop {
            match bus_rx.recv().await {
                Ok(msg) => {
                    let Some(frp) = outbound(&msg, peer) else {
                        continue;
                    };
                    if !send_all(&mut ws_tx, std::slice::from_ref(frp)).await {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return,
                // Behind: the greeting again (state and `bus.sync`), then on.
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("frp: peer fell {n} messages behind, greeting it again");
                    bus_rx = send_state.bus_tx.subscribe();
                    if !send_all(&mut ws_tx, &greeting(&send_state)).await {
                        return;
                    }
                }
            }
        }
    });
    // What the peer sends goes onto the bus, tagged with its link.
    let recv_state = Arc::clone(&state);
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_rx.next().await {
            let text = match msg {
                Message::Close(_) => break,
                Message::Text(text) => text,
                _ => continue,
            };
            match inbound(&recv_state.root, &text, peer) {
                Inbound::Bus(msg) => {
                    let _ = recv_state.bus_tx.send(*msg);
                }
                Inbound::OwnBack(from) => {
                    if recv_state.own_back(peer) {
                        tracing::warn!(
                            "frp: peer sent this flighthook's own actor {from}'s messages \
                             back; dropping them (relay loop or copied settings)"
                        );
                        emit_telemetry(&recv_state, &recv_state.bus_tx);
                    }
                }
                Inbound::Nothing => {}
            }
        }
    });

    tokio::select! {
        _ = &mut send_task => recv_task.abort(),
        _ = &mut recv_task => send_task.abort(),
    }
    state.ws_count.fetch_sub(1, Ordering::Relaxed);
    state.link_closed(peer);
    emit_telemetry(&state, &state.bus_tx);
    tracing::info!("frp: peer disconnected");
}

#[cfg(test)]
mod tests {
    use super::super::tests::test_state;
    use super::*;
    use crate::bus::LocalEvent;
    use flighthook::Club;
    use flightrelay::{BusActor, GolferTurn, Handedness, Mode, ShotKey, ShotTrigger};

    #[test]
    fn only_local_frp_messages_go_out_and_never_shot_raw() {
        let me = PeerId(1);
        let trigger = FrpMessage::new("lm1", ShotTrigger::new(ShotKey::new("a", 1)));
        assert!(outbound(&BusMsg::frp(trigger.clone()), me).is_some());
        // A peer's message goes to every other peer, never back to it.
        let theirs = BusMsg::from_peer(trigger.clone(), PeerId(2));
        assert!(outbound(&theirs, me).is_some());
        assert!(outbound(&theirs, PeerId(2)).is_none());
        // A turn goes out without the session's internal cause.
        let turn = BusMsg::Frp {
            msg: FrpMessage::new(
                "me",
                GolferTurn::new(None, Some(Club::Iron7), Handedness::Right, Mode::Full),
            ),
            peer: None,
            cause: Some("openconnect_client.0".into()),
        };
        let json = serde_json::to_string(outbound(&turn, me).expect("out")).expect("ser");
        assert!(
            !json.contains("cause") && !json.contains("openconnect_client"),
            "{json}"
        );
        let raw: FrpMessage = serde_json::from_str(
            r#"{"kind":"shot.raw","from":"lm1","key":{"shot_id":"a","shot_number":1},"frames":[]}"#,
        )
        .expect("raw");
        assert_eq!(raw.body.kind(), SHOT_RAW);
        assert!(outbound(&BusMsg::frp(raw), me).is_none());
        assert!(outbound(&BusMsg::local("web", LocalEvent::ClearShotHistory), me).is_none());
    }

    #[tokio::test]
    async fn the_greeting_is_each_local_actor_then_its_state_then_bus_sync() {
        let (state, _writer, _dir) = test_state();
        let me = state.root.flighthook_id();
        if let Ok(mut bus) = state.bus_state.lock() {
            bus.apply(&FrpMessage::new("lm1", BusActor::new("Range", 2.0)));
            bus.apply(&FrpMessage::new(
                me.clone(),
                GolferTurn::new(None, Some(Club::Driver), Handedness::Right, Mode::Full),
            ));
            bus.apply(&FrpMessage::new(
                me.clone(),
                BusActor::new("flighthook", 2.0),
            ));
        }
        let greeted = greeting(&state);
        let kinds: Vec<(String, &str)> = greeted
            .iter()
            .map(|m| (m.from.clone(), m.body.kind()))
            .collect();
        // A remote actor's state (`lm1`) is not greeted.
        let want = vec![
            (me.clone(), "bus.actor"),
            (me.clone(), "golfer.turn"),
            (me.clone(), "bus.sync"),
        ];
        assert_eq!(kinds, want);
    }

    /// A plain FRP 0.3.0 collector, read the way cyberflight's intake reads
    /// the bus (an SDK `ShotAggregator` over the raw WebSocket), gets the
    /// greeting and then each local shot, without `shot.raw`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_frp_collector_gets_the_greeting_and_each_shot() {
        use flightrelay::{Launch, ShotAggregator, ShotEvent, ShotFinished, ShotLaunch, Velocity};
        let (state, _writer, _dir) = test_state();
        let state = Arc::new(state);
        if let Ok(mut bus) = state.bus_state.lock() {
            bus.apply(&FrpMessage::new(
                state.root.flighthook_id(),
                BusActor::new("flighthook", 2.0),
            ));
        }
        let app = axum::Router::new()
            .route(flighthook::FRP_PATH, axum::routing::get(ws_upgrade))
            .with_state(Arc::clone(&state));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move { axum::serve(listener, app).await });
        let url = format!("ws://{addr}{}", flighthook::FRP_PATH);

        let (greeted_tx, greeted_rx) = std::sync::mpsc::channel();
        let collector = std::thread::spawn(move || {
            let (mut ws, _) = tungstenite::connect(url).expect("connect");
            let mut shots = ShotAggregator::default();
            let mut kinds = Vec::new();
            loop {
                let tungstenite::Message::Text(text) = ws.read().expect("read") else {
                    continue;
                };
                let msg: FrpMessage = serde_json::from_str(&text).expect("an FRP message");
                kinds.push(msg.body.kind().to_owned());
                if msg.body.kind() == "bus.sync" {
                    greeted_tx.send(()).expect("send");
                }
                if let Some(ShotEvent::Finished(shot)) = shots.feed(&msg) {
                    return (kinds, shot);
                }
            }
        });
        tokio::task::spawn_blocking(move || {
            greeted_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("greeted");
        })
        .await
        .expect("join");

        let key = ShotKey::new("shot-1", 1);
        let launch = Launch {
            launch_speed: Some(Velocity::MetersPerSecond(60.0)),
            ..Launch::default()
        };
        let raw: FrpMessage = serde_json::from_str(
            r#"{"kind":"shot.raw","from":"lm1","key":{"shot_id":"shot-1","shot_number":1},"frames":[]}"#,
        )
        .expect("raw");
        for msg in [
            FrpMessage::new("lm1", ShotTrigger::new(key.clone())),
            FrpMessage::new(
                "lm1",
                ShotLaunch {
                    key: key.clone(),
                    launch,
                },
            ),
            raw,
            FrpMessage::new("lm1", ShotFinished { key: key.clone() }),
        ] {
            let _ = state.bus_tx.send(BusMsg::frp(msg));
        }
        let (kinds, shot) = tokio::task::spawn_blocking(move || collector.join().expect("join"))
            .await
            .expect("join");
        assert_eq!(
            kinds,
            [
                "bus.actor",
                "bus.sync",
                "shot.trigger",
                "shot.launch",
                "shot.finished"
            ]
        );
        assert_eq!(shot.from, "lm1");
        assert_eq!(shot.key, key);
        assert_eq!(
            shot.launch.and_then(|l| l.launch_speed),
            Some(Velocity::MetersPerSecond(60.0))
        );
    }

    /// Serve `/frp` for `state` on a free local port; its URL.
    async fn serve(state: &Arc<WebState>) -> String {
        let app = axum::Router::new()
            .route(flighthook::FRP_PATH, axum::routing::get(ws_upgrade))
            .with_state(Arc::clone(state));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move { axum::serve(listener, app).await });
        format!("ws://{addr}{}", flighthook::FRP_PATH)
    }

    /// A blocking peer: connected, greeted (read up to the `bus.sync`).
    fn greeted_peer(
        url: &str,
    ) -> tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>> {
        let (mut ws, _) = tungstenite::connect(url).expect("connect");
        loop {
            if let tungstenite::Message::Text(text) = ws.read().expect("read")
                && text.contains("\"bus.sync\"")
            {
                return ws;
            }
        }
    }

    /// A peer's message goes onto the bus with its link and to every other
    /// peer, never back to it; one from this process's own actor id is
    /// dropped and the Web Server card says so.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_peers_message_reaches_the_bus_and_the_other_peers_but_not_itself() {
        let (state, _writer, _dir) = test_state();
        let state = Arc::new(state);
        let me = state.root.flighthook_id();
        let url = serve(&state).await;
        let mut bus = state.bus_tx.subscribe();
        let (tx, rx) = std::sync::mpsc::channel();
        let peers = std::thread::spawn(move || {
            let mut a = greeted_peer(&url);
            let mut b = greeted_peer(&url);
            a.send(tungstenite::Message::text(
                r#"{"kind":"shot.trigger","from":"remote1","key":{"shot_id":"r-1","shot_number":1}}"#,
            ))
            .expect("send");
            // B gets it.
            loop {
                if let tungstenite::Message::Text(text) = b.read().expect("read")
                    && text.contains("remote1")
                {
                    break;
                }
            }
            // A, its own sender, never does: B's own message is the next
            // thing A reads.
            b.send(tungstenite::Message::text(
                r#"{"kind":"shot.trigger","from":"remote2","key":{"shot_id":"r-2","shot_number":1}}"#,
            ))
            .expect("send");
            let first = loop {
                if let tungstenite::Message::Text(text) = a.read().expect("read") {
                    break text.to_string();
                }
            };
            // A message from this flighthook's own actor id comes back.
            a.send(tungstenite::Message::text(format!(
                r#"{{"kind":"shot.trigger","from":"{me}","key":{{"shot_id":"loop","shot_number":1}}}}"#
            )))
            .expect("send");
            tx.send(first).expect("send");
            // Keep both links open until the test has looked.
            let _ = a.read();
        });
        let mut seen = Vec::new();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while seen.len() < 2 {
            let msg = tokio::time::timeout_at(deadline, bus.recv())
                .await
                .expect("in time")
                .expect("bus");
            if let BusMsg::Frp {
                msg, peer: Some(_), ..
            } = msg
            {
                seen.push(msg.from);
            }
        }
        assert_eq!(seen, ["remote1", "remote2"]);
        let first = tokio::task::spawn_blocking(move || rx.recv().expect("first"))
            .await
            .expect("join");
        assert!(first.contains("remote2"), "{first}");
        // The looped message never reaches the bus; the guard flags the link.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !state.looped() {
            assert!(std::time::Instant::now() < deadline, "guard not raised");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        while let Ok(msg) = bus.try_recv() {
            if let BusMsg::Frp { msg, .. } = msg {
                assert_ne!(msg.from, state.root.flighthook_id(), "{msg:?}");
            }
        }
        drop(peers);
    }

    #[test]
    fn a_frame_from_an_own_actor_id_is_flagged_and_raw_or_junk_is_dropped() {
        let (state, _writer, _dir) = test_state();
        let me = state.root.flighthook_id();
        let own = format!(r#"{{"kind":"bus.sync","from":"{me}"}}"#);
        assert!(matches!(inbound(&state.root, &own, PeerId(9)), Inbound::OwnBack(f) if f == me));
        assert!(matches!(
            inbound(&state.root, "nope", PeerId(9)),
            Inbound::Nothing
        ));
        let raw =
            r#"{"kind":"shot.raw","from":"x","key":{"shot_id":"a","shot_number":1},"frames":[]}"#;
        assert!(matches!(
            inbound(&state.root, raw, PeerId(9)),
            Inbound::Nothing
        ));
        let sync = r#"{"kind":"bus.sync","from":"x"}"#;
        assert!(matches!(
            inbound(&state.root, sync, PeerId(9)),
            Inbound::Bus(m) if matches!(*m, BusMsg::Frp { peer: Some(PeerId(9)), .. })
        ));
    }

    /// Tournament mode holds a remote virtual launch monitor's shots (a
    /// Trackball served over FRP) at the hub, the whole shot from its
    /// trigger through its `shot.finished`; a measured remote monitor's
    /// shots, a shot begun before the switch, and any shot once it is off
    /// go through. A simulator's round alone holds nothing here.
    #[test]
    fn tournament_mode_holds_a_remote_virtual_monitors_shots() {
        let (state, _writer, _dir) = test_state();
        let root = &state.root;
        root.round
            .set_remote("tb00000000000001", Some(flighthook::Source::Virtual));
        root.round
            .set_remote("rd00000000000001", Some(flighthook::Source::Measured));
        let shot = |from: &str, id: &str| -> Vec<String> {
            let key = format!(r#""key":{{"shot_id":"{id}","shot_number":1}}"#);
            vec![
                format!(r#"{{"kind":"shot.trigger","from":"{from}",{key}}}"#),
                format!(
                    r#"{{"kind":"shot.launch","from":"{from}",{key},"launch":{{"launch_speed":"60.0mps"}}}}"#
                ),
                format!(r#"{{"kind":"shot.finished","from":"{from}",{key}}}"#),
            ]
        };
        let passed = |text: &str| matches!(inbound(root, text, PeerId(9)), Inbound::Bus(_));
        let all_pass = |texts: &[String]| texts.iter().all(|t| passed(t));
        let none_pass = |texts: &[String]| texts.iter().all(|t| !passed(t));

        // Off: everything goes through.
        assert!(all_pass(&shot("tb00000000000001", "a")));
        // A shot that began before tournament mode went on finishes.
        let early = shot("tb00000000000001", "b");
        assert!(passed(&early[0]));
        root.round.set_manual(true);
        assert!(all_pass(&early[1..]));
        // On: the virtual monitor's shot is held whole, the measured one's
        // and the virtual monitor's other messages go through.
        assert!(none_pass(&shot("tb00000000000001", "c")));
        assert!(all_pass(&shot("rd00000000000001", "d")));
        assert!(passed(r#"{"kind":"bus.sync","from":"tb00000000000001"}"#));
        // Off again: shots go through.
        root.round.set_manual(false);
        assert!(all_pass(&shot("tb00000000000001", "e")));

        // A simulator's round alone holds the virtual monitor's shot too,
        // and the measured one's still goes through.
        let round = root
            .round
            .begin_round(&crate::state::round::RoundRequest {
                holder: "Pureplay Connect",
                feeds: &[],
            })
            .expect("a round");
        assert!(none_pass(&shot("tb00000000000001", "f")));
        assert!(all_pass(&shot("rd00000000000001", "g")));
        // No round: it goes through again.
        drop(round);
        assert!(all_pass(&shot("tb00000000000001", "h")));
    }

    /// A peer message whose `from` is a local card key (here a mock
    /// monitor card with a generated actor id) is dropped like an own id.
    #[test]
    fn a_frame_from_a_local_card_key_is_flagged() {
        let (state, _writer, _dir) = test_state();
        state.root.system.update(|c| {
            c.mock_monitor.insert(
                "0".into(),
                flighthook::MockMonitorSection {
                    actor_id: Some("1234567890abcdef".into()),
                    ..flighthook::MockMonitorSection::default()
                },
            );
        });
        for from in ["mock_monitor.0", "system", flighthook::AUTO_CHIP_ACTOR] {
            let text = format!(r#"{{"kind":"bus.sync","from":"{from}"}}"#);
            assert!(
                matches!(inbound(&state.root, &text, PeerId(9)), Inbound::OwnBack(f) if f == from),
                "{from}"
            );
        }
    }
}
