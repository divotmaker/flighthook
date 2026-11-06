//! egui application — FlighthookApp.

use std::collections::{HashMap, HashSet};

use crate::battery;
use crate::chart::{Cached, ChartKey, MembersKey};
use crate::groups::{self, GroupEvent, ShotGroups};
use crate::net::{self, PendingHandle, WsPollEvent};
use crate::panels::devices::{CardModes, mark};
use crate::panels::dispersion::{self, DispersionKey, EllipseSize, Points};
use crate::panels::settings::{PendingRemoval, SettingsForm};
use crate::panels::shots::{ShotFocus, ShotsLayout, ShotsView};
use crate::panels::{Tab, flight};
use crate::scale::{self, UiScale};
use crate::theme;
use crate::types::{
    ActorStatus, ActorStatusResponse, CameraListResponse, Club, FlighthookEvent, FlighthookMessage,
    GameStateSnapshot, Handedness, LogEntry, MevoSection, OpenConnectClientSection,
    OpenConnectServerSection, R10Section, ShotDetectionMode, ShotRow, ShotVideo, SquareSection,
    UnitSystem,
};
use chrono::{SecondsFormat, Utc};

const API_DOCS_MD: &str = include_str!("../../docs/API.md");

/// Longest a reconnect answered "starts later" keeps its Reconnect
/// disabled while the actor is not seen starting again.
pub(crate) const RECONNECT_WAIT_MS: u64 = 60_000;

/// The card's reason when a reconnect answered "starts later" has not
/// started within [`RECONNECT_WAIT_MS`].
pub(crate) const RECONNECT_UNFINISHED: &str = "the reconnect has not finished";

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

pub(crate) fn new_actor(name: String) -> ActorStatusResponse {
    ActorStatusResponse::new(name, ActorStatus::Disconnected)
}

pub struct FlighthookApp {
    // Unified actor state
    pub(crate) actors: HashMap<String, ActorStatusResponse>,
    /// The frame's time in milliseconds (`egui` input time): the clock of
    /// the actors' connection histories.
    pub(crate) now_ms: u64,

    // Data
    pub(crate) shots: Vec<ShotRow>,

    // Global state
    pub(crate) current_mode: String,
    /// Club in play: game state's club, or the user's own pick until the
    /// server echoes it.
    pub(crate) selected_club: Option<Club>,
    /// A club picked over REST that no `/api/status` reply has carried yet.
    pub(crate) club_pick: Option<Club>,
    pub(crate) club_picker_open: bool,
    /// The Auto-chip distance (yards) sent and not answered yet.
    pub(crate) auto_chip_in_flight: Option<u32>,
    /// Why the last Auto-chip was not sent, shown in the error banner until
    /// the next one.
    pub(crate) auto_chip_error: Option<String>,
    /// Player handedness in game state (`None` until the server reports
    /// one), or the user's own pick until the server echoes it. Stamped on
    /// shots as they trigger, like the club.
    pub(crate) handed: Option<Handedness>,
    /// A hand picked over REST that no `/api/status` reply has carried yet.
    pub(crate) hand_pick: Option<Handedness>,

    // Shot groups of the webserver the UI talks to
    pub(crate) groups: ShotGroups,
    /// Set after a `shot_group` found the rows out of step, until one finds
    /// them in step again, so a mismatch the refetch cannot fix refetches
    /// only once.
    groups_out_of_step: bool,

    // Settings
    pub(crate) settings: SettingsForm,

    // Networking
    pub(crate) pending: PendingHandle,
    pub(crate) ws_sender: Option<ewebsock::WsSender>,
    pub(crate) ws_receiver: Option<ewebsock::WsReceiver>,

    // UI state
    pub(crate) ws_connected: bool,
    pub(crate) ws_ever_connected: bool,
    pub(crate) active_tab: Tab,
    /// The side each device card on the Devices tab shows.
    pub(crate) card_modes: CardModes,
    /// Actors whose reconnect is in flight: their Reconnect waits until
    /// the request fails or the new instance has started.
    pub(crate) reconnecting: HashSet<String>,
    /// Reconnects answered "starts later": since when (ms), until the
    /// actor is seen starting again.
    pub(crate) reconnect_later: HashMap<String, u64>,
    /// Actors in flight seen starting again before their reply came.
    reconnect_seen: HashSet<String>,
    /// Why an actor's last reconnect request failed, shown as its card's
    /// `error` until the actor's next status or the next press.
    pub(crate) reconnect_errors: HashMap<String, String>,
    pub(crate) confirm_remove: Option<PendingRemoval>,
    /// The Clear history confirmation is open.
    pub(crate) confirm_clear: bool,
    pub(crate) show_api_docs: bool,
    pub(crate) api_docs_cache: egui_commonmark::CommonMarkCache,

    // Shots tab
    pub(crate) shots_view: ShotsView,
    pub(crate) shot_focus: ShotFocus,
    /// Bumped at every change to `shots`; keys the caches derived from it.
    pub(crate) shots_rev: u64,
    /// `shots` converted to [`FlighthookApp::units`], for the list, cards and table.
    pub(crate) display_rows: Cached<(u64, UnitSystem), Vec<ShotRow>>,
    /// Distinct actor ids in `shots`, exact and sorted.
    pub(crate) device_ids: Cached<u64, Vec<String>>,
    /// Device filter: one exact actor id, or every device (`None`). It
    /// applies to everything on the Shots tab and lasts for the session.
    pub(crate) device_filter: Option<String>,
    /// Indices into `shots` of the rows `device_filter` passes.
    pub(crate) filtered_rows: Cached<(u64, Option<String>), Vec<usize>>,
    /// Indices into `shots` of the view's members around the shown shot,
    /// after `device_filter`.
    pub(crate) view_members: Cached<MembersKey, Vec<usize>>,
    /// The Flight panel's projected geometry.
    pub(crate) flight_scene: Cached<ChartKey, flight::Scene>,
    /// What the Dispersion panel plots: rest positions or landings.
    pub(crate) dispersion_points: Points,
    /// Size of the Dispersion panel's ellipses.
    pub(crate) ellipse_size: EllipseSize,
    /// The Dispersion panel's projected geometry.
    pub(crate) dispersion_scene: Cached<DispersionKey, dispersion::Scene>,

    /// Generation of the shot history: counted up by every Clear history
    /// and every new session. Shot and set fetches carry the generation they
    /// were requested under, so a reply from before either is never applied
    /// over it.
    pub(crate) history_generation: u64,
    /// `history_generation` as of the last Clear history.
    cleared_generation: u64,

    // Deferred refresh
    /// Status, shots and sets have been requested once, on the first frame.
    started: bool,
    pub(crate) needs_status_refresh: bool,
    pub(crate) needs_groups_refresh: bool,
    pub(crate) needs_shots_refresh: bool,

    // Log state
    pub(crate) log_entries: Vec<LogEntry>,
    pub(crate) log_auto_scroll: bool,
    pub(crate) log_type_filters: HashMap<String, bool>,
    pub(crate) log_filter_open: bool,

    // Setup wizard
    pub(crate) wizard_checked: bool,
    pub(crate) wizard_dismissed: bool,
    pub(crate) wizard_mevo: bool,
    pub(crate) wizard_r10: bool,
    pub(crate) wizard_square: bool,
    pub(crate) wizard_uneekor: bool,
    pub(crate) wizard_gspro: bool,
    pub(crate) wizard_saving: bool,
    /// Why the last Get Started failed.
    pub(crate) wizard_error: Option<String>,

    /// The UI zoom: fitted to the window every frame, plus the session's
    /// Ctrl/Cmd +/- steps.
    ui_scale: UiScale,

    // Cameras and replay
    /// The server can record from cameras (`/api/status`).
    pub(crate) cameras_supported: bool,
    /// The folder the server writes clips to (`/api/status`).
    pub(crate) clip_dir: Option<String>,
    /// Why the server's config file was not used (`/api/status`): the
    /// error frame over every tab shows it until a save replaces the file.
    pub(crate) config_error: Option<crate::types::ConfigLoadError>,
    /// The camera list for the camera cards' pickers.
    pub(crate) camera_list: CameraList,
    /// Each camera card's still frame, by actor ID.
    pub(crate) stills: HashMap<String, crate::panels::replay::Still>,
    /// The open replay view.
    pub(crate) replay: Option<crate::panels::replay::ReplayView>,
    /// Number of the last replay or camera card clip opened: replies for
    /// another are dropped.
    pub(crate) replay_seq: u64,
    /// What each card in the Cards view's row beside the range bars shows.
    pub(crate) card_row: crate::panels::card_row::CardRow,
    /// The camera cards' clips of the shown shot, by camera actor ID.
    pub(crate) card_clips: HashMap<String, crate::panels::replay::CardClip>,
    /// The Shots tab shows the shots' data or the shown shot's cameras
    /// (the Data | Cameras toggle), kept on this device.
    pub(crate) shots_pane: crate::panels::cameras::ShotsPane,
    /// Live is on (the Cameras pane's Live switch): the pane shows the
    /// live views.
    pub(crate) live_selected: bool,
    /// The Cameras pane was on screen at the last
    /// [`FlighthookApp::sync_cameras_pane`]: coming on screen picks Live or
    /// the latest shot.
    pub(crate) cameras_pane_was_shown: bool,
    /// The Cameras pane's player of the shown shot's clips.
    pub(crate) shot_player: Option<crate::panels::replay::ReplayView>,
    /// The Cameras pane's live views, by camera actor ID, while Live is on
    /// screen.
    pub(crate) live: crate::panels::cameras::LiveViews,
    /// Number of the last live view opened: replies for another are
    /// dropped.
    pub(crate) live_seq: u64,
    /// The skeleton is drawn over camera pictures (the Cameras pane's `B`),
    /// kept on this device.
    pub(crate) pose_overlay: bool,
    /// The body tracking values the Live keys set, and their saves.
    pub(crate) tuning: crate::panels::cameras::Tuning,
    /// Body tracking's model download prompt: the dialog and the overlay.
    pub(crate) body_models: crate::panels::body_models::ModelsPrompt,
}

/// The server's camera list, as the camera cards' pickers use it.
#[derive(Default)]
pub(crate) struct CameraList {
    pub(crate) list: Option<CameraListResponse>,
    /// Why the last fetch failed.
    pub(crate) error: Option<String>,
    /// A fetch is out.
    pub(crate) loading: bool,
}

