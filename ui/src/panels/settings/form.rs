//! `SettingsForm`: the whole Settings form (loaded config, entries, dirty cards, saves in
//! flight, cancel) and the requests it builds; plus the save target and pending removal.

use super::*;

/// Pending removal confirmation: the actor's global ID and display name.
pub(crate) struct PendingRemoval {
    pub(crate) id: String,
    pub(crate) name: String,
}

/// What a save writes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SaveTarget {
    /// The global settings: the Global and Simulation cards.
    Global,
    /// The golfers: the Golfers card.
    Golfers,
    /// One actor's section, by global ID.
    Actor(String),
    /// One actor's removal, by global ID.
    Remove(String),
    /// The whole config: the setup wizard.
    Full,
}

impl SaveTarget {
    /// The `scope` its `POST /api/settings` carries.
    pub(crate) fn scope(&self) -> Option<&str> {
        match self {
            SaveTarget::Global => Some("global"),
            SaveTarget::Golfers => Some("golfers"),
            SaveTarget::Actor(id) | SaveTarget::Remove(id) => Some(id),
            SaveTarget::Full => None,
        }
    }
}

/// A posted save waiting for its reply: what it writes and the body sent.
#[derive(Clone)]
pub(super) struct InFlightSave {
    pub(super) target: SaveTarget,
    pub(super) posted: FlighthookConfig,
}

/// A save that failed: what it wrote and why, shown on its card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SaveError {
    pub(crate) target: SaveTarget,
    pub(crate) message: String,
}

/// One form entry per instance in `s`, in no particular order.
pub(super) fn actor_entries(s: &FlighthookConfig) -> Vec<ActorFormEntry> {
    let mut actors = Vec::new();
    for (id, section) in &s.mevo {
        actors.push(ActorFormEntry::Device(DeviceFormEntry::from_mevo(
            id, section,
        )));
    }
    for (id, section) in &s.square {
        actors.push(ActorFormEntry::Device(DeviceFormEntry::from_square(
            id, section,
        )));
    }
    for (id, section) in &s.r10 {
        actors.push(ActorFormEntry::Device(DeviceFormEntry::from_r10(
            id, section,
        )));
    }
    for (id, section) in &s.exputt {
        actors.push(ActorFormEntry::Device(DeviceFormEntry::from_exputt(
            id, section,
        )));
    }
    for (id, section) in &s.trackball {
        actors.push(ActorFormEntry::Device(DeviceFormEntry::from_trackball(
            id, section,
        )));
    }
    for (id, section) in &s.openconnect_server {
        actors.push(ActorFormEntry::Device(DeviceFormEntry::from_openconnect(
            id, section,
        )));
    }
    for (id, section) in &s.pureplay_accessory {
        actors.push(ActorFormEntry::Device(
            DeviceFormEntry::from_pureplay_accessory(id, section),
        ));
    }
    for (id, section) in &s.mock_monitor {
        actors.push(ActorFormEntry::Device(DeviceFormEntry::from_mock(
            id, section,
        )));
    }
    for (id, section) in &s.camera {
        actors.push(ActorFormEntry::Device(DeviceFormEntry::from_camera(
            id, section,
        )));
    }
    for (id, section) in &s.openconnect_client {
        actors.push(ActorFormEntry::Integration(IntegrationFormEntry {
            id: id.clone(),
            integration_type: "openconnect_client".into(),
            name: section.name.clone(),
            enabled: section.enabled,
            address: section.address.clone().unwrap_or_default(),
            full_monitor: section.full_monitor.clone().unwrap_or_default(),
            chipping_monitor: section.chipping_monitor.clone().unwrap_or_default(),
            putting_monitor: section.putting_monitor.clone().unwrap_or_default(),
            chipping_clubs: section.chipping_clubs.clone(),
            putting_clubs: section.putting_clubs.clone(),
            dirty: false,
        }));
    }
    for (id, section) in &s.pureplay {
        actors.push(ActorFormEntry::Integration(IntegrationFormEntry {
            id: id.clone(),
            integration_type: "pureplay".into(),
            name: section.name.clone(),
            enabled: section.enabled,
            address: section.address.clone().unwrap_or_default(),
            full_monitor: section.full_monitor.clone().unwrap_or_default(),
            chipping_monitor: section.chipping_monitor.clone().unwrap_or_default(),
            putting_monitor: section.putting_monitor.clone().unwrap_or_default(),
            chipping_clubs: Vec::new(),
            putting_clubs: Vec::new(),
            dirty: false,
        }));
    }
    for (id, section) in &s.random_club {
        actors.push(ActorFormEntry::Integration(IntegrationFormEntry {
            id: id.clone(),
            integration_type: "random_club".into(),
            name: section.name.clone(),
            enabled: section.enabled,
            address: String::new(),
            full_monitor: String::new(),
            chipping_monitor: String::new(),
            putting_monitor: String::new(),
            chipping_clubs: Vec::new(),
            putting_clubs: Vec::new(),
            dirty: false,
        }));
    }
    for (id, section) in &s.webserver {
        actors.push(ActorFormEntry::Integration(IntegrationFormEntry {
            id: id.clone(),
            integration_type: "webserver".into(),
            name: section.name.clone(),
            enabled: true,
            address: section.bind.clone(),
            full_monitor: String::new(),
            chipping_monitor: String::new(),
            putting_monitor: String::new(),
            chipping_clubs: Vec::new(),
            putting_clubs: Vec::new(),
            dirty: false,
        }));
    }
    actors
}

