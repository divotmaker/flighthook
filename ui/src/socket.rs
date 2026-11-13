//! What the UI does with each message of the UI socket `/api/ws`
//! ([`UiMessage`]): it keeps a plain map of the bus's FRP states, takes
//! the turn from flighthook's own `golfer.turn`, card rows from each local
//! card's `bus.actor`, and the shots as the server's store sends them.

use std::collections::HashMap;

use chrono::{SecondsFormat, Utc};
use flighthook::{
    ActorStatus, ShotData, ShotKey, ShotVideo, UiLocal, UiLocalEvent, UiMessage, UiSnapshot,
};
use flighthook::{BusMessage as FrpMessage, FrpBody, StateKind};

use crate::app::{FlighthookApp, new_actor};
use crate::types::{LogEntry, ShotRow};

/// Log rows kept for the Log tab.
const MAX_LOG_ENTRIES: usize = 500;

/// Whether card `card` is an output card (Open Connect client, Pureplay
/// Connect, random club), whose `bus.actor` carries nothing for its card.
fn is_output_card(card: &str) -> bool {
    matches!(
        card.split_once('.').map(|(kind, _)| kind),
        Some("openconnect_client" | "pureplay" | "random_club")
    )
}

impl FlighthookApp {
    /// Handle one message of the UI socket.
    pub(crate) fn handle_ui_message(&mut self, msg: UiMessage) {
        self.log_ui(&msg);
        match msg {
            UiMessage::Snapshot(snap) => self.apply_snapshot(*snap),
            UiMessage::Frp(frp) => self.apply_frp(*frp),
            UiMessage::Local(local) => self.apply_local(*local),
            UiMessage::Shot(data) => self.apply_shot(*data),
        }
    }

    fn apply_snapshot(&mut self, snap: UiSnapshot) {
        // A later snapshot (the server's resync after this UI fell behind)
        // can follow missed shots and sets: fetch them again.
        if !self.flighthook_id.is_empty() {
            self.needs_shots_refresh = true;
            self.needs_groups_refresh = true;
            self.needs_status_refresh = true;
        }
        self.flighthook_id = snap.actor_id;
        self.card_keys = snap.card_keys;
        self.bus_states.clear();
        self.actors = snap.cards;
        for actor in self.actors.values_mut() {
            actor.received(self.now_ms);
        }
        self.round = snap.round;
        for msg in snap.states {
            self.apply_frp(msg);
        }
    }

    /// Keep the state map and act on the messages the UI shows.
    fn apply_frp(&mut self, msg: FrpMessage) {
        if let Some(key) = msg.state_key() {
            match &msg.body {
                FrpBody::Removed(r) if r.kind == StateKind::BusActor => {
                    self.bus_states.retain(|k, _| k.from != msg.from);
                }
                FrpBody::Removed(_) => {
                    self.bus_states.remove(&key);
                }
                _ => {
                    self.bus_states.insert(key, msg.clone());
                }
            }
        }
        match &msg.body {
            FrpBody::GolferProfile(profile) if !profile.name.trim().is_empty() => {
                self.golfer_names
                    .insert(profile.id.as_str().to_owned(), profile.name.clone());
            }
            FrpBody::BusActor(actor) => {
                if !self.card_keys.contains_key(&msg.from) && !actor.name.trim().is_empty() {
                    self.remote_names
                        .insert(msg.from.clone(), actor.name.trim().to_owned());
                }
                // An output card's `bus.actor` only names it: no rows.
                if let Some(card) = self.card_keys.get(&msg.from)
                    && !is_output_card(card)
                    && let Some(status) = self.actors.get_mut(card)
                {
                    for (k, v) in flighthook::bus_actor_rows(actor) {
                        if v.is_empty() {
                            status.telemetry.remove(&k);
                        } else {
                            status.telemetry.insert(k, v);
                        }
                    }
                }
            }
            FrpBody::GolferTurn(turn) if msg.from == self.flighthook_id => {
                self.apply_turn(turn);
            }
            // flighthook's golfers changed (a save, a hand picked in the
            // header): the Golfers card reads them again.
            FrpBody::GolferProfile(_) if msg.from == self.flighthook_id => {
                self.settings.request_reload();
            }
            FrpBody::Removed(r)
                if r.kind == StateKind::GolferProfile && msg.from == self.flighthook_id =>
            {
                self.settings.request_reload();
            }
            // A launch monitor's strike, local or remote: the pending row.
            FrpBody::ShotTrigger(trigger) => {
                if let Some(card) = self.shot_row_key(&msg.from) {
                    self.apply_trigger(card, &trigger.key);
                }
            }
            // The shot ended: a row still without values reads as no read.
            FrpBody::ShotFinished(finished) => {
                if let Some(card) = self.shot_row_key(&msg.from)
                    && let Some(row) = self
                        .shots
                        .iter_mut()
                        .rev()
                        .find(|r| r.shot_id == finished.key.shot_id && r.actor == card)
                    && !row.finished
                {
                    row.finished = true;
                    self.shots_rev += 1;
                }
            }
            // The store's sets follow the practice actor's blocks.
            FrpBody::PracticeBlock(_) if msg.from == self.flighthook_id => {
                self.needs_groups_refresh = true;
            }
            FrpBody::Removed(r)
                if r.kind == StateKind::PracticeBlock && msg.from == self.flighthook_id =>
            {
                self.needs_groups_refresh = true;
            }
            _ => {}
        }
    }

