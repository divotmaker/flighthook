use crate::app::{FlighthookApp, hand_code};
use crate::theme;
use crate::types::{LogEntry, Severity};
use flighthook::{FrpBody, UiLocalEvent, UiMessage};

use super::extract_time;

/// All message type keys in display order: the FRP kinds the UI socket
/// carries, flighthook's local events, and the store's shots.
pub(crate) const MESSAGE_TYPES: &[&str] = &[
    "alert_error",
    "alert_warn",
    "alert_critical",
    "shot.trigger",
    "shot.launch",
    "shot.flight",
    "shot.club",
    "shot.impact",
    "shot.finished",
    "shot",
    "bus.actor",
    "shot_video",
    "shot_video_removed",
    "shot_pose",
    "shot_swing",
    "golfer.turn",
    "golfer.select",
    "golfer.profile",
    "practice.block",
    "practice.new_block",
    "game.round",
    "game.ball",
    "game.stroke",
    "game.scores",
    "game.aim",
    "clear_shot_history",
    "card_status",
    "bus.sync",
    "bus.reply",
    "config_outcome",
    "card_keys",
    "round_state",
];

/// Filter groups for the dropdown.
const FILTER_GROUPS: &[(&str, &[&str])] = &[
    (
        "Launch Monitor",
        &[
            "shot.trigger",
            "shot.launch",
            "shot.flight",
            "shot.club",
            "shot.impact",
            "shot.finished",
            "shot",
            "bus.actor",
        ],
    ),
    (
        "Game",
        &[
            "golfer.turn",
            "golfer.select",
            "golfer.profile",
            "practice.block",
            "practice.new_block",
            "game.round",
            "game.ball",
            "game.stroke",
            "game.scores",
            "game.aim",
            "clear_shot_history",
        ],
    ),
    (
        "Camera",
        &[
            "shot_video",
            "shot_video_removed",
            "shot_pose",
            "shot_swing",
        ],
    ),
    (
        "System",
        &[
            "card_status",
            "bus.sync",
            "bus.reply",
            "config_outcome",
            "card_keys",
            "round_state",
        ],
    ),
    ("Alert", &["alert_error", "alert_warn", "alert_critical"]),
];

/// The Log tab's name for an alert of `severity`.
fn alert_type(severity: Severity) -> &'static str {
    match severity {
        Severity::Critical => "alert_critical",
        Severity::Error => "alert_error",
        Severity::Warn => "alert_warn",
    }
}

/// The message type key of `msg`: the FRP kind, the local event's kind,
/// `shot`, or `alert_<severity>`.
pub(crate) fn message_type(msg: &UiMessage) -> String {
    match msg {
        UiMessage::Snapshot(_) => "snapshot".into(),
        UiMessage::Frp(frp) => match &frp.body {
            FrpBody::BusAlert(alert) => alert_type(alert.severity).into(),
            body => body.kind().to_owned(),
        },
        UiMessage::Local(local) => match &local.event {
            UiLocalEvent::Alert { severity, .. } => alert_type(*severity).into(),
            event => serde_json::to_value(event)
                .ok()
                .and_then(|v| v["kind"].as_str().map(str::to_owned))
                .unwrap_or_default(),
        },
        UiMessage::Shot(_) => "shot".into(),
    }
}

/// The alert severity of `msg`, when it is an alert.
pub(crate) fn alert_severity(msg: &UiMessage) -> Option<Severity> {
    match msg {
        UiMessage::Frp(frp) => match &frp.body {
            FrpBody::BusAlert(alert) => Some(alert.severity),
            _ => None,
        },
        UiMessage::Local(local) => match &local.event {
            UiLocalEvent::Alert { severity, .. } => Some(*severity),
            _ => None,
        },
        _ => None,
    }
}