/// The settings-only cards of the General group. They save, and cancel,
/// together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GeneralCard {
    Global,
    Simulation,
    Replay,
    BodyTracking,
}

/// Settings form state.
#[derive(Clone)]
pub(crate) struct SettingsForm {
    pub(crate) default_units: UnitSystem,
    pub(crate) shot_grouping: ShotGrouping,
    pub(crate) simulation: SimulationForm,
    pub(crate) replay: ReplayForm,
    pub(crate) body_tracking: BodyTrackingForm,
    /// The Golfers card, saved on its own.
    pub(crate) golfers: GolfersForm,
    /// The Global, Simulation, Replay and Body tracking cards (one save)
    /// have unsaved edits.
    pub(crate) global_dirty: bool,
    /// Which of the cards the unsaved Global edits were made on, by
    /// [`GeneralCard`]; meaningful only while `global_dirty`.
    pub(super) general_edited: [bool; 4],
    pub(crate) actors: Vec<ActorFormEntry>,
    pub(crate) loaded: bool,
    /// The save posted and not answered yet.
    pub(super) save: Option<InFlightSave>,
    /// The last save's failure, until the next save, or a reload that finds
    /// its edits saved.
    pub(crate) save_error: Option<SaveError>,
    /// A failed save is waiting for the settings to be read again, to find
    /// out what the server holds.
    pub(super) resync: bool,
    /// When the settings were last requested (egui time, s); `None` until
    /// the first request, and again after a failed save.
    pub(crate) fetched_at: Option<f64>,
    /// The server's config as of the last load or save: the base of every
    /// request and what Cancel goes back to.
    pub(super) original_config: Option<FlighthookConfig>,
}

/// Least time between two settings requests while they have not loaded (s).
pub(super) const SETTINGS_RETRY_S: f64 = 2.0;

impl Default for SettingsForm {
    fn default() -> Self {
        Self {
            default_units: UnitSystem::default(),
            shot_grouping: ShotGrouping::default(),
            simulation: SimulationForm::default(),
            replay: ReplayForm::default(),
            body_tracking: BodyTrackingForm::from_section(&BodyTrackingSection::default()),
            golfers: GolfersForm::default(),
            global_dirty: false,
            general_edited: [false; 4],
            actors: Vec::new(),
            loaded: false,
            save: None,
            save_error: None,
            resync: false,
            fetched_at: None,
            original_config: None,
        }
    }
}

impl SettingsForm {
    /// Whether to request the settings at `now`: while they have not
    /// loaded, or a failed save waits for them, at most once every
    /// [`SETTINGS_RETRY_S`]. Records the request.
    pub(crate) fn fetch_due(&mut self, now: f64) -> bool {
        if (self.loaded && !self.resync)
            || self.fetched_at.is_some_and(|t| now - t < SETTINGS_RETRY_S)
        {
            return false;
        }
        self.fetched_at = Some(now);
        true
    }

    /// Read the settings again: the server's config may have changed.
    pub(crate) fn request_reload(&mut self) {
        if self.loaded {
            self.resync = true;
            self.fetched_at = None;
        }
    }

    /// Take the server's config `s`. Every entry without unsaved edits, and
    /// the global settings unless they have unsaved edits, become the
    /// server's. An entry (or the global settings) with unsaved edits keeps
    /// them, and turns clean when the server already holds exactly those
    /// values: a save whose reply was lost. `s` becomes the saved config.
    pub(crate) fn load_from(&mut self, s: &FlighthookConfig) {
        if self.global_dirty && global_matches(&self.build_global_request(), s) {
            self.global_dirty = false;
        }
        if !self.global_dirty {
            self.default_units = s.default_units;
            self.shot_grouping = s.shot_grouping;
            self.simulation = SimulationForm::from_section(&s.simulation, s.default_units);
            self.replay = ReplayForm::from_section(&s.replay);
            self.body_tracking = BodyTrackingForm::from_section(&s.body_tracking);
            self.general_edited = [false; 4];
        }
        if self.golfers.dirty
            && self
                .golfers
                .to_records()
                .is_some_and(|golfers| golfers == s.golfers)
        {
            self.golfers.dirty = false;
        }
        if !self.golfers.dirty {
            self.golfers.reload(&s.golfers, s.default_units);
        }

        let edited: Vec<ActorFormEntry> = std::mem::take(&mut self.actors)
            .into_iter()
            .filter(ActorFormEntry::dirty)
            .collect();
        let mut actors: Vec<ActorFormEntry> = actor_entries(s)
            .into_iter()
            .filter(|a| !edited.iter().any(|e| e.global_id() == a.global_id()))
            .collect();
        for mut entry in edited {
            if entry_matches(s, &entry) {
                entry.set_dirty(false);
            }
            actors.push(entry);
        }
        actors.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
        self.actors = actors;

        self.original_config = Some(s.clone());
        self.loaded = true;
        self.resync = false;
        if self
            .save_error
            .as_ref()
            .is_some_and(|e| self.settled(&e.target))
        {
            self.save_error = None;
        }
    }

    /// Whether nothing `target` would write is still unsaved.
    pub(super) fn settled(&self, target: &SaveTarget) -> bool {
        match target {
            SaveTarget::Global => !self.global_dirty,
            SaveTarget::Golfers => !self.golfers.dirty,
            SaveTarget::Actor(id) => self
                .actors
                .iter()
                .all(|a| a.global_id() != *id || !a.dirty()),
            SaveTarget::Remove(id) => self.actors.iter().all(|a| a.global_id() != *id),
            SaveTarget::Full => !self.dirty(),
        }
    }

