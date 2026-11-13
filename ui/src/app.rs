//! egui application — FlighthookApp.

use std::collections::{HashMap, HashSet};

use crate::chart::{self, Cached, ChartKey, MembersKey};
use crate::groups::ShotGroups;
use crate::net::{self, PendingHandle, WsPollEvent};
use crate::panels::cards::{self, AddPreset, CardGroup};
use crate::panels::devices::mark;
use crate::panels::dispersion::{self, DispersionKey, EllipseSize, Points};
use crate::panels::settings::{PendingRemoval, SettingsForm};
use crate::panels::shots::{ShotFocus, ShotsLayout, ShotsView};
use crate::panels::{Tab, flight};
use crate::scale::{self, UiScale};
use crate::theme;
use crate::types::{
    ActorStatus, ActorStatusResponse, CameraListResponse, Club, Handedness, LogEntry, RoundState,
    ShotRow, UnitSystem,
};

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
    /// When [`Self::club_pick`] was made ([`Self::now_ms`]).
    pub(crate) club_picked_ms: u64,
    /// The golfer up: the raw id flighthook's `golfer.turn` names.
    pub(crate) turn_golfer: Option<String>,
    /// The top bar's open dropdown.
    pub(crate) header_menu: Option<crate::topbar::HeaderMenu>,
    /// Whether the view follows the turn (session state; default on).
    pub(crate) follow_turns: bool,
    /// The toast showing: its text and when it was set (input time, s).
    pub(crate) toast: Option<(String, f64)>,
    /// Each tournament mode switch the header sent, in order (tests only).
    #[cfg(test)]
    pub(crate) tournament_sent: Vec<bool>,
    /// The Auto-chip distance (yards) sent and not answered yet.
    pub(crate) auto_chip_in_flight: Option<u32>,
    /// Why the last Auto-chip was not sent, shown in the error banner until
    /// the next one.
    pub(crate) auto_chip_error: Option<String>,
    /// Why the last tournament mode switch failed, shown in the error
    /// banner until the next switch.
    pub(crate) tournament_error: Option<String>,
    /// The turn's handedness (`None` until the server reports one).
    /// Stamped on shots as they trigger, like the club.
    pub(crate) handed: Option<Handedness>,
    /// The tournament round, as the server's round policy reports it.
    pub(crate) round: RoundState,

    // Shot groups of the webserver the UI talks to
    pub(crate) groups: ShotGroups,

    // Settings
    pub(crate) settings: SettingsForm,

    // Networking
    pub(crate) pending: PendingHandle,
    pub(crate) ws_sender: Option<ewebsock::WsSender>,
    pub(crate) ws_receiver: Option<ewebsock::WsReceiver>,
    /// The golfer selects this UI sent, in order (tests only).
    #[cfg(test)]
    pub(crate) sent_selects: Vec<flighthook::UiCommand>,
    /// flighthook's own actor id (from the UI socket's snapshot).
    pub(crate) flighthook_id: String,
    /// The local card key of each flighthook actor, by actor id.
    pub(crate) card_keys: HashMap<String, String>,
    /// Every FRP state on the bus, as the UI socket tells it.
    pub(crate) bus_states: std::collections::BTreeMap<flighthook::StateKey, flighthook::BusMessage>,
    /// The `bus.actor` name of every remote actor heard this session, by
    /// actor id; kept after the actor leaves, so its shots stay named.
    pub(crate) remote_names: HashMap<String, String>,
    /// The last `golfer.profile` name heard for each golfer id this
    /// session; kept after the profile is removed, so a removed golfer's
    /// shots stay named.
    pub(crate) golfer_names: HashMap<String, String>,
    /// The golfer keys shown as "Unknown golfer N", in the order they were
    /// first shown: N is the position plus one, for the session.
    pub(crate) unknown_golfers: Vec<String>,

    // UI state
    pub(crate) ws_connected: bool,
    pub(crate) ws_ever_connected: bool,
    pub(crate) active_tab: Tab,
    /// The Settings section of the item shown, kept for the session.
    pub(crate) settings_section: crate::panels::settings_nav::SettingsSection,
    /// The Settings item picked; none, or one no longer listed, shows the
    /// section's first item.
    pub(crate) settings_item: Option<crate::panels::settings_nav::SettingsItem>,
    /// On the phone, the item's page is open over the Settings list.
    pub(crate) settings_open: bool,
    /// Tests only: Settings draws every item's page, one after another.
    #[cfg(test)]
    pub(crate) all_settings_sections: bool,
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
    /// The viewed golfer: a golfer key by the lookup rule (`None` = no
    /// golfer). Every view on the Shots tab shows this golfer's shots; it
    /// is the golfer up while the view follows the turn.
    pub(crate) viewed_golfer: Option<String>,
    /// A view-only row of the Golfer card picked while the view follows
    /// the turn: its key and the golfer up's key at the pick. The view stays
    /// on it until the next event of the golfer up: a turn event, a new
    /// shot, a click on a known golfer's row, or another golfer up.
    pub(crate) view_hold: Option<(Option<String>, Option<String>)>,
    /// The Golfer card's rows, worked out once a frame in
    /// `refresh_shot_caches`.
    pub(crate) golfer_list: crate::topbar::GolferList,
    /// Indices into `shots` of the rows the filter passes.
    pub(crate) filtered_rows: Cached<(u64, u64, chart::FilterKey), Vec<usize>>,
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
    /// The welcome screen's checked presets, by (+) menu label.
    pub(crate) wizard_picked: HashSet<&'static str>,
    pub(crate) wizard_saving: bool,
    /// Why the last Get Started failed.
    pub(crate) wizard_error: Option<String>,

    /// The UI zoom: fitted to the window every frame, plus the session's
    /// Ctrl/Cmd +/- steps.
    ui_scale: UiScale,
    /// The window is a phone held upright (`scale::portrait`), set every
    /// frame: each screen lays out its portrait variant.
    pub(crate) portrait: bool,

    // Cameras and replay
    /// The server can record from cameras (`/api/status`).
    pub(crate) cameras_supported: bool,
    /// The server can read a trackball, so the Launch monitors menu offers
    /// the Trackball card.
    pub(crate) trackball_supported: bool,
    /// A status reply has been applied, so `cameras_supported` and
    /// `trackball_supported` are the server's.
    pub(crate) status_loaded: bool,
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
    /// Which of the Shots and Cameras tabs shows while the shots
    /// navigation is on screen (the nav icons), kept on this device.
    pub(crate) shots_pane: crate::panels::cameras::ShotsPane,
    /// Live is on (the Cameras pane's Live switch): the pane shows the
    /// live views.
    pub(crate) live_selected: bool,
    /// Which population's bands the Cameras pane colors the swing numbers
    /// against (the toolbar's reference picker), kept on this device.
    pub(crate) body_reference: flighthook::ReferenceKind,
    /// The Cameras pane's Last shot strip under the live pictures.
    pub(crate) live_strip: crate::panels::last_shot::LiveStrip,
    /// The Cameras pane was on screen at the last
    /// [`FlighthookApp::sync_cameras_pane`]: coming on screen picks Live or
    /// the latest shot.
    pub(crate) cameras_pane_was_shown: bool,
    /// The shots navigation (the Shots or Cameras tab) was on screen at the
    /// last [`FlighthookApp::sync_cameras_pane`]: a switch between the two
    /// tabs keeps the selected shot.
    pub(crate) shots_nav_was_shown: bool,
    /// The Cameras pane's player of the shown shot's clips.
    pub(crate) shot_player: Option<crate::panels::replay::ReplayView>,
    /// The (shot ID, camera) clips asked to be tracked again this session
    /// because their camera's rotation changed since they were tracked.
    pub(crate) retracked: HashSet<(String, String)>,
    /// The Cameras pane's live views, by camera actor ID, while Live is on
    /// screen.
    pub(crate) live: crate::panels::cameras::LiveViews,
    /// Number of the last live view opened: replies for another are
    /// dropped.
    pub(crate) live_seq: u64,
    /// Body tracking values saved at once, and their saves.
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
        // which is deferred to give the page time to stabilize on iOS
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
            club_picked_ms: 0,
            turn_golfer: None,
            header_menu: None,
            follow_turns: true,
            toast: None,
            #[cfg(test)]
            sent_selects: Vec::new(),
            #[cfg(test)]
            tournament_sent: Vec::new(),
            auto_chip_in_flight: None,
            auto_chip_error: None,
            tournament_error: None,
            handed: None,
            round: RoundState::default(),
            groups: ShotGroups::default(),
            settings: SettingsForm::default(),
            pending,
            ws_sender: None,
            ws_receiver: None,
            flighthook_id: String::new(),
            card_keys: HashMap::new(),
            bus_states: std::collections::BTreeMap::new(),
            remote_names: HashMap::new(),
            golfer_names: HashMap::new(),
            unknown_golfers: Vec::new(),
            ws_connected: false,
            ws_ever_connected: false,
            active_tab: Tab::Shots,
            settings_section: crate::panels::settings_nav::SettingsSection::default(),
            settings_item: None,
            settings_open: false,
            #[cfg(test)]
            all_settings_sections: false,
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
            viewed_golfer: None,
            view_hold: None,
            golfer_list: crate::topbar::GolferList::default(),
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
            wizard_picked: HashSet::new(),
            wizard_saving: false,
            wizard_error: None,
            ui_scale: UiScale::default(),
            portrait: false,
            cameras_supported: false,
            trackball_supported: false,
            status_loaded: false,
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
            body_reference: crate::panels::cameras::stored_reference(),
            live_strip: crate::panels::last_shot::LiveStrip::default(),
            cameras_pane_was_shown: false,
            shots_nav_was_shown: false,
            shot_player: None,
            retracked: HashSet::new(),
            live: HashMap::new(),
            live_seq: 0,
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
            // Card keys of actors that joined since the socket's snapshot.
            for (id, actor) in &status.actors {
                if let Some(card) = &actor.card {
                    self.card_keys.insert(id.clone(), card.clone());
                }
            }
            let turn = status.turn.as_ref();
            if let Some(mode) = turn.and_then(|t| t.mode) {
                self.current_mode = mode.to_string();
            }
            let club_age = self.now_ms.saturating_sub(self.club_picked_ms);
            if let Some(club) = status_pick(
                self.ws_connected,
                &mut self.club_pick,
                club_age,
                turn.and_then(|t| t.club),
            ) {
                self.selected_club = club;
            }
            // With the WebSocket up, its turn and `round_state` are the
            // live source.
            if !self.ws_connected {
                self.handed = turn.and_then(|t| t.handedness);
                self.round = status.round;
            }
            self.cameras_supported = status.cameras_supported;
            self.trackball_supported = status.trackball_supported;
            self.status_loaded = true;
            self.clip_dir = status.clip_dir;
            self.config_error = status.config_error;
            // The reply lists every running actor: one it leaves out has
            // stopped. Red time counts down from now.
            self.actors = status.cards;
            for actor in self.actors.values_mut() {
                actor.received(self.now_ms);
            }
        }

        // A history cleared since the request makes a reply stale: the empty
        // history and the live events since are the whole of it.
        if let Some(shots) = p.shots.take()
            && p.shots_generation >= self.cleared_generation
        {
            for shot in &shots {
                keep_remote_name(&mut self.remote_names, &self.card_keys, shot);
                keep_golfer_name(&mut self.golfer_names, shot);
            }
            self.shots = shots.into_iter().map(ShotRow::from).collect();
            self.shots_rev += 1;
            self.shot_focus.sync(
                &self.shots,
                crate::panels::shots::shot_filter(
                    &self.device_filter,
                    &self.viewed_golfer,
                    &self.groups,
                ),
            );
        }

        // A sets reply requested before the last clear is dropped.
        if let Some(groups) = p.groups.take()
            && p.groups_generation >= self.cleared_generation
        {
            self.groups.replace(groups);
        }

        if std::mem::take(&mut p.groups_posted) {
            self.needs_groups_refresh = true;
        }

        if let Some(reply) = p.auto_chip.take() {
            self.auto_chip_in_flight = None;
            self.auto_chip_error = reply.err();
        }

        if let Some(reply) = p.tournament.take() {
            self.tournament_error = reply.err();
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
            // The saved default view.
            self.groups.mode = s.shot_grouping;
        }

        if let Some(reply) = p.settings_save.take() {
            if let Ok(response) = &reply {
                self.settings.keep_issued_golfer_ids(&response.golfers);
            }
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
            let _ = self.settings.finish_save(result);
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
            } else if let Some(strip) = self.live_strip.find_mut(seq) {
                strip.reply(reply);
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
    pub(crate) fn reconnect_started(&mut self, id: &str) {
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
            ws_disconnected |= self.handle_ws_event(event);
        }
        if ws_disconnected {
            self.ws_sender = None;
            self.ws_receiver = None;
            self.ws_connected = false;
        }
    }

    /// Handle one WebSocket event. Returns whether the socket is gone: then
    /// the server's state it carried is dropped with it (the actors on a
    /// close, and the tournament round either way, so no banner outlives
    /// the server that held the round).
    pub(crate) fn handle_ws_event(&mut self, event: WsPollEvent) -> bool {
        match event {
            WsPollEvent::Opened => false,
            WsPollEvent::Message(msg) => {
                self.handle_ui_message(*msg);
                false
            }
            WsPollEvent::Error(_) => {
                self.round = RoundState::default();
                true
            }
            WsPollEvent::Disconnected => {
                self.actors.clear();
                self.round = RoundState::default();
                true
            }
        }
    }

    /// Take the club, hand and mode of the session's turn (the WebSocket
    /// handshake's, and each change).
    pub(crate) fn apply_turn(&mut self, turn: &flighthook::GolferTurn) {
        self.view_hold = None;
        self.turn_golfer = turn.golfer.as_ref().map(|g| g.as_str().to_owned());
        self.selected_club = turn.club;
        self.club_pick = None;
        self.handed = turn.handedness;
        if let Some(mode) = turn.mode {
            self.current_mode = mode.to_string();
        }
    }
}