    fn apply_local(&mut self, local: UiLocal) {
        let UiLocal { card, event } = local;
        match event {
            UiLocalEvent::CardStatus { status, telemetry } => {
                self.apply_card_status(card, status, &telemetry);
            }
            UiLocalEvent::RoundState { round } => self.round = round,
            UiLocalEvent::ConfigOutcome {
                restarted, stopped, ..
            } => self.apply_config_outcome(&restarted, &stopped),
            UiLocalEvent::CardKeys { card_keys } => self.card_keys = card_keys,
            UiLocalEvent::ClearShotHistory => self.clear_shot_history(),
            UiLocalEvent::ShotVideo {
                keys,
                position,
                duration_ms,
                fps,
                trigger_offset_ms,
                impact_offset_ms,
                complete,
                dropped,
            } => {
                let video = ShotVideo {
                    camera: card,
                    position,
                    duration_ms,
                    fps,
                    trigger_offset_ms,
                    impact_offset_ms,
                    complete,
                    dropped,
                    pose: false,
                    swing: false,
                    rotation: 0,
                    analyzed_with: None,
                };
                self.apply_shot_video(&keys, &video);
            }
            UiLocalEvent::ShotVideoRemoved { keys } => self.apply_video_removed(&keys),
            UiLocalEvent::ShotPose { keys, rotation } => {
                self.apply_shot_pose(&card, &keys, rotation);
            }
            UiLocalEvent::ShotSwing { keys } => self.apply_shot_swing(&card, &keys),
            // Alerts are in the log.
            _ => {}
        }
    }

    /// The key of the shot rows of actor `from`: a local card's key, a
    /// remote launch monitor's actor id (as the store files its shots);
    /// `None` for any other actor.
    fn shot_row_key(&self, from: &str) -> Option<String> {
        if let Some(card) = self.card_keys.get(from) {
            return Some(card.clone());
        }
        let remote_monitor = from != self.flighthook_id
            && self.bus_states.values().any(|m| {
                m.from == from
                    && matches!(&m.body, FrpBody::BusActor(a)
                        if a.has_role(flighthook::roles::LAUNCH_MONITOR))
            });
        remote_monitor.then(|| from.to_owned())
    }