    /// Whether the General cards can be saved: the simulation, replay,
    /// body tracking and player fields are in range.
    pub(crate) fn general_valid(&self) -> bool {
        self.simulation.to_section().is_some()
            && self.replay.to_section().is_some()
            && self.body_tracking.to_section().is_some()
    }

    /// The whole form as a config.
    pub(crate) fn to_request(&self) -> FlighthookConfig {
        let mut config = FlighthookConfig {
            default_units: self.default_units,
            frp: self
                .original_config
                .as_ref()
                .map(|c| c.frp.clone())
                .unwrap_or_default(),
            shot_grouping: self.shot_grouping,
            simulation: self.simulation.to_section_or_loaded(),
            replay: self.replay.to_section_or_loaded(),
            body_tracking: self.body_tracking.to_section_or_loaded(),
            selected_golfer: self
                .original_config
                .as_ref()
                .and_then(|c| c.selected_golfer.clone()),
            golfers: self.golfers.to_records_or_loaded(),
            webserver: HashMap::new(),
            mevo: HashMap::new(),
            r10: HashMap::new(),
            square: HashMap::new(),
            exputt: HashMap::new(),
            trackball: HashMap::new(),
            mock_monitor: HashMap::new(),
            openconnect_server: HashMap::new(),
            openconnect_client: HashMap::new(),
            pureplay: HashMap::new(),
            pureplay_accessory: HashMap::new(),
            random_club: HashMap::new(),
            camera: HashMap::new(),
        };
        for actor in &self.actors {
            apply_actor_to_config(&mut config, actor);
        }
        if let Some(saved) = &self.original_config {
            config.keep_actor_ids_from(saved);
        }
        config.simulation.tournament = self.live_tournament();
        config
    }

    /// Tournament mode as the server last reported it. Only the toggle
    /// writes it, so a save carries this value, never the form's copy.
    pub(super) fn live_tournament(&self) -> bool {
        self.original_config
            .as_ref()
            .map_or(self.simulation.loaded.tournament, |c| {
                c.simulation.tournament
            })
    }

    /// Append a new, unsaved entry from (+) menu `preset` at the lowest
    /// index its type does not use yet. Returns its actor ID.
    pub(crate) fn add_preset(&mut self, preset: &AddPreset) -> Option<String> {
        let existing: Vec<&str> = self
            .actors
            .iter()
            .filter(|a| a.type_key() == preset.type_key)
            .map(ActorFormEntry::index)
            .collect();
        let id = next_index(&existing);
        let entry = new_actor_entry(preset, id)?;
        let global_id = entry.global_id();
        // A duplicate may be created, but not enabled: it starts off when an
        // enabled card of its type has its address.
        let mut probe = self.saved_config();
        apply_actor_to_config(&mut probe, &entry);
        let mut entry = entry;
        if probe.address_clash(&global_id).is_some() {
            entry.set_enabled(false);
        }
        self.actors.push(entry);
        Some(global_id)
    }

    /// [`add_preset`](Self::add_preset) with the first (+) menu preset of
    /// `type_key`.
    #[cfg(test)]
    pub(crate) fn add_actor(&mut self, type_key: &str) -> Option<String> {
        let preset = crate::panels::cards::CardGroup::ALL
            .iter()
            .flat_map(|g| crate::panels::cards::add_options(*g))
            .find(|p| p.type_key == type_key)?;
        self.add_preset(preset)
    }

    /// Whether anything in the form has unsaved edits.
    pub(crate) fn dirty(&self) -> bool {
        self.global_dirty || self.golfers.dirty || self.actors.iter().any(ActorFormEntry::dirty)
    }

    /// Whether a save has been posted and not answered yet.
    pub(crate) fn saving(&self) -> bool {
        self.save.is_some()
    }

    /// The save that is out: what it writes and its request body.
    #[cfg(test)]
    pub(crate) fn posted(&self) -> Option<(&SaveTarget, &FlighthookConfig)> {
        self.save.as_ref().map(|s| (&s.target, &s.posted))
    }

    /// The saved cameras, as of the last load or save: actor ID and section,
    /// by index.
    pub(crate) fn saved_cameras(&self) -> Vec<(String, CameraSection)> {
        let Some(config) = &self.original_config else {
            return Vec::new();
        };
        let mut cameras: Vec<(&String, &CameraSection)> = config.camera.iter().collect();
        cameras.sort_by(|(a, _), (b, _)| {
            (a.parse::<u64>().ok(), a.as_str()).cmp(&(b.parse::<u64>().ok(), b.as_str()))
        });
        cameras
            .into_iter()
            .map(|(index, section)| (format!("camera.{index}"), section.clone()))
            .collect()
    }

    /// The saved cameras that are on ([`Self::saved_cameras`] without the
    /// ones turned off): the ones that record and have a live picture.
    pub(crate) fn cameras_on(&self) -> Vec<(String, CameraSection)> {
        let mut cameras = self.saved_cameras();
        cameras.retain(|(_, section)| section.enabled);
        cameras
    }