impl FlighthookApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::install(&cc.egui_ctx);
        scale::install(&cc.egui_ctx);
        let pending = net::new_pending();

        // Settings at once, for wizard detection. Status, shots and sets are
        // fetched on the first update(), with the WebSocket connection,
        // which is deferred to give the page time to stabilise on iOS
        // Safari before opening a second connection.
        net::fetch_settings(&cc.egui_ctx, &pending);

        Self::with_pending(pending)
    }

    /// An app with no data yet, draining REST results from `pending`.
    pub(crate) fn with_pending(pending: PendingHandle) -> Self {
        Self {
            actors: HashMap::new(),
            now_ms: 0,
            shots: Vec::new(),
            current_mode: "full".into(),
            selected_club: None,
            club_pick: None,
            club_picker_open: false,
            auto_chip_in_flight: None,
            auto_chip_error: None,
            handed: None,
            hand_pick: None,
            groups: ShotGroups::default(),
            groups_out_of_step: false,
            settings: SettingsForm::default(),
            pending,
            ws_sender: None,
            ws_receiver: None,
            ws_connected: false,
            ws_ever_connected: false,
            active_tab: Tab::Devices,
            card_modes: CardModes::default(),
            reconnecting: HashSet::new(),
            reconnect_later: HashMap::new(),
            reconnect_seen: HashSet::new(),
            reconnect_errors: HashMap::new(),
            confirm_remove: None,
            confirm_clear: false,
            shots_view: ShotsView::default(),
            shot_focus: ShotFocus::default(),
            shots_rev: 0,
            display_rows: Cached::default(),
            device_ids: Cached::default(),
            device_filter: None,
            filtered_rows: Cached::default(),
            view_members: Cached::default(),
            flight_scene: Cached::default(),
            dispersion_points: Points::default(),
            ellipse_size: EllipseSize::default(),
            dispersion_scene: Cached::default(),
            history_generation: 0,
            cleared_generation: 0,
            started: false,
            needs_status_refresh: false,
            needs_groups_refresh: false,
            needs_shots_refresh: false,
            show_api_docs: false,
            api_docs_cache: egui_commonmark::CommonMarkCache::default(),
            log_entries: Vec::new(),
            log_auto_scroll: true,
            log_type_filters: crate::panels::log::MESSAGE_TYPES
                .iter()
                .map(|&k| (k.to_string(), true))
                .collect(),
            log_filter_open: false,
            wizard_checked: false,
            wizard_dismissed: false,
            wizard_mevo: false,
            wizard_r10: false,
            wizard_square: false,
            wizard_uneekor: false,
            wizard_gspro: false,
            wizard_saving: false,
            wizard_error: None,
            ui_scale: UiScale::default(),
            cameras_supported: false,
            clip_dir: None,
            config_error: None,
            camera_list: CameraList::default(),
            stills: HashMap::new(),
            replay: None,
            replay_seq: 0,
            card_row: crate::panels::card_row::CardRow::default(),
            card_clips: HashMap::new(),
            shots_pane: crate::panels::cameras::ShotsPane::stored(),
            live_selected: false,
            cameras_pane_was_shown: false,
            shot_player: None,
            live: HashMap::new(),
            live_seq: 0,
            pose_overlay: crate::local::flag(crate::panels::cameras::OVERLAY_KEY, true),
            tuning: crate::panels::cameras::Tuning::default(),
            body_models: crate::panels::body_models::ModelsPrompt::default(),
        }
    }

    /// Drain pending REST results.
    pub(crate) fn apply_pending(&mut self) {
        let Ok(mut p) = self.pending.try_lock() else {
            return;
        };

        if let Some(status) = p.status.take() {
            if let Some(mode) = status.mode {
                self.current_mode = mode.to_string();
            }
            if let Some(club) = status_pick(self.ws_connected, &mut self.club_pick, status.club) {
                self.selected_club = club;
            }
            if let Some(handed) = status_pick(self.ws_connected, &mut self.hand_pick, status.handed)
            {
                self.handed = handed;
            }
            self.cameras_supported = status.cameras_supported;
            self.clip_dir = status.clip_dir;
            self.config_error = status.config_error;
            // The reply lists every running actor: one it leaves out has
            // stopped. Red time counts down from now.
            self.actors = status.actors;
            for actor in self.actors.values_mut() {
                actor.received(self.now_ms);
            }
        }

        // A history cleared since the request makes a reply stale: the empty
        // history and the live events since are the whole of it.
        if let Some(shots) = p.shots.take()
            && p.shots_generation >= self.cleared_generation
        {
            self.shots = shots.into_iter().map(ShotRow::from).collect();
            self.shots_rev += 1;
            self.shot_focus
                .sync(&self.shots, self.device_filter.as_deref());
        }

        // Sets also go stale with a new session (it counts the session up
        // and closes the open set): fetch them again.
        if let Some(groups) = p.groups.take()
            && p.groups_generation >= self.cleared_generation
        {
            if p.groups_generation < self.history_generation {
                self.needs_groups_refresh = true;
            } else {
                self.settings.sync_shot_grouping(groups.mode);
                self.groups.replace(groups);
            }
        }

        if std::mem::take(&mut p.groups_posted) {
            self.needs_groups_refresh = true;
        }

        if let Some(reply) = p.auto_chip.take() {
            self.auto_chip_in_flight = None;
            self.auto_chip_error = reply.err();
        }

        if let Some(reply) = p.cameras.take() {
            self.camera_list.loading = false;
            match reply {
                Ok(list) => {
                    self.camera_list.list = Some(list);
                    self.camera_list.error = None;
                }
                Err(e) => self.camera_list.error = Some(e),
            }
        }
        let reconnects = std::mem::take(&mut p.reconnects);
        let stills = std::mem::take(&mut p.stills);
        let replies = std::mem::take(&mut p.replay);
        let live = std::mem::take(&mut p.live);
        let tuning = p.tuning.take();
        let body_models = std::mem::take(&mut p.body_models);

        // Unsaved edits survive a reload; one the server already holds
        // turns clean.
        if let Some(s) = p.settings.take() {
            // Check if we need to show the setup wizard (no user actors)
            if !self.wizard_checked {
                self.wizard_checked = true;
                self.wizard_dismissed = s.has_user_actors();
            }
            self.settings.load_from(&s);
        }

        if let Some(reply) = p.settings_save.take() {
            let result = reply.map(|_| ());
            if std::mem::take(&mut self.wizard_saving) {
                match &result {
                    Ok(()) => {
                        self.wizard_dismissed = true;
                        self.wizard_error = None;
                        self.needs_status_refresh = true;
                    }
                    Err(e) => self.wizard_error = Some(e.clone()),
                }
            }
            if let Some(removed) = self.settings.finish_save(result) {
                self.card_modes.forget(&removed);
            }
        }
        drop(p);

        for (actor, reply) in reconnects {
            self.reconnect_reply(actor, reply);
        }
        self.expire_reconnects();
        for (actor, reply) in stills {
            self.stills.entry(actor).or_default().reply(reply);
        }
        self.live_replies(live, self.now_ms as f64 / 1000.0);
        if let Some(reply) = tuning {
            self.tuning.reply(reply);
        }
        for (call, reply) in body_models {
            self.body_models_reply(call, reply);
        }
        for reply in replies {
            let seq = reply.seq();
            if let Some(view) = self.replay.as_mut().filter(|v| v.seq() == seq) {
                view.reply(reply);
            } else if let Some(view) = self.shot_player.as_mut().filter(|v| v.seq() == seq) {
                view.reply(reply);
            } else if let Some(card) = self.card_clips.values_mut().find(|c| c.seq() == seq) {
                card.reply(reply);
            }
        }
    }

    /// Actor `actor`'s reconnect was answered: started, it is done; to
    /// start later, it waits until the actor is seen starting again; a
    /// failure frees it and shows on the card.
    fn reconnect_reply(&mut self, actor: String, reply: Result<net::Reconnected, String>) {
        match reply {
            Ok(net::Reconnected::Started) => self.reconnect_done(&actor),
            Ok(net::Reconnected::Later) if self.reconnect_seen.contains(&actor) => {
                self.reconnect_done(&actor);
            }
            Ok(net::Reconnected::Later) => {
                self.reconnect_later.insert(actor, self.now_ms);
            }
            Err(e) => {
                log::warn!("reconnect {actor}: {e}");
                self.reconnect_done(&actor);
                self.reconnect_errors.insert(actor, e);
            }
        }
    }

    /// Actor `id` is starting again (its `Starting` status, or an outcome
    /// listing it restarted; a `Connected` may still come from the
    /// instance being replaced): a reconnect of it answered
    /// "starts later" is done; one not yet answered is done with its
    /// answer.
    fn reconnect_started(&mut self, id: &str) {
        if !self.reconnecting.contains(id) {
            return;
        }
        if self.reconnect_later.contains_key(id) {
            self.reconnect_done(id);
        } else {
            self.reconnect_seen.insert(id.to_owned());
        }
    }

    /// Actor `id`'s reconnect is over: its Reconnect takes clicks again.
    pub(crate) fn reconnect_done(&mut self, id: &str) {
        self.reconnecting.remove(id);
        self.reconnect_later.remove(id);
        self.reconnect_seen.remove(id);
    }

    /// Reconnects answered "starts later" that have not started within
    /// [`RECONNECT_WAIT_MS`] are given up on: freed, with the reason on the
    /// card unless the actor's own `error` row already gives one.
    fn expire_reconnects(&mut self) {
        let now = self.now_ms;
        let expired: Vec<String> = self
            .reconnect_later
            .iter()
            .filter(|(_, since)| now.saturating_sub(**since) >= RECONNECT_WAIT_MS)
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            self.reconnect_done(&id);
            let explained = self
                .actors
                .get(&id)
                .and_then(|a| a.telemetry.get("error"))
                .is_some_and(|e| !e.is_empty());
            if !explained {
                self.reconnect_errors
                    .insert(id, RECONNECT_UNFINISHED.into());
            }
        }
    }

    /// Poll WebSocket for unified bus events.
    fn poll_ws(&mut self) {
        let Some(rx) = &mut self.ws_receiver else {
            return;
        };
        let (events, _) = net::poll_ws(rx);
        let mut ws_disconnected = false;
        for event in events {
            match event {
                WsPollEvent::Opened => {
                    // Browser WebSocket actually opened — send the init handshake now.
                    if let Some(tx) = &mut self.ws_sender {
                        net::send_ws_start(tx);
                    }
                }
                WsPollEvent::Init { global_state, .. } => {
                    if let Some(state) = global_state {
                        self.apply_global_state(&state);
                    }
                }
                WsPollEvent::Message(msg) => self.handle_bus_event(msg),
                WsPollEvent::Error(_) => {
                    ws_disconnected = true;
                }
                WsPollEvent::Disconnected => {
                    self.actors.clear();
                    ws_disconnected = true;
                }
            }
        }
        if ws_disconnected {
            self.ws_sender = None;
            self.ws_receiver = None;
            self.ws_connected = false;
        }
    }

    /// Take the club and handedness from the game state the server sends
    /// at the WebSocket handshake.
    pub(crate) fn apply_global_state(&mut self, state: &GameStateSnapshot) {
        self.selected_club = state.club_info.map(|c| c.club);
        self.club_pick = None;
        self.handed = state.handed;
        self.hand_pick = None;
    }

    pub(crate) fn handle_bus_event(&mut self, msg: FlighthookMessage) {
        // Buffer all events for the log panel
        {
            use crate::panels::log::{alert_severity, event_debug, message_type};

            let actor_name = self
                .actors
                .get(&msg.actor)
                .map(|a| a.name.as_str())
                .unwrap_or("")
                .to_string();
            // A finished shot's raw frames can run to hundreds of KiB; the
            // log keeps a count, and the export has the frames.
            let raw = match (&msg.raw_payload, &msg.event) {
                (Some(r), _) => r.to_string(),
                (None, FlighthookEvent::ShotFinished { raw: Some(r), .. }) => {
                    format!("{} raw frames", r.frames.len())
                }
                (None, event) => serde_json::to_string(event).unwrap_or_default(),
            };
            self.log_entries.push(LogEntry {
                timestamp: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
                actor_name,
                actor_id: msg.actor.clone(),
                message_type: message_type(&msg.event).to_string(),
                event_debug: event_debug(&msg.event),
                raw,
                alert_severity: alert_severity(&msg.event),
            });
            const MAX_LOG_ENTRIES: usize = 500;
            if self.log_entries.len() > MAX_LOG_ENTRIES {
                self.log_entries
                    .drain(..self.log_entries.len() - MAX_LOG_ENTRIES);
            }
        }

        // Route event to UI state
        let actor = msg.actor.clone();
        match msg.event {
            FlighthookEvent::ActorStatus { status, telemetry } => {
                // The actor speaks for itself again.
                self.reconnect_errors.remove(&actor);
                if status == ActorStatus::Starting {
                    self.reconnect_started(&actor);
                }
                let actor = self
                    .actors
                    .entry(actor)
                    .or_insert_with(|| new_actor(String::new()));
                actor.apply_status(status, &telemetry, self.now_ms);
            }
            FlighthookEvent::DeviceTelemetry {
                telemetry: Some(tel),
                ..
            } => {
                if let Some(actor) = self.actors.get_mut(&actor) {
                    for (k, v) in tel {
                        actor.telemetry.insert(k, v);
                    }
                }
            }
            FlighthookEvent::ShotTrigger { key } => {
                // The server stamps the club game state has at the trigger,
                // which is the club this UI shows (the putter for a putting
                // device); `shot_group` confirms it.
                let club_selected = if crate::panels::cards::split_actor_id(&actor).0 == "exputt" {
                    Some(Club::Putter)
                } else {
                    self.selected_club
                };
                self.shots.push(ShotRow {
                    actor: actor.clone(),
                    shot_id: key.shot_id.clone(),
                    shot_number: key.shot_number,
                    ball: None,
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
            FlighthookEvent::ShotVideo {
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
                    camera: actor,
                    position,
                    duration_ms,
                    fps,
                    trigger_offset_ms,
                    impact_offset_ms,
                    complete,
                    dropped,
                    pose: false,
                    swing: false,
                };
                if crate::types::stamp_video(&mut self.shots, &keys, &video) {
                    self.shots_rev += 1;
                }
            }
            FlighthookEvent::ShotPose { keys } => {
                if crate::types::mark_pose(&mut self.shots, &keys, &actor) {
                    self.shots_rev += 1;
                }
                // The open players draw it once they have the keypoints.
                for player in [&mut self.replay, &mut self.shot_player]
                    .into_iter()
                    .flatten()
                    .filter(|r| keys.iter().any(|k| k.shot_id == r.shot_id()))
                {
                    player.pose_ready(&actor);
                }
            }
            FlighthookEvent::ShotSwing { keys } => {
                if crate::types::mark_swing(&mut self.shots, &keys, &actor) {
                    self.shots_rev += 1;
                }
                if let Some(player) = self
                    .shot_player
                    .as_mut()
                    .filter(|r| keys.iter().any(|k| k.shot_id == r.shot_id()))
                {
                    player.swing_ready(&actor);
                }
            }
            FlighthookEvent::ShotVideoRemoved { keys } => {
                if crate::types::remove_video(&mut self.shots, &keys) {
                    self.shots_rev += 1;
                }
                // The open players' clips are gone: close them.
                if self
                    .shot_player
                    .as_ref()
                    .is_some_and(|r| keys.iter().any(|k| k.shot_id == r.shot_id()))
                {
                    self.shot_player = None;
                }
                if self
                    .replay
                    .as_ref()
                    .is_some_and(|r| keys.iter().any(|k| k.shot_id == r.shot_id()))
                {
                    self.replay = None;
                }
            }
            FlighthookEvent::BallFlight { key, ball, derived } => {
                if let Some(i) = groups::find_row(&self.shots, &actor, &key) {
                    let row = &mut self.shots[i];
                    row.ball = Some(*ball);
                    row.derived = derived.map(|d| *d);
                    self.shots_rev += 1;
                }
            }
            FlighthookEvent::ClubPath { key, club } => {
                if let Some(i) = groups::find_row(&self.shots, &actor, &key) {
                    self.shots[i].club = Some(*club);
                    self.shots_rev += 1;
                }
            }
            FlighthookEvent::FaceImpact { key, impact } => {
                if let Some(i) = groups::find_row(&self.shots, &actor, &key) {
                    self.shots[i].impact = Some(*impact);
                    self.shots_rev += 1;
                }
            }
            FlighthookEvent::ShotFinished { key, .. } => {
                if let Some(i) = groups::find_row(&self.shots, &actor, &key) {
                    self.shots[i].finished = true;
                    self.shots_rev += 1;
                }
            }
            FlighthookEvent::PlayerInfo { player_info } => {
                if let Some(ref name) = player_info.name
                    && let Some(actor) = self.actors.get_mut(&actor)
                {
                    actor.telemetry.insert("name".into(), name.clone());
                }
            }
            FlighthookEvent::ClubInfo { club_info } => {
                self.selected_club = Some(club_info.club);
                self.club_pick = None;
                if let Some(actor) = self.actors.get_mut(&actor) {
                    actor
                        .telemetry
                        .insert("club".into(), club_info.club.to_string());
                }
            }
            FlighthookEvent::HandInfo { hand_info } => {
                self.handed = Some(hand_info.handed);
                self.hand_pick = None;
                if let Some(actor) = self.actors.get_mut(&actor) {
                    actor
                        .telemetry
                        .insert("handed".into(), hand_info.handed.to_string());
                }
            }
            FlighthookEvent::SetDetectionMode { mode: Some(m), .. } => {
                self.current_mode = m.to_string();
            }
            FlighthookEvent::ConfigOutcome {
                ref restarted,
                ref stopped,
                ..
            } => {
                // A stopped actor sends no further status: drop it. A
                // stopped Open Connect client then leaves the club picker
                // live.
                for id in stopped {
                    self.actors.remove(id);
                }
                // A restarted actor begins a new history.
                for id in restarted {
                    if let Some(actor) = self.actors.get_mut(id) {
                        actor.restart(self.now_ms);
                    }
                    self.reconnect_started(id);
                }
                // WS events carry no names, and a rename restarts nothing:
                // re-fetch the status for the configured names, new actors
                // included, and so a status reply requested before a stop
                // is replaced.
                self.needs_status_refresh = true;
                // The default view may have changed, and another client's
                // save changes the settings.
                self.needs_groups_refresh = true;
                self.settings.request_reload();
            }
            FlighthookEvent::NewShotSession => self.start_new_session(),
            FlighthookEvent::ClearShotHistory => self.clear_shot_history(),
            FlighthookEvent::ShotGroup { group, shot } => {
                let joined = shot.as_ref().map(|s| (s.actor.as_str(), &s.key));
                let event = self.groups.apply(&actor, group, joined, &mut self.shots);
                if event != GroupEvent::Foreign {
                    // The joined shot's row may have been stamped.
                    self.shots_rev += 1;
                }
                match event {
                    GroupEvent::Foreign => {}
                    GroupEvent::Applied => self.groups_out_of_step = false,
                    GroupEvent::OutOfStep => {
                        if !self.groups_out_of_step {
                            self.groups_out_of_step = true;
                            self.needs_groups_refresh = true;
                            self.needs_shots_refresh = true;
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

impl FlighthookApp {
    /// A new session (`new_shot_session` on the bus): the UI's sets start
    /// the next session, and a sets reply requested before it is fetched
    /// again.
    pub(crate) fn start_new_session(&mut self) {
        self.groups.new_session();
        self.history_generation += 1;
    }

    /// The server cleared its shot history (`clear_shot_history` on the
    /// bus): drop every shot and set, the shown shot, the device filter, an
    /// open Clear history confirmation, an open replay and the camera cards'
    /// clips (the clips are deleted with the history), so the Shots tab
    /// shows its empty state; face impact's next appearance takes the
    /// rightmost card beside the range bars again. A Cameras pane on screen
    /// counts as shown anew, so it turns Live on.
    /// The caches are emptied; their keys (`shots_rev`, the sets' revision)
    /// move on. A shots or sets reply requested before the clear,
    /// queued or still in flight, is dropped when it lands.
    pub(crate) fn clear_shot_history(&mut self) {
        self.history_generation += 1;
        self.cleared_generation = self.history_generation;
        self.shots.clear();
        self.shots_rev += 1;
        self.groups.clear();
        self.groups_out_of_step = false;
        self.shot_focus = ShotFocus::default();
        self.device_filter = None;
        self.display_rows = Cached::default();
        self.device_ids = Cached::default();
        self.filtered_rows = Cached::default();
        self.view_members = Cached::default();
        self.flight_scene = Cached::default();
        self.dispersion_scene = Cached::default();
        self.confirm_clear = false;
        self.replay = None;
        self.shot_player = None;
        self.card_row.clear();
        self.card_clips.clear();
        self.cameras_pane_was_shown = false;
    }

    /// The camera cards hold their clips only while the card row can be on
    /// screen: on the Shots tab's Cards view, showing the shot's data.
    /// Elsewhere they are dropped, and fetched again when the row returns.
    /// The Cameras pane's player likewise lives only while it shows a shot.
    pub(crate) fn drop_hidden_card_clips(&mut self) {
        if self.active_tab != Tab::Shots || self.shots_layout() != ShotsLayout::Cards {
            self.card_clips.clear();
        }
        if !self.cameras_pane_shown() || self.live_selected {
            self.shot_player = None;
        }
    }

    fn show_wizard(&self) -> bool {
        self.settings.loaded && !self.wizard_dismissed && self.wizard_checked
    }

    fn render_wizard(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::BG))
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() * 0.2);

                    ui.label(
                        egui::RichText::new("FLIGHTHOOK")
                            .strong()
                            .size(30.0)
                            .extra_letter_spacing(3.0)
                            .color(theme::ACCENT),
                    );
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new("What do you have?")
                            .size(17.0)
                            .color(theme::TEXT_MUTED),
                    );
                    ui.add_space(24.0);

                    // Left-aligned checkbox group
                    let checkbox_width = 260.0;
                    ui.allocate_ui_with_layout(
                        egui::vec2(checkbox_width, 0.0),
                        egui::Layout::top_down(egui::Align::LEFT),
                        |ui| {
                            ui.label(
                                egui::RichText::new("Launch Monitors")
                                    .size(12.0)
                                    .color(theme::TEXT_DIM),
                            );
                            ui.add_space(4.0);
                            ui.checkbox(
                                &mut self.wizard_mevo,
                                egui::RichText::new("FlightScope Mevo / Mevo+ / Xi").size(15.0),
                            );
                            ui.add_space(4.0);
                            ui.checkbox(
                                &mut self.wizard_r10,
                                egui::RichText::new("Garmin R10").size(15.0),
                            );
                            ui.add_space(4.0);
                            ui.checkbox(
                                &mut self.wizard_square,
                                egui::RichText::new("Square Golf Omni").size(15.0),
                            );
                            ui.add_space(4.0);
                            ui.checkbox(
                                &mut self.wizard_uneekor,
                                egui::RichText::new("Uneekor (OpenConnect)").size(15.0),
                            )
                            .on_hover_text(
                                "Uneekor's own software pushes shots to flighthook over GSPro \
                                 Open Connect. Needs a Uneekor third-party licence, and \
                                 flighthook binds port 921 — to run GSPro on the same host, \
                                 move it to 922 with OpenAPIUseAltPort.",
                            );
                            ui.add_space(12.0);
                            ui.label(
                                egui::RichText::new("Simulators")
                                    .size(12.0)
                                    .color(theme::TEXT_DIM),
                            );
                            ui.add_space(4.0);
                            ui.checkbox(
                                &mut self.wizard_gspro,
                                egui::RichText::new("GSPro").size(15.0),
                            );
                        },
                    );

                    ui.add_space(24.0);

                    let any_selected = self.wizard_mevo
                        || self.wizard_r10
                        || self.wizard_square
                        || self.wizard_uneekor
                        || self.wizard_gspro;
                    let btn = theme::primary_button("Get Started", 16.0)
                        .min_size(egui::vec2(160.0, 40.0));

                    if ui
                        .add_enabled(any_selected && !self.wizard_saving, btn)
                        .clicked()
                    {
                        self.apply_wizard(ctx);
                    }
                    if let Some(error) = &self.wizard_error {
                        ui.add_space(8.0);
                        crate::panels::settings::save_error_line(ui, "wizard", error);
                    }

                    if !any_selected {
                        ui.add_space(8.0);
                        if ui
                            .link(egui::RichText::new("Skip — configure manually").size(12.0))
                            .clicked()
                        {
                            self.wizard_dismissed = true;
                            self.active_tab = Tab::Devices;
                        }
                    }
                });
            });
    }

    pub(crate) fn apply_wizard(&mut self, ctx: &egui::Context) {
        let mut config = self.settings.to_request();

        if self.wizard_mevo {
            config.mevo.insert("0".into(), MevoSection::default());
        }
        if self.wizard_r10 {
            config.r10.insert("0".into(), R10Section::default());
        }
        if self.wizard_square {
            config.square.insert("0".into(), SquareSection::default());
        }
        if self.wizard_uneekor {
            config
                .openconnect_server
                .insert("0".into(), OpenConnectServerSection::default());
        }
        if self.wizard_gspro {
            config
                .openconnect_client
                .insert("0".into(), OpenConnectClientSection::default());
        }

        self.wizard_saving = true;
        self.wizard_error = None;
        self.settings
            .begin_save_with(crate::panels::settings::SaveTarget::Full, config.clone());
        net::post_settings(ctx, &self.pending, &config, None);
    }
}