impl FlighthookApp {
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
        self.shots_nav_was_shown = false;
    }

    /// The camera cards hold their clips only while the card row can be on
    /// screen: on the Shots tab's Cards view, showing the shot's data, with
    /// the camera cards on in the settings. Elsewhere they are dropped, and
    /// fetched again when the row returns. The Cameras pane's player
    /// likewise lives only while it shows a shot.
    pub(crate) fn drop_hidden_card_clips(&mut self) {
        if self.active_tab != Tab::Shots
            || self.shots_layout() != ShotsLayout::Cards
            || !self.settings.shot_cards_enabled()
        {
            self.card_clips.clear();
        }
        if !self.cameras_pane_shown() || self.live_selected {
            self.shot_player = None;
        }
    }

    /// Whether the welcome screen is up: the settings hold no user actors
    /// and the status has said what the server offers (the Trackball).
    fn show_wizard(&self) -> bool {
        self.wizard_waiting() && self.status_loaded
    }

    /// Whether the welcome screen is due: the settings have loaded with no
    /// user actors and it has not been dismissed.
    fn wizard_waiting(&self) -> bool {
        self.settings.loaded && !self.wizard_dismissed && self.wizard_checked
    }

    /// What the welcome screen offers, under its group titles.
    fn wizard_groups(&self) -> [(&'static str, Vec<&'static AddPreset>); 2] {
        [
            (
                "Launch Monitors",
                cards::welcome_options(CardGroup::LaunchMonitors, self.trackball_supported),
            ),
            (
                "Simulators",
                cards::welcome_options(CardGroup::Software, self.trackball_supported),
            ),
        ]
    }

    /// The checked presets that are offered, in welcome screen order.
    fn wizard_selection(&self) -> Vec<&'static AddPreset> {
        self.wizard_groups()
            .into_iter()
            .flat_map(|(_, presets)| presets)
            .filter(|p| self.wizard_picked.contains(p.label))
            .collect()
    }

    pub(crate) fn render_wizard(&mut self, ctx: &egui::Context) {
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
                            for (i, (title, presets)) in
                                self.wizard_groups().into_iter().enumerate()
                            {
                                if i > 0 {
                                    ui.add_space(12.0);
                                }
                                ui.label(
                                    egui::RichText::new(title).size(12.0).color(theme::TEXT_DIM),
                                );
                                for preset in presets {
                                    ui.add_space(4.0);
                                    self.wizard_checkbox(ui, preset);
                                }
                            }
                        },
                    );

                    ui.add_space(24.0);

                    let any_selected = !self.wizard_selection().is_empty();
                    let btn = theme::primary_button("Get Started", 16.0)
                        .min_size(egui::vec2(160.0, 40.0));

                    let start = ui.add_enabled(any_selected && !self.wizard_saving, btn);
                    mark(ui, || "wizard:get_started".into(), &start);
                    if start.clicked() {
                        self.apply_wizard(ctx);
                    }
                    if let Some(error) = &self.wizard_error {
                        ui.add_space(8.0);
                        crate::panels::settings::save_error_line(ui, "wizard", error);
                    }

                    if !any_selected {
                        ui.add_space(8.0);
                        let skip =
                            ui.link(egui::RichText::new("Skip — configure manually").size(12.0));
                        mark(ui, || "wizard:skip".into(), &skip);
                        if skip.clicked() {
                            self.wizard_dismissed = true;
                            self.active_tab = Tab::Devices;
                        }
                    }
                });
            });
    }

    /// The welcome screen's checkbox for `preset`. Checking it unchecks a
    /// preset that adds the same connection.
    fn wizard_checkbox(&mut self, ui: &mut egui::Ui, preset: &'static AddPreset) {
        let mut checked = self.wizard_picked.contains(preset.label);
        let mut response = ui.checkbox(
            &mut checked,
            egui::RichText::new(preset.welcome_text()).size(15.0),
        );
        if let Some(hover) = preset.welcome_hover {
            response = response.on_hover_text(hover);
        }
        mark(ui, || format!("wizard:{}", preset.label), &response);
        if response.changed() {
            if checked {
                let clashing: Vec<&'static str> = self
                    .wizard_groups()
                    .into_iter()
                    .flat_map(|(_, presets)| presets)
                    .filter(|p| p.same_connection(preset))
                    .map(|p| p.label)
                    .collect();
                for label in clashing {
                    self.wizard_picked.remove(label);
                }
                self.wizard_picked.insert(preset.label);
            } else {
                self.wizard_picked.remove(preset.label);
            }
        }
    }

    /// Get Started: add each checked preset as its (+) menu entry does
    /// ([`SettingsForm::add_preset`]) and save the whole config. The form
    /// takes the new cards once the save has been applied.
    pub(crate) fn apply_wizard(&mut self, ctx: &egui::Context) {
        let mut form = self.settings.clone();
        for preset in self.wizard_selection() {
            let _ = form.add_preset(preset);
        }
        let config = form.to_request();

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
pub(crate) const CLUB_PICKER_ROWS: [std::ops::Range<usize>; 6] =
    [0..2, 2..10, 10..19, 19..28, 28..32, 32..33];

/// Size of a club pill in the picker: equal widths line the rows up as
/// columns, and the height is a comfortable touch target.
pub(crate) const CLUB_PILL: egui::Vec2 = egui::vec2(52.0, 38.0);

/// The code a hand is shown with: `RH` or `LH`.
pub(crate) fn hand_code(handed: Handedness) -> &'static str {
    match handed {
        Handedness::Right => "RH",
        Handedness::Left => "LH",
    }
}