    /// The rotation camera `actor` (e.g. `camera.0`) is shown with, in
    /// clockwise degrees: its saved `rotation_deg` in effect. `None` until
    /// the settings load, and for a camera that is not configured.
    /// What a shot's grades depend on besides its clip: the golfer who hit
    /// it (`golfer`, the shot's golfer, else the golfer up, as the server
    /// reads it), resolved by the lookup rule, and that golfer's saved sex
    /// and height. Empty before the settings load.
    pub(crate) fn grades_key(&self, golfer: Option<&str>) -> String {
        self.original_config
            .as_ref()
            .map(|c| match golfer.and_then(|id| c.golfer_of(id)) {
                Some(g) => format!("{:?} {:?}", g.own_id(), g.facts()),
                None => format!("{golfer:?}"),
            })
            .unwrap_or_default()
    }

    pub(crate) fn saved_rotation(&self, actor: &str) -> Option<u16> {
        let index = actor.strip_prefix("camera.")?;
        let section = self.original_config.as_ref()?.camera.get(index)?;
        Some(flighthook::rotation_in_effect_deg(section.rotation_deg))
    }

    /// Whether the Devices tab is locked: something has unsaved edits, or a
    /// save is out. Only the dirty cards stay editable, and only while no
    /// save is out.
    pub(crate) fn locked(&self) -> bool {
        self.dirty() || self.saving()
    }

    /// The form index of actor `global_id`.
    pub(crate) fn position(&self, global_id: &str) -> Option<usize> {
        self.actors.iter().position(|a| a.global_id() == global_id)
    }

    /// Record a save of `target` as posted and return its request body.
    pub(crate) fn begin_save(&mut self, target: SaveTarget) -> FlighthookConfig {
        let posted = match &target {
            SaveTarget::Global => self.build_global_request(),
            SaveTarget::Golfers => {
                let mut config = self.saved_config();
                config.golfers = self.golfers.to_records_or_loaded();
                config
            }
            SaveTarget::Actor(id) => self.build_actor_request(id),
            SaveTarget::Remove(id) => {
                let mut config = self.saved_config();
                set_section(&mut config, None, id);
                config
            }
            SaveTarget::Full => self.to_request(),
        };
        self.begin_save_with(target, posted.clone());
        posted
    }

    /// The in-flight golfers save came back with the golfers as saved
    /// (`issued`, in the posted order): a golfer posted without its own id
    /// takes the one the save gave it, in the posted body and on the card,
    /// so a second save before the settings are read again sends that id
    /// rather than asking for another.
    pub(crate) fn keep_issued_golfer_ids(&mut self, issued: &[flighthook::GolferRecord]) {
        let Some(save) = self
            .save
            .as_mut()
            .filter(|s| s.target == SaveTarget::Golfers)
        else {
            return;
        };
        if issued.len() != save.posted.golfers.len() {
            return;
        }
        let same_card = self.golfers.golfers.len() == issued.len();
        for (i, (posted, saved)) in save.posted.golfers.iter_mut().zip(issued).enumerate() {
            let Some(id) = saved.own_id().filter(|_| posted.own_id().is_none()) else {
                continue;
            };
            posted.identifiers.insert(0, id.to_owned());
            if same_card
                && let Some(form) = self.golfers.golfers.get_mut(i)
                && form.record.own_id().is_none()
            {
                form.record.identifiers.insert(0, id.to_owned());
            }
        }
    }

    /// Record a save of `target` with body `posted` as posted.
    pub(crate) fn begin_save_with(&mut self, target: SaveTarget, posted: FlighthookConfig) {
        self.save_error = None;
        self.save = Some(InFlightSave { target, posted });
    }

    /// Settle the posted save with its reply: `Ok` once the server applied
    /// it. A failure keeps every edit, records the error for the card and
    /// asks for the settings again, since the server may have applied the
    /// save even though its reply was lost. Returns the global ID of the
    /// actor a successful removal took out of the form.
    pub(crate) fn finish_save(&mut self, result: Result<(), String>) -> Option<String> {
        let save = self.save.take()?;
        match result {
            Ok(()) => {
                self.save_error = None;
                self.apply_saved(save)
            }
            Err(message) => {
                self.save_error = Some(SaveError {
                    target: save.target,
                    message,
                });
                self.resync = true;
                self.fetched_at = None;
                None
            }
        }
    }

    /// The server applied `save`: it becomes part of the saved config, and
    /// the edits it carried turn clean ([`Self::load_from`]). An edit that
    /// differs from what was posted, or an entry the server does not hold,
    /// stays dirty.
    pub(super) fn apply_saved(&mut self, save: InFlightSave) -> Option<String> {
        let InFlightSave { target, posted } = save;
        let mut saved = self.saved_config();
        match &target {
            SaveTarget::Global => copy_global(&mut saved, &posted),
            SaveTarget::Golfers => {
                saved.golfers = posted.golfers;
                // A new golfer's own id is given by the server: read it.
                self.resync = true;
                self.fetched_at = None;
            }
            SaveTarget::Actor(id) => set_section(&mut saved, Some(&posted), id),
            SaveTarget::Remove(id) => {
                set_section(&mut saved, Some(&posted), id);
                if let Some(idx) = self.position(id) {
                    self.actors.remove(idx);
                }
            }
            SaveTarget::Full => {
                // The server keeps tournament mode on a whole-config save.
                let tournament = saved.simulation.tournament;
                saved = posted;
                saved.simulation.tournament = tournament;
            }
        }
        self.load_from(&saved);
        match target {
            SaveTarget::Remove(id) if self.position(&id).is_none() => Some(id),
            _ => None,
        }
    }