/// `#1 #2`: the shot numbers of `keys`.
fn shot_numbers(keys: &[crate::types::ShotKey]) -> String {
    keys.iter()
        .map(|k| format!("#{}", k.shot_number))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A one-line summary of `msg` for the Log tab.
pub(crate) fn event_debug(msg: &UiMessage) -> String {
    match msg {
        UiMessage::Snapshot(_) => "snapshot".into(),
        UiMessage::Shot(shot) => format!("shot #{} stored", shot.shot_number),
        UiMessage::Frp(frp) => frp_debug(&frp.body),
        UiMessage::Local(local) => local_debug(&local.event),
    }
}

fn frp_debug(body: &FrpBody) -> String {
    match body {
        FrpBody::ShotTrigger(m) => format!("trigger #{}", m.key.shot_number),
        FrpBody::ShotLaunch(m) => format!("launch #{}", m.key.shot_number),
        FrpBody::ShotFlight(m) => format!("flight #{} ({})", m.key.shot_number, m.flight.basis),
        FrpBody::ShotClub(m) => format!("club #{}", m.key.shot_number),
        FrpBody::ShotImpact(m) => format!("impact #{}", m.key.shot_number),
        FrpBody::ShotFinished(m) => format!("finished #{}", m.key.shot_number),
        FrpBody::BusActor(actor) => {
            if actor.is_clear() {
                format!("{}: ready", actor.name)
            } else {
                format!("{}: {}", actor.name, actor.blockers.join(", "))
            }
        }
        FrpBody::BusAlert(alert) => format!("[{}] {}", alert.severity, alert.message),
        FrpBody::BusReply(reply) => match &reply.error {
            None => format!("{} ok", reply.command),
            Some(error) => format!("{} refused: {error:?}", reply.command),
        },
        FrpBody::GolferTurn(turn) => {
            let parts: Vec<String> = [
                turn.mode.map(|m| format!("{m:?}")),
                turn.handedness.map(|h| format!("hand={}", hand_code(h))),
                turn.club.map(|c| format!("club={c}")),
            ]
            .into_iter()
            .flatten()
            .collect();
            parts.join(" ")
        }
        FrpBody::PracticeBlock(block) => {
            let club = block
                .club
                .map_or_else(|| "no club".to_owned(), |c| c.to_string());
            format!("set {} {club} (session {})", block.number, block.session)
        }
        FrpBody::PracticeNewBlock(block) if block.new_session => "new session".into(),
        FrpBody::PracticeNewBlock(_) => "new set".into(),
        FrpBody::Removed(removal) => format!("{} removed", removal.kind),
        body => body.kind().to_owned(),
    }
}

/// The Log line of the round `after`, which follows `before`: tournament
/// mode switched on or off reads as such; a simulator's round names its
/// holder, with tournament mode underneath when it is on.
pub(crate) fn round_line(
    before: &flighthook::RoundState,
    after: &flighthook::RoundState,
) -> String {
    let simulator = after.held.then_some(after.holder.as_deref()).flatten();
    match (before.manual != after.manual, after.manual, simulator) {
        (true, on, held) => {
            let word = if on { "on" } else { "off" };
            match held {
                Some(holder) => format!("tournament mode turned {word} (round held by {holder})"),
                None => format!("tournament mode turned {word}"),
            }
        }
        (false, true, Some(holder)) => format!("round held by {holder} (tournament mode on)"),
        (false, false, Some(holder)) => format!("round held by {holder}"),
        (false, true, None) => "tournament mode on".into(),
        (false, false, None) => "no round".into(),
    }
}

fn local_debug(event: &UiLocalEvent) -> String {
    match event {
        UiLocalEvent::CardStatus { status, .. } => format!("{status:?}"),
        UiLocalEvent::Alert { severity, message } => format!("[{severity}] {message}"),
        UiLocalEvent::RoundState { round } => round_line(round, round),
        UiLocalEvent::ConfigOutcome { request_id, .. } => match request_id {
            Some(rid) => format!("outcome({rid})"),
            None => "outcome".into(),
        },
        UiLocalEvent::CardKeys { card_keys } => format!("{} cards", card_keys.len()),
        UiLocalEvent::ClearShotHistory => "clear history".into(),
        UiLocalEvent::ShotVideo {
            keys,
            position,
            duration_ms,
            fps,
            complete,
            dropped,
            ..
        } => {
            let state = if *complete { "complete" } else { "incomplete" };
            #[allow(clippy::cast_precision_loss)] // a clip lasts seconds
            let seconds = *duration_ms as f64 / 1000.0;
            format!(
                "clip {}: {}, {seconds:.1} s at {fps:.0} fps, {state}, {dropped} dropped",
                shot_numbers(keys),
                position.label(),
            )
        }
        UiLocalEvent::ShotVideoRemoved { keys } => match keys.len() {
            1 => "clip of 1 shot removed".into(),
            n => format!("clips of {n} shots removed"),
        },
        UiLocalEvent::ShotPose { keys, .. } => {
            format!("body tracking of clip {} done", shot_numbers(keys))
        }
        UiLocalEvent::ShotSwing { keys } => {
            format!("swing analysis of clip {} done", shot_numbers(keys))
        }
        _ => String::new(),
    }
}

impl FlighthookApp {
    pub(crate) fn render_log_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if self.portrait {
                ui.spacing_mut().interact_size.y = crate::topbar::TOUCH_TARGET;
            }
            if ui.button("Clear").clicked() {
                self.log_entries.clear();
            }
            ui.checkbox(&mut self.log_auto_scroll, "Auto-scroll");
            ui.separator();

            // Filter dropdown button
            let enabled_count = self.log_type_filters.values().filter(|v| **v).count();
            let total_count = self.log_type_filters.len();
            let filter_label = if enabled_count == total_count {
                "Filters: all".to_string()
            } else {
                format!("Filters: {enabled_count}/{total_count}")
            };
            let popup_id = ui.make_persistent_id("log_filter_popup");
            let filter_btn = ui.button(&filter_label);
            if filter_btn.clicked() {
                self.log_filter_open = !self.log_filter_open;
            }
            if self.log_filter_open {
                let area = egui::Area::new(popup_id)
                    .order(egui::Order::Foreground)
                    .fixed_pos(filter_btn.rect.left_bottom())
                    .show(ui.ctx(), |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            // Toggle all
                            let all_checked = self.log_type_filters.iter().all(|(_, v)| *v);
                            let mut toggle_all = all_checked;
                            if ui.checkbox(&mut toggle_all, "All").clicked() {
                                for v in self.log_type_filters.values_mut() {
                                    *v = toggle_all;
                                }
                            }
                            ui.separator();

                            for &(group_name, keys) in FILTER_GROUPS {
                                // Group heading with toggle-all checkbox
                                let group_all = keys.iter().all(|k| {
                                    self.log_type_filters.get(*k).copied().unwrap_or(true)
                                });
                                let mut group_toggle = group_all;
                                let heading = egui::RichText::new(group_name).strong();
                                if ui.checkbox(&mut group_toggle, heading).clicked() {
                                    for &k in keys {
                                        if let Some(v) = self.log_type_filters.get_mut(k) {
                                            *v = group_toggle;
                                        }
                                    }
                                }

                                for &key in keys {
                                    if let Some(checked) = self.log_type_filters.get_mut(key) {
                                        ui.horizontal(|ui| {
                                            ui.add_space(16.0);
                                            ui.checkbox(checked, key);
                                        });
                                    }
                                }
                                ui.add_space(4.0);
                            }
                        });
                    });

                // Close popup when clicking outside it
                if ui.input(|i| i.pointer.any_click())
                    && !area.response.contains_pointer()
                    && !filter_btn.contains_pointer()
                {
                    self.log_filter_open = false;
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(format!("{} events", self.log_entries.len()));
            });
        });

        // A phone held upright scrolls down only and wraps each line at the
        // screen's edge.
        let scroll = if self.portrait {
            egui::ScrollArea::vertical()
        } else {
            egui::ScrollArea::both()
        };
        scroll
            .stick_to_bottom(self.log_auto_scroll)
            .auto_shrink(false)
            .show(ui, |ui| {
                // Dense rows: the log is a wall of monospace, not a form.
                ui.spacing_mut().interact_size.y = 14.0;
                if self.portrait {
                    self.render_log_wrapped(ui);
                } else {
                    self.render_log_grid(ui);
                }
            });
    }

    /// The entries the type filters let through.
    fn visible_log_entries(&self) -> impl Iterator<Item = &LogEntry> {
        self.log_entries.iter().filter(|entry| {
            self.log_type_filters
                .get(entry.message_type.as_str())
                .copied()
                .unwrap_or(true)
        })
    }

    /// Each entry as two columns-aligned rows: time, actor name, summary;
    /// then message type, actor id, raw payload.
    fn render_log_grid(&self, ui: &mut egui::Ui) {
        egui::Grid::new("log_grid")
            .num_columns(3)
            .min_col_width(0.0)
            .spacing(egui::vec2(12.0, 1.0))
            .show(ui, |ui| {
                for entry in self.visible_log_entries() {
                    let [time, actor, debug, kind, id, raw] = log_cells(entry);
                    ui.label(time);
                    ui.label(actor);
                    ui.label(debug);
                    ui.end_row();
                    ui.label(kind);
                    ui.label(id);
                    ui.label(raw);
                    ui.end_row();

                    // Spacer row between entries
                    ui.allocate_space(egui::vec2(0.0, 4.0));
                    ui.label("");
                    ui.label("");
                    ui.end_row();
                }
            });
    }

    /// The same two rows per entry, each wrapped at the panel's width.
    fn render_log_wrapped(&self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing = egui::vec2(12.0, 1.0);
        for entry in self.visible_log_entries() {
            let [time, actor, debug, kind, id, raw] = log_cells(entry);
            for row in [[time, actor, debug], [kind, id, raw]] {
                ui.horizontal_wrapped(|ui| {
                    for cell in row {
                        ui.add(egui::Label::new(cell).wrap());
                    }
                });
            }
            ui.add_space(4.0);
        }
    }
}

