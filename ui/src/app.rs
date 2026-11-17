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
    ActorStatus, ActorStatusResponse, CameraListResponse, Club, Handedness, LogEntry, ShotRow,
    UnitSystem,
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
    /// The golfer up: the raw id flighthook's `golfer.turn` names.
    pub(crate) turn_golfer: Option<String>,
    /// The top bar's open dropdown.
    pub(crate) header_menu: Option<crate::topbar::HeaderMenu>,
    /// Whether the view follows the turn (session state; default on).
    pub(crate) follow_turns: bool,
    /// The toast showing: its text and when it was set (input time, s).
    pub(crate) toast: Option<(String, f64)>,
    /// The Auto-chip distance (yards) sent and not answered yet.
    pub(crate) auto_chip_in_flight: Option<u32>,
    /// Why the last Auto-chip was not sent, shown in the error banner until
    /// the next one.
    pub(crate) auto_chip_error: Option<String>,
    /// The turn's handedness (`None` until the server reports one).
    /// Stamped on shots as they trigger, like the club.
    pub(crate) handed: Option<Handedness>,

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
    /// The camera cards whose live picture was on screen in the last
    /// screen update, by actor ID: the next one keeps their live views.
    pub(crate) card_live: HashSet<String>,
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
            turn_golfer: None,
            header_menu: None,
            follow_turns: true,
            toast: None,
            #[cfg(test)]
            sent_selects: Vec::new(),
            auto_chip_in_flight: None,
            auto_chip_error: None,
            handed: None,
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
            card_live: HashSet::new(),
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
            // With the WebSocket up, its turn is the live source.
            if !self.ws_connected {
                self.selected_club = turn.and_then(|t| t.club);
                self.handed = turn.and_then(|t| t.handedness);
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
            let explained = self.actors.get(&id).and_then(|a| a.error()).is_some();
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
    /// close).
    pub(crate) fn handle_ws_event(&mut self, event: WsPollEvent) -> bool {
        match event {
            WsPollEvent::Opened => false,
            WsPollEvent::Message(msg) => {
                self.handle_ui_message(*msg);
                false
            }
            WsPollEvent::Error(_) => true,
            WsPollEvent::Disconnected => {
                self.actors.clear();
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
    /// Select `club` as the club in play, over the WebSocket.
    pub(crate) fn pick_club(&mut self, club: Club) {
        self.selected_club = Some(club);
        if let Some(tx) = &mut self.ws_sender {
            net::send_command(tx, &net::select_command(|s| s.club = Some(Some(club))));
        }
    }
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
mod tests;