    /// The saved config, or the form's own while nothing has loaded.
    pub(super) fn saved_config(&self) -> FlighthookConfig {
        self.original_config
            .clone()
            .unwrap_or_else(|| self.to_request())
    }

    /// The server's config as of the last load or save, `None` before the
    /// first load.
    pub(crate) fn saved_config_ref(&self) -> Option<&FlighthookConfig> {
        self.original_config.as_ref()
    }

    /// Record an edit on a General card.
    pub(crate) fn mark_general(&mut self, card: GeneralCard) {
        if !self.global_dirty {
            self.general_edited = [false; 4];
        }
        self.general_edited[card as usize] = true;
        self.global_dirty = true;
    }

    /// Whether `card` shows Save and Cancel: it has unsaved edits. Global
    /// edits made elsewhere than on any General card count as the Global
    /// card's.
    pub(crate) fn general_card_dirty(&self, card: GeneralCard) -> bool {
        if !self.global_dirty {
            return false;
        }
        match card {
            GeneralCard::Global => {
                self.general_edited[GeneralCard::Global as usize]
                    || !(self.general_edited[GeneralCard::Simulation as usize]
                        || self.general_edited[GeneralCard::Replay as usize]
                        || self.general_edited[GeneralCard::BodyTracking as usize])
            }
            GeneralCard::Simulation | GeneralCard::Replay | GeneralCard::BodyTracking => {
                self.general_edited[card as usize]
            }
        }
    }

    /// Put both General cards back to the saved config.
    pub(crate) fn cancel_general(&mut self) {
        let saved = self.original_config.clone().unwrap_or_default();
        self.default_units = saved.default_units;
        self.shot_grouping = saved.shot_grouping;
        self.simulation = SimulationForm::from_section(&saved.simulation, saved.default_units);
        self.replay = ReplayForm::from_section(&saved.replay);
        self.body_tracking = BodyTrackingForm::from_section(&saved.body_tracking);
        self.global_dirty = false;
        self.general_edited = [false; 4];
        if self.save_error.as_ref().map(|e| &e.target) == Some(&SaveTarget::Global) {
            self.save_error = None;
        }
    }

    /// The saved config's entry for actor `global_id`.
    pub(super) fn saved_entry(&self, global_id: &str) -> Option<ActorFormEntry> {
        actor_entries(self.original_config.as_ref()?)
            .into_iter()
            .find(|a| a.global_id() == global_id)
    }

    /// Put the entry at `idx` back to the saved config, or drop it if it is
    /// a new card. Returns whether it was dropped.
    pub(crate) fn cancel_actor(&mut self, idx: usize) -> bool {
        let Some(actor) = self.actors.get(idx) else {
            return false;
        };
        let global_id = actor.global_id();
        let removed = match self.saved_entry(&global_id) {
            Some(saved) => {
                self.actors[idx] = saved;
                false
            }
            None => {
                self.actors.remove(idx);
                true
            }
        };
        if self
            .save_error
            .as_ref()
            .is_some_and(|e| e.target.scope() == Some(global_id.as_str()))
        {
            self.save_error = None;
        }
        removed
    }

    /// Take the grouping mode the server reports (it also changes outside
    /// this form): into the loaded config, and into the field unless the
    /// Global section has unsaved edits.
    pub(crate) fn sync_shot_grouping(&mut self, mode: ShotGrouping) {
        if let Some(orig) = &mut self.original_config {
            orig.shot_grouping = mode;
        }
        if !self.global_dirty {
            self.shot_grouping = mode;
        }
    }

    /// The saved config with the form's global settings.
    pub(crate) fn build_global_request(&self) -> FlighthookConfig {
        let mut config = self.saved_config();
        config.default_units = self.default_units;
        config.shot_grouping = self.shot_grouping;
        config.simulation = self.simulation.to_section_or_loaded();
        config.simulation.tournament = self.live_tournament();
        config.replay = self.replay.to_section_or_loaded();
        config.body_tracking = self.body_tracking.to_section_or_loaded();
        config
    }

    /// Put the Golfers card back to the saved golfers.
    pub(crate) fn cancel_golfers(&mut self) {
        let saved = self.original_config.clone().unwrap_or_default();
        self.golfers.reload(&saved.golfers, saved.default_units);
        if self.save_error.as_ref().map(|e| &e.target) == Some(&SaveTarget::Golfers) {
            self.save_error = None;
        }
    }

    /// Whether the saved settings offer Auto-chip. False until the settings
    /// have loaded, so a disabled chip never flashes at startup.
    pub(crate) fn auto_chip_enabled(&self) -> bool {
        self.original_config
            .as_ref()
            .is_some_and(|c| c.simulation.auto_chip)
    }

    /// Whether the saved settings show the camera cards in the Shots tab's
    /// card row. True until the settings have loaded, as the default is.
    pub(crate) fn shot_cards_enabled(&self) -> bool {
        self.original_config
            .as_ref()
            .is_none_or(|c| c.replay.shot_cards)
    }

    /// The saved `[body_tracking]` settings in effect (defaults until the
    /// settings have loaded).
    pub(crate) fn saved_body_tracking(&self) -> BodyTrackingSection {
        self.original_config
            .as_ref()
            .map(|c| c.body_tracking.in_effect())
            .unwrap_or_default()
    }