    /// The card `card` saw a strike: a pending row joins the list until
    /// the store sends the shot. The club shown is the one in play, or the
    /// putter when the store stamped the card's last shot a putt while
    /// another club was in play (a putting device).
    fn apply_trigger(&mut self, card: String, key: &flighthook::ShotKey) {
        if self
            .shots
            .iter()
            .any(|r| r.shot_id == key.shot_id && r.actor == card)
        {
            return;
        }
        let putting = self
            .shots
            .iter()
            .rev()
            .find(|r| r.actor == card && r.has_data())
            .is_some_and(|r| r.club_selected == Some(flighthook::Club::Putter));
        let club_selected = if putting {
            Some(flighthook::Club::Putter)
        } else {
            self.selected_club
        };
        // A new shot ends a view-only pick.
        self.view_hold = None;
        self.shots.push(ShotRow {
            golfer: self.turn_golfer.clone(),
            actor: card,
            shot_id: key.shot_id.clone(),
            shot_number: key.shot_number,
            launch: None,
            flight: None,
            club: None,
            impact: None,
            derived: None,
            club_selected,
            handed: self.handed,
            group: None,
            finished: false,
            video: Vec::new(),
        });
        self.shots_rev += 1;
    }

    /// A shot the store finished or updated replaces its row, or joins the
    /// list.
    fn apply_shot(&mut self, data: ShotData) {
        crate::app::keep_remote_name(&mut self.remote_names, &self.card_keys, &data);
        let named = crate::app::shot_golfer_name(&data);
        let row = ShotRow::from(data);
        let held = self
            .shots
            .iter()
            .rposition(|r| r.shot_id == row.shot_id && r.actor == row.actor);
        // An update of an older shot leaves the name a newer shot of the
        // same golfer gave.
        let newer = held.is_some_and(|at| {
            row.golfer.is_some() && self.shots[at + 1..].iter().any(|r| r.golfer == row.golfer)
        });
        if !newer && let Some((id, name)) = named {
            self.golfer_names.insert(id, name);
        }
        match held {
            Some(at) => self.shots[at] = row,
            None => {
                self.view_hold = None;
                self.shots.push(row);
            }
        }
        self.shots_rev += 1;
        self.needs_groups_refresh = true;
    }

    /// The card `card` reported its status and card rows.
    pub(crate) fn apply_card_status(
        &mut self,
        card: String,
        status: ActorStatus,
        telemetry: &HashMap<String, String>,
    ) {
        // The actor speaks for itself again.
        self.reconnect_errors.remove(&card);
        if status == ActorStatus::Starting {
            self.reconnect_started(&card);
        }
        let actor = self
            .actors
            .entry(card)
            .or_insert_with(|| new_actor(String::new()));
        actor.apply_status(status, telemetry, self.now_ms);
    }

    /// A configuration change was applied: stopped actors go (or show
    /// Disabled), restarted ones begin a new history, and the status, sets
    /// and settings are fetched again.
    pub(crate) fn apply_config_outcome(&mut self, restarted: &[String], stopped: &[String]) {
        // A stopped actor sends no further status: drop it, or, when it was
        // turned off, show it Disabled. A stopped Open Connect client then
        // leaves the club picker live.
        for id in stopped {
            match self.actors.remove(id) {
                Some(actor) if self.settings.turned_off(id) => {
                    self.actors.insert(
                        id.clone(),
                        flighthook::ActorStatusResponse::disabled(actor.name),
                    );
                }
                _ => {}
            }
        }
        // A restarted actor begins a new history.
        for id in restarted {
            if let Some(actor) = self.actors.get_mut(id) {
                actor.restart(self.now_ms);
            }
            self.reconnect_started(id);
        }
        // Card names and card keys come with the status, and a rename
        // restarts nothing: re-fetch it, so a status reply requested before
        // a stop is replaced too.
        self.needs_status_refresh = true;
        // The default view may have changed, and another client's save
        // changes the settings.
        self.needs_groups_refresh = true;
        self.settings.request_reload();
    }

    /// A camera's clip of the shots `keys`.
    pub(crate) fn apply_shot_video(&mut self, keys: &[ShotKey], video: &ShotVideo) {
        if crate::types::stamp_video(&mut self.shots, keys, video) {
            self.shots_rev += 1;
        }
    }

    /// Body tracking of `camera`'s clip of the shots `keys` is done.
    pub(crate) fn apply_shot_pose(&mut self, camera: &str, keys: &[ShotKey], rotation: u16) {
        if crate::types::mark_pose(&mut self.shots, keys, camera, rotation) {
            self.shots_rev += 1;
        }
        // The open players draw it once they have the keypoints.
        for player in [&mut self.replay, &mut self.shot_player]
            .into_iter()
            .flatten()
            .filter(|r| keys.iter().any(|k| k.shot_id == r.shot_id()))
        {
            player.pose_ready(camera);
        }
    }