/// Whether a simulator output is connected: any `openconnect_client.*` or
/// `pureplay.*` actor is Connected. Auto-chip's chips reach the simulator
/// through it.
pub(crate) fn simulator_connected(actors: &HashMap<String, ActorStatusResponse>) -> bool {
    actors.iter().any(|(id, a)| {
        (id.starts_with("openconnect_client.") || id.starts_with("pureplay."))
            && a.status == ActorStatus::Connected
    })
}

/// The `golfer.profile` published for golfer id `id`, by any actor.
pub(crate) fn golfer_profile<'a>(
    states: &'a std::collections::BTreeMap<flighthook::StateKey, flighthook::BusMessage>,
    id: &str,
) -> Option<&'a flighthook::GolferProfile> {
    states.values().find_map(|m| match &m.body {
        flighthook::FrpBody::GolferProfile(p) if p.id.as_str() == id => Some(p),
        _ => None,
    })
}

/// Label of a flighthook golfer id that names no golfer and no profile.
pub(crate) const UNKNOWN_GOLFER: &str = "Unknown golfer";

/// The name golfer id `id` shows with: the name of the golfer of `config`
/// it means (the lookup rule; "Golfer N" for one saved without a name),
/// else its `golfer.profile` name, else the last profile name heard for it
/// (`heard`), else "Unknown golfer" for a flighthook id and the id itself
/// for a game's.
pub(crate) fn golfer_name(
    config: Option<&flighthook::FlighthookConfig>,
    states: &std::collections::BTreeMap<flighthook::StateKey, flighthook::BusMessage>,
    heard: &HashMap<String, String>,
    id: &str,
) -> String {
    let named = |name: &str| (!name.trim().is_empty()).then(|| name.to_owned());
    if let Some(config) = config
        && let Some(at) = config.golfers.iter().position(|g| g.is(id))
    {
        return named(&config.golfers[at].name).unwrap_or_else(|| format!("Golfer {}", at + 1));
    }
    golfer_profile(states, id)
        .and_then(|p| named(&p.name))
        .or_else(|| heard.get(id).and_then(|n| named(n)))
        .unwrap_or_else(|| {
            if flighthook::is_own_golfer_id(id) {
                UNKNOWN_GOLFER.to_owned()
            } else {
                id.to_owned()
            }
        })
}

/// The clubs the header's club popup offers: the turn golfer's bag, every
/// club when it has none. The club in play always shows. A game id's bag is
/// its `golfer.profile`'s, else that of the golfer it means (the lookup
/// rule); a flighthook golfer's is its saved record's (the profile
/// flighthook publishes for it), whoever else publishes under its id.
pub(crate) fn offered_clubs(
    config: Option<&flighthook::FlighthookConfig>,
    states: &std::collections::BTreeMap<flighthook::StateKey, flighthook::BusMessage>,
    golfer: Option<&str>,
    in_play: Option<Club>,
) -> Vec<Club> {
    let record_bag = |id: &str| config?.golfer_of(id).map(|g| g.bag.clone());
    let bag = golfer
        .and_then(|id| {
            if flighthook::is_own_golfer_id(id) {
                return record_bag(id);
            }
            golfer_profile(states, id)
                .map(|p| p.bag.clone())
                .or_else(|| record_bag(id))
        })
        .unwrap_or_default();
    Club::ALL
        .iter()
        .copied()
        .filter(|c| bag.is_empty() || bag.contains(c) || in_play == Some(*c))
        .collect()
}

/// A remote launch monitor's name stored on its shot names it in `names`
/// after it has left the bus (a live `bus.actor` name wins); local cards
/// (`card_keys`) are named by their cards.
pub(crate) fn keep_remote_name(
    names: &mut HashMap<String, String>,
    card_keys: &HashMap<String, String>,
    shot: &crate::types::ShotData,
) {
    if let Some(name) = &shot.from_name
        && !card_keys.contains_key(&shot.from)
    {
        names
            .entry(shot.from.clone())
            .or_insert_with(|| name.clone());
    }
}

/// A game golfer's name stored on its shot names the golfer in `names`
/// after the round is over. Shots are kept oldest first, so the newest
/// shot's name wins (a live `golfer.profile` name wins over any of them in
/// [`golfer_name`]).
pub(crate) fn keep_golfer_name(names: &mut HashMap<String, String>, shot: &crate::types::ShotData) {
    if let Some((id, name)) = shot_golfer_name(shot) {
        names.insert(id, name);
    }
}

/// The game golfer id and name a shot carries, when it carries a name.
pub(crate) fn shot_golfer_name(shot: &crate::types::ShotData) -> Option<(String, String)> {
    let name = shot.golfer_name.clone()?;
    let id = shot.turn.as_ref()?.golfer.as_ref()?;
    Some((id.as_str().to_owned(), name))
}

impl FlighthookApp {
    /// Select golfer `id` (only the golfer, so the session gives the
    /// golfer their own last club and hand) over the WebSocket.
    /// While the view does not follow the turn, the pick moves only the
    /// view and nothing is sent.
    pub(crate) fn pick_golfer(&mut self, id: &str) {
        self.view_hold = None;
        if !self.follows_turns() {
            self.set_view(Some(id.to_owned()));
            return;
        }
        let Ok(golfer) = id.parse::<flighthook::GolferId>() else {
            return;
        };
        let command = net::select_command(|s| s.golfer = Some(Some(golfer)));
        #[cfg(test)]
        self.sent_selects.push(command.clone());
        if let Some(tx) = &mut self.ws_sender {
            net::send_command(tx, &command);
        }
    }
}

impl FlighthookApp {
    /// Select `club` as the club in play: over the WebSocket when it is up,
    /// else over REST.
    pub(crate) fn pick_club(&mut self, ctx: &egui::Context, club: Club) {
        self.selected_club = Some(club);
        if let Some(tx) = &mut self.ws_sender {
            net::send_command(tx, &net::select_command(|s| s.club = Some(Some(club))));
        } else {
            self.club_pick = Some(club);
            self.club_picked_ms = self.now_ms;
            net::post_club(ctx, club);
        }
    }
}

/// How long a club picked over REST holds against `/api/status`
/// replies that do not carry it, milliseconds. The server can take a pick
/// back (a Pureplay Accessory card puts Pureplay's club back), and then no
/// reply ever carries it.
const REST_PICK_HOLD_MS: u64 = 2_000;

/// The club a `/api/status` reply sets, if it sets one. With the
/// WebSocket up, its `init` and `club_info` are the live source
/// and the reply is ignored. Without it, a reply that does not carry the
/// value picked over REST (`pick`, made `age_ms` ago) is ignored too, for
/// [`REST_PICK_HOLD_MS`]; the first reply that carries it, or any reply
/// after that, clears `pick`.
fn status_pick<T: Copy + PartialEq>(
    ws_connected: bool,
    pick: &mut Option<T>,
    age_ms: u64,
    reply: Option<T>,
) -> Option<Option<T>> {
    if ws_connected {
        return None;
    }
    if pick.is_some_and(|p| reply != Some(p)) && age_ms < REST_PICK_HOLD_MS {
        return None;
    }
    *pick = None;
    Some(reply)
}

/// The frame's input time in whole milliseconds.
pub(crate) fn input_ms(ctx: &egui::Context) -> u64 {
    (ctx.input(|i| i.time).max(0.0) * 1000.0) as u64
}