impl FlighthookApp {
    /// The Remove confirmation: modal, and its Remove waits while a save is
    /// out. Removes the actor by ID.
    pub(crate) fn render_remove_confirm(&mut self, ctx: &egui::Context) {
        let Some(PendingRemoval { id, name }) = &self.confirm_remove else {
            return;
        };
        let (id, name) = (id.clone(), name.clone());
        let saving = self.settings.saving();
        let modal = egui::Modal::new(egui::Id::new("remove_confirm"))
            .frame(theme::card().inner_margin(egui::Margin::same(20)))
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new(format!("Remove \"{name}\"?"))
                        .size(17.0)
                        .strong()
                        .color(theme::TEXT),
                );
                ui.add_space(18.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let remove = ui.add_enabled(!saving, theme::danger_button("Remove", 15.0));
                    crate::panels::devices::mark(ui, || "confirm:remove".into(), &remove);
                    let remove = remove.clicked();
                    let cancel = ui.button("Cancel").clicked();
                    (remove, cancel)
                })
                .inner
            });
        let (remove, cancel) = modal.inner;
        if remove {
            self.confirm_remove = None;
            if self.settings.position(&id).is_some() {
                self.post_save(ctx, crate::panels::settings::SaveTarget::Remove(id));
            }
        } else if cancel || modal.should_close() {
            self.confirm_remove = None;
        }
    }
}

/// Rows of the club picker, as ranges of `Club::ALL`: driver + mini driver,
/// woods, hybrids, irons, wedges, putter.
const CLUB_PICKER_ROWS: [std::ops::Range<usize>; 6] = [0..2, 2..10, 10..19, 19..28, 28..32, 32..33];

/// Size of a club pill in the picker: equal widths line the rows up as
/// columns, and the height is a comfortable touch target.
const CLUB_PILL: egui::Vec2 = egui::vec2(52.0, 38.0);