    /// The saved config with `body_tracking` in place of its own: what a
    /// save outside the card's Save sends (`scope=body_tracking`, which
    /// writes only that section).
    pub(crate) fn tuning_request(&self, body_tracking: &BodyTrackingSection) -> FlighthookConfig {
        let mut config = self.saved_config();
        config.body_tracking = body_tracking.clone();
        config
    }

    /// The saved config with the form's entry for actor `global_id`.
    pub(crate) fn build_actor_request(&self, global_id: &str) -> FlighthookConfig {
        let mut config = self.saved_config();
        if let Some(idx) = self.position(global_id) {
            let live = config.clone();
            apply_actor_to_config(&mut config, &self.actors[idx]);
            // A card saved on an address another enabled card has is saved
            // off.
            config.demote_clashing(Some(&live));
        }
        config
    }

    /// Why card `global_id`, which is off in the saved settings, cannot be
    /// turned on: an enabled card of its type has its address. `None` for a
    /// card that is on, or that has no clash.
    pub(crate) fn enable_clash(&self, global_id: &str) -> Option<flighthook::AddressClash> {
        let saved = self.original_config.as_ref()?;
        (!saved.actor_enabled(global_id))
            .then(|| saved.address_clash(global_id))
            .flatten()
    }
    /// Whether card `global_id` is off: in the save that is out, else in
    /// the saved settings. False for a card the settings don't hold.
    pub(crate) fn turned_off(&self, global_id: &str) -> bool {
        self.save
            .as_ref()
            .map(|s| &s.posted)
            .or(self.original_config.as_ref())
            .is_some_and(|c| c.disabled_actors().iter().any(|id| id == global_id))
    }

    /// Record a save that turns card `global_id` on or off and nothing
    /// else, scoped to it, and return its request body: the saved settings
    /// with only that card's `enabled` changed. `None` for a card that has
    /// no such switch or that the saved settings don't hold.
    pub(crate) fn begin_enable(&mut self, global_id: &str, on: bool) -> Option<FlighthookConfig> {
        let mut posted = self.saved_config();
        if !posted.set_actor_enabled(global_id, on) {
            return None;
        }
        self.begin_save_with(SaveTarget::Actor(global_id.to_owned()), posted.clone());
        Some(posted)
    }
}

/// Whether a card of actor type `type_key` has the on/off switch: every
/// launch monitor, camera and output, not the web server.
pub(crate) fn switchable(type_key: &str) -> bool {
    type_key != "webserver"
}

/// Whether `a` and `b` hold the same global settings.
/// Tournament mode is left out: the General cards never write it.
pub(super) fn global_matches(a: &FlighthookConfig, b: &FlighthookConfig) -> bool {
    a.default_units == b.default_units
        && a.shot_grouping == b.shot_grouping
        && SimulationSection {
            tournament: b.simulation.tournament,
            ..a.simulation.clone()
        } == b.simulation
        && a.replay == b.replay
        && a.body_tracking == b.body_tracking
}

/// Copy `from`'s global settings into `to`, all but tournament mode (the
/// server keeps it on a General save).
pub(super) fn copy_global(to: &mut FlighthookConfig, from: &FlighthookConfig) {
    to.default_units = from.default_units;
    to.shot_grouping = from.shot_grouping;
    to.simulation = SimulationSection {
        tournament: to.simulation.tournament,
        ..from.simulation.clone()
    };
    to.replay = from.replay.clone();
    to.body_tracking = from.body_tracking.clone();
}

/// Set `to`'s section for actor `global_id` to `from`'s: copied, or removed
/// when `from` is `None` or has none.
pub(super) fn set_section(
    to: &mut FlighthookConfig,
    from: Option<&FlighthookConfig>,
    global_id: &str,
) {
    fn set<T: Clone>(to: &mut HashMap<String, T>, from: Option<&T>, index: &str) {
        match from {
            Some(section) => {
                to.insert(index.to_string(), section.clone());
            }
            None => {
                to.remove(index);
            }
        }
    }
    let Some((type_key, i)) = global_id.split_once('.') else {
        return;
    };
    match type_key {
        "webserver" => set(&mut to.webserver, from.and_then(|f| f.webserver.get(i)), i),
        "mevo" => set(&mut to.mevo, from.and_then(|f| f.mevo.get(i)), i),
        "r10" => set(&mut to.r10, from.and_then(|f| f.r10.get(i)), i),
        "square" => set(&mut to.square, from.and_then(|f| f.square.get(i)), i),
        "exputt" => set(&mut to.exputt, from.and_then(|f| f.exputt.get(i)), i),
        "trackball" => set(&mut to.trackball, from.and_then(|f| f.trackball.get(i)), i),
        "mock_monitor" => set(
            &mut to.mock_monitor,
            from.and_then(|f| f.mock_monitor.get(i)),
            i,
        ),
        "openconnect_server" => set(
            &mut to.openconnect_server,
            from.and_then(|f| f.openconnect_server.get(i)),
            i,
        ),
        "openconnect_client" => set(
            &mut to.openconnect_client,
            from.and_then(|f| f.openconnect_client.get(i)),
            i,
        ),
        "pureplay" => set(&mut to.pureplay, from.and_then(|f| f.pureplay.get(i)), i),
        "pureplay_accessory" => set(
            &mut to.pureplay_accessory,
            from.and_then(|f| f.pureplay_accessory.get(i)),
            i,
        ),
        "random_club" => set(
            &mut to.random_club,
            from.and_then(|f| f.random_club.get(i)),
            i,
        ),
        "camera" => set(&mut to.camera, from.and_then(|f| f.camera.get(i)), i),
        _ => {}
    }
}

