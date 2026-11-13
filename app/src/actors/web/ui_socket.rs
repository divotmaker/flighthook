//! The UI socket `/api/ws` (see [`flighthook::UiMessage`] for the wire).
//!
//! The UI is part of the flighthook actor. On connect the server sends a
//! snapshot: every FRP state on the bus, each local card's status, the
//! round and the configuration generation. Then every bus message in bus
//! order, as `{"frp": ...}` or `{"local": ...}`, and `{"shot": ...}` each
//! time the store finishes or updates a shot.
//!
//! The UI's FRP commands (`golfer.select`, `practice.new_block`) go on the
//! bus from the flighthook actor, their `ref` prefixed with the
//! connection's tag (two windows share the actor). The matching `bus.reply`
//! goes back to that connection only, with the UI's own `ref`; other
//! connections never see it.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::broadcast;

use super::{WebState, actor_statuses, emit_telemetry};
use crate::actors::actor_names;
use crate::bus::{BusMsg, LocalEvent};
use crate::state::config;
use flighthook::{
    ConfigAction, UiCommand, UiLocal, UiLocalCommand, UiLocalEvent, UiMessage, UiSnapshot,
};
use flightrelay::{FrpBody, FrpMessage, Removal, StateKind};

/// The kind the UI never gets: raw device frames.
const SHOT_RAW: &str = "shot.raw";

/// GET /api/ws — upgrade to the UI socket.
pub async fn upgrade(
    State(state): State<Arc<WebState>>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| serve(socket, state))
}

/// Keep the bus state current from one bus message and pass it on to the
/// UI sockets. Called by the state updater for every message, in bus order.
pub(super) fn follow(state: &WebState, msg: &BusMsg) {
    let out = match msg {
        BusMsg::Frp { msg, .. } => {
            if let Ok(mut bus) = state.bus_state.lock() {
                bus.apply(msg);
            }
            if msg.body.kind() == SHOT_RAW {
                return;
            }
            UiMessage::Frp(Box::new(msg.clone()))
        }
        BusMsg::Local { card, event } => {
            let Some(ui) = ui_event(state, event) else {
                return;
            };
            let _ = state
                .ui_out
                .send(Arc::new(UiMessage::Local(Box::new(UiLocal {
                    card: card.clone(),
                    event: ui,
                }))));
            // A configuration change can add or remove cards: the UI gets
            // the card keys again.
            if matches!(event, LocalEvent::ConfigOutcome { .. }) {
                UiMessage::Local(Box::new(UiLocal {
                    card: card.clone(),
                    event: UiLocalEvent::CardKeys {
                        card_keys: state.root.card_keys(),
                    },
                }))
            } else {
                return;
            }
        }
    };
    let _ = state.ui_out.send(Arc::new(out));
}

/// Send the UI a shot the store finished or updated.
pub(super) fn send_shot(state: &WebState, data: flighthook::ShotData) {
    if data.has_data() {
        let _ = state.ui_out.send(Arc::new(UiMessage::Shot(Box::new(data))));
    }
}

/// Forget the gone remote actor `actor` ([`LocalEvent::ActorGone`]) and
/// tell the UI sockets with the actor's `bus.actor` removal.
pub(super) fn gone(state: &WebState, actor: &str) {
    if let Ok(mut bus) = state.bus_state.lock() {
        bus.forget_actor(actor);
    }
    let removal = FrpMessage::new(actor.to_owned(), Removal::of(StateKind::BusActor));
    let _ = state
        .ui_out
        .send(Arc::new(UiMessage::Frp(Box::new(removal))));
}

/// The UI's form of a local event; `None` for the ones it does not hear.
fn ui_event(state: &WebState, event: &LocalEvent) -> Option<UiLocalEvent> {
    Some(match event {
        LocalEvent::CardStatus { status, telemetry } => UiLocalEvent::CardStatus {
            status: *status,
            telemetry: telemetry.clone(),
        },
        LocalEvent::Alert { severity, message } => UiLocalEvent::Alert {
            severity: *severity,
            message: message.clone(),
        },
        LocalEvent::RoundState { round } => UiLocalEvent::RoundState {
            round: round.clone(),
        },
        LocalEvent::ConfigOutcome {
            request_id,
            restarted,
            stopped,
            started,
        } => UiLocalEvent::ConfigOutcome {
            request_id: request_id.clone(),
            restarted: restarted.clone(),
            stopped: stopped.clone(),
            started: started.clone(),
            config_generation: state.config_generation.fetch_add(1, Ordering::Relaxed) + 1,
        },
        LocalEvent::ClearShotHistory => UiLocalEvent::ClearShotHistory,
        LocalEvent::ShotVideo {
            keys,
            position,
            duration_ms,
            fps,
            trigger_offset_ms,
            impact_offset_ms,
            complete,
            dropped,
        } => UiLocalEvent::ShotVideo {
            keys: keys.clone(),
            position: *position,
            duration_ms: *duration_ms,
            fps: *fps,
            trigger_offset_ms: *trigger_offset_ms,
            impact_offset_ms: *impact_offset_ms,
            complete: *complete,
            dropped: *dropped,
        },
        LocalEvent::ShotVideoRemoved { keys } => {
            UiLocalEvent::ShotVideoRemoved { keys: keys.clone() }
        }
        LocalEvent::ShotPose { keys, rotation } => UiLocalEvent::ShotPose {
            keys: keys.clone(),
            rotation: *rotation,
        },
        LocalEvent::ShotSwing { keys } => UiLocalEvent::ShotSwing { keys: keys.clone() },
        _ => return None,
    })
}