/// The hand picker's options, in display order.
const HANDS: [Handedness; 2] = [Handedness::Right, Handedness::Left];

/// The code a hand is shown with: `RH` or `LH`.
pub(crate) fn hand_code(handed: Handedness) -> &'static str {
    match handed {
        Handedness::Right => "RH",
        Handedness::Left => "LH",
    }
}

/// Whether the simulator owns the club and the hand: any
/// `openconnect_client.*` actor is connected. A connected simulator replaces
/// both on every player message it sends, so a pick made in the UI would not
/// last.
pub(crate) fn set_by_simulator(actors: &HashMap<String, ActorStatusResponse>) -> bool {
    actors
        .iter()
        .any(|(id, a)| id.starts_with("openconnect_client.") && a.status == ActorStatus::Connected)
}

/// The header's hand: an RH | LH control with `shown`, the player's hand (RH
/// while none is set, the hand the devices then use). Returns the hand a click
/// picks. With `read_only` the hand is a static pill and nothing can be
/// picked.
fn hand_control(
    ui: &mut egui::Ui,
    row_height: f32,
    shown: Handedness,
    read_only: bool,
) -> Option<Handedness> {
    let selected = HANDS.iter().position(|&h| h == shown).unwrap_or(0);
    let mut picked = None;
    let size = egui::vec2(ui.available_width(), row_height);
    let row = egui::Layout::left_to_right(egui::Align::Center);
    ui.allocate_ui_with_layout(size, row, |ui| {
        if read_only {
            theme::pill_static(ui, hand_code(shown)).on_hover_text("Set by the simulator");
            return;
        }
        let labels = HANDS.map(hand_code);
        let control = theme::segmented(ui, &labels, selected, None);
        control
            .response
            .on_hover_text("Player's hand. Devices that follow it are told on a change.");
        picked = control.inner.map(|i| HANDS[i]);
    });
    picked
}

impl FlighthookApp {
    /// The header's club: a caps CLUB label and a pill with the club in
    /// play. The pill opens a popup with every club, or is read-only while
    /// an Open Connect client is connected.
    fn render_club_picker(&mut self, ui: &mut egui::Ui, row_height: f32) {
        let read_only = set_by_simulator(&self.actors);
        if read_only {
            self.club_picker_open = false;
        }
        let code = self
            .selected_club
            .map_or_else(|| "\u{2014}".to_owned(), |c| c.to_string());
        let mut picked = None;
        let mut chip = None;
        // Laid out at the header row's height so the label and the pill
        // centre on one line.
        let size = egui::vec2(ui.available_width(), row_height);
        let row = egui::Layout::left_to_right(egui::Align::Center);
        ui.allocate_ui_with_layout(size, row, |ui| {
            ui.label(theme::caps("Club", 11.0, theme::TEXT_MUTED));
            if read_only {
                let pill = theme::pill_static(ui, &code).on_hover_text("Set by the simulator");
                mark(ui, || "club".to_owned(), &pill);
                chip = self.header_auto_chip(ui);
                return;
            }
            let pill = theme::pill(ui, false, &code)
                .on_hover_text("Club in play. Sets the detection mode; a change starts a new set.");
            mark(ui, || "club".to_owned(), &pill);
            if pill.clicked() {
                self.club_picker_open = !self.club_picker_open;
            }
            let frame = egui::Frame::new()
                .fill(theme::SURFACE_RAISED)
                .stroke(egui::Stroke::new(1.0, theme::BORDER))
                .corner_radius(theme::CARD_RADIUS)
                .inner_margin(egui::Margin::same(10));
            let selected = self.selected_club;
            let settings = &self.settings;
            egui::Popup::from_response(&pill)
                .open_bool(&mut self.club_picker_open)
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                .align(egui::RectAlign::BOTTOM_START)
                .frame(frame)
                .show(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                    for row in CLUB_PICKER_ROWS {
                        ui.horizontal(|ui| {
                            for &club in &Club::ALL[row] {
                                let on = selected == Some(club);
                                let mut pill =
                                    theme::pill_sized(ui, on, &club.to_string(), CLUB_PILL);
                                if let Some(mode) = club_mode_hint(settings, club) {
                                    pill = pill.on_hover_text(mode);
                                }
                                if pill.clicked() {
                                    picked = Some(club);
                                }
                            }
                        });
                    }
                });
            chip = self.header_auto_chip(ui);
        });
        if let Some(yards) = chip {
            self.send_auto_chip(ui.ctx(), yards);
        }
        if let Some(club) = picked {
            self.club_picker_open = false;
            self.pick_club(ui.ctx(), club);
        }
    }

    /// The header's hand: see [`hand_control`]. Read-only while an Open
    /// Connect client is connected.
    fn render_hand_picker(&mut self, ui: &mut egui::Ui, row_height: f32) {
        let read_only = set_by_simulator(&self.actors);
        let shown = self.handed.unwrap_or(Handedness::Right);
        if let Some(handed) = hand_control(ui, row_height, shown, read_only) {
            self.pick_hand(ui.ctx(), handed);
        }
    }

    /// Set the player's hand: over the WebSocket when it is up, else over
    /// REST.
    fn pick_hand(&mut self, ctx: &egui::Context, handed: Handedness) {
        self.handed = Some(handed);
        if let Some(tx) = &mut self.ws_sender {
            let msg = format!(r#"{{"cmd":"hand","handed":"{handed}"}}"#);
            tx.send(ewebsock::WsMessage::Text(msg));
        } else {
            self.hand_pick = Some(handed);
            net::post_hand(ctx, handed);
        }
    }

    /// Select `club` as the club in play: over the WebSocket when it is up,
    /// else over REST.
    fn pick_club(&mut self, ctx: &egui::Context, club: Club) {
        self.selected_club = Some(club);
        if let Some(tx) = &mut self.ws_sender {
            let msg = format!(r#"{{"cmd":"club","club":"{club}"}}"#);
            tx.send(ewebsock::WsMessage::Text(msg));
        } else {
            self.club_pick = Some(club);
            net::post_club(ctx, club);
        }
    }
}

/// The club or hand a `/api/status` reply sets, if it sets one. With the
/// WebSocket up, its `init`, `club_info` and `hand_info` are the live source
/// and the reply is ignored. Without it, a reply that does not carry the
/// value picked over REST yet (`pick`) is ignored too; the first that does
/// clears `pick`.
fn status_pick<T: Copy + PartialEq>(
    ws_connected: bool,
    pick: &mut Option<T>,
    reply: Option<T>,
) -> Option<Option<T>> {
    if ws_connected {
        return None;
    }
    if pick.is_some_and(|p| reply != Some(p)) {
        return None;
    }
    *pick = None;
    Some(reply)
}

/// Hover text for `club` in the picker: the detection mode it switches to,
/// from the loaded settings' club lists (else the club's own default), or
/// `None` for Full.
fn club_mode_hint(settings: &SettingsForm, club: Club) -> Option<&'static str> {
    let mode = if !settings.loaded {
        club.mode()
    } else if settings.putting_clubs.contains(&club) {
        ShotDetectionMode::Putting
    } else if settings.chipping_clubs.contains(&club) {
        ShotDetectionMode::Chipping
    } else {
        ShotDetectionMode::Full
    };
    match mode {
        ShotDetectionMode::Full => None,
        ShotDetectionMode::Chipping => Some("Chipping mode"),
        ShotDetectionMode::Putting => Some("Putting mode"),
    }
}

/// The frame's input time in whole milliseconds.
pub(crate) fn input_ms(ctx: &egui::Context) -> u64 {
    (ctx.input(|i| i.time).max(0.0) * 1000.0) as u64
}

impl FlighthookApp {
    /// The top bar: tabs, detection mode, hand, club and, right-aligned, the
    /// battery indicators and the title.
    pub(crate) fn render_header(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("header")
            .frame(
                egui::Frame::new()
                    .fill(theme::SURFACE)
                    .inner_margin(egui::Margin::symmetric(14, 10)),
            )
            .show_separator_line(true)
            .show(ctx, |ui| {
                // Top-aligned: the tab group and the segmented track share
                // the same 3 px margin, so their pills line up exactly.
                let row = egui::Layout::left_to_right(egui::Align::Min);
                ui.with_layout(row, |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let tab_group = self.render_tabs(ui);
                    let row_height = tab_group.rect.height();

                    ui.add_space(10.0);
                    let (divider, _) =
                        ui.allocate_exact_size(egui::vec2(1.0, row_height), egui::Sense::hover());
                    ui.painter().vline(
                        divider.center().x,
                        divider.shrink2(egui::vec2(0.0, 6.0)).y_range(),
                        egui::Stroke::new(1.0, theme::BORDER),
                    );
                    ui.add_space(10.0);

                    // Global mode selector
                    let modes = [
                        ("full", "Full"),
                        ("chipping", "Chipping"),
                        ("putting", "Putting"),
                    ];
                    let selected = modes
                        .iter()
                        .position(|(val, _)| self.current_mode == *val)
                        .unwrap_or(usize::MAX);
                    let labels = modes.map(|(_, label)| label);
                    if let Some(i) = theme::segmented(ui, &labels, selected, None).inner {
                        let mode = modes[i].0;
                        self.current_mode = mode.to_string();
                        if let Some(tx) = &mut self.ws_sender {
                            let msg = format!(r#"{{"cmd":"mode","mode":"{mode}"}}"#);
                            tx.send(ewebsock::WsMessage::Text(msg));
                        } else {
                            net::post_mode(ui.ctx(), mode);
                        }
                    }

                    ui.add_space(10.0);
                    self.render_hand_picker(ui, row_height);
                    ui.add_space(10.0);
                    self.render_club_picker(ui, row_height);

                    // Right-aligned title
                    let title = egui::Layout::right_to_left(egui::Align::Center);
                    let title_size = egui::vec2(ui.available_width(), row_height);
                    ui.allocate_ui_with_layout(title_size, title, |ui| {
                        ui.label(
                            egui::RichText::new(concat!("v", env!("CARGO_PKG_VERSION")))
                                .size(11.0)
                                .color(theme::TEXT_DIM),
                        );
                        ui.add_space(2.0);
                        let label = ui.label(
                            egui::RichText::new("FLIGHTHOOK")
                                .strong()
                                .size(15.0)
                                .extra_letter_spacing(2.5)
                                .color(theme::ACCENT),
                        );
                        mark(ui, || "title".to_owned(), &label);
                        let (dot, _) =
                            ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                        ui.painter().circle_filled(dot.center(), 4.0, theme::ACCENT);

                        // Batteries, left of the title: the first device
                        // furthest left, so this right-to-left row adds the
                        // last one first.
                        let batteries = battery::batteries(&self.actors, &self.settings.actors);
                        if !batteries.is_empty() {
                            ui.add_space(10.0);
                        }
                        for (i, b) in batteries.iter().rev().enumerate() {
                            if i > 0 {
                                ui.add_space(battery::GAP - ui.spacing().item_spacing.x);
                            }
                            let response = battery::show(ui, b);
                            mark(ui, || format!("battery:{}", b.actor), &response);
                        }
                    });
                });
            });
    }
}