/// `config`'s section for actor `global_id`, as JSON.
pub(super) fn section_json(
    config: &FlighthookConfig,
    global_id: &str,
) -> Option<serde_json::Value> {
    fn get<T: serde::Serialize>(
        map: &HashMap<String, T>,
        index: &str,
    ) -> Option<serde_json::Value> {
        map.get(index).and_then(|s| serde_json::to_value(s).ok())
    }
    let (type_key, i) = global_id.split_once('.')?;
    match type_key {
        "webserver" => get(&config.webserver, i),
        "mevo" => get(&config.mevo, i),
        "r10" => get(&config.r10, i),
        "square" => get(&config.square, i),
        "exputt" => get(&config.exputt, i),
        "trackball" => get(&config.trackball, i),
        "mock_monitor" => get(&config.mock_monitor, i),
        "openconnect_server" => get(&config.openconnect_server, i),
        "openconnect_client" => get(&config.openconnect_client, i),
        "pureplay" => get(&config.pureplay, i),
        "pureplay_accessory" => get(&config.pureplay_accessory, i),
        "random_club" => get(&config.random_club, i),
        "camera" => get(&config.camera, i),
        _ => None,
    }
}

/// Whether `config` holds exactly what form entry `entry` would save.
pub(super) fn entry_matches(config: &FlighthookConfig, entry: &ActorFormEntry) -> bool {
    let mut own = config.clone();
    let id = apply_actor_to_config(&mut own, entry);
    let saved = section_json(config, &id);
    saved.is_some() && saved == section_json(&own, &id)
}

/// Whether form entry `actor` can be saved. Each reason it cannot shows
/// under its field on the card's page.
pub(crate) fn actor_valid(actor: &ActorFormEntry) -> bool {
    match actor {
        ActorFormEntry::Device(dev) => {
            if dev.name.is_empty() {
                return false;
            }
            if (dev.has_bind_address() || dev.has_pureplay_address())
                && !dev.address.is_empty()
                && dev.address.parse::<std::net::SocketAddr>().is_err()
            {
                return false;
            }
            // A BLE address is optional — blank means auto-discover.
            let ble = dev.address.trim();
            if dev.has_ble_address() && !ble.is_empty() && !is_ble_address(ble) {
                return false;
            }
            if dev.is_mevo() && launch_only_spin_value(&dev.launch_only_spin).is_none() {
                return false;
            }
            if dev.has_camera_timing() && impact_lead_value(&dev.impact_lead).is_none() {
                return false;
            }
            if dev.driver_blocks_save() {
                return false;
            }
            if dev.is_camera()
                && (dev.cam.id.trim().is_empty()
                    || !video_mode_valid(&dev.cam.mode)
                    || latency_value(&dev.cam.latency).is_none()
                    || !flighthook::rotation_valid(dev.cam.rotation)
                    || angle_value(&dev.cam.yaw).is_none()
                    || angle_value(&dev.cam.pitch).is_none())
            {
                return false;
            }
            // Face impact calibration: blank means "use the default",
            // anything else must be a finite, non-negative mm value.
            !(dev.is_square()
                && dev
                    .square_impact_mm
                    .values()
                    .any(|v| !is_valid_impact_mm(v)))
        }
        ActorFormEntry::Integration(entry) => {
            !entry.name.is_empty()
                && (entry.integration_type == "random_club"
                    || entry.address.parse::<std::net::SocketAddr>().is_ok())
        }
    }
}

/// The clubs of `clubs` the card's Chipping or Putting picker offers
/// (`options`): a club in the config it does not offer (e.g. a hand-edited
/// `9I`) is dropped on save.
pub(super) fn offered(clubs: &[Club], options: &[Club]) -> Vec<Club> {
    clubs
        .iter()
        .copied()
        .filter(|c| options.contains(c))
        .collect()
}