    /// The swing analysis of `camera`'s clip of the shots `keys` is done.
    pub(crate) fn apply_shot_swing(&mut self, camera: &str, keys: &[ShotKey]) {
        if crate::types::mark_swing(&mut self.shots, keys, camera) {
            self.shots_rev += 1;
        }
        if let Some(player) = self
            .shot_player
            .as_mut()
            .filter(|r| keys.iter().any(|k| k.shot_id == r.shot_id()))
        {
            player.swing_ready(camera);
        }
    }

    /// The clips of the shots `keys` were deleted: the open players close.
    pub(crate) fn apply_video_removed(&mut self, keys: &[ShotKey]) {
        if crate::types::remove_video(&mut self.shots, keys) {
            self.shots_rev += 1;
        }
        let shown = |shot_id: &str| keys.iter().any(|k| k.shot_id == shot_id);
        if self
            .shot_player
            .as_ref()
            .is_some_and(|r| shown(r.shot_id()))
        {
            self.shot_player = None;
        }
        if self.replay.as_ref().is_some_and(|r| shown(r.shot_id())) {
            self.replay = None;
        }
    }

    /// One Log row per message (the snapshot excepted), with the message's
    /// JSON as its raw text.
    fn log_ui(&mut self, msg: &UiMessage) {
        use crate::panels::log::{alert_severity, event_debug, message_type};
        // A heartbeat (an actor's `bus.actor` as it was) is no row.
        if let UiMessage::Frp(frp) = msg
            && matches!(frp.body, FrpBody::BusActor(_))
            && frp
                .state_key()
                .and_then(|key| self.bus_states.get(&key))
                .is_some_and(|held| held.body == frp.body)
        {
            return;
        }
        let actor_id = match msg {
            UiMessage::Snapshot(_) => return,
            UiMessage::Frp(frp) => self
                .card_keys
                .get(&frp.from)
                .cloned()
                .unwrap_or_else(|| frp.from.clone()),
            UiMessage::Local(local) => local.card.clone(),
            UiMessage::Shot(data) => data.card_key().to_owned(),
        };
        let actor_name = self
            .actors
            .get(&actor_id)
            .map(|a| a.name.clone())
            .unwrap_or_default();
        self.log_entries.push(LogEntry {
            timestamp: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            actor_name,
            actor_id,
            message_type: message_type(msg),
            event_debug: match msg {
                // A round line says what changed from the round held now.
                UiMessage::Local(local) => match &local.event {
                    UiLocalEvent::RoundState { round } => {
                        crate::panels::log::round_line(&self.round, round)
                    }
                    _ => event_debug(msg),
                },
                _ => event_debug(msg),
            },
            raw: serde_json::to_string(msg).unwrap_or_default(),
            alert_severity: alert_severity(msg),
        });
        if self.log_entries.len() > MAX_LOG_ENTRIES {
            self.log_entries
                .drain(..self.log_entries.len() - MAX_LOG_ENTRIES);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net;
    use flighthook::{ActorStatusResponse, Club, GolferTurn, Handedness, Mode};
    use flighthook::{BusActor, Removal};

    fn app() -> FlighthookApp {
        FlighthookApp::with_pending(net::new_pending())
    }

    fn turn(club: Club) -> GolferTurn {
        GolferTurn::new(None, Some(club), Handedness::Left, Mode::Full)
    }

    /// Live shots of one game golfer id under two names: the newest shot's
    /// name wins, and an update of the older shot does not bring its name
    /// back.
    #[test]
    fn a_live_update_of_an_older_shot_keeps_the_newest_name() {
        let mut app = app();
        let id = "pureplay.local-3";
        let shot = |n: u32, name: &str| ShotData {
            shot_number: n,
            shot_id: Some(format!("s{n}")),
            turn: Some(GolferTurn::new(
                Some(id.parse().expect("id")),
                Some(Club::Iron7),
                Handedness::Right,
                Mode::Full,
            )),
            golfer_name: Some(name.into()),
            ..ShotData::default()
        };
        let name = |app: &FlighthookApp| {
            crate::app::golfer_name(None, &app.bus_states, &app.golfer_names, id)
        };
        app.handle_ui_message(UiMessage::Shot(Box::new(shot(1, "Joe"))));
        app.handle_ui_message(UiMessage::Shot(Box::new(shot(2, "Guest"))));
        assert_eq!(name(&app), "Guest");
        app.handle_ui_message(UiMessage::Shot(Box::new(shot(1, "Joe"))));
        assert_eq!(app.shots.len(), 2, "the update replaced its row");
        assert_eq!(name(&app), "Guest");
    }

    /// A removed golfer keeps the last `golfer.profile` name heard for it;
    /// a flighthook id never heard of is "Unknown golfer", never the id; a
    /// golfer saved without a name is "Golfer N".
    #[test]
    fn a_removed_golfer_keeps_its_last_profile_name() {
        let mut app = app();
        let id = "flighthook.0123456789abcdef0123456789abcdef";
        let profile = flighthook::GolferProfile::new(id.parse().unwrap(), "Eric");
        app.handle_ui_message(UiMessage::Frp(Box::new(FrpMessage::new("me", profile))));
        app.handle_ui_message(UiMessage::Frp(Box::new(FrpMessage::new(
            "me",
            Removal::of_id(flighthook::StateKind::GolferProfile, id),
        ))));
        assert!(app.bus_states.is_empty(), "the profile is gone");
        let name = |app: &FlighthookApp, config: Option<&flighthook::FlighthookConfig>, id| {
            crate::app::golfer_name(config, &app.bus_states, &app.golfer_names, id)
        };
        assert_eq!(name(&app, None, id), "Eric");
        assert_eq!(
            name(&app, None, "flighthook.fedcba9876543210fedcba9876543210"),
            crate::app::UNKNOWN_GOLFER
        );
        assert_eq!(name(&app, None, "pureplay.local-3"), "pureplay.local-3");
        let config = flighthook::FlighthookConfig {
            golfers: vec![
                flighthook::GolferRecord::new("flighthook.s", "Sam"),
                flighthook::GolferRecord::new("flighthook.b", " "),
            ],
            ..Default::default()
        };
        assert_eq!(name(&app, Some(&config), "flighthook.b"), "Golfer 2");
    }

    /// The snapshot sets the state map, the cards and the turn; only
    /// flighthook's own turn counts.
    #[test]
    fn the_snapshot_and_frp_states_drive_the_turn_and_card_rows() {
        let mut app = app();
        let mut monitor = BusActor::new("Range", 2.0);
        monitor.device = Some(flighthook::DeviceInfo {
            battery_pct: Some(80),
            ..Default::default()
        });
        app.handle_ui_message(UiMessage::Snapshot(Box::new(UiSnapshot {
            actor_id: "me".into(),
            card_keys: HashMap::from([("lm1".to_string(), "mevo.0".to_string())]),
            states: vec![
                FrpMessage::new("me", turn(Club::Iron7)),
                FrpMessage::new("lm1", monitor),
            ],
            cards: HashMap::from([(
                "mevo.0".to_string(),
                ActorStatusResponse::new("Range", ActorStatus::Connected),
            )]),
            ..Default::default()
        })));
        assert_eq!(app.selected_club, Some(Club::Iron7));
        assert_eq!(app.handed, Some(Handedness::Left));
        assert_eq!(app.actors["mevo.0"].telemetry["battery_pct"], "80");
        assert_eq!(app.actors["mevo.0"].telemetry["ready"], "true");
        assert_eq!(app.bus_states.len(), 2);

        // Another session's turn changes nothing.
        app.handle_ui_message(UiMessage::Frp(Box::new(FrpMessage::new(
            "other",
            turn(Club::Driver),
        ))));
        assert_eq!(app.selected_club, Some(Club::Iron7));
        app.handle_ui_message(UiMessage::Frp(Box::new(FrpMessage::new(
            "me",
            turn(Club::Driver),
        ))));
        assert_eq!(app.selected_club, Some(Club::Driver));

        // A gone actor's states go with it.
        app.handle_ui_message(UiMessage::Frp(Box::new(FrpMessage::new(
            "lm1",
            Removal::of(StateKind::BusActor),
        ))));
        assert!(app.bus_states.keys().all(|k| k.from != "lm1"));
        assert_eq!(app.log_entries.len(), 3, "the snapshot is no log row");
    }

    /// A local card's `shot.trigger` puts the pending row up at once, as
    /// before the shot's values exist; the store's shot fills it in and
    /// the pulse starts then. A remote actor's trigger adds nothing.
    /// A card added after the snapshot: the card keys sent after the
    /// configuration change let its first trigger put up a pending row.
    #[test]
    fn a_new_cards_first_trigger_shows_after_the_card_keys() {
        let mut app = app();
        let key = flighthook::ShotKey {
            shot_id: "n".into(),
            shot_number: 1,
        };
        feed::frp(&mut app, "lm2", flighthook::ShotTrigger::new(key.clone()));
        assert!(app.shots.is_empty(), "an unknown actor puts up no row");
        app.handle_ui_message(feed::local_msg(
            "system",
            UiLocalEvent::CardKeys {
                card_keys: HashMap::from([("lm2".to_string(), "mevo.1".to_string())]),
            },
        ));
        let key = flighthook::ShotKey {
            shot_id: "m".into(),
            shot_number: 2,
        };
        feed::frp(&mut app, "lm2", flighthook::ShotTrigger::new(key));
        assert_eq!(app.shots.len(), 1);
        assert_eq!(app.shots[0].actor, "mevo.1");
        assert!(!app.shots[0].finished, "pending");
    }

    #[test]
    fn a_trigger_puts_up_the_pending_row_and_the_store_fills_it() {
        let mut app = app();
        app.card_keys.insert("lm1".into(), "mevo.0".into());
        app.selected_club = Some(Club::Driver);
        let key = flighthook::ShotKey {
            shot_id: "a".into(),
            shot_number: 1,
        };
        let trigger = flighthook::ShotTrigger::new(key.clone());
        feed::frp(&mut app, "lm1", trigger.clone());
        assert_eq!(app.shots.len(), 1);
        let row = &app.shots[0];
        assert_eq!((row.actor.as_str(), row.shot_id.as_str()), ("mevo.0", "a"));
        assert!(!row.finished && !row.has_data(), "pending");
        assert_eq!(row.club_selected, Some(Club::Driver));
        // The pending row does not pulse: nothing to show yet.
        app.shot_focus
            .observe(&app.shots, crate::chart::ShotFilter::ALL, 1.0);
        assert_eq!(app.shot_focus.progress(1.0), None);
        // A repeat changes nothing; a remote actor that is no launch
        // monitor puts up no row.
        feed::frp(&mut app, "lm1", trigger.clone());
        feed::frp(&mut app, "remote", trigger);
        assert_eq!(app.shots.len(), 1);

        app.handle_ui_message(UiMessage::Shot(Box::new(ShotData {
            card: Some("mevo.0".into()),
            shot_id: Some("a".into()),
            shot_number: 1,
            launch: Some(flighthook::Launch::default()),
            ..Default::default()
        })));
        assert_eq!(app.shots.len(), 1, "the row filled in place");
        assert!(app.shots[0].finished && app.shots[0].has_data());
        app.shot_focus
            .observe(&app.shots, crate::chart::ShotFilter::ALL, 2.0);
        assert!(app.shot_focus.progress(2.0).is_some(), "pulsing");

        // A shot that ends with no values reads as no read.
        let missed = flighthook::ShotKey {
            shot_id: "b".into(),
            shot_number: 2,
        };
        feed::frp(
            &mut app,
            "lm1",
            flighthook::ShotTrigger::new(missed.clone()),
        );
        feed::frp(&mut app, "lm1", flighthook::ShotFinished { key: missed });
        assert_eq!(app.shots.len(), 2);
        assert!(app.shots[1].finished && !app.shots[1].has_data());
    }

    /// A remote launch monitor's trigger puts up the pending row, keyed by
    /// its actor id as the store files its shots; the store's shot fills
    /// it, and a finish with no values reads as no read.
    #[test]
    fn a_remote_launch_monitors_trigger_puts_up_the_pending_row() {
        let mut app = app();
        app.flighthook_id = "me".into();
        app.selected_club = Some(Club::Driver);
        let mut lm = BusActor::new("Far radar", 2.0);
        lm.roles = vec![flighthook::roles::LAUNCH_MONITOR.into()];
        feed::frp(&mut app, "far", lm);
        let key = flighthook::ShotKey {
            shot_id: "r1".into(),
            shot_number: 1,
        };
        feed::frp(&mut app, "far", flighthook::ShotTrigger::new(key.clone()));
        assert_eq!(app.shots.len(), 1);
        let row = &app.shots[0];
        assert_eq!((row.actor.as_str(), row.shot_id.as_str()), ("far", "r1"));
        assert!(!row.finished && !row.has_data(), "pending");
        assert_eq!(row.club_selected, Some(Club::Driver));
        app.handle_ui_message(UiMessage::Shot(Box::new(ShotData {
            from: "far".into(),
            shot_id: Some("r1".into()),
            shot_number: 1,
            launch: Some(flighthook::Launch::default()),
            ..Default::default()
        })));
        assert_eq!(app.shots.len(), 1, "the row filled in place");
        assert!(app.shots[0].has_data());
        let missed = flighthook::ShotKey {
            shot_id: "r2".into(),
            shot_number: 2,
        };
        feed::frp(
            &mut app,
            "far",
            flighthook::ShotTrigger::new(missed.clone()),
        );
        feed::frp(&mut app, "far", flighthook::ShotFinished { key: missed });
        assert_eq!(app.shots.len(), 2);
        assert!(app.shots[1].finished && !app.shots[1].has_data());
    }

    /// The Log tab gets a `bus.actor` row only when it differs from that
    /// actor's last one: heartbeats add none.
    #[test]
    fn heartbeats_add_no_log_rows() {
        let mut app = app();
        let rows = |app: &FlighthookApp| {
            app.log_entries
                .iter()
                .filter(|e| e.raw.contains("bus.actor"))
                .count()
        };
        app.handle_ui_message(feed::bus_actor_msg("lm1", &[("tilt", "1")]));
        assert_eq!(rows(&app), 1);
        for _ in 0..3 {
            app.handle_ui_message(feed::bus_actor_msg("lm1", &[("tilt", "1")]));
        }
        assert_eq!(rows(&app), 1, "heartbeats");
        app.handle_ui_message(feed::bus_actor_msg("lm1", &[("tilt", "2")]));
        assert_eq!(rows(&app), 2, "a change");
        app.handle_ui_message(feed::bus_actor_msg("lm2", &[("tilt", "2")]));
        assert_eq!(rows(&app), 3, "another actor");
    }

    /// A second snapshot (the server's resync after a lag) fetches the
    /// shots, sets and status again; the first one does not.
    #[test]
    fn a_resync_snapshot_fetches_the_history_again() {
        let mut app = app();
        let snap = || {
            UiMessage::Snapshot(Box::new(UiSnapshot {
                actor_id: "me".into(),
                ..UiSnapshot::default()
            }))
        };
        app.handle_ui_message(snap());
        assert!(!app.needs_shots_refresh && !app.needs_groups_refresh);
        app.handle_ui_message(snap());
        assert!(app.needs_shots_refresh && app.needs_groups_refresh && app.needs_status_refresh);
    }

    /// The store's shots replace their rows by shot id and device.
    #[test]
    fn shots_come_from_the_store() {
        let mut app = app();
        let shot = |carry: Option<f64>| ShotData {
            card: Some("mevo.0".into()),
            shot_id: Some("a".into()),
            shot_number: 1,
            turn: Some(flighthook::GolferTurn {
                golfer: None,
                club: carry.map(|_| Club::Iron7),
                handedness: None,
                mode: None,
            }),
            ..Default::default()
        };
        app.handle_ui_message(UiMessage::Shot(Box::new(shot(None))));
        app.handle_ui_message(UiMessage::Shot(Box::new(shot(Some(1.0)))));
        assert_eq!(app.shots.len(), 1);
        assert_eq!(app.shots[0].club_selected, Some(Club::Iron7));
        assert!(app.shots[0].finished);
        // A device that reconnected counts from 1 again: a new shot id is a
        // new row.
        app.handle_ui_message(UiMessage::Shot(Box::new(ShotData {
            shot_id: Some("b".into()),
            ..shot(None)
        })));
        assert_eq!(app.shots.len(), 2);
        assert_eq!(app.shots[0].club_selected, Some(Club::Iron7));
    }

    /// Card statuses and clear history arrive as local events.
    #[test]
    fn local_events() {
        let mut app = app();
        app.handle_ui_message(UiMessage::Local(Box::new(UiLocal {
            card: "mevo.0".into(),
            event: UiLocalEvent::CardStatus {
                status: ActorStatus::Connected,
                telemetry: HashMap::from([("radar_mode".to_string(), "x".to_string())]),
            },
        })));
        assert_eq!(app.actors["mevo.0"].status, ActorStatus::Connected);
        app.handle_ui_message(UiMessage::Shot(Box::new(ShotData {
            card: Some("mevo.0".into()),
            shot_id: Some("a".into()),
            shot_number: 1,
            ..Default::default()
        })));
        app.handle_ui_message(UiMessage::Local(Box::new(UiLocal {
            card: "web".into(),
            event: UiLocalEvent::ClearShotHistory,
        })));
        assert!(app.shots.is_empty());
        assert_eq!(app.log_entries[0].message_type, "card_status");
    }

    /// An output card's `bus.actor` (no role) adds no rows to its card.
    #[test]
    fn an_output_cards_bus_actor_adds_no_rows() {
        let mut app = app();
        app.card_keys
            .insert("gs1".into(), "openconnect_client.0".into());
        app.actors.insert(
            "openconnect_client.0".into(),
            ActorStatusResponse::new("GSPro", ActorStatus::Connected),
        );
        app.handle_ui_message(UiMessage::Frp(Box::new(FrpMessage::new(
            "gs1",
            BusActor::new("GSPro", 2.0),
        ))));
        assert!(app.actors["openconnect_client.0"].telemetry.is_empty());
    }
}

/// Test helpers: feed the app UI socket messages.
#[cfg(test)]
pub(crate) mod feed {
    use super::*;

    /// The status message of `card`.
    pub(crate) fn card_msg(
        card: &str,
        status: ActorStatus,
        telemetry: &[(&str, &str)],
    ) -> UiMessage {
        local_msg(
            card,
            UiLocalEvent::CardStatus {
                status,
                telemetry: telemetry
                    .iter()
                    .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                    .collect(),
            },
        )
    }

    /// A local event message of `card`.
    pub(crate) fn local_msg(card: &str, event: UiLocalEvent) -> UiMessage {
        UiMessage::Local(Box::new(UiLocal {
            card: card.into(),
            event,
        }))
    }

    /// A `bus.actor` from `from` with `telemetry`.
    pub(crate) fn bus_actor_msg(from: &str, telemetry: &[(&str, &str)]) -> UiMessage {
        let mut actor = flighthook::BusActor::new("device", 2.0);
        actor.telemetry = Some(
            telemetry
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        );
        UiMessage::Frp(Box::new(FrpMessage::new(from, actor)))
    }

    /// The store's shot `key` of `actor`, with no values yet.
    pub(crate) fn shot_msg(actor: &str, key: ShotKey) -> UiMessage {
        UiMessage::Shot(Box::new(ShotData {
            card: Some(actor.into()),
            shot_id: Some(key.shot_id),
            shot_number: key.shot_number,
            ..Default::default()
        }))
    }

    /// A shot the store finished or updated.
    pub(crate) fn shot(app: &mut FlighthookApp, data: ShotData) {
        app.handle_ui_message(UiMessage::Shot(Box::new(data)));
    }

    /// An FRP message from `from`.
    pub(crate) fn frp(app: &mut FlighthookApp, from: &str, body: impl Into<FrpBody>) {
        app.handle_ui_message(UiMessage::Frp(Box::new(FrpMessage::new(from, body))));
    }
}