impl eframe::App for FlighthookApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Before any layout: a changed zoom discards this pass.
        self.ui_scale.apply(ctx);
        self.now_ms = input_ms(ctx);
        // Poll for WebSocket events ~10x/sec instead of only on user input.
        ctx.request_repaint_after(std::time::Duration::from_millis(100));

        // Connect WebSocket on first frame only — no auto-reconnect.
        // User should refresh the page to pick up new WASM code after a deploy.
        if self.ws_sender.is_none()
            && !self.ws_ever_connected
            && let Some((tx, rx)) = net::connect_ws()
        {
            self.ws_sender = Some(tx);
            self.ws_receiver = Some(rx);
            self.ws_connected = true;
            self.ws_ever_connected = true;
        }
        // Once, with the WebSocket opening (or failing): the live events
        // after this request add to its reply.
        if !self.started {
            self.started = true;
            net::fetch_status(ctx, &self.pending);
            net::fetch_shots(ctx, &self.pending, self.history_generation);
            net::fetch_groups(ctx, &self.pending, self.history_generation);
        }

        self.apply_pending();
        self.poll_ws();
        self.refresh_shot_caches();
        self.observe_shots(ctx.input(|i| i.time));
        self.groups.observe_club(self.selected_club, &self.shots);
        self.sync_cameras_pane();
        self.drop_hidden_card_clips();
        self.sync_live_views(ctx);
        self.drive_body_models(ctx);

        if self.needs_status_refresh {
            self.needs_status_refresh = false;
            net::fetch_status(ctx, &self.pending);
        }
        if self.needs_shots_refresh {
            self.needs_shots_refresh = false;
            net::fetch_shots(ctx, &self.pending, self.history_generation);
        }
        if self.needs_groups_refresh {
            self.needs_groups_refresh = false;
            net::fetch_groups(ctx, &self.pending, self.history_generation);
        }

        if self.confirm_remove.is_some() {
            self.render_remove_confirm(ctx);
        }
        self.render_body_models(ctx);

        // API documentation popup
        if self.show_api_docs {
            let screen = ctx.content_rect();
            let win_size = [screen.width() * 0.9, screen.height() * 0.9];
            egui::Window::new("API Documentation")
                .open(&mut self.show_api_docs)
                .collapsible(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .fixed_size(win_size)
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink(false)
                        .show(ui, |ui| {
                            egui_commonmark::CommonMarkViewer::new().show(
                                ui,
                                &mut self.api_docs_cache,
                                API_DOCS_MD,
                            );
                        });
                });
        }

        // Setup wizard — shown when config has no user actors
        if self.show_wizard() {
            self.render_wizard(ctx);
            return;
        }

        // Dark overlay when backend WS is gone (no actors = not connected to backend)
        if !self.ws_connected && self.actors.is_empty() {
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(theme::BG))
                .show(ctx, |ui| {
                    ui.centered_and_justified(|ui| {
                        ui.label(
                            egui::RichText::new("DISCONNECTED")
                                .size(32.0)
                                .color(theme::TEXT_DIM)
                                .extra_letter_spacing(4.0)
                                .strong(),
                        );
                    });
                });
            return;
        }

        self.render_header(ctx);
        self.camera_keys(ctx, crate::panels::cameras::KEY_BINDINGS);

        let content_frame = egui::Frame::new()
            .fill(theme::BG)
            .inner_margin(egui::Margin::same(14));
        egui::CentralPanel::default()
            .frame(content_frame)
            .show(ctx, |ui| self.render_content(ui));
    }
}