/// Apply a single actor form entry's data into a FlighthookConfig, keeping
/// the actor id the card holds in it. Returns the actor's global ID (e.g.
/// "mevo.0").
pub(super) fn apply_actor_to_config(
    config: &mut FlighthookConfig,
    actor: &ActorFormEntry,
) -> String {
    match actor {
        ActorFormEntry::Device(dev) => {
            let global_id = format!("{}.{}", dev.monitor_type, dev.id);
            let actor_id = config.actor_of_card(&global_id);
            match dev.monitor_type.as_str() {
                "mevo" => {
                    config.mevo.insert(
                        dev.id.clone(),
                        MevoSection {
                            actor_id: actor_id.clone(),
                            name: dev.name.clone(),
                            enabled: dev.enabled,
                            launch_only_spin: launch_only_spin_value(&dev.launch_only_spin)
                                .unwrap_or(MevoSection::DEFAULT_LAUNCH_ONLY_SPIN),
                            impact_lead_s: impact_lead_value(&dev.impact_lead).flatten(),
                            driver: dev.mevo_config(),
                        },
                    );
                }
                "square" => {
                    config.square.insert(
                        dev.id.clone(),
                        flighthook::SquareSection {
                            actor_id: actor_id.clone(),
                            name: dev.name.clone(),
                            enabled: dev.enabled,
                            impact_lead_s: impact_lead_value(&dev.impact_lead).flatten(),
                            driver: dev.square_config(),
                        },
                    );
                }
                "exputt" => {
                    config.exputt.insert(
                        dev.id.clone(),
                        flighthook::ExPuttSection {
                            actor_id: actor_id.clone(),
                            name: dev.name.clone(),
                            enabled: dev.enabled,
                            impact_lead_s: impact_lead_value(&dev.impact_lead).flatten(),
                            driver: dev.exputt_config(),
                        },
                    );
                }
                "trackball" => {
                    config.trackball.insert(
                        dev.id.clone(),
                        flighthook::TrackballSection {
                            actor_id: actor_id.clone(),
                            name: dev.name.clone(),
                            enabled: dev.enabled,
                            driver: dev.trackball_config(),
                        },
                    );
                }
                "r10" => {
                    config.r10.insert(
                        dev.id.clone(),
                        R10Section {
                            actor_id: actor_id.clone(),
                            name: dev.name.clone(),
                            enabled: dev.enabled,
                            impact_lead_s: impact_lead_value(&dev.impact_lead).flatten(),
                            driver: dev.r10_config(),
                        },
                    );
                }
                "openconnect_server" => {
                    config.openconnect_server.insert(
                        dev.id.clone(),
                        flighthook::OpenConnectServerSection {
                            actor_id: actor_id.clone(),
                            name: dev.name.clone(),
                            enabled: dev.enabled,
                            bind: if dev.address.is_empty() {
                                None
                            } else {
                                Some(dev.address.clone())
                            },
                            impact_lead_s: impact_lead_value(&dev.impact_lead).flatten(),
                        },
                    );
                }
                "pureplay_accessory" => {
                    config.pureplay_accessory.insert(
                        dev.id.clone(),
                        flighthook::PureplayAccessorySection {
                            actor_id: actor_id.clone(),
                            name: dev.name.clone(),
                            enabled: dev.enabled,
                            address: if dev.address.is_empty() {
                                None
                            } else {
                                Some(dev.address.clone())
                            },
                            shots: dev.pureplay_shots,
                            impact_lead_s: impact_lead_value(&dev.impact_lead).flatten(),
                        },
                    );
                }
                "mock_monitor" => {
                    config.mock_monitor.insert(
                        dev.id.clone(),
                        MockMonitorSection {
                            actor_id: actor_id.clone(),
                            name: dev.name.clone(),
                            enabled: dev.enabled,
                            impact_lead_s: impact_lead_value(&dev.impact_lead).flatten(),
                            driver: dev.mock.clone(),
                        },
                    );
                }
                "camera" => {
                    config.camera.insert(
                        dev.id.clone(),
                        CameraSection {
                            name: dev.name.clone(),
                            enabled: dev.enabled,
                            camera: dev.cam.id.trim().to_string(),
                            camera_name: dev.cam.name.clone(),
                            position: dev.cam.position,
                            mode: dev.cam.mode.trim().to_string(),
                            latency_s: latency_value(&dev.cam.latency).flatten(),
                            rotation_deg: dev.cam.rotation,
                            yaw_deg: angle_value(&dev.cam.yaw).flatten(),
                            pitch_deg: angle_value(&dev.cam.pitch).flatten(),
                        },
                    );
                }
                _ => {}
            }
            global_id
        }
        ActorFormEntry::Integration(entry) => {
            let global_id = format!("{}.{}", entry.integration_type, entry.id);
            let actor_id = config.actor_of_card(&global_id);
            match entry.integration_type.as_str() {
                "openconnect_client" => {
                    config.openconnect_client.insert(
                        entry.id.clone(),
                        OpenConnectClientSection {
                            name: entry.name.clone(),
                            enabled: entry.enabled,
                            address: if entry.address.is_empty() {
                                None
                            } else {
                                Some(entry.address.clone())
                            },
                            full_monitor: if entry.full_monitor.is_empty() {
                                None
                            } else {
                                Some(entry.full_monitor.clone())
                            },
                            chipping_monitor: if entry.chipping_monitor.is_empty() {
                                None
                            } else {
                                Some(entry.chipping_monitor.clone())
                            },
                            putting_monitor: if entry.putting_monitor.is_empty() {
                                None
                            } else {
                                Some(entry.putting_monitor.clone())
                            },
                            chipping_clubs: offered(
                                &entry.chipping_clubs,
                                flighthook::chipping_club_options(),
                            ),
                            putting_clubs: offered(
                                &entry.putting_clubs,
                                flighthook::putting_club_options(),
                            ),
                            actor_id: actor_id.clone(),
                        },
                    );
                }
                "pureplay" => {
                    let set = |v: &String| (!v.is_empty()).then(|| v.clone());
                    config.pureplay.insert(
                        entry.id.clone(),
                        PureplaySection {
                            actor_id: actor_id.clone(),
                            name: entry.name.clone(),
                            enabled: entry.enabled,
                            address: set(&entry.address),
                            full_monitor: set(&entry.full_monitor),
                            chipping_monitor: set(&entry.chipping_monitor),
                            putting_monitor: set(&entry.putting_monitor),
                        },
                    );
                }
                "random_club" => {
                    config.random_club.insert(
                        entry.id.clone(),
                        RandomClubSection {
                            actor_id: actor_id.clone(),
                            name: entry.name.clone(),
                            enabled: entry.enabled,
                        },
                    );
                }
                "webserver" => {
                    config.webserver.insert(
                        entry.id.clone(),
                        WebserverSection {
                            name: entry.name.clone(),
                            bind: entry.address.clone(),
                        },
                    );
                }
                _ => {}
            }
            global_id
        }
    }
}