impl eframe::App for FlighthookApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Before any layout: a changed zoom discards this pass.
        self.portrait = self.ui_scale.apply(ctx);
        crate::topbar::touch_tips_frame(ctx);
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

        // Setup wizard — shown when config has no user actors, once the
        // status has said what the server offers. Until then a blank page
        // stands in for it.
        if self.show_wizard() {
            self.render_wizard(ctx);
            return;
        }
        if self.wizard_waiting() {
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(theme::BG))
                .show(ctx, |_| {});
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

    /// flighthook's `golfer.turn` on the UI socket (the app's
    /// `flighthook_id` is empty until a snapshot names it).
    fn turn_msg(
        club: Option<Club>,
        handedness: Option<Handedness>,
        mode: Option<flighthook::Mode>,
    ) -> flighthook::UiMessage {
        flighthook::UiMessage::Frp(Box::new(flighthook::BusMessage::new(
            "",
            flighthook::GolferTurn {
                golfer: None,
                club,
                handedness,
                mode,
            },
        )))
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

    #[test]
    fn a_config_error_shows_in_a_frame_on_every_tab_until_the_status_drops_it() {
        let ctx = egui::Context::default();
        let mut app = FlighthookApp::with_pending(net::new_pending());
        assert!(!content_marks(&ctx, &mut app).contains_key("config_error"));

        let status = |config_error| crate::types::StatusResponse {
            cards: HashMap::new(),
            actors: HashMap::new(),
            turn: None,
            cameras_supported: false,
            trackball_supported: false,
            clip_dir: None,
            config_error,
            round: Default::default(),
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

    /// A `/api/blocks` reply listing `(id, session, club, open, shots)`.
    fn blocks(
        session: u32,
        sets: &[(&str, u32, Option<Club>, bool, u32)],
    ) -> flighthook::BlocksResponse {
        flighthook::BlocksResponse {
            session,
            blocks: sets
                .iter()
                .enumerate()
                .map(
                    |(i, &(id, session, club, open, shots))| flighthook::BlockInfo {
                        block: flighthook::PracticeBlock {
                            id: id.into(),
                            golfer: None,
                            club,
                            number: u32::try_from(i).unwrap_or(0) + 1,
                            session,
                            started_at: 1_793_000_000_000,
                        },
                        open,
                        shots,
                        first_shot_at: None,
                    },
                )
                .collect(),
        }
    }

    #[test]
    fn the_view_follows_the_saved_settings_and_a_pick_saves_it() {
        use crate::types::ShotGrouping;
        let mut app = FlighthookApp::with_pending(net::new_pending());
        let config = |mode| crate::types::FlighthookConfig {
            shot_grouping: mode,
            ..crate::types::FlighthookConfig::default()
        };
        app.pending.lock().expect("lock").settings = Some(config(ShotGrouping::ByClub));
        app.apply_pending();
        assert_eq!(app.groups.mode, ShotGrouping::ByClub);
        // A sets reply leaves the view alone.
        app.pending.lock().expect("lock").groups = Some(blocks(1, &[]));
        app.apply_pending();
        assert_eq!(app.groups.mode, ShotGrouping::ByClub);
        // Another client's save reloads the settings, and the view follows.
        app.pending.lock().expect("lock").settings = Some(config(ShotGrouping::None));
        app.apply_pending();
        assert_eq!(app.groups.mode, ShotGrouping::None);
        assert_eq!(app.settings.shot_grouping, ShotGrouping::None);
    }

    /// By set and By club from `/api/blocks`: sets in session and number
    /// order, the session from the reply, the open set the next shot joins.
    #[test]
    fn the_sets_come_from_the_blocks_reply() {
        use crate::types::ShotGrouping;
        let mut app = FlighthookApp::with_pending(net::new_pending());
        app.pending.lock().expect("lock").groups = Some(blocks(
            2,
            &[
                ("a", 1, Some(Club::Iron7), false, 3),
                ("b", 2, Some(Club::Iron7), false, 2),
                ("c", 2, Some(Club::Driver), true, 1),
            ],
        ));
        let rev = app.groups.rev();
        app.apply_pending();
        assert!(app.groups.rev() > rev);
        assert_eq!(app.groups.session(), 2);
        let ids: Vec<&str> = app.groups.iter_sets().map(|g| g.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c"]);
        assert_eq!(app.groups.open_set().map(|g| g.id.as_str()), Some("c"));
        assert_eq!(
            app.groups
                .current_group(Some(Club::Driver))
                .map(|g| g.id.as_str()),
            Some("c")
        );
        let b = app.groups.get("b").expect("b");
        assert_eq!(
            (b.session, b.club, b.shots, b.open),
            (2, Some(Club::Iron7), 2, false)
        );
        for mode in [ShotGrouping::BySet, ShotGrouping::ByClub] {
            app.groups.mode = mode;
            let strip = app.groups.strip(Some(Club::Driver));
            assert!(
                strip.actions && strip.new_group && strip.new_session,
                "{mode:?}"
            );
        }
    }

    #[test]
    fn a_practice_block_fetches_the_sets_again() {
        let mut app = FlighthookApp::with_pending(net::new_pending());
        app.flighthook_id = "me".into();
        let block = |session| flighthook::PracticeBlock {
            id: "b1".into(),
            golfer: None,
            club: Some(Club::Iron7),
            session,
            number: 1,
            started_at: 0,
        };
        crate::socket::feed::frp(&mut app, "other", block(1));
        assert!(!app.needs_groups_refresh);
        crate::socket::feed::frp(&mut app, "me", block(2));
        assert!(app.needs_groups_refresh);
        app.needs_groups_refresh = false;
        crate::socket::feed::frp(
            &mut app,
            "me",
            flighthook::Removal {
                kind: flighthook::StateKind::PracticeBlock,
                id: Some("b1".into()),
            },
        );
        assert!(app.needs_groups_refresh);
    }

    #[test]
    fn status_reply_never_clobbers_a_live_or_pending_club() {
        let mut pick = None;
        // The WebSocket is the live source while it is up.
        assert_eq!(status_pick(true, &mut pick, 0, Some(Club::Driver)), None);
        // Without it, the reply is the source...
        assert_eq!(
            status_pick(false, &mut pick, 0, Some(Club::Driver)),
            Some(Some(Club::Driver))
        );
        assert_eq!(status_pick(false, &mut pick, 0, None), Some(None));
        // ...except a reply that predates a REST pick.
        pick = Some(Club::Iron7);
        assert_eq!(status_pick(false, &mut pick, 0, Some(Club::Driver)), None);
        assert_eq!(pick, Some(Club::Iron7));
        assert_eq!(
            status_pick(false, &mut pick, 0, Some(Club::Iron7)),
            Some(Some(Club::Iron7))
        );
        assert_eq!(pick, None, "confirmed");
        assert_eq!(
            status_pick(false, &mut pick, 0, Some(Club::Driver)),
            Some(Some(Club::Driver))
        );
        // A pick the server took back (Pureplay holds another club): the
        // reply is taken as it is once the pick is older than the hold.
        pick = Some(Club::Iron8);
        assert_eq!(
            status_pick(false, &mut pick, REST_PICK_HOLD_MS - 1, Some(Club::Iron7)),
            None,
            "still within the hold"
        );
        assert_eq!(
            status_pick(false, &mut pick, REST_PICK_HOLD_MS, Some(Club::Iron7)),
            Some(Some(Club::Iron7)),
            "the pick expired"
        );
        assert_eq!(pick, None);
    }

    /// Without the WebSocket, the hand picked over REST shows until a status
    /// reply carries it; with it, `hand_info` is the source and the reply's
    /// hand is ignored.
    #[test]
    fn the_hand_follows_status_replies_and_hand_info() {
        let status = |handed: Option<flighthook::Handedness>| crate::types::StatusResponse {
            cards: HashMap::new(),
            actors: HashMap::new(),
            turn: handed
                .map(|h| flighthook::GolferTurn::new(None, None, h, flighthook::Mode::Full)),
            cameras_supported: false,
            trackball_supported: false,
            clip_dir: None,
            config_error: None,
            round: Default::default(),
        };
        let mut app = FlighthookApp::with_pending(net::new_pending());
        app.pending.lock().expect("lock").status = Some(status(Some(Handedness::Left)));
        app.apply_pending();
        assert_eq!(app.handed, Some(Handedness::Left));

        app.pending.lock().expect("lock").status = Some(status(Some(Handedness::Right)));
        app.apply_pending();
        assert_eq!(app.handed, Some(Handedness::Right));

        // With the WebSocket up the reply does not move it; the session's
        // turn does.
        app.ws_connected = true;
        app.pending.lock().expect("lock").status = Some(status(Some(Handedness::Left)));
        app.apply_pending();
        assert_eq!(app.handed, Some(Handedness::Right));
        crate::socket::feed::frp(
            &mut app,
            "",
            flighthook::GolferTurn::new(None, None, Handedness::Left, flighthook::Mode::Full),
        );
        assert_eq!(app.handed, Some(Handedness::Left));
    }

    #[test]
    fn hand_codes_are_rh_and_lh() {
        assert_eq!(
            [Handedness::Right, Handedness::Left].map(hand_code),
            ["RH", "LH"]
        );
    }

    /// Without the WebSocket, a club picked over REST that the server takes
    /// back (Pureplay holds another club) leaves the header once the pick
    /// is older than the hold: the next status reply sets Pureplay's club.
    #[test]
    fn a_rest_pick_the_server_takes_back_leaves_the_header() {
        let status = |club| crate::types::StatusResponse {
            cards: HashMap::new(),
            actors: HashMap::new(),
            turn: Some(flighthook::GolferTurn::new(
                None,
                club,
                flighthook::Handedness::Right,
                flighthook::Mode::Full,
            )),
            cameras_supported: false,
            trackball_supported: false,
            clip_dir: None,
            config_error: None,
            round: Default::default(),
        };
        let mut app = FlighthookApp::with_pending(net::new_pending());
        app.now_ms = 10_000;
        app.selected_club = Some(Club::Iron8);
        app.club_pick = Some(Club::Iron8);
        app.club_picked_ms = app.now_ms;
        app.now_ms += 500;
        app.pending.lock().expect("lock").status = Some(status(Some(Club::Iron7)));
        app.apply_pending();
        assert_eq!(
            app.selected_club,
            Some(Club::Iron8),
            "a reply that predates it"
        );
        app.now_ms += REST_PICK_HOLD_MS;
        app.pending.lock().expect("lock").status = Some(status(Some(Club::Iron7)));
        app.apply_pending();
        assert_eq!(app.selected_club, Some(Club::Iron7), "Pureplay's club");
        assert_eq!(app.club_pick, None);
    }

    #[test]
    fn a_status_reply_keeps_the_red_and_live_statuses_carry_it_on() {
        use crate::panels::settings_alert;
        use crate::types::{ConnectionState, RECONNECT_ALERT_MS};
        let status = |status| crate::socket::feed::card_msg("openconnect_server.0", status, &[]);
        let mut app = FlighthookApp::with_pending(net::new_pending());
        // The UI opened 5 minutes after the drop, its clock unrelated to the
        // server's: the red time counts from receipt.
        app.now_ms = 7_000;
        app.pending.lock().expect("lock").status = Some(
            serde_json::from_str(
                r#"{"cards":{"openconnect_server.0":{"name":"Range Bridge",
                "status":"disconnected","telemetry":{},
                "state":"reconnecting","red_for_ms":300000}}}"#,
            )
            .expect("reply"),
        );
        app.apply_pending();
        let server = &app.actors["openconnect_server.0"];
        assert_eq!(server.state(), ConnectionState::Reconnecting);
        assert!(server.red(7_000 + 299_999));
        assert!(!server.red(7_000 + 300_000));
        assert!(settings_alert(&app.actors, 7_000));

        // The same Disconnected again is no new drop.
        app.handle_ui_message(status(ActorStatus::Disconnected));
        assert!(!app.actors["openconnect_server.0"].red(7_000 + 300_000));
        // A peer connects and leaves: a new ten minutes.
        app.now_ms = 400_000;
        app.handle_ui_message(status(ActorStatus::Connected));
        assert!(!settings_alert(&app.actors, app.now_ms));
        app.handle_ui_message(status(ActorStatus::Disconnected));
        assert!(app.actors["openconnect_server.0"].red(400_000 + RECONNECT_ALERT_MS - 1));
        // A restart from a config change starts again at Connecting.
        app.handle_ui_message(crate::socket::feed::local_msg(
            "system",
            flighthook::UiLocalEvent::ConfigOutcome {
                request_id: None,
                restarted: vec!["openconnect_server.0".into()],
                stopped: Vec::new(),
                started: Vec::new(),
                config_generation: 0,
            },
        ));
        let server = &app.actors["openconnect_server.0"];
        assert_eq!(server.state(), ConnectionState::Connecting);
        assert!(!settings_alert(&app.actors, app.now_ms));
    }

    #[test]
    fn an_empty_status_telemetry_value_removes_the_key() {
        let mut app = FlighthookApp::with_pending(net::new_pending());
        let status = |status, telemetry: &[(&str, &str)]| {
            crate::socket::feed::card_msg("openconnect_client.0", status, telemetry)
        };
        app.handle_ui_message(status(
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
        app.handle_ui_message(status(ActorStatus::Reconnecting, &[]));
        assert!(
            app.actors["openconnect_client.0"]
                .telemetry
                .contains_key("error")
        );
        app.handle_ui_message(status(ActorStatus::Connected, &[("error", "")]));
        let telemetry = &app.actors["openconnect_client.0"].telemetry;
        assert!(!telemetry.contains_key("error"));
        assert_eq!(telemetry.get("club").map(String::as_str), Some("DR"));
    }

    #[test]
    fn a_lost_connection_or_a_new_instance_clears_the_cards_telemetry() {
        let mut app = FlighthookApp::with_pending(net::new_pending());
        let status = |status, telemetry: &[(&str, &str)]| {
            crate::socket::feed::card_msg("mevo.0", status, telemetry)
        };
        let device = |pairs: &[(&str, &str)]| crate::socket::feed::bus_actor_msg("lm", pairs);
        let held = |app: &FlighthookApp| {
            let mut keys: Vec<String> = app.actors["mevo.0"].telemetry.keys().cloned().collect();
            keys.sort();
            keys
        };

        app.card_keys.insert("lm".into(), "mevo.0".into());
        app.handle_ui_message(status(ActorStatus::Connected, &[("device_info", "Xi")]));
        app.handle_ui_message(device(&[("gain", "1")]));
        app.handle_ui_message(device(&[("club", "DR")]));
        assert_eq!(held(&app), ["club", "device_info", "gain", "ready"]);

        // Connected, Connected: merges.
        app.handle_ui_message(status(ActorStatus::Connected, &[("shot_count", "1")]));
        assert_eq!(held(&app).len(), 5);

        // Lost: only the message's own keys remain.
        app.handle_ui_message(status(ActorStatus::Reconnecting, &[("error", "gone")]));
        assert_eq!(held(&app), ["error"]);
        // Repeated non-Connected statuses merge.
        app.handle_ui_message(status(ActorStatus::Disconnected, &[("phase", "x")]));
        assert_eq!(held(&app), ["error", "phase"]);
        app.handle_ui_message(device(&[]));
        assert_eq!(held(&app), ["error", "phase", "ready"]);

        // A new instance starts over.
        app.handle_ui_message(status(ActorStatus::Starting, &[("bind", "b")]));
        assert_eq!(held(&app), ["bind"]);
        app.handle_ui_message(status(ActorStatus::Connected, &[]));
        app.handle_ui_message(status(ActorStatus::Disconnected, &[]));
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
    fn shots_rev_bumps_at_every_shot_change_and_keys_the_caches() {
        use crate::types::{ClubData, Launch};
        use flighthook::{ShotData, ShotKey};

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
            card: Some("mevo.0".into()),
            shot_number: 9,
            launch: Some(Launch::default()),
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

        let stored = |launch: Option<Launch>, club: Option<ClubData>| ShotData {
            card: Some("square.0".into()),
            shot_id: Some(key.shot_id.clone()),
            shot_number: key.shot_number,
            launch,
            club,
            block: Some("s1".into()),
            ..ShotData::default()
        };
        crate::socket::feed::shot(&mut app, stored(None, None));
        bumped(&app, "a new shot");
        crate::socket::feed::shot(&mut app, stored(Some(Launch::default()), None));
        bumped(&app, "its ball");
        crate::socket::feed::shot(
            &mut app,
            stored(Some(Launch::default()), Some(ClubData::default())),
        );
        bumped(&app, "its club");
        assert_eq!(app.shots.len(), 2);
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
        use crate::types::Launch;
        use flighthook::ShotData;

        let stored = |actor: &str, n| ShotData {
            card: Some(actor.into()),
            shot_number: n,
            launch: Some(Launch::default()),
            block: Some("s1".into()),
            ..ShotData::default()
        };
        let mut app = FlighthookApp::with_pending(net::new_pending());
        {
            let mut p = app.pending.lock().expect("lock");
            p.shots = Some(vec![
                stored("mevo.0", 1),
                stored("square.0", 1),
                stored("mevo.0", 2),
            ]);
            p.groups = Some(blocks(2, &[("s1", 2, Some(Club::Iron7), true, 3)]));
        }
        app.apply_pending();
        // A live shot arrives and pulses.
        crate::socket::feed::shot(
            &mut app,
            ShotData {
                card: Some("mevo.0".into()),
                shot_id: Some("m3".into()),
                shot_number: 3,
                launch: Some(Launch::default()),
                ..ShotData::default()
            },
        );
        // The store's shot asks for the sets; that fetch is not the point.
        app.needs_groups_refresh = false;
        app.observe_shots(10.0);
        app.device_filter = Some("square.0".into());
        app.confirm_clear = true;
        app.refresh_shot_caches();
        assert!(app.device_ids.peek(&app.shots_rev).is_some());
        let (shots_rev, groups_rev) = (app.shots_rev, app.groups.rev());
        assert!(app.shot_focus.progress(10.0).is_some(), "pulsing");
        // A reply fetched before the clear, not drained yet.
        app.pending.lock().expect("lock").shots = Some(vec![stored("mevo.0", 1)]);

        // Pressed in another tab: every UI sees the command.
        app.handle_ui_message(crate::socket::feed::local_msg(
            "web",
            flighthook::UiLocalEvent::ClearShotHistory,
        ));
        assert!(app.shots.is_empty());
        assert!(app.groups.is_empty());
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
        app.handle_ui_message(crate::socket::feed::shot_msg(
            "mevo.0",
            flighthook::ShotKey {
                shot_id: "n1".into(),
                shot_number: 1,
            },
        ));
        assert_eq!(app.shots.len(), 1);
    }

    #[test]
    fn group_commands_over_rest_update_the_sending_ui() {
        use crate::panels::shots::GroupCommand;
        let mut app = FlighthookApp::with_pending(net::new_pending());
        for n in 1..=3 {
            app.handle_ui_message(crate::socket::feed::shot_msg(
                "mevo.0",
                flighthook::ShotKey {
                    shot_id: format!("s{n}"),
                    shot_number: n,
                },
            ));
        }
        app.needs_groups_refresh = false;
        app.confirm_clear = true;

        // A New set or New session answered: the sets are fetched again.
        app.pending.lock().expect("lock").groups_posted = true;
        app.apply_pending();
        assert!(app.needs_groups_refresh);
        assert!(!app.pending.lock().expect("lock").groups_posted, "taken");

        // Clear history over REST: the tab empties as for the socket's event.
        let ctx = egui::Context::default();
        app.send_group_command(&ctx, GroupCommand::ClearHistory);
        assert!(app.shots.is_empty());
        assert!(app.groups.is_empty());
        assert!(!app.confirm_clear);
    }

    #[test]
    fn replies_requested_before_a_clear_are_dropped() {
        use crate::types::Launch;
        use flighthook::ShotData;

        let stored = |n| ShotData {
            card: Some("mevo.0".into()),
            shot_number: n,
            launch: Some(Launch::default()),
            block: Some("s1".into()),
            ..ShotData::default()
        };
        let sets = |session| {
            Some(blocks(
                session,
                &[("s1", session, Some(Club::Iron7), true, 2)],
            ))
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
        land(&mut app, 0, Some(vec![stored(1), stored(2)]), sets(4));
        assert_eq!((app.shots.len(), app.groups.session()), (2, 4));

        // A refetch goes out, then the history is cleared before it lands.
        let asked = app.history_generation;
        app.handle_ui_message(crate::socket::feed::local_msg(
            "web",
            flighthook::UiLocalEvent::ClearShotHistory,
        ));
        land(&mut app, asked, Some(vec![stored(1), stored(2)]), sets(4));
        assert!(app.shots.is_empty(), "no pre-clear shot comes back");
        assert!(app.groups.is_empty(), "no pre-clear set comes back");
        assert!(!app.needs_shots_refresh && !app.needs_groups_refresh);

        // One requested after the clear applies.
        let asked = app.history_generation;
        land(&mut app, asked, Some(vec![stored(1)]), sets(5));
        assert_eq!((app.shots.len(), app.groups.session()), (1, 5));

        assert!(app.groups.get("s1").is_some());
    }

    #[test]
    fn device_filter_falls_back_to_all_when_its_device_leaves() {
        use crate::types::Launch;
        use flighthook::ShotData;

        let stored = |actor: &str, n| ShotData {
            card: Some(actor.into()),
            shot_number: n,
            launch: Some(Launch::default()),
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
        use crate::types::{Distance, FaceImpact, Handedness};
        use flighthook::Mode;
        use flighthook::{ShotData, ShotKey};

        let mut app = FlighthookApp::with_pending(net::new_pending());
        // Handedness from the WebSocket handshake, then from the bus; a
        // mode-only change keeps it.
        app.handle_ui_message(turn_msg(Some(Club::Iron7), Some(Handedness::Left), None));
        assert_eq!(app.handed, Some(Handedness::Left));
        assert_eq!(app.selected_club, Some(Club::Iron7));
        app.handle_ui_message(turn_msg(
            Some(Club::Iron7),
            Some(Handedness::Right),
            Some(Mode::Full),
        ));
        assert_eq!(app.handed, Some(Handedness::Right));
        app.handle_ui_message(turn_msg(
            Some(Club::Iron7),
            Some(Handedness::Right),
            Some(Mode::Chipping),
        ));
        assert_eq!(app.handed, Some(Handedness::Right));
        assert_eq!(app.current_mode, Mode::Chipping.to_string());
        app.handle_ui_message(turn_msg(
            Some(Club::Iron7),
            Some(Handedness::Left),
            Some(Mode::Chipping),
        ));

        // The store's shot carries the hand at its trigger and its face
        // impact.
        let key = ShotKey {
            shot_id: "s1".into(),
            shot_number: 1,
        };
        let impact = FaceImpact {
            lateral: Some(Distance::Inches(0.31)),
            vertical: Some(Distance::Millimeters(-2.0)),
        };
        let rev = app.shots_rev;
        crate::socket::feed::shot(
            &mut app,
            ShotData {
                card: Some("square.0".into()),
                shot_id: Some(key.shot_id.clone()),
                shot_number: key.shot_number,
                impact: Some(impact.clone()),
                turn: Some(flighthook::GolferTurn {
                    golfer: None,
                    club: None,
                    handedness: Some(Handedness::Left),
                    mode: None,
                }),
                ..ShotData::default()
            },
        );
        assert!(app.shots_rev > rev);
        assert_eq!(app.shots[0].handed, Some(Handedness::Left));
        assert_eq!(app.shots[0].impact, Some(impact.clone()));

        // Rows from `/api/shots` keep both, and a unit change leaves the
        // impact as sent (it is read in millimeters everywhere).
        let row = ShotRow::from(ShotData {
            card: Some("square.0".into()),
            shot_number: 2,
            impact: Some(impact.clone()),
            turn: Some(flighthook::GolferTurn {
                golfer: None,
                club: None,
                handedness: Some(Handedness::Left),
                mode: None,
            }),
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
            crate::socket::feed::local_msg(
                camera,
                flighthook::UiLocalEvent::ShotVideo {
                    keys,
                    position: CameraPosition::GolferFront,
                    duration_ms,
                    fps: 120.0,
                    trigger_offset_ms: 4000,
                    impact_offset_ms: 4000,
                    complete: true,
                    dropped: 0,
                },
            )
        };
        let mut app = FlighthookApp::with_pending(net::new_pending());
        // Two launch monitors saw one swing; a third shot has no clip.
        for (actor, k) in [
            ("mevo.0", key("a", 1)),
            ("square.0", key("b", 1)),
            ("mevo.0", key("c", 2)),
        ] {
            app.handle_ui_message(crate::socket::feed::shot_msg(actor, k));
        }
        // The clip lands before either shot finished.
        let rev = app.shots_rev;
        app.handle_ui_message(clip("camera.0", vec![key("a", 1), key("b", 1)], 5500));
        assert!(app.shots_rev > rev);
        assert_eq!(app.shots[0].video.len(), 1);
        assert_eq!(app.shots[1].video[0].camera, "camera.0");
        assert!(app.shots[2].video.is_empty());
        // Finishing keeps it; a second camera adds its own; the same camera
        // again replaces its clip.
        app.handle_ui_message(clip("camera.1", vec![key("a", 1)], 5500));
        app.handle_ui_message(clip("camera.0", vec![key("a", 1)], 4000));
        let cameras: Vec<(&str, u64)> = app.shots[0]
            .video
            .iter()
            .map(|v| (v.camera.as_str(), v.duration_ms))
            .collect();
        assert_eq!(cameras, [("camera.0", 4000), ("camera.1", 5500)]);
        // A clip after its shot finished stamps it too.
        app.handle_ui_message(clip("camera.0", vec![key("c", 2)], 5500));
        assert_eq!(app.shots[2].video.len(), 1);
        // A clip of a shot the UI doesn't hold changes nothing.
        let rev = app.shots_rev;
        app.handle_ui_message(clip("camera.0", vec![key("zzz", 9)], 5500));
        assert_eq!(app.shots_rev, rev);

        app.handle_ui_message(crate::socket::feed::local_msg(
            "camera.0",
            flighthook::UiLocalEvent::ShotVideoRemoved {
                keys: vec![key("a", 1), key("b", 1)],
            },
        ));
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
            app.handle_ui_message(crate::socket::feed::shot_msg(actor, key(id)));
            app.handle_ui_message(crate::socket::feed::local_msg(
                "camera.0",
                flighthook::UiLocalEvent::ShotVideo {
                    keys: vec![key(id)],
                    position: CameraPosition::GolferFront,
                    duration_ms: 5500,
                    fps: 60.0,
                    trigger_offset_ms: 4000,
                    impact_offset_ms: 4000,
                    complete: true,
                    dropped: 0,
                },
            ));
        }
        let ctx = egui::Context::default();
        app.open_replay(&ctx, 0);
        assert_eq!(app.replay.as_ref().map(|r| r.shot_id()), Some("a"));
        let removed = |id: &str| {
            crate::socket::feed::local_msg(
                "camera.0",
                flighthook::UiLocalEvent::ShotVideoRemoved {
                    keys: vec![key(id)],
                },
            )
        };
        app.handle_ui_message(removed("b"));
        assert!(app.replay.is_some(), "another shot's clips");
        app.handle_ui_message(removed("a"));
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
            rotation: 0,
            analyzed_with: None,
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
                cards: HashMap::new(),
                actors: HashMap::new(),
                turn: None,
                cameras_supported: true,
                trackball_supported: false,
                clip_dir: Some("/clips".into()),
                config_error: None,
                round: Default::default(),
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

    // -- Tournament banner ---------------------------------------------------

    fn flatten_shapes(shape: &egui::Shape, out: &mut Vec<String>) {
        match shape {
            egui::Shape::Vec(inner) => inner.iter().for_each(|s| flatten_shapes(s, out)),
            egui::Shape::Text(t) => out.push(t.galley.text().to_owned()),
            _ => {}
        }
    }

    /// One pass of the header in a window of `size` at `time`: its marked
    /// controls and every text drawn.
    fn header_pass(
        ctx: &egui::Context,
        app: &mut FlighthookApp,
        size: egui::Vec2,
        time: f64,
        events: Vec<egui::Event>,
    ) -> (crate::panels::devices::Marks, Vec<String>) {
        use crate::panels::devices::Marks;
        ctx.data_mut(|d| d.remove_temp::<Marks>(egui::Id::NULL));
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            time: Some(time),
            events,
            ..Default::default()
        };
        let output = ctx.run(input, |ctx| app.render_header_in_test(ctx));
        let mut texts = Vec::new();
        for clipped in &output.shapes {
            flatten_shapes(&clipped.shape, &mut texts);
        }
        let marks = ctx
            .data(|d| d.get_temp::<Marks>(egui::Id::NULL))
            .unwrap_or_default();
        (marks, texts)
    }

    /// An app with Auto-chip on, GSPro and a Pureplay output connected and
    /// an Omni at 74 %: the fullest header.
    fn full_header_app() -> FlighthookApp {
        let mut app = FlighthookApp::with_pending(crate::net::new_pending());
        let mut config = crate::types::FlighthookConfig::default();
        config.simulation.auto_chip = true;
        app.settings.load_from(&config);
        app.actors.insert(
            "openconnect_client.0".into(),
            ActorStatusResponse::new("GSPro", ActorStatus::Connected),
        );
        app.actors.insert(
            "pureplay.0".into(),
            ActorStatusResponse::new("Pureplay", ActorStatus::Connected),
        );
        let mut omni = ActorStatusResponse::new("Square Omni", ActorStatus::Connected);
        omni.telemetry.insert("battery_pct".into(), "74".into());
        app.actors.insert("square.0".into(), omni);
        app
    }

    fn round(holder: &str) -> RoundState {
        RoundState {
            active: true,
            holder: Some(holder.into()),
            held: true,
            manual: false,
        }
    }

    /// Two golfers, Eric (bag DR and PW, profile published) up.
    fn golfers_app() -> FlighthookApp {
        let mut app = full_header_app();
        let mut config = app.settings.saved_config_ref().cloned().expect("loaded");
        let mut eric = flighthook::GolferRecord::new("flighthook.e", "Eric");
        eric.identifiers.push("pureplay.local-3".into());
        eric.bag = vec![Club::Driver, Club::PitchingWedge];
        config.golfers = vec![eric, flighthook::GolferRecord::new("flighthook.s", "Sam")];
        app.settings.load_from(&config);
        let mut profile = flighthook::GolferProfile::new("flighthook.e".parse().unwrap(), "Eric");
        profile.bag = vec![Club::Driver, Club::PitchingWedge];
        let msg = flighthook::BusMessage::new("fh", profile);
        app.bus_states.insert(msg.state_key().unwrap(), msg);
        let game = flighthook::GolferProfile::new("pureplay.player-7".parse().unwrap(), "Guest");
        let msg = flighthook::BusMessage::new("pp", game);
        app.bus_states.insert(msg.state_key().unwrap(), msg);
        app.turn_golfer = Some("flighthook.e".into());
        app.selected_club = Some(Club::Iron7);
        app
    }

    #[test]
    fn the_golfer_pill_names_the_golfer_up_by_the_lookup_rule() {
        let app = golfers_app();
        let config = app.settings.saved_config_ref();
        assert_eq!(
            golfer_name(
                config,
                &app.bus_states,
                &app.golfer_names,
                "pureplay.local-3"
            ),
            "Eric"
        );
        assert_eq!(
            golfer_name(
                config,
                &app.bus_states,
                &app.golfer_names,
                "pureplay.player-7"
            ),
            "Guest"
        );
        assert_eq!(
            golfer_name(config, &app.bus_states, &app.golfer_names, "x.y"),
            "x.y"
        );
        // The bag limits the clubs; the club in play still shows.
        assert_eq!(
            offered_clubs(
                config,
                &app.bus_states,
                Some("flighthook.e"),
                Some(Club::Iron7)
            ),
            [Club::Driver, Club::Iron7, Club::PitchingWedge]
        );
        // A linked game id with no profile of its own: its golfer's bag.
        assert_eq!(
            offered_clubs(config, &app.bus_states, Some("pureplay.local-3"), None),
            [Club::Driver, Club::PitchingWedge]
        );
        // No bag: every club.
        assert_eq!(
            offered_clubs(config, &app.bus_states, Some("flighthook.s"), None).len(),
            Club::ALL.len()
        );
        assert_eq!(
            offered_clubs(config, &app.bus_states, None, None).len(),
            Club::ALL.len()
        );
    }

    /// Another actor's `golfer.profile` under a
    /// `flighthook.*` id never supplies the club popup's bag: a flighthook
    /// golfer's bag is its saved record's.
    #[test]
    fn another_actors_profile_under_a_flighthook_id_gives_no_bag() {
        let mut app = golfers_app();
        let mut fake = flighthook::GolferProfile::new("flighthook.s".parse().unwrap(), "Sam");
        fake.bag = vec![Club::Putter];
        let msg = flighthook::BusMessage::new("intruder", fake);
        app.bus_states.insert(msg.state_key().unwrap(), msg);
        let config = app.settings.saved_config_ref();
        assert_eq!(
            offered_clubs(config, &app.bus_states, Some("flighthook.s"), None).len(),
            Club::ALL.len()
        );
        assert_eq!(
            offered_clubs(config, &app.bus_states, Some("flighthook.e"), None),
            [Club::Driver, Club::PitchingWedge]
        );
    }

    /// The round comes from the server: its `round_state` events set it,
    /// and a status reply does while the WebSocket is down.
    #[test]
    fn the_round_follows_the_server() {
        let mut app = FlighthookApp::with_pending(crate::net::new_pending());
        app.handle_ui_message(crate::socket::feed::local_msg(
            "system",
            flighthook::UiLocalEvent::RoundState {
                round: round("Pureplay"),
            },
        ));
        assert_eq!(app.round, round("Pureplay"));
        app.handle_ui_message(crate::socket::feed::local_msg(
            "system",
            flighthook::UiLocalEvent::RoundState {
                round: RoundState::default(),
            },
        ));
        assert!(!app.round.active);

        app.ws_connected = false;
        app.pending.lock().expect("pending").status = Some(crate::types::StatusResponse {
            cards: HashMap::new(),
            actors: HashMap::new(),
            turn: None,
            cameras_supported: false,
            trackball_supported: false,
            clip_dir: None,
            config_error: None,
            round: round("Pureplay 2"),
        });
        app.apply_pending();
        assert_eq!(app.round, round("Pureplay 2"));
    }

    /// A WebSocket that closes or fails takes the round with it: the
    /// banner does not outlive the server that held the round.
    #[test]
    fn a_dropped_websocket_ends_the_round_on_screen() {
        let size = egui::vec2(1600.0, 900.0);
        for event in [
            WsPollEvent::Disconnected,
            WsPollEvent::Error("connection reset".into()),
        ] {
            let ctx = egui::Context::default();
            theme::install(&ctx);
            let mut app = full_header_app();
            app.handle_ui_message(crate::socket::feed::local_msg(
                "system",
                flighthook::UiLocalEvent::RoundState {
                    round: round("Pureplay"),
                },
            ));
            header_pass(&ctx, &mut app, size, 0.0, Vec::new());
            let (marks, _) = header_pass(&ctx, &mut app, size, 0.0, Vec::new());
            assert!(marks.contains_key("title"));
            assert!(marks.contains_key("tournament"));

            assert!(app.handle_ws_event(event), "the socket is gone");
            assert_eq!(app.round, RoundState::default());
            header_pass(&ctx, &mut app, size, 0.0, Vec::new());
            let (_, texts) = header_pass(&ctx, &mut app, size, 0.0, Vec::new());
            assert!(
                !texts.iter().any(|t| t.starts_with("TOURNAMENT")),
                "{texts:?}"
            );
        }
    }

    /// A status reply that says whether the server reads a trackball.
    fn welcome_status(trackball: bool) -> crate::types::StatusResponse {
        crate::types::StatusResponse {
            cards: HashMap::new(),
            actors: HashMap::new(),
            turn: None,
            cameras_supported: false,
            trackball_supported: trackball,
            clip_dir: None,
            config_error: None,
            round: RoundState::default(),
        }
    }

    /// An app on a first run: the default config, with no user actors, and
    /// a status that says whether the server reads a trackball.
    fn welcome_app(trackball: bool) -> FlighthookApp {
        let mut app = FlighthookApp::with_pending(net::new_pending());
        {
            let mut p = app.pending.lock().expect("pending");
            p.settings = Some(crate::types::FlighthookConfig::default());
            p.status = Some(welcome_status(trackball));
        }
        app.apply_pending();
        app
    }

    /// Draws the app's welcome screen (or what stands in for it) in a
    /// window of the given size, at zoom 1.
    struct Welcome(egui::Context, egui::Vec2);

    impl Welcome {
        fn sized(width: f32, height: f32) -> Self {
            let ctx = egui::Context::default();
            theme::install(&ctx);
            Self(ctx, egui::vec2(width, height))
        }

        /// One pass with `events`. Returns the marked controls.
        fn frame(
            &self,
            app: &mut FlighthookApp,
            events: Vec<egui::Event>,
        ) -> crate::panels::devices::Marks {
            self.0
                .data_mut(|d| d.remove_temp::<crate::panels::devices::Marks>(egui::Id::NULL));
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, self.1)),
                events,
                ..Default::default()
            };
            let _ = self.0.run(input, |ctx| {
                if app.show_wizard() {
                    app.render_wizard(ctx);
                }
            });
            self.0
                .data(|d| d.get_temp::<crate::panels::devices::Marks>(egui::Id::NULL))
                .unwrap_or_default()
        }

        /// Click the middle of control `name`, then draw once more.
        fn click(&self, app: &mut FlighthookApp, name: &str) -> crate::panels::devices::Marks {
            let marks = self.frame(app, Vec::new());
            let at = marks
                .get(name)
                .unwrap_or_else(|| panic!("no {name}: {:?}", marks.keys()))
                .0
                .center();
            let button = |pressed| egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            self.frame(app, vec![egui::Event::PointerMoved(at)]);
            self.frame(app, vec![button(true)]);
            self.frame(app, vec![button(false)]);
            self.frame(app, Vec::new())
        }
    }

    /// The welcome screen's checkboxes, by (+) menu label.
    fn welcome_boxes(marks: &crate::panels::devices::Marks) -> Vec<&str> {
        let mut boxes: Vec<&str> = marks
            .keys()
            .filter_map(|k| k.strip_prefix("wizard:"))
            .filter(|k| !matches!(*k, "get_started" | "skip"))
            .collect();
        boxes.sort_unstable();
        boxes
    }

    /// The welcome screen lists what the Devices tab's (+) menus offer for
    /// launch monitors and Software (the Pureplay Accessory card once,
    /// among the launch monitors), without the cameras, and the Trackball
    /// only where the server reads one.
    #[test]
    fn the_welcome_screen_offers_every_preset_of_the_add_menus() {
        for trackball in [false, true] {
            let welcome = Welcome::sized(1600.0, 900.0);
            let mut app = welcome_app(trackball);
            let marks = welcome.frame(&mut app, Vec::new());
            let mut want: Vec<&str> = [CardGroup::LaunchMonitors, CardGroup::Software]
                .into_iter()
                .flat_map(|g| cards::offered(g, trackball))
                .map(|p| p.label)
                .collect();
            want.sort_unstable();
            assert_eq!(welcome_boxes(&marks), want, "trackball {trackball}");
            for label in [
                "Mevo",
                "R10",
                "Square Golf Omni",
                "ExPutt",
                "Open Connect Server",
                "Pureplay Accessory",
                "GSPro (Open Connect)",
                "Pureplay (Open Connect)",
                "Muni Golf (Open Connect)",
            ] {
                assert!(marks.contains_key(&format!("wizard:{label}")), "{label}");
            }
            assert!(!marks.contains_key("wizard:Pureplay (Pureplay Connect)"));
            assert_eq!(marks.contains_key("wizard:Trackball"), trackball);
            assert!(!marks.contains_key("wizard:Camera"));
            assert!(!enabled_mark(&marks, "wizard:get_started"));
            assert!(marks.contains_key("wizard:skip"));
        }
    }

    fn enabled_mark(marks: &crate::panels::devices::Marks, name: &str) -> bool {
        marks.get(name).is_some_and(|(_, enabled)| *enabled)
    }

    /// The welcome screen waits for the status, so the Trackball is listed
    /// from its first frame; a later status keeps a checked Trackball.
    #[test]
    fn the_welcome_screen_waits_for_the_status() {
        let welcome = Welcome::sized(1600.0, 900.0);
        let mut app = FlighthookApp::with_pending(net::new_pending());
        app.pending.lock().expect("pending").settings =
            Some(crate::types::FlighthookConfig::default());
        app.apply_pending();
        assert!(app.wizard_waiting());
        assert!(!app.show_wizard(), "not before the status");
        assert!(welcome.frame(&mut app, Vec::new()).is_empty());

        app.pending.lock().expect("pending").status = Some(welcome_status(true));
        app.apply_pending();
        assert!(app.show_wizard());
        let marks = welcome.click(&mut app, "wizard:Trackball");
        assert!(app.wizard_picked.contains("Trackball"));
        assert!(enabled_mark(&marks, "wizard:get_started"));

        app.pending.lock().expect("pending").status = Some(welcome_status(true));
        app.apply_pending();
        let marks = welcome.frame(&mut app, Vec::new());
        assert!(marks.contains_key("wizard:Trackball"));
        assert!(app.wizard_picked.contains("Trackball"));

        // Status first, settings second: the same screen.
        let mut app = FlighthookApp::with_pending(net::new_pending());
        app.pending.lock().expect("pending").status = Some(welcome_status(false));
        app.apply_pending();
        assert!(!app.show_wizard());
        app.pending.lock().expect("pending").settings =
            Some(crate::types::FlighthookConfig::default());
        app.apply_pending();
        let marks = welcome.frame(&mut app, Vec::new());
        assert!(marks.contains_key("wizard:ExPutt"));
        assert!(!marks.contains_key("wizard:Trackball"));
    }

    /// Get Started adds each checked preset as its (+) menu entry does and
    /// posts the whole config; the form takes the cards once it is saved.
    #[test]
    fn get_started_adds_the_checked_presets_as_the_add_menu_does() {
        let welcome = Welcome::sized(1600.0, 900.0);
        let mut app = welcome_app(false);
        for label in ["ExPutt", "Muni Golf (Open Connect)"] {
            welcome.click(&mut app, &format!("wizard:{label}"));
        }
        let marks = welcome.frame(&mut app, Vec::new());
        assert!(!marks.contains_key("wizard:skip"));
        welcome.click(&mut app, "wizard:get_started");
        assert!(app.wizard_saving);

        let (target, posted) = app.settings.posted().expect("posted");
        assert!(matches!(target, crate::panels::settings::SaveTarget::Full));
        let exputt = &posted.exputt["0"];
        assert_eq!(exputt.name, "ExPutt");
        assert!(exputt.enabled);
        let muni = &posted.openconnect_client["0"];
        assert_eq!(muni.name, "Muni Golf");
        assert_eq!(muni.address.as_deref(), Some("127.0.0.1:9210"));
        assert!(muni.enabled);
        assert_eq!(posted.exputt.len(), 1);
        assert_eq!(posted.openconnect_client.len(), 1);
        assert!(posted.pureplay.is_empty());
        assert!(posted.mevo.is_empty() && posted.r10.is_empty() && posted.square.is_empty());
        assert!(posted.openconnect_server.is_empty() && posted.trackball.is_empty());
        assert!(posted.webserver.contains_key("0"), "the web server stays");
        assert!(
            app.settings.position("exputt.0").is_none(),
            "the form waits for the save"
        );

        app.pending.lock().expect("pending").settings_save =
            Some(Ok(crate::types::PostSettingsResponse::default()));
        app.apply_pending();
        assert!(app.wizard_dismissed);
        for id in ["exputt.0", "openconnect_client.0"] {
            let i = app.settings.position(id).expect(id);
            assert!(!app.settings.actors[i].dirty(), "{id}");
        }
        assert!(!app.settings.locked());
    }

    /// GSPro and Pureplay over Open Connect add the same connection: only
    /// one can be checked. Any other pair of welcome presets saves both
    /// cards switched on.
    #[test]
    fn gspro_and_pureplay_over_open_connect_exclude_each_other() {
        let welcome = Welcome::sized(1600.0, 900.0);
        let mut app = welcome_app(true);
        welcome.click(&mut app, "wizard:GSPro (Open Connect)");
        welcome.click(&mut app, "wizard:Muni Golf (Open Connect)");
        welcome.click(&mut app, "wizard:Pureplay (Open Connect)");
        assert!(app.wizard_picked.contains("Pureplay (Open Connect)"));
        assert!(!app.wizard_picked.contains("GSPro (Open Connect)"));
        assert!(app.wizard_picked.contains("Muni Golf (Open Connect)"));
        welcome.click(&mut app, "wizard:GSPro (Open Connect)");
        assert!(app.wizard_picked.contains("GSPro (Open Connect)"));
        assert!(!app.wizard_picked.contains("Pureplay (Open Connect)"));
        welcome.click(&mut app, "wizard:GSPro (Open Connect)");
        assert!(app.wizard_picked.contains("Muni Golf (Open Connect)"));
        assert!(!app.wizard_picked.contains("GSPro (Open Connect)"));

        // Everything that can be checked together, checked: every card on.
        let mut app = welcome_app(true);
        let marks = welcome.frame(&mut app, Vec::new());
        for label in welcome_boxes(&marks) {
            if label != "Pureplay (Open Connect)" {
                welcome.click(&mut app, &format!("wizard:{label}"));
            }
        }
        assert_eq!(app.wizard_picked.len(), welcome_boxes(&marks).len() - 1);
        welcome.click(&mut app, "wizard:get_started");
        let (_, posted) = app.settings.posted().expect("posted");
        let on = [
            posted.mevo.values().map(|s| s.enabled).collect::<Vec<_>>(),
            posted.r10.values().map(|s| s.enabled).collect(),
            posted.square.values().map(|s| s.enabled).collect(),
            posted.exputt.values().map(|s| s.enabled).collect(),
            posted.trackball.values().map(|s| s.enabled).collect(),
            posted
                .openconnect_server
                .values()
                .map(|s| s.enabled)
                .collect(),
            posted
                .openconnect_client
                .values()
                .map(|s| s.enabled)
                .collect(),
        ];
        assert_eq!(
            on.iter().map(Vec::len).collect::<Vec<_>>(),
            [1, 1, 1, 1, 1, 1, 2]
        );
        assert!(posted.pureplay.is_empty());
        assert!(on.iter().flatten().all(|e| *e), "every card on: {on:?}");
    }

    /// The welcome screen fits a 1024×768 window without scrolling, with
    /// every launch monitor and simulator listed, both before anything is
    /// checked (Skip shown) and after a failed Get Started (error shown).
    #[test]
    fn the_welcome_screen_fits_1024_by_768() {
        let welcome = Welcome::sized(1024.0, 768.0);
        let mut app = welcome_app(true);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1024.0, 768.0));
        let inside = |marks: &crate::panels::devices::Marks| {
            for (name, (rect, _)) in marks {
                assert!(screen.contains_rect(*rect), "{name} at {rect:?}");
            }
        };
        let marks = welcome.frame(&mut app, Vec::new());
        assert_eq!(welcome_boxes(&marks).len(), 10);
        inside(&marks);
        assert!(marks.contains_key("wizard:skip"));

        welcome.click(&mut app, "wizard:Trackball");
        welcome.click(&mut app, "wizard:get_started");
        app.pending.lock().expect("pending").settings_save =
            Some(Err("Failed to fetch".to_owned()));
        app.apply_pending();
        let marks = welcome.frame(&mut app, Vec::new());
        assert!(marks.contains_key("error:wizard"));
        inside(&marks);
    }
}