/// The snapshot a new connection gets.
pub(super) async fn snapshot(state: &WebState) -> UiSnapshot {
    let states: Vec<FrpMessage> = state
        .bus_state
        .lock()
        .map(|bus| bus.states().cloned().collect())
        .unwrap_or_default();
    let card_keys = state.root.card_keys();
    let names = actor_names(&state.root.system.snapshot());
    let cards = actor_statuses(&*state.actors.read().await, &names, state.now_ms());
    UiSnapshot {
        actor_id: state.root.flighthook_id(),
        card_keys,
        states,
        cards,
        round: state.root.round.state(),
        config_generation: state.config_generation.load(Ordering::Relaxed),
    }
}

/// One connection's tag: the prefix of the `ref`s of its commands.
fn connection_tag() -> String {
    format!("ui.{}:", config::generate_id())
}

/// What `msg` becomes for the connection `tag`: a `bus.reply` to one of its
/// commands gets the UI's own `ref` back; a reply to another connection's
/// command is dropped.
fn for_connection(msg: &Arc<UiMessage>, tag: &str) -> Option<Arc<UiMessage>> {
    let UiMessage::Frp(frp) = &**msg else {
        return Some(Arc::clone(msg));
    };
    let FrpBody::BusReply(reply) = &frp.body else {
        return Some(Arc::clone(msg));
    };
    if let Some(own) = reply.r#ref.strip_prefix(tag) {
        let mut reply = reply.clone();
        reply.r#ref = own.to_owned();
        return Some(Arc::new(UiMessage::Frp(Box::new(FrpMessage::new(
            frp.from.clone(),
            reply,
        )))));
    }
    if reply.r#ref.starts_with("ui.") && reply.r#ref.contains(':') {
        return None;
    }
    Some(Arc::clone(msg))
}

/// Put one command from the connection `tag` on the bus. Unknown text and
/// FRP kinds other than `golfer.select` and `practice.new_block` are
/// ignored.
pub(super) fn handle_command(text: &str, tag: &str, state: &WebState) {
    let Ok(command) = serde_json::from_str::<UiCommand>(text) else {
        tracing::debug!("ui socket: ignored {text}");
        return;
    };
    match command {
        UiCommand::Frp(msg) => {
            let body = match msg.body {
                FrpBody::GolferSelect(mut select) => {
                    select.r#ref = format!("{tag}{}", select.r#ref);
                    FrpBody::from(select)
                }
                FrpBody::PracticeNewBlock(mut block) => {
                    block.r#ref = format!("{tag}{}", block.r#ref);
                    FrpBody::from(block)
                }
                other => {
                    tracing::debug!("ui socket: ignored {}", other.kind());
                    return;
                }
            };
            let _ = state.bus_tx.send(BusMsg::frp(FrpMessage::new(
                state.root.flighthook_id(),
                body,
            )));
        }
        UiCommand::Local(UiLocalCommand::ClearHistory) => {
            let _ = state.bus_tx.send(BusMsg::local(
                state.actor_id.clone(),
                LocalEvent::ClearShotHistory,
            ));
        }
        UiCommand::Local(UiLocalCommand::Grouping { mode }) => {
            let _ = state.bus_tx.send(BusMsg::local(
                state.actor_id.clone(),
                LocalEvent::ConfigCommand {
                    request_id: None,
                    action: Box::new(ConfigAction::SetShotGrouping { mode }),
                },
            ));
        }
        UiCommand::Local(other) => tracing::debug!("ui socket: ignored {other:?}"),
    }
}

