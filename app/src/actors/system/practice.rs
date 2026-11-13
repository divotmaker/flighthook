//! The practice actor's part of the system actor: follow the turn and shots, answer
//! `practice.new_block`, publish the blocks.

use super::*;

/// The practice actor's part of the system actor: follow the session's own
/// turn, count every `shot.trigger` for the golfer up's block, answer
/// `practice.new_block` (after publishing what it changed) and republish
/// the open blocks on `bus.sync`.
pub(crate) fn handle_practice(msg: &BusMsg, practice: &mut Practice, sender: &BusSender) {
    if let Some(own) = sender.frp_id()
        && let Some((turn, _)) = msg.turn_of(own)
    {
        publish_blocks(practice.turn(turn.clone(), now_ms()), sender);
        return;
    }
    if let Some((_, LocalEvent::ActorGone { actor })) = msg.as_local() {
        publish_blocks(practice.gone(actor), sender);
        return;
    }
    let Some(frp) = msg.as_frp() else {
        return;
    };
    publish_blocks(practice.expire(now_ms()), sender);
    let from = frp.from.as_str();
    match &frp.body {
        FrpBody::ShotTrigger(trigger) => {
            practice.trigger(&trigger.key.shot_id, from, now_ms());
        }
        FrpBody::ShotLaunch(m) => practice.data(&m.key.shot_id, from, false),
        FrpBody::ShotFlight(m) => practice.data(&m.key.shot_id, from, true),
        FrpBody::ShotClub(m) => practice.data(&m.key.shot_id, from, false),
        FrpBody::ShotImpact(m) => practice.data(&m.key.shot_id, from, false),
        FrpBody::ShotFinished(m) => {
            publish_blocks(practice.finished(&m.key.shot_id, from), sender);
        }
        FrpBody::PracticeNewBlock(command) => {
            publish_blocks(
                practice.new_block(command.golfer.clone(), command.new_session, now_ms()),
                sender,
            );
            sender.frp(BusReply::ok(
                frp.from.clone(),
                "practice.new_block",
                command.r#ref.clone(),
            ));
        }
        FrpBody::BusSync(_) => {
            for block in practice.open_blocks() {
                sender.frp(block);
            }
        }
        _ => {}
    }
}

/// Publish what the practice actor changed, in order.
pub(crate) fn publish_blocks(changes: Vec<BlockChange>, sender: &BusSender) {
    for change in changes {
        match change {
            BlockChange::Open(block) => sender.frp(block),
            BlockChange::Closed(id) => sender.frp(flightrelay::Removal::of_id(
                flightrelay::StateKind::PracticeBlock,
                id,
            )),
        }
    }
}