impl FlighthookApp {
    /// The main content: the error frames over the active tab.
    pub(crate) fn render_content(&mut self, ui: &mut egui::Ui) {
        self.render_error_banner(ui);

        match self.active_tab {
            Tab::Devices => {
                let ctx = ui.ctx().clone();
                self.render_devices_panel(&ctx, ui);
            }
            Tab::Shots => self.render_shots_panel(ui),
            Tab::Log => self.render_log_panel(ui),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor(status: ActorStatus) -> ActorStatusResponse {
        ActorStatusResponse::new("", status)
    }

    /// One pass of the main content on a 1600×900 screen. Returns the
    /// marked controls.
    fn content_marks(
        ctx: &egui::Context,
        app: &mut FlighthookApp,
    ) -> crate::panels::devices::Marks {
        use crate::panels::devices::Marks;
        ctx.data_mut(|d| d.remove_temp::<Marks>(egui::Id::NULL));
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1600.0, 900.0),
            )),
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| app.render_content(ui));
        });
        ctx.data(|d| d.get_temp::<Marks>(egui::Id::NULL))
            .unwrap_or_default()
    }

    /// A config file the server could not use shows as a red frame across
    /// the top of every tab, saying which file and what is wrong, until a
    /// status reply says it is gone.
    #[test]
    fn a_config_error_shows_in_a_frame_on_every_tab_until_the_status_drops_it() {
        let ctx = egui::Context::default();
        let mut app = FlighthookApp::with_pending(net::new_pending());
        assert!(!content_marks(&ctx, &mut app).contains_key("config_error"));

        let status = |config_error| crate::types::StatusResponse {
            actors: HashMap::new(),
            mode: None,
            club: None,
            handed: None,
            cameras_supported: false,
            clip_dir: None,
            config_error,
        };
        let error = crate::types::ConfigLoadError {
            path: "/home/golfer/.config/flighthook/config.toml".into(),
            error: "line 4, column 8: unknown variant `liv`".into(),
        };
        app.pending.lock().unwrap().status = Some(status(Some(error.clone())));
        app.apply_pending();
        assert_eq!(app.config_error.as_ref(), Some(&error));
        for tab in [Tab::Devices, Tab::Shots, Tab::Log] {
            app.active_tab = tab;
            let marks = content_marks(&ctx, &mut app);
            let (rect, _) = marks["config_error"];
            assert!(
                rect.width() > 1200.0,
                "{tab:?}: across the content {rect:?}"
            );
            assert!(rect.min.y < 40.0, "{tab:?}: at the top {rect:?}");
        }

        app.pending.lock().unwrap().status = Some(status(None));
        app.apply_pending();
        assert!(!content_marks(&ctx, &mut app).contains_key("config_error"));
    }

    #[test]
    fn groups_reply_after_an_optimistic_mode_switch_keeps_the_servers_mode() {
        use crate::types::{ShotGrouping, ShotGroupsResponse};
        let reply = |mode| ShotGroupsResponse {
            server: "webserver.0".into(),
            mode,
            session: 0,
            current: None,
            groups: Vec::new(),
        };
        let mut app = FlighthookApp::with_pending(net::new_pending());
        app.pending.lock().expect("lock").groups = Some(reply(ShotGrouping::BySet));
        app.apply_pending();
        assert_eq!(app.groups.mode, ShotGrouping::BySet);
        // The Shots tab's switch applies at once...
        app.groups.mode = ShotGrouping::ByClub;
        // ...and the refetch it triggers carries the mode the server applied.
        app.pending.lock().expect("lock").groups = Some(reply(ShotGrouping::ByClub));
        app.apply_pending();
        assert_eq!(app.groups.mode, ShotGrouping::ByClub);
        assert_eq!(app.settings.shot_grouping, ShotGrouping::ByClub);
        // A reply naming another mode (another client switched) wins too.
        app.pending.lock().expect("lock").groups = Some(reply(ShotGrouping::None));
        app.apply_pending();
        assert_eq!(app.groups.mode, ShotGrouping::None);
        assert_eq!(app.settings.shot_grouping, ShotGrouping::None);
    }

    #[test]
    fn new_session_on_the_bus_starts_the_uis_session() {
        use crate::types::{ShotGrouping, ShotGroupsResponse};
        let mut app = FlighthookApp::with_pending(net::new_pending());
        app.pending.lock().expect("lock").groups = Some(ShotGroupsResponse {
            server: "webserver.0".into(),
            mode: ShotGrouping::ByClub,
            session: 2,
            current: None,
            groups: Vec::new(),
        });
        app.apply_pending();
        assert_eq!(app.groups.session(), 2);
        let rev = app.groups.rev();
        // Sent by whichever client pressed it; every webserver acts on it.
        app.handle_bus_event(FlighthookMessage::new(FlighthookEvent::NewShotSession).actor("web"));
        assert_eq!(app.groups.session(), 3);
        assert!(app.groups.rev() > rev, "the view's members rebuild");
    }

    #[test]
    fn status_reply_never_clobbers_a_live_or_pending_club() {
        let mut pick = None;
        // The WebSocket is the live source while it is up.
        assert_eq!(status_pick(true, &mut pick, Some(Club::Driver)), None);
        // Without it, the reply is the source...
        assert_eq!(
            status_pick(false, &mut pick, Some(Club::Driver)),
            Some(Some(Club::Driver))
        );
        assert_eq!(status_pick(false, &mut pick, None), Some(None));
        // ...except a reply that predates a REST pick.
        pick = Some(Club::Iron7);
        assert_eq!(status_pick(false, &mut pick, Some(Club::Driver)), None);
        assert_eq!(pick, Some(Club::Iron7));
        assert_eq!(
            status_pick(false, &mut pick, Some(Club::Iron7)),
            Some(Some(Club::Iron7))
        );
        assert_eq!(pick, None, "confirmed");
        assert_eq!(
            status_pick(false, &mut pick, Some(Club::Driver)),
            Some(Some(Club::Driver))
        );
    }

    /// Without the WebSocket, the hand picked over REST shows until a status
    /// reply carries it; with it, `hand_info` is the source and the reply's
    /// hand is ignored.
    #[test]
    fn the_hand_follows_status_replies_and_hand_info() {
        let status = |handed| crate::types::StatusResponse {
            actors: HashMap::new(),
            mode: None,
            club: None,
            handed,
            cameras_supported: false,
            clip_dir: None,
            config_error: None,
        };
        let mut app = FlighthookApp::with_pending(net::new_pending());
        app.pending.lock().expect("lock").status = Some(status(Some(Handedness::Left)));
        app.apply_pending();
        assert_eq!(app.handed, Some(Handedness::Left));

        // A REST pick holds against a reply that does not carry it yet.
        app.handed = Some(Handedness::Right);
        app.hand_pick = Some(Handedness::Right);
        app.pending.lock().expect("lock").status = Some(status(Some(Handedness::Left)));
        app.apply_pending();
        assert_eq!(app.handed, Some(Handedness::Right));
        app.pending.lock().expect("lock").status = Some(status(Some(Handedness::Right)));
        app.apply_pending();
        assert_eq!(app.hand_pick, None);

        // With the WebSocket up the reply does not move it; hand_info does,
        // and the source's card learns it.
        app.ws_connected = true;
        app.pending.lock().expect("lock").status = Some(status(Some(Handedness::Left)));
        app.apply_pending();
        assert_eq!(app.handed, Some(Handedness::Right));
        app.actors
            .insert("openconnect_client.0".into(), new_actor("GSPro".into()));
        app.handle_bus_event(
            FlighthookMessage::new(FlighthookEvent::HandInfo {
                hand_info: crate::types::HandInfo {
                    handed: Handedness::Left,
                },
            })
            .actor("openconnect_client.0"),
        );
        assert_eq!(app.handed, Some(Handedness::Left));
        assert_eq!(app.actors["openconnect_client.0"].telemetry["handed"], "lh");
    }

    #[test]
    fn hand_codes_are_rh_and_lh() {
        assert_eq!(HANDS.map(hand_code), ["RH", "LH"]);
    }

    #[test]
    fn club_picker_is_read_only_only_while_a_simulator_is_connected() {
        let mut actors = HashMap::new();
        assert!(!set_by_simulator(&actors));
        actors.insert("mevo.0".to_owned(), actor(ActorStatus::Connected));
        assert!(!set_by_simulator(&actors));
        for status in [
            ActorStatus::Starting,
            ActorStatus::Reconnecting,
            ActorStatus::Disconnected,
        ] {
            actors.insert("openconnect_client.0".to_owned(), actor(status));
            assert!(!set_by_simulator(&actors), "{status:?} leaves it live");
        }
        actors.insert(
            "openconnect_client.1".to_owned(),
            actor(ActorStatus::Connected),
        );
        assert!(set_by_simulator(&actors));
    }

    #[test]
    fn hand_control_is_read_only_only_while_a_simulator_is_connected() {
        use egui::{Event, Modifiers, PointerButton, Pos2, Rect, pos2, vec2};

        // Click along the control's centre line; the hand a click picks, if
        // any.
        let click_across = |read_only: bool| {
            let ctx = egui::Context::default();
            theme::install(&ctx);
            let frame = |events: Vec<Event>| {
                let input = egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 60.0))),
                    events,
                    ..Default::default()
                };
                let mut picked = None;
                let _ = ctx.run(input, |ctx| {
                    egui::CentralPanel::default()
                        .frame(egui::Frame::NONE)
                        .show(ctx, |ui| {
                            picked = hand_control(ui, 30.0, Handedness::Right, read_only);
                        });
                });
                picked
            };
            let button = |at: Pos2, pressed| Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            };
            frame(Vec::new());
            let mut picks = Vec::new();
            for x in (0..200).step_by(4) {
                let at = pos2(x as f32 + 2.0, 20.0);
                frame(vec![Event::PointerMoved(at)]);
                frame(vec![button(at, true)]);
                picks.extend(frame(vec![button(at, false)]));
            }
            picks
        };
        let live = click_across(false);
        assert!(!live.is_empty(), "live: LH is pickable");
        assert!(live.iter().all(|&h| h == Handedness::Left), "{live:?}");
        assert!(click_across(true).is_empty(), "read-only: no click picks");
    }

    #[test]
    fn stopping_the_simulator_frees_the_club_picker() {
        use crate::types::StatusResponse;
        let status = |actors: &[(&str, ActorStatus)]| StatusResponse {
            actors: actors
                .iter()
                .map(|(id, s)| ((*id).to_owned(), actor(*s)))
                .collect(),
            mode: None,
            club: None,
            handed: None,
            cameras_supported: false,
            clip_dir: None,
            config_error: None,
        };
        let outcome = |stopped: &[&str]| {
            FlighthookMessage::new(FlighthookEvent::ConfigOutcome {
                request_id: None,
                restarted: Vec::new(),
                stopped: stopped.iter().map(|s| (*s).to_owned()).collect(),
                started: Vec::new(),
            })
            .actor("system")
        };
        let mut app = FlighthookApp::with_pending(net::new_pending());
        app.pending.lock().expect("lock").status = Some(status(&[
            ("mevo.0", ActorStatus::Connected),
            ("openconnect_client.0", ActorStatus::Connected),
        ]));
        app.apply_pending();
        assert!(set_by_simulator(&app.actors));

        // Removed in Settings: the outcome drops it and asks for the status.
        app.handle_bus_event(outcome(&["openconnect_client.0"]));
        assert!(!set_by_simulator(&app.actors), "picker live");
        assert!(!app.actors.contains_key("openconnect_client.0"));
        assert!(app.actors.contains_key("mevo.0"));
        assert!(app.needs_status_refresh);

        // A status reply replaces the map: an actor it leaves out is gone,
        // even one the UI last saw connected.
        app.actors.insert(
            "openconnect_client.0".to_owned(),
            actor(ActorStatus::Connected),
        );
        app.pending.lock().expect("lock").status =
            Some(status(&[("mevo.0", ActorStatus::Connected)]));
        app.apply_pending();
        assert!(!set_by_simulator(&app.actors));
        assert_eq!(app.actors.len(), 1);

        // Stopping something else leaves a connected simulator in charge.
        app.actors.insert(
            "openconnect_client.1".to_owned(),
            actor(ActorStatus::Connected),
        );
        app.needs_status_refresh = false;
        app.handle_bus_event(outcome(&["mevo.0"]));
        assert!(set_by_simulator(&app.actors));
        assert!(app.needs_status_refresh);
    }

    #[test]
    fn a_status_reply_keeps_the_red_and_live_statuses_carry_it_on() {
        use crate::panels::tab_alert;
        use crate::types::{ConnectionState, RECONNECT_ALERT_MS};
        let status = |status| {
            FlighthookMessage::new(FlighthookEvent::ActorStatus {
                status,
                telemetry: HashMap::new(),
            })
            .actor("openconnect_server.0")
        };
        let mut app = FlighthookApp::with_pending(net::new_pending());
        // The UI opened 5 minutes after the drop, its clock unrelated to the
        // server's: the red time counts from receipt.
        app.now_ms = 7_000;
        app.pending.lock().expect("lock").status = Some(
            serde_json::from_str(
                r#"{"actors":{"openconnect_server.0":{"name":"Uneekor",
                "status":"disconnected","telemetry":{},
                "state":"reconnecting","red_for_ms":300000}},"mode":"full"}"#,
            )
            .expect("reply"),
        );
        app.apply_pending();
        let server = &app.actors["openconnect_server.0"];
        assert_eq!(server.state(), ConnectionState::Reconnecting);
        assert!(server.red(7_000 + 299_999));
        assert!(!server.red(7_000 + 300_000));
        assert_eq!(tab_alert(&app.actors, 7_000), Some(0));

        // The same Disconnected again is no new drop.
        app.handle_bus_event(status(ActorStatus::Disconnected));
        assert!(!app.actors["openconnect_server.0"].red(7_000 + 300_000));
        // A peer connects and leaves: a new ten minutes.
        app.now_ms = 400_000;
        app.handle_bus_event(status(ActorStatus::Connected));
        assert_eq!(tab_alert(&app.actors, app.now_ms), None);
        app.handle_bus_event(status(ActorStatus::Disconnected));
        assert!(app.actors["openconnect_server.0"].red(400_000 + RECONNECT_ALERT_MS - 1));
        // A restart from a config change starts again at Connecting.
        app.handle_bus_event(
            FlighthookMessage::new(FlighthookEvent::ConfigOutcome {
                request_id: None,
                restarted: vec!["openconnect_server.0".into()],
                stopped: Vec::new(),
                started: Vec::new(),
            })
            .actor("system"),
        );
        let server = &app.actors["openconnect_server.0"];
        assert_eq!(server.state(), ConnectionState::Connecting);
        assert_eq!(tab_alert(&app.actors, app.now_ms), None);
    }

    #[test]
    fn an_empty_status_telemetry_value_removes_the_key() {
        let mut app = FlighthookApp::with_pending(net::new_pending());
        let status = |status, telemetry: &[(&str, &str)]| {
            FlighthookMessage::new(FlighthookEvent::ActorStatus {
                status,
                telemetry: telemetry
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect(),
            })
            .actor("openconnect_client.0")
        };
        app.handle_bus_event(status(
            ActorStatus::Reconnecting,
            &[("error", "refused"), ("club", "DR")],
        ));
        assert_eq!(
            app.actors["openconnect_client.0"]
                .telemetry
                .get("error")
                .map(String::as_str),
            Some("refused")
        );
        // A status without the key keeps it.
        app.handle_bus_event(status(ActorStatus::Reconnecting, &[]));
        assert!(
            app.actors["openconnect_client.0"]
                .telemetry
                .contains_key("error")
        );
        app.handle_bus_event(status(ActorStatus::Connected, &[("error", "")]));
        let telemetry = &app.actors["openconnect_client.0"].telemetry;
        assert!(!telemetry.contains_key("error"));
        assert_eq!(telemetry.get("club").map(String::as_str), Some("DR"));
    }

    #[test]
    fn a_lost_connection_or_a_new_instance_clears_the_cards_telemetry() {
        let mut app = FlighthookApp::with_pending(net::new_pending());
        let status = |status, telemetry: &[(&str, &str)]| {
            FlighthookMessage::new(FlighthookEvent::ActorStatus {
                status,
                telemetry: telemetry
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect(),
            })
            .actor("mevo.0")
        };
        let device = |pairs: &[(&str, &str)]| {
            FlighthookMessage::new(FlighthookEvent::DeviceTelemetry {
                manufacturer: None,
                model: None,
                firmware: None,
                telemetry: Some(
                    pairs
                        .iter()
                        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                        .collect(),
                ),
            })
            .actor("mevo.0")
        };
        let held = |app: &FlighthookApp| {
            let mut keys: Vec<String> = app.actors["mevo.0"].telemetry.keys().cloned().collect();
            keys.sort();
            keys
        };

        app.handle_bus_event(status(ActorStatus::Connected, &[("device_info", "Xi")]));
        app.handle_bus_event(device(&[("ready", "true"), ("battery_pct", "80")]));
        app.handle_bus_event(
            FlighthookMessage::new(FlighthookEvent::ClubInfo {
                club_info: crate::types::ClubInfo { club: Club::Driver },
            })
            .actor("mevo.0"),
        );
        assert_eq!(held(&app), ["battery_pct", "club", "device_info", "ready"]);

        // Connected, Connected: merges.
        app.handle_bus_event(status(ActorStatus::Connected, &[("shot_count", "1")]));
        assert_eq!(held(&app).len(), 5);

        // Lost: only the message's own keys remain.
        app.handle_bus_event(status(ActorStatus::Reconnecting, &[("error", "gone")]));
        assert_eq!(held(&app), ["error"]);
        // Repeated non-Connected statuses merge.
        app.handle_bus_event(status(ActorStatus::Disconnected, &[("phase", "x")]));
        assert_eq!(held(&app), ["error", "phase"]);
        app.handle_bus_event(device(&[("ready", "false")]));
        assert_eq!(held(&app), ["error", "phase", "ready"]);

        // A new instance starts over.
        app.handle_bus_event(status(ActorStatus::Starting, &[("bind", "b")]));
        assert_eq!(held(&app), ["bind"]);
        app.handle_bus_event(status(ActorStatus::Connected, &[]));
        app.handle_bus_event(status(ActorStatus::Disconnected, &[]));
        assert!(held(&app).is_empty());
    }

    #[test]
    fn club_picker_rows_cover_the_bag_by_kind() {
        let rows: Vec<String> = CLUB_PICKER_ROWS
            .iter()
            .map(|r| {
                Club::ALL[r.clone()]
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        assert_eq!(
            rows,
            [
                "DR MD",
                "2W 3W 4W 5W 6W 7W 8W 9W",
                "1H 2H 3H 4H 5H 6H 7H 8H 9H",
                "1I 2I 3I 4I 5I 6I 7I 8I 9I",
                "PW GW SW LW",
                "PT",
            ]
        );
        assert_eq!(
            CLUB_PICKER_ROWS.last().map(|r| r.end),
            Some(Club::ALL.len())
        );
    }

    #[test]
    fn club_mode_hint_follows_the_settings_lists() {
        let mut settings = SettingsForm::default();
        // Before settings load: the club's own default mode.
        assert_eq!(
            club_mode_hint(&settings, Club::Putter),
            Some("Putting mode")
        );
        assert_eq!(
            club_mode_hint(&settings, Club::SandWedge),
            Some("Chipping mode")
        );
        assert_eq!(club_mode_hint(&settings, Club::Driver), None);
        settings.loaded = true;
        settings.chipping_clubs = vec![Club::Iron9];
        settings.putting_clubs = vec![];
        assert_eq!(
            club_mode_hint(&settings, Club::Iron9),
            Some("Chipping mode")
        );
        assert_eq!(club_mode_hint(&settings, Club::SandWedge), None);
        assert_eq!(club_mode_hint(&settings, Club::Putter), None);
    }

    #[test]
    fn ws_reconnect_refetch_keeps_the_sets_and_tells_repeated_shot_numbers_apart() {
        use crate::types::{BallFlight, ShotGrouping, ShotGroupsResponse};
        use flighthook::{GroupedShot, ShotData, ShotGroupInfo, ShotKey};

        let set = |id: &str, number, club, open, shots| ShotGroupInfo {
            id: id.into(),
            number,
            session: 0,
            golfer: None,
            club: Some(club),
            started_at: "2026-10-26T15:41:28.300Z".into(),
            manual: false,
            open,
            shots,
        };
        let stored = |n, id: &str, group: &str| ShotData {
            actor: "mevo.0".into(),
            shot_number: n,
            ball: Some(BallFlight::default()),
            shot_id: Some(id.into()),
            club_selected: Some(Club::Iron7),
            group: Some(group.into()),
            ..ShotData::default()
        };
        let reply = |groups| ShotGroupsResponse {
            server: "webserver.0".into(),
            mode: ShotGrouping::BySet,
            session: 0,
            current: None,
            groups,
        };
        let key = |n, id: &str| ShotKey {
            shot_id: id.into(),
            shot_number: n,
        };
        let device = |app: &mut FlighthookApp, event| {
            app.handle_bus_event(FlighthookMessage::new(event).actor("mevo.0"));
        };
        let server = |app: &mut FlighthookApp, group, shot: Option<ShotKey>| {
            let shot = shot.map(|key| GroupedShot {
                actor: "mevo.0".into(),
                key,
            });
            app.handle_bus_event(
                FlighthookMessage::new(FlighthookEvent::ShotGroup { group, shot })
                    .actor("webserver.0"),
            );
        };
        let shot = |app: &mut FlighthookApp, key: ShotKey, group| {
            device(app, FlighthookEvent::ShotTrigger { key: key.clone() });
            device(
                app,
                FlighthookEvent::BallFlight {
                    key: key.clone(),
                    ball: Box::new(BallFlight::default()),
                    derived: None,
                },
            );
            server(app, group, Some(key));
        };
        let set_ids = |app: &FlighthookApp| -> Vec<String> {
            app.groups.iter_sets().map(|g| g.id.clone()).collect()
        };

        let mut app = FlighthookApp::with_pending(net::new_pending());
        {
            let mut p = app.pending.lock().expect("lock");
            p.groups = Some(reply(vec![
                set("dr", 1, Club::Driver, false, 1),
                set("i7", 2, Club::Iron7, true, 2),
            ]));
            p.shots = Some(vec![
                stored(1, "a1", "dr"),
                stored(2, "a2", "i7"),
                stored(3, "a3", "i7"),
            ]);
        }
        app.apply_pending();
        app.selected_club = Some(Club::Iron7);
        app.groups.observe_club(app.selected_club, &app.shots);
        let selected = app.groups.selected_at(Some(Club::Iron7), &app.shots);
        assert_eq!(selected, Some((7, 1)), "after a3");

        // The device reconnects and counts from 1 again: its shot #1 joins
        // the open set, and the earlier #1 keeps its set.
        shot(&mut app, key(1, "b1"), set("i7", 2, Club::Iron7, true, 3));
        assert!(!app.needs_groups_refresh && !app.needs_shots_refresh);
        assert_eq!(app.shots[3].group.as_deref(), Some("i7"));
        assert_eq!(app.shots[0].group.as_deref(), Some("dr"));

        // The WS reconnects: groups and shots are fetched afresh.
        {
            let mut p = app.pending.lock().expect("lock");
            p.groups = Some(reply(vec![
                set("dr", 1, Club::Driver, false, 1),
                set("i7", 2, Club::Iron7, true, 3),
            ]));
            p.shots = Some(vec![
                stored(1, "a1", "dr"),
                stored(2, "a2", "i7"),
                stored(3, "a3", "i7"),
                stored(1, "b1", "i7"),
            ]);
        }
        app.apply_pending();
        assert_eq!(set_ids(&app), ["dr", "i7"]);
        let current = app.groups.current_group(app.selected_club);
        assert_eq!(current.map(|g| (g.id.as_str(), g.shots)), Some(("i7", 3)));
        assert_eq!(
            app.groups.selected_at(Some(Club::Iron7), &app.shots),
            selected,
            "the club selection still sits after a3"
        );
        assert_eq!(
            groups::find_row(&app.shots, "mevo.0", &key(1, "a1")),
            Some(0)
        );
        assert_eq!(
            groups::find_row(&app.shots, "mevo.0", &key(1, "b1")),
            Some(3)
        );

        // DR starts a new set; the new #2 joins it, and the earlier #2 keeps
        // its set.
        app.handle_bus_event(
            FlighthookMessage::new(FlighthookEvent::ClubInfo {
                club_info: crate::types::ClubInfo { club: Club::Driver },
            })
            .actor("web"),
        );
        server(&mut app, set("i7", 2, Club::Iron7, false, 3), None);
        server(&mut app, set("dr2", 3, Club::Driver, true, 0), None);
        shot(&mut app, key(2, "b2"), set("dr2", 3, Club::Driver, true, 1));
        assert!(!app.needs_groups_refresh && !app.needs_shots_refresh);
        assert_eq!(app.shots[4].group.as_deref(), Some("dr2"));
        assert_eq!(app.shots[4].club_selected, Some(Club::Driver));
        assert_eq!(app.shots[1].group.as_deref(), Some("i7"));
        assert_eq!(app.shots[1].club_selected, Some(Club::Iron7));
        assert_eq!(set_ids(&app), ["dr", "i7", "dr2"]);
    }

    #[test]
    fn shots_rev_bumps_at_every_shot_change_and_keys_the_caches() {
        use crate::types::{BallFlight, ClubData, ShotGrouping, ShotGroupsResponse};
        use flighthook::{GroupedShot, ShotData, ShotGroupInfo, ShotKey};

        let mut app = FlighthookApp::with_pending(net::new_pending());
        let key = ShotKey {
            shot_id: "a1".into(),
            shot_number: 1,
        };
        let rev = std::cell::Cell::new(app.shots_rev);
        let bumped = |app: &FlighthookApp, what: &str| {
            assert!(app.shots_rev > rev.get(), "{what}");
            rev.set(app.shots_rev);
        };

        // REST load.
        app.pending.lock().expect("lock").shots = Some(vec![ShotData {
            actor: "mevo.0".into(),
            shot_number: 9,
            ball: Some(BallFlight::default()),
            ..ShotData::default()
        }]);
        app.apply_pending();
        bumped(&app, "REST load");
        app.refresh_shot_caches();
        assert_eq!(
            app.display_rows
                .peek(&(app.shots_rev, app.units()))
                .map(Vec::len),
            Some(1)
        );

        let device = |app: &mut FlighthookApp, event| {
            app.handle_bus_event(FlighthookMessage::new(event).actor("square.0"));
        };
        device(&mut app, FlighthookEvent::ShotTrigger { key: key.clone() });
        bumped(&app, "trigger");
        device(
            &mut app,
            FlighthookEvent::BallFlight {
                key: key.clone(),
                ball: Box::new(BallFlight::default()),
                derived: None,
            },
        );
        bumped(&app, "ball flight");
        device(
            &mut app,
            FlighthookEvent::ClubPath {
                key: key.clone(),
                club: Box::new(ClubData::default()),
            },
        );
        bumped(&app, "club path");
        // An event for no listed shot changes nothing.
        device(
            &mut app,
            FlighthookEvent::ClubPath {
                key: ShotKey {
                    shot_id: "zz".into(),
                    shot_number: 77,
                },
                club: Box::new(ClubData::default()),
            },
        );
        assert_eq!(app.shots_rev, rev.get(), "unknown shot");

        // A shot_group event stamps the joined row.
        app.pending.lock().expect("lock").groups = Some(ShotGroupsResponse {
            server: "webserver.0".into(),
            mode: ShotGrouping::BySet,
            session: 0,
            current: None,
            groups: Vec::new(),
        });
        app.apply_pending();
        let groups_rev = app.groups.rev();
        app.handle_bus_event(
            FlighthookMessage::new(FlighthookEvent::ShotGroup {
                group: ShotGroupInfo {
                    id: "s1".into(),
                    number: 1,
                    session: 0,
                    golfer: None,
                    club: Some(Club::Iron7),
                    started_at: "2026-10-26T15:41:28.300Z".into(),
                    manual: false,
                    open: true,
                    shots: 1,
                },
                shot: Some(GroupedShot {
                    actor: "square.0".into(),
                    key,
                }),
            })
            .actor("webserver.0"),
        );
        bumped(&app, "shot_group");
        assert!(app.groups.rev() > groups_rev);
        assert_eq!(app.shots[1].group.as_deref(), Some("s1"));

        // The caches follow: the display rows and the device ids.
        app.refresh_shot_caches();
        let rows = app.display_rows.peek(&(app.shots_rev, app.units()));
        assert_eq!(rows.map(Vec::len), Some(2));
        assert_eq!(rows.and_then(|r| r[1].group.as_deref()), Some("s1"));
        assert_eq!(
            app.device_ids.peek(&app.shots_rev).cloned(),
            Some(vec!["mevo.0".to_owned(), "square.0".to_owned()])
        );
        // A unit flip rebuilds the rows without touching the shots.
        let rev = rev.get();
        app.settings.default_units = UnitSystem::Metric;
        assert!(app.display_rows.peek(&(rev, UnitSystem::Metric)).is_none());
        app.refresh_shot_caches();
        assert!(app.display_rows.peek(&(rev, UnitSystem::Metric)).is_some());
        assert_eq!(app.shots_rev, rev);
    }

    #[test]
    fn clear_history_on_the_bus_empties_the_shots_tab() {
        use crate::types::{BallFlight, ShotGrouping, ShotGroupsResponse};
        use flighthook::{ShotData, ShotGroupInfo};

        let stored = |actor: &str, n| ShotData {
            actor: actor.into(),
            shot_number: n,
            ball: Some(BallFlight::default()),
            group: Some("s1".into()),
            ..ShotData::default()
        };
        let set = ShotGroupInfo {
            id: "s1".into(),
            number: 1,
            session: 2,
            golfer: None,
            club: Some(Club::Iron7),
            started_at: "2026-10-26T15:41:28.300Z".into(),
            manual: false,
            open: true,
            shots: 3,
        };
        let mut app = FlighthookApp::with_pending(net::new_pending());
        {
            let mut p = app.pending.lock().expect("lock");
            p.shots = Some(vec![
                stored("mevo.0", 1),
                stored("square.0", 1),
                stored("mevo.0", 2),
            ]);
            p.groups = Some(ShotGroupsResponse {
                server: "webserver.0".into(),
                mode: ShotGrouping::BySet,
                session: 2,
                current: Some("s1".into()),
                groups: vec![set],
            });
        }
        app.apply_pending();
        // A live shot arrives and pulses.
        let live = flighthook::ShotKey {
            shot_id: "m3".into(),
            shot_number: 3,
        };
        for event in [
            FlighthookEvent::ShotTrigger { key: live.clone() },
            FlighthookEvent::BallFlight {
                key: live,
                ball: Box::new(BallFlight::default()),
                derived: None,
            },
        ] {
            app.handle_bus_event(FlighthookMessage::new(event).actor("mevo.0"));
        }
        app.shot_focus.observe(&app.shots, None, 10.0);
        app.device_filter = Some("square.0".into());
        app.confirm_clear = true;
        app.refresh_shot_caches();
        assert!(app.device_ids.peek(&app.shots_rev).is_some());
        let (shots_rev, groups_rev) = (app.shots_rev, app.groups.rev());
        assert!(app.shot_focus.progress(10.0).is_some(), "pulsing");
        // A reply fetched before the clear, not drained yet.
        app.pending.lock().expect("lock").shots = Some(vec![stored("mevo.0", 1)]);

        // Pressed in another tab: every UI sees the command.
        app.handle_bus_event(
            FlighthookMessage::new(FlighthookEvent::ClearShotHistory).actor("web"),
        );
        assert!(app.shots.is_empty());
        assert!(app.groups.is_empty());
        assert_eq!(app.groups.session(), 3, "a new session");
        assert!(app.shots_rev > shots_rev);
        assert!(app.groups.rev() > groups_rev);
        assert_eq!(app.device_filter, None);
        assert!(!app.confirm_clear, "an open confirmation closes");
        assert_eq!(app.shot_focus.progress(10.0), None);
        assert!(app.device_ids.peek(&shots_rev).is_none(), "caches emptied");
        app.apply_pending();
        assert!(app.shots.is_empty(), "the stale reply is dropped");
        assert!(!app.needs_shots_refresh && !app.needs_groups_refresh);
        assert_eq!(
            app.log_entries.last().map(|e| e.message_type.as_str()),
            Some("clear_shot_history")
        );

        // The next shot starts afresh.
        app.handle_bus_event(
            FlighthookMessage::new(FlighthookEvent::ShotTrigger {
                key: flighthook::ShotKey {
                    shot_id: "n1".into(),
                    shot_number: 1,
                },
            })
            .actor("mevo.0"),
        );
        assert_eq!(app.shots.len(), 1);
    }

    #[test]
    fn group_commands_over_rest_update_the_sending_ui() {
        use crate::panels::shots::GroupCommand;
        use crate::types::{ShotGrouping, ShotGroupsResponse};
        let mut app = FlighthookApp::with_pending(net::new_pending());
        app.pending.lock().expect("lock").groups = Some(ShotGroupsResponse {
            server: "webserver.0".into(),
            mode: ShotGrouping::BySet,
            session: 1,
            current: None,
            groups: Vec::new(),
        });
        app.apply_pending();
        for n in 1..=3 {
            app.handle_bus_event(
                FlighthookMessage::new(FlighthookEvent::ShotTrigger {
                    key: flighthook::ShotKey {
                        shot_id: format!("s{n}"),
                        shot_number: n,
                    },
                })
                .actor("mevo.0"),
            );
        }
        app.confirm_clear = true;
        let generation = app.history_generation;

        // New session: the UI's session starts at once, and the sets are
        // fetched once the POST is answered.
        app.apply_posted(GroupCommand::NewSession);
        assert_eq!(app.groups.session(), 2);
        assert_eq!(app.history_generation, generation + 1);
        assert!(!app.needs_groups_refresh);
        app.pending.lock().expect("lock").groups_posted = true;
        app.apply_pending();
        assert!(app.needs_groups_refresh);
        assert!(!app.pending.lock().expect("lock").groups_posted, "taken");

        // New set: nothing until the answer, then the sets.
        app.needs_groups_refresh = false;
        app.apply_posted(GroupCommand::NewGroup);
        assert_eq!(app.shots.len(), 3);
        assert!(!app.needs_groups_refresh);

        // Clear history: the tab empties as for the bus event.
        app.apply_posted(GroupCommand::ClearHistory);
        assert!(app.shots.is_empty());
        assert!(app.groups.is_empty());
        assert!(!app.confirm_clear);
    }

    #[test]
    fn replies_requested_before_a_clear_or_new_session_are_dropped() {
        use crate::types::{BallFlight, ShotGrouping, ShotGroupsResponse};
        use flighthook::{ShotData, ShotGroupInfo};

        let stored = |n| ShotData {
            actor: "mevo.0".into(),
            shot_number: n,
            ball: Some(BallFlight::default()),
            group: Some("s1".into()),
            ..ShotData::default()
        };
        let sets = |session| ShotGroupsResponse {
            server: "webserver.0".into(),
            mode: ShotGrouping::BySet,
            session,
            current: Some("s1".into()),
            groups: vec![ShotGroupInfo {
                id: "s1".into(),
                number: 1,
                session,
                golfer: None,
                club: Some(Club::Iron7),
                started_at: "2026-10-26T15:41:28.300Z".into(),
                manual: false,
                open: true,
                shots: 2,
            }],
        };
        let land = |app: &mut FlighthookApp, generation, shots: Option<Vec<ShotData>>, groups| {
            {
                let mut p = app.pending.lock().expect("lock");
                p.shots = shots;
                p.shots_generation = generation;
                p.groups = groups;
                p.groups_generation = generation;
            }
            app.apply_pending();
        };
        let mut app = FlighthookApp::with_pending(net::new_pending());
        land(&mut app, 0, Some(vec![stored(1), stored(2)]), Some(sets(4)));
        assert_eq!((app.shots.len(), app.groups.session()), (2, 4));

        // A refetch goes out, then the history is cleared before it lands.
        let asked = app.history_generation;
        app.handle_bus_event(
            FlighthookMessage::new(FlighthookEvent::ClearShotHistory).actor("web"),
        );
        land(
            &mut app,
            asked,
            Some(vec![stored(1), stored(2)]),
            Some(sets(4)),
        );
        assert!(app.shots.is_empty(), "no pre-clear shot comes back");
        assert!(app.groups.is_empty(), "no pre-clear set comes back");
        assert_eq!(app.groups.session(), 5);
        assert!(!app.needs_shots_refresh && !app.needs_groups_refresh);

        // One requested after the clear applies.
        let asked = app.history_generation;
        land(&mut app, asked, Some(vec![stored(1)]), Some(sets(5)));
        assert_eq!((app.shots.len(), app.groups.session()), (1, 5));

        // A new session before the replies land: the shots still hold, the
        // sets are fetched again rather than winding the session back.
        let asked = app.history_generation;
        app.handle_bus_event(FlighthookMessage::new(FlighthookEvent::NewShotSession).actor("web"));
        assert_eq!(app.groups.session(), 6);
        land(
            &mut app,
            asked,
            Some(vec![stored(1), stored(2)]),
            Some(sets(5)),
        );
        assert_eq!(app.shots.len(), 2);
        assert_eq!(app.groups.session(), 6, "the stale sets are dropped");
        assert!(app.needs_groups_refresh && !app.needs_shots_refresh);
        let asked = app.history_generation;
        land(&mut app, asked, None, Some(sets(6)));
        assert_eq!(app.groups.session(), 6);
        assert!(app.groups.get("s1").is_some());
    }

    #[test]
    fn device_filter_falls_back_to_all_when_its_device_leaves() {
        use crate::types::BallFlight;
        use flighthook::ShotData;

        let stored = |actor: &str, n| ShotData {
            actor: actor.into(),
            shot_number: n,
            ball: Some(BallFlight::default()),
            ..ShotData::default()
        };
        let mut app = FlighthookApp::with_pending(net::new_pending());
        app.pending.lock().expect("lock").shots =
            Some(vec![stored("mevo.0", 1), stored("square.0", 1)]);
        app.apply_pending();
        app.device_filter = Some("square.0".into());
        app.refresh_shot_caches();
        assert_eq!(app.device_filter.as_deref(), Some("square.0"));
        // The Omni's shots age out of the history.
        app.pending.lock().expect("lock").shots = Some(vec![stored("mevo.0", 1)]);
        app.apply_pending();
        app.refresh_shot_caches();
        assert_eq!(app.device_filter, None);
    }

    #[test]
    fn face_impact_and_handedness_reach_the_rows() {
        use crate::types::{ClubInfo, Distance, FaceImpact, Handedness};
        use flighthook::{ShotData, ShotKey};

        let mut app = FlighthookApp::with_pending(net::new_pending());
        // Handedness from the WebSocket handshake, then from the bus; a
        // mode-only change keeps it.
        app.apply_global_state(&GameStateSnapshot {
            player_info: None,
            club_info: Some(ClubInfo { club: Club::Iron7 }),
            mode: None,
            handed: Some(Handedness::Left),
        });
        assert_eq!(app.handed, Some(Handedness::Left));
        assert_eq!(app.selected_club, Some(Club::Iron7));
        let hand = |app: &mut FlighthookApp, handed| {
            app.handle_bus_event(
                FlighthookMessage::new(FlighthookEvent::HandInfo {
                    hand_info: crate::types::HandInfo { handed },
                })
                .actor("openconnect_client.0"),
            );
        };
        let set = |app: &mut FlighthookApp, mode, handed| {
            app.handle_bus_event(
                FlighthookMessage::new(FlighthookEvent::SetDetectionMode {
                    mode,
                    handed,
                    club: None,
                })
                .actor("system"),
            );
        };
        hand(&mut app, Handedness::Right);
        assert_eq!(app.handed, Some(Handedness::Right));
        set(&mut app, Some(ShotDetectionMode::Chipping), None);
        assert_eq!(app.handed, Some(Handedness::Right));
        assert_eq!(app.current_mode, ShotDetectionMode::Chipping.to_string());
        // The device command is not where the UI takes the hand from.
        set(&mut app, None, Some(Handedness::Left));
        assert_eq!(app.handed, Some(Handedness::Right));
        hand(&mut app, Handedness::Left);

        // A live shot is stamped with the handedness at its trigger, and
        // its face impact fills the row.
        let key = ShotKey {
            shot_id: "s1".into(),
            shot_number: 1,
        };
        let device = |app: &mut FlighthookApp, event| {
            app.handle_bus_event(FlighthookMessage::new(event).actor("square.0"));
        };
        device(&mut app, FlighthookEvent::ShotTrigger { key: key.clone() });
        assert_eq!(app.shots[0].handed, Some(Handedness::Left));
        let rev = app.shots_rev;
        let impact = FaceImpact {
            lateral: Some(Distance::Inches(0.31)),
            vertical: Some(Distance::Millimeters(-2.0)),
        };
        device(
            &mut app,
            FlighthookEvent::FaceImpact {
                key,
                impact: Box::new(impact.clone()),
            },
        );
        assert!(app.shots_rev > rev);
        assert_eq!(app.shots[0].impact, Some(impact.clone()));

        // Rows from `/api/shots` keep both, and a unit change leaves the
        // impact as sent (it is read in millimetres everywhere).
        let row = ShotRow::from(ShotData {
            actor: "square.0".into(),
            shot_number: 2,
            impact: Some(impact.clone()),
            handed: Some(Handedness::Left),
            ..ShotData::default()
        });
        assert_eq!(row.handed, Some(Handedness::Left));
        for units in [UnitSystem::Imperial, UnitSystem::Metric] {
            let converted = row.to_unit_system(units);
            assert_eq!(converted.impact, Some(impact.clone()));
            assert_eq!(converted.handed, Some(Handedness::Left));
            let lateral = converted.impact.unwrap().lateral.unwrap();
            assert!((lateral.as_millimeters() - 7.874).abs() < 1e-9);
        }
    }

    #[test]
    fn a_clip_stamps_every_row_of_its_shots_and_a_removal_clears_them() {
        use crate::types::{CameraPosition, ShotKey};

        let key = |id: &str, n: u32| ShotKey {
            shot_id: id.into(),
            shot_number: n,
        };
        let clip = |camera: &str, keys: Vec<ShotKey>, duration_ms| {
            FlighthookMessage::new(FlighthookEvent::ShotVideo {
                keys,
                position: CameraPosition::GolferFront,
                duration_ms,
                fps: 120.0,
                trigger_offset_ms: 4000,
                impact_offset_ms: 4000,
                complete: true,
                dropped: 0,
            })
            .actor(camera)
        };
        let mut app = FlighthookApp::with_pending(net::new_pending());
        // Two launch monitors saw one swing; a third shot has no clip.
        for (actor, k) in [
            ("mevo.0", key("a", 1)),
            ("square.0", key("b", 1)),
            ("mevo.0", key("c", 2)),
        ] {
            app.handle_bus_event(
                FlighthookMessage::new(FlighthookEvent::ShotTrigger { key: k }).actor(actor),
            );
        }
        // The clip lands before either shot finished.
        let rev = app.shots_rev;
        app.handle_bus_event(clip("camera.0", vec![key("a", 1), key("b", 1)], 5500));
        assert!(app.shots_rev > rev);
        assert_eq!(app.shots[0].video.len(), 1);
        assert_eq!(app.shots[1].video[0].camera, "camera.0");
        assert!(app.shots[2].video.is_empty());
        // Finishing keeps it; a second camera adds its own; the same camera
        // again replaces its clip.
        app.handle_bus_event(
            FlighthookMessage::new(FlighthookEvent::ShotFinished {
                key: key("a", 1),
                raw: None,
            })
            .actor("mevo.0"),
        );
        app.handle_bus_event(clip("camera.1", vec![key("a", 1)], 5500));
        app.handle_bus_event(clip("camera.0", vec![key("a", 1)], 4000));
        let cameras: Vec<(&str, u64)> = app.shots[0]
            .video
            .iter()
            .map(|v| (v.camera.as_str(), v.duration_ms))
            .collect();
        assert_eq!(cameras, [("camera.0", 4000), ("camera.1", 5500)]);
        // A clip after its shot finished stamps it too.
        app.handle_bus_event(
            FlighthookMessage::new(FlighthookEvent::ShotFinished {
                key: key("c", 2),
                raw: None,
            })
            .actor("mevo.0"),
        );
        app.handle_bus_event(clip("camera.0", vec![key("c", 2)], 5500));
        assert_eq!(app.shots[2].video.len(), 1);
        // A clip of a shot the UI doesn't hold changes nothing.
        let rev = app.shots_rev;
        app.handle_bus_event(clip("camera.0", vec![key("zzz", 9)], 5500));
        assert_eq!(app.shots_rev, rev);

        app.handle_bus_event(
            FlighthookMessage::new(FlighthookEvent::ShotVideoRemoved {
                keys: vec![key("a", 1), key("b", 1)],
            })
            .actor("camera.0"),
        );
        assert!(app.shots[0].video.is_empty() && app.shots[1].video.is_empty());
        assert_eq!(app.shots[2].video.len(), 1);
        assert_eq!(
            app.log_entries.last().unwrap().message_type,
            "shot_video_removed"
        );
    }

    #[test]
    fn an_open_replay_closes_when_its_clips_are_removed() {
        use crate::types::{CameraPosition, ShotKey};

        let key = |id: &str| ShotKey {
            shot_id: id.into(),
            shot_number: 1,
        };
        let mut app = FlighthookApp::with_pending(net::new_pending());
        for (actor, id) in [("mevo.0", "a"), ("mevo.1", "b")] {
            app.handle_bus_event(
                FlighthookMessage::new(FlighthookEvent::ShotTrigger { key: key(id) }).actor(actor),
            );
            app.handle_bus_event(
                FlighthookMessage::new(FlighthookEvent::ShotVideo {
                    keys: vec![key(id)],
                    position: CameraPosition::GolferFront,
                    duration_ms: 5500,
                    fps: 60.0,
                    trigger_offset_ms: 4000,
                    impact_offset_ms: 4000,
                    complete: true,
                    dropped: 0,
                })
                .actor("camera.0"),
            );
        }
        let ctx = egui::Context::default();
        app.open_replay(&ctx, 0);
        assert_eq!(app.replay.as_ref().map(|r| r.shot_id()), Some("a"));
        let removed = |id: &str| {
            FlighthookMessage::new(FlighthookEvent::ShotVideoRemoved {
                keys: vec![key(id)],
            })
            .actor("camera.0")
        };
        app.handle_bus_event(removed("b"));
        assert!(app.replay.is_some(), "another shot's clips");
        app.handle_bus_event(removed("a"));
        assert!(app.replay.is_none());
    }

    #[test]
    fn rows_from_the_server_carry_their_clips_and_the_camera_flags() {
        use crate::types::{CameraPosition, ShotData, ShotVideo, StatusResponse};

        let video = ShotVideo {
            camera: "camera.0".into(),
            position: CameraPosition::TargetLine,
            duration_ms: 5500,
            fps: 60.0,
            trigger_offset_ms: 4000,
            impact_offset_ms: 2500,
            complete: false,
            dropped: 3,
            pose: false,
            swing: false,
        };
        let row: ShotData = serde_json::from_value(serde_json::json!({
            "actor": "mevo.0", "shot_number": 4, "shot_id": "s4",
            "ball": {}, "video": [video],
        }))
        .unwrap();
        let mut app = FlighthookApp::with_pending(net::new_pending());
        {
            let mut p = app.pending.lock().unwrap();
            p.shots = Some(vec![row]);
            p.status = Some(StatusResponse {
                actors: HashMap::new(),
                mode: None,
                club: None,
                handed: None,
                cameras_supported: true,
                clip_dir: Some("/clips".into()),
                config_error: None,
            });
        }
        app.apply_pending();
        assert_eq!(app.shots[0].video, [video]);
        assert!(app.cameras_supported);
        assert_eq!(app.clip_dir.as_deref(), Some("/clips"));
        // Converting units keeps the clips.
        let metric = app.shots[0].to_unit_system(UnitSystem::Metric);
        assert_eq!(metric.video.len(), 1);
    }
}