/// The six cells of a log entry: time, actor name and summary (colored by
/// alert severity), then message type, actor id and raw payload (dimmed).
fn log_cells(entry: &LogEntry) -> [egui::RichText; 6] {
    let mono = |text: &str| egui::RichText::new(text).monospace().size(11.0);
    let dim = theme::TEXT_MUTED;
    let debug = mono(&entry.event_debug);
    let debug = match entry.alert_severity {
        Some(Severity::Error | Severity::Critical) => debug.color(theme::DANGER),
        Some(Severity::Warn) => debug.color(theme::GOLD),
        None => debug,
    };
    let raw = if entry.raw.is_empty() {
        "(no payload)"
    } else {
        entry.raw.as_str()
    };
    [
        mono(&extract_time(&entry.timestamp)),
        mono(&entry.actor_name).color(theme::LOG_ACTOR),
        debug,
        mono(&entry.message_type).color(dim),
        mono(&entry.actor_id).color(dim),
        mono(raw).color(dim),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use flighthook::{BusMessage, ShotKey, UiLocal};

    fn local(event: UiLocalEvent) -> UiMessage {
        UiMessage::Local(Box::new(UiLocal {
            card: "camera.0".into(),
            event,
        }))
    }

    /// The manual holder reads as tournament mode switched on or off, never
    /// "round held by Manual"; a simulator's round names its holder.
    #[test]
    fn the_round_line_reads_plainly() {
        use flighthook::RoundState;
        let off = RoundState::default();
        let manual = RoundState {
            active: true,
            holder: Some("Manual".into()),
            manual: true,
            held: false,
        };
        let held = |manual: bool| RoundState {
            active: true,
            holder: Some("Pureplay Connect".into()),
            manual,
            held: true,
        };
        assert_eq!(round_line(&off, &manual), "tournament mode turned on");
        assert_eq!(round_line(&manual, &off), "tournament mode turned off");
        assert_eq!(round_line(&manual, &manual), "tournament mode on");
        assert_eq!(
            round_line(&off, &held(false)),
            "round held by Pureplay Connect"
        );
        assert_eq!(
            round_line(&manual, &held(true)),
            "round held by Pureplay Connect (tournament mode on)"
        );
        assert_eq!(
            round_line(&held(false), &held(true)),
            "tournament mode turned on (round held by Pureplay Connect)"
        );
        assert_eq!(round_line(&held(false), &off), "no round");

        // Through the app: the line follows the round it held before.
        let mut app = FlighthookApp::with_pending(crate::net::new_pending());
        app.handle_ui_message(local(UiLocalEvent::RoundState {
            round: manual.clone(),
        }));
        app.handle_ui_message(local(UiLocalEvent::RoundState { round: off }));
        let lines: Vec<&str> = app
            .log_entries
            .iter()
            .map(|e| e.event_debug.as_str())
            .collect();
        assert_eq!(
            lines,
            ["tournament mode turned on", "tournament mode turned off"]
        );
    }

    #[test]
    fn every_kind_is_in_exactly_one_filter_group() {
        for kind in MESSAGE_TYPES {
            let groups = FILTER_GROUPS
                .iter()
                .filter(|(_, kinds)| kinds.contains(kind))
                .count();
            assert_eq!(groups, 1, "{kind}");
        }
        let grouped: usize = FILTER_GROUPS.iter().map(|(_, k)| k.len()).sum();
        assert_eq!(grouped, MESSAGE_TYPES.len());
        // Every FRP kind is listed, but raw frames (never on the UI socket)
        // and alerts (listed by severity).
        for kind in flighthook::FRP_KINDS {
            let listed = !matches!(*kind, "shot.raw" | "bus.alert");
            assert_eq!(MESSAGE_TYPES.contains(kind), listed, "{kind}");
        }
    }

    #[test]
    fn frp_messages_go_by_their_kind_and_alerts_by_severity() {
        let turn = UiMessage::Frp(Box::new(BusMessage::new(
            "me",
            flighthook::GolferTurn::new(
                None,
                Some(flighthook::Club::Iron7),
                flighthook::Handedness::Left,
                flighthook::Mode::Full,
            ),
        )));
        assert_eq!(message_type(&turn), "golfer.turn");
        assert_eq!(event_debug(&turn), "Full hand=LH club=7I");
        let alert = UiMessage::Frp(Box::new(BusMessage::new(
            "lm",
            flightrelay_alert(Severity::Warn),
        )));
        assert_eq!(message_type(&alert), "alert_warn");
        assert_eq!(alert_severity(&alert), Some(Severity::Warn));
        let block = UiMessage::Frp(Box::new(BusMessage::new(
            "me",
            flighthook::PracticeNewBlock {
                r#ref: "1".into(),
                golfer: None,
                new_session: false,
            },
        )));
        assert_eq!(event_debug(&block), "new set");
    }

    fn flightrelay_alert(severity: Severity) -> flighthook::BusAlert {
        flighthook::BusAlert::new(severity, "low battery")
    }

    #[test]
    fn camera_clips_are_summarized_in_their_own_group() {
        let keys = vec![
            ShotKey {
                shot_id: "a".into(),
                shot_number: 4,
            },
            ShotKey {
                shot_id: "b".into(),
                shot_number: 5,
            },
        ];
        let removed = local(UiLocalEvent::ShotVideoRemoved { keys: keys.clone() });
        assert_eq!(message_type(&removed), "shot_video_removed");
        assert_eq!(event_debug(&removed), "clips of 2 shots removed");
        let swing = local(UiLocalEvent::ShotSwing { keys });
        assert_eq!(message_type(&swing), "shot_swing");
        assert_eq!(event_debug(&swing), "swing analysis of clip #4 #5 done");
        let clear = local(UiLocalEvent::ClearShotHistory);
        assert_eq!(message_type(&clear), "clear_shot_history");
    }
}