/// The next message for the connection `tag` from its stream `out`;
/// `None` once the stream is closed. A connection that fell behind gets a
/// fresh snapshot instead of what it missed: `out` subscribes anew before
/// the snapshot is taken, so nothing falls between, and the stream goes on
/// from there.
async fn next_out(
    out: &mut broadcast::Receiver<Arc<UiMessage>>,
    state: &WebState,
    tag: &str,
) -> Option<Arc<UiMessage>> {
    loop {
        match out.recv().await {
            Ok(msg) => {
                if let Some(msg) = for_connection(&msg, tag) {
                    return Some(msg);
                }
            }
            Err(broadcast::error::RecvError::Closed) => return None,
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("ui socket: {tag} fell {n} messages behind, sending a snapshot");
                *out = state.ui_out.subscribe();
                return Some(Arc::new(UiMessage::Snapshot(Box::new(
                    snapshot(state).await,
                ))));
            }
        }
    }
}

async fn serve(socket: WebSocket, state: Arc<WebState>) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let tag = connection_tag();
    // Subscribed before the snapshot is taken, so nothing falls between.
    let mut out = state.ui_out.subscribe();
    let first = UiMessage::Snapshot(Box::new(snapshot(&state).await));
    let Ok(json) = serde_json::to_string(&first) else {
        return;
    };
    if ws_tx.send(Message::text(json)).await.is_err() {
        return;
    }
    state.ws_count.fetch_add(1, Ordering::Relaxed);
    emit_telemetry(&state, &state.bus_tx);
    tracing::info!("ui socket: {tag} connected");

    let send_tag = tag.clone();
    let send_state = Arc::clone(&state);
    let mut send_task = tokio::spawn(async move {
        while let Some(msg) = next_out(&mut out, &send_state, &send_tag).await {
            if let Ok(json) = serde_json::to_string(&*msg)
                && ws_tx.send(Message::text(json)).await.is_err()
            {
                return;
            }
        }
    });

    let recv_state = Arc::clone(&state);
    let recv_tag = tag.clone();
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_rx.next().await {
            match msg {
                Message::Text(text) => handle_command(&text, &recv_tag, &recv_state),
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    tokio::select! {
        _ = &mut send_task => recv_task.abort(),
        _ = &mut recv_task => send_task.abort(),
    }
    state.ws_count.fetch_sub(1, Ordering::Relaxed);
    emit_telemetry(&state, &state.bus_tx);
    tracing::info!("ui socket: {tag} disconnected");
}

#[cfg(test)]
mod tests {
    use super::super::tests::test_state;
    use super::*;
    use flighthook::{Club, ShotGrouping};
    use flightrelay::{BusReply, GolferSelect, PracticeNewBlock};

    fn reply(r#ref: &str) -> Arc<UiMessage> {
        let body = BusReply::ok("flighthook", "golfer.select", r#ref);
        Arc::new(UiMessage::Frp(Box::new(FrpMessage::new("session", body))))
    }

    fn reply_ref(msg: &UiMessage) -> &str {
        match msg {
            UiMessage::Frp(frp) => match &frp.body {
                FrpBody::BusReply(r) => &r.r#ref,
                _ => panic!("not a reply"),
            },
            _ => panic!("not frp"),
        }
    }

    /// A command goes on the bus from the flighthook actor with the
    /// connection's tag on its `ref`; the reply comes back to that
    /// connection only, with the UI's own `ref`.
    #[test]
    fn commands_carry_the_connection_tag_and_replies_go_back_only_to_it() {
        let (state, _writer, _dir) = test_state();
        let mut rx = state.bus_tx.subscribe();
        let (a, b) = (connection_tag(), connection_tag());
        assert_ne!(a, b);
        handle_command(
            r#"{"frp":{"kind":"golfer.select","from":"ui","ref":"7","club":"7I"}}"#,
            &a,
            &state,
        );
        let msg = rx.try_recv().expect("on the bus");
        let frp = msg.as_frp().expect("frp");
        assert_eq!(frp.from, state.root.flighthook_id());
        let FrpBody::GolferSelect(GolferSelect { r#ref, club, .. }) = &frp.body else {
            panic!("not a select: {frp:?}");
        };
        assert_eq!(r#ref, &format!("{a}7"));
        assert_eq!(*club, Some(Some(Club::Iron7)));

        let answer = reply(&format!("{a}7"));
        assert_eq!(reply_ref(&for_connection(&answer, &a).expect("own")), "7");
        assert!(
            for_connection(&answer, &b).is_none(),
            "another window's reply"
        );
        // A reply to anyone else's command goes to every window as it is.
        let other = reply("web-123");
        assert_eq!(
            reply_ref(&for_connection(&other, &b).expect("passes")),
            "web-123"
        );

        handle_command(
            r#"{"frp":{"kind":"practice.new_block","from":"x","ref":"n","new_session":true}}"#,
            &b,
            &state,
        );
        let msg = rx.try_recv().expect("on the bus");
        let FrpBody::PracticeNewBlock(PracticeNewBlock {
            r#ref, new_session, ..
        }) = &msg.as_frp().expect("frp").body
        else {
            panic!("not new_block");
        };
        assert_eq!(
            (r#ref.as_str(), *new_session),
            (format!("{b}n").as_str(), true)
        );
    }

    #[test]
    fn local_commands_and_unknown_text() {
        let (state, _writer, _dir) = test_state();
        let mut rx = state.bus_tx.subscribe();
        let tag = connection_tag();
        handle_command(r#"{"local":{"cmd":"clear_history"}}"#, &tag, &state);
        assert!(matches!(
            rx.try_recv().expect("sent").as_local(),
            Some((_, LocalEvent::ClearShotHistory))
        ));
        handle_command(
            r#"{"local":{"cmd":"grouping","mode":"by_club"}}"#,
            &tag,
            &state,
        );
        let msg = rx.try_recv().expect("sent");
        let Some((_, LocalEvent::ConfigCommand { action, .. })) = msg.as_local() else {
            panic!("not a config command");
        };
        assert!(matches!(
            **action,
            ConfigAction::SetShotGrouping {
                mode: ShotGrouping::ByClub
            }
        ));
        // Not a command the UI may send, or not a command at all.
        handle_command(
            r#"{"frp":{"kind":"shot.trigger","from":"x","shot_id":"a","shot_number":1}}"#,
            &tag,
            &state,
        );
        handle_command("nope", &tag, &state);
        handle_command(r#"{"cmd":"club","club":"7I"}"#, &tag, &state);
        assert!(rx.try_recv().is_err());
    }

    /// Every bus message reaches the UI stream (raw frames excepted), FRP
    /// states land in the snapshot, and a config outcome counts the
    /// generation up.
    #[tokio::test]
    async fn the_stream_follows_the_bus_and_the_snapshot_holds_its_states() {
        let (state, _writer, _dir) = test_state();
        let mut out = state.ui_out.subscribe();
        let select = FrpMessage::new("aaaa", flightrelay::BusActor::new("Sim", 2.0));
        follow(&state, &BusMsg::frp(select.clone()));
        follow(
            &state,
            &BusMsg::local(
                "system",
                LocalEvent::ConfigOutcome {
                    request_id: None,
                    restarted: vec![],
                    stopped: vec![],
                    started: vec!["mevo.0".into()],
                },
            ),
        );
        follow(
            &state,
            &BusMsg::local("mevo.0", LocalEvent::FindMat { target: "x".into() }),
        );

        assert!(matches!(&*out.try_recv().expect("frp"), UiMessage::Frp(m) if **m == select));
        let msg = out.try_recv().expect("local");
        let UiMessage::Local(local) = &*msg else {
            panic!("not local");
        };
        assert!(matches!(
            local.event,
            UiLocalEvent::ConfigOutcome {
                config_generation: 1,
                ..
            }
        ));
        let msg = out.try_recv().expect("card keys");
        let UiMessage::Local(local) = &*msg else {
            panic!("not local");
        };
        assert!(
            matches!(&local.event, UiLocalEvent::CardKeys { card_keys } if *card_keys == state.root.card_keys()),
            "{local:?}"
        );
        assert!(out.try_recv().is_err(), "find_mat is no UI event");

        let snap = snapshot(&state).await;
        assert_eq!(snap.states, vec![select]);
        assert_eq!(snap.actor_id, state.root.flighthook_id());
        assert_eq!(snap.config_generation, 1);
        assert!(!snap.round.active);
    }

    /// A connection that fell behind gets a fresh snapshot, then the
    /// stream from there on, without reconnecting.
    #[tokio::test]
    async fn a_lagging_connection_gets_a_fresh_snapshot_and_goes_on() {
        let (state, _writer, _dir) = test_state();
        let mut out = state.ui_out.subscribe();
        let msg = || {
            Arc::new(UiMessage::Local(Box::new(UiLocal {
                card: "web".into(),
                event: UiLocalEvent::ClearShotHistory,
            })))
        };
        for _ in 0..1100 {
            let _ = state.ui_out.send(msg());
        }
        let first = next_out(&mut out, &state, "ui.x:").await.expect("open");
        assert!(matches!(*first, UiMessage::Snapshot(_)), "{first:?}");
        // The backlog is gone; the next message is the next one sent.
        let _ = state.ui_out.send(Arc::new(UiMessage::Shot(Box::default())));
        let next = next_out(&mut out, &state, "ui.x:").await.expect("open");
        assert!(matches!(*next, UiMessage::Shot(_)), "{next:?}");
    }
}
