//! The Devices tab: the settings-only Global, Simulation and Replay cards,
//! then one card per web server, launch monitor, camera and output that
//! flips between its live telemetry and its settings.
//!
//! While any card has unsaved edits the tab is locked to it: the tab bar,
//! the other cards' toggles and edit controls, and the (+) buttons are
//! disabled until the edits are saved or cancelled. While a save is out,
//! every card's controls wait for its reply.

use std::collections::HashMap;

use crate::app::FlighthookApp;
use crate::net;
use crate::panels::cards::{self, CardGroup};
use crate::panels::monitoring;
use crate::panels::settings::{self, ActorFormEntry, Pending, PendingRemoval, SaveTarget};
use crate::theme;
use crate::types::{ActorStatus, ActorStatusResponse};

/// The side a device card shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum CardMode {
    #[default]
    Telemetry,
    Settings,
}

impl CardMode {
    /// The toggle's options, in order.
    const ALL: [CardMode; 2] = [CardMode::Telemetry, CardMode::Settings];

    fn label(self) -> &'static str {
        match self {
            CardMode::Telemetry => "Telemetry",
            CardMode::Settings => "Settings",
        }
    }
}

/// The side each device card shows, by actor ID: Telemetry until flipped.
#[derive(Default)]
pub(crate) struct CardModes(HashMap<String, CardMode>);

impl CardModes {
    pub(crate) fn get(&self, id: &str) -> CardMode {
        self.0.get(id).copied().unwrap_or_default()
    }

    pub(crate) fn set(&mut self, id: &str, mode: CardMode) {
        self.0.insert(id.to_string(), mode);
    }

    pub(crate) fn forget(&mut self, id: &str) {
        self.0.remove(id);
    }
}

/// A device card: an actor ID, and its entry in the settings form (an index
/// into its actors) when the actor is configured.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DeviceCard {
    pub(crate) id: String,
    pub(crate) form: Option<usize>,
}

/// The device cards by group, in card order ([`cards::grouped`]): every
/// configured actor, and every running one the form does not list.
pub(crate) fn device_card_order(
    actors: &HashMap<String, ActorStatusResponse>,
    form: &[ActorFormEntry],
) -> Vec<(CardGroup, Vec<DeviceCard>)> {
    let mut ids: Vec<(String, Option<usize>)> = form
        .iter()
        .enumerate()
        .map(|(i, a)| (a.global_id(), Some(i)))
        .collect();
    for id in actors.keys() {
        if !ids.iter().any(|(known, _)| known == id) {
            ids.push((id.clone(), None));
        }
    }
    cards::grouped(ids.iter().map(|(id, form)| {
        let (type_key, index) = cards::split_actor_id(id);
        (
            type_key,
            index,
            DeviceCard {
                id: id.clone(),
                form: *form,
            },
        )
    }))
}

/// Height of every card on the Devices tab: General, Launch monitors,
/// Cameras and Outputs, on both sides, whatever it shows. The Telemetry side
/// of a connected Mevo+, Omni or Open Connect client and the Settings side of
/// the Global card fit without scrolling; a longer body scrolls inside its
/// card.
pub(crate) const CARD_HEIGHT: f32 = 280.0;

/// Height of a card's action row (Reconnect, Find mat, Still frame,
/// Remove): one button.
fn action_row(ui: &egui::Ui) -> f32 {
    ui.spacing().interact_size.y
}

/// A Devices-tab card being laid out, top to bottom: its header and the
/// lines under it ([`top`](Self::top)), its [`body`](Self::body), then its
/// [`actions`](Self::actions) at the bottom.
pub(crate) struct CardRegions<'a> {
    ui: &'a mut egui::Ui,
    /// The card's ID, which keys its scroll position.
    key: &'a str,
}

impl CardRegions<'_> {
    /// The top of the card: its header and any line under it.
    pub(crate) fn top(&mut self) -> &mut egui::Ui {
        self.ui
    }

    /// The card's body: everything between the top and the action row
    /// (`with_actions`) or the bottom. It scrolls inside the card when it
    /// does not fit, keeping its position for the session.
    pub(crate) fn body<R>(
        &mut self,
        with_actions: bool,
        add: impl FnOnce(&mut egui::Ui) -> R,
    ) -> R {
        let spacing = self.ui.spacing().item_spacing.y;
        let reserve = if with_actions {
            action_row(self.ui) + spacing
        } else {
            0.0
        };
        let height = (self.ui.available_height() - reserve).max(0.0);
        // Whole points, so the scroll area's pixel rounding never reaches
        // past the card's width, with or without its bar.
        let width = self.ui.available_width().floor();
        let key = self.key;
        self.ui
            .allocate_ui_with_layout(
                egui::vec2(width, height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_min_size(egui::vec2(width, height));
                    // Keyed by the card, not its place in the grid.
                    ui.scope_builder(
                        egui::UiBuilder::new().id(("devices_card_body", key)),
                        |ui| {
                            // A thin bar, shown whenever the body overflows.
                            ui.spacing_mut().scroll = egui::style::ScrollStyle::thin();
                            let shown = egui::ScrollArea::vertical()
                                .auto_shrink([false, false])
                                .max_height(height)
                                .show(ui, add);
                            mark_body(ui, key, &shown);
                            shown.inner
                        },
                    )
                    .inner
                },
            )
            .inner
    }

    /// The action row along the card's bottom, right to left: the first
    /// button added sits furthest right.
    pub(crate) fn actions<R>(&mut self, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
        let width = self.ui.available_width();
        let height = action_row(self.ui);
        self.ui
            .allocate_ui_with_layout(
                egui::vec2(width, height),
                egui::Layout::right_to_left(egui::Align::Center),
                add,
            )
            .inner
    }
}

/// A Devices-tab card, [`CARD_HEIGHT`] tall, with an `accent` stripe;
/// `add` lays out its regions. `key` names it.
pub(crate) fn fixed_card<R>(
    ui: &mut egui::Ui,
    key: &str,
    accent: egui::Color32,
    add: impl FnOnce(&mut CardRegions<'_>) -> R,
) -> egui::InnerResponse<R> {
    theme::accent_card(ui, accent, CARD_HEIGHT, |ui| {
        add(&mut CardRegions { ui, key })
    })
}

/// What a click on a device card asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CardAction {
    Mode(CardMode),
    Save,
    Cancel,
    Remove,
    FindMat,
    /// Fetch the camera's still frame.
    Still,
    /// Look for cameras again.
    RefreshCameras,
    /// Stop the actor and start it again.
    Reconnect,
}

/// How a device card is drawn.
struct CardView<'a> {
    telemetry: Option<&'a ActorStatusResponse>,
    /// The frame's time, for the red of a lost connection.
    now_ms: u64,
    /// The side picked on the toggle.
    mode: CardMode,
    /// Its toggle and edit controls take input: nothing else is dirty and
    /// no save is out.
    editable: bool,
    can_save: bool,
    saving: bool,
    /// Why its last save failed.
    error: Option<&'a str>,
    /// A reconnect of it is in flight.
    reconnecting: bool,
    /// Launch monitors for the routing dropdowns: (actor ID, name).
    monitors: &'a [(String, String)],
    /// The camera list, for a camera card's picker.
    cameras: &'a crate::app::CameraList,
}

/// One device card. Its header holds the name on the left and, on the
/// right, the status badge (Telemetry side) or the type tag (Settings side,
/// left out when it reads the same as the name), then the Telemetry |
/// Settings toggle, or Save and Cancel while its settings have unsaved
/// edits. The action row along the bottom holds, on the settings side,
/// Remove, except on a web server's card; on the telemetry side, Reconnect
/// on a launch monitor's or output's card (disabled while one is in flight
/// and while the tab is locked), then Find mat on an ExPutt's, enabled while
/// it is connected, and Still frame on a camera's (`still`), whose frame
/// shows in the body. The left stripe is the actor's status colour, dim while
/// it is not running.
fn device_card(
    ui: &mut egui::Ui,
    id: &str,
    view: &CardView<'_>,
    form: Option<&mut ActorFormEntry>,
    mut still: Option<&mut crate::panels::replay::Still>,
) -> Option<CardAction> {
    let accent = view.telemetry.map_or(theme::TEXT_DIM, |a| {
        monitoring::status_color(a, view.now_ms)
    });
    let dirty = form.as_ref().is_some_and(|f| f.dirty());
    let mode = match &form {
        None => CardMode::Telemetry,
        Some(_) if dirty => CardMode::Settings,
        Some(_) => view.mode,
    };
    let form_name = form.as_ref().map(|f| f.name().to_string());
    let title = match mode {
        CardMode::Telemetry => view
            .telemetry
            .map(|t| t.name.clone())
            .filter(|n| !n.is_empty())
            .or(form_name)
            .unwrap_or_else(|| id.to_string()),
        CardMode::Settings => form_name.unwrap_or_default(),
    };
    // The type tag on the Settings side, unless the title already says it
    // ("Web Server [Web Server]"); the title's hover names the type on both
    // sides.
    let tag = form
        .as_ref()
        .filter(|_| mode == CardMode::Settings)
        .map(|f| f.type_label().to_string())
        .filter(|t| t.to_lowercase() != title.trim().to_lowercase());
    let hover = form.as_ref().map(|f| f.type_tooltip().to_string());

    let card = fixed_card(ui, id, accent, |card| {
        let mut action = None;
        let ui = card.top();
        let tag = tag.as_deref().map(|t| (t, settings::TagPlace::Buttons));
        settings::card_header(ui, id, &title, tag, hover.as_deref(), |ui| {
            if dirty {
                action = match settings::save_cancel(ui, id, view.can_save, view.saving) {
                    Some(Pending::Save) => Some(CardAction::Save),
                    Some(Pending::Cancel) => Some(CardAction::Cancel),
                    None => None,
                };
                return;
            }
            if form.is_some() {
                let selected = CardMode::ALL.iter().position(|m| *m == mode).unwrap_or(0);
                let labels = CardMode::ALL.map(CardMode::label);
                let toggle = ui
                    .add_enabled_ui(view.editable, |ui| {
                        theme::segmented_sized(ui, &labels, selected, None, 11.0)
                    })
                    .inner;
                mark(ui, || format!("toggle:{id}"), &toggle.response);
                if let Some(i) = toggle.inner {
                    action = Some(CardAction::Mode(CardMode::ALL[i]));
                }
            }
            if mode == CardMode::Telemetry
                && let Some(actor) = view.telemetry
            {
                let badge = theme::badge(ui, actor.state().label(), accent);
                mark(ui, || format!("badge:{id}"), &badge);
            }
        });
        if dirty {
            settings::lock_hint(ui);
        }
        if let Some(error) = view.error {
            settings::save_error_line(ui, id, error);
        }
        match (mode, form) {
            (CardMode::Settings, Some(entry)) => {
                let removable = cards::removable(entry_type(id));
                let refresh = card.body(removable, |ui| {
                    ui.add_space(6.0);
                    ui.add_enabled_ui(view.editable, |ui| {
                        settings::actor_fields(ui, entry, view.monitors, view.cameras)
                    })
                    .inner
                });
                if refresh {
                    action = Some(CardAction::RefreshCameras);
                }
                if removable && card.actions(|ui| settings::remove_button(ui, id, view.editable)) {
                    action = Some(CardAction::Remove);
                }
            }
            _ => {
                let reconnect = cards::reconnectable(entry_type(id));
                let find_mat = entry_type(id) == "exputt";
                let connected = view
                    .telemetry
                    .is_some_and(|a| a.status == ActorStatus::Connected);
                card.body(reconnect || find_mat || still.is_some(), |ui| {
                    // With nothing to show, the actor's state instead.
                    if let Some(actor) = view.telemetry {
                        if monitoring::has_telemetry(actor)
                            || still.as_deref().is_some_and(|s| s.shown())
                        {
                            monitoring::telemetry_body(ui, id, actor);
                        } else {
                            monitoring::no_telemetry(ui, id, actor, accent);
                        }
                    }
                    if let Some(still) = still.as_deref_mut() {
                        crate::panels::replay::still_picture(ui, id, still);
                    }
                });
                if reconnect || find_mat || still.is_some() {
                    card.actions(|ui| {
                        if reconnect
                            && settings::action_button(
                                ui,
                                "Reconnect",
                                &format!("reconnect:{id}"),
                                view.editable && !view.reconnecting,
                            )
                        {
                            action = Some(CardAction::Reconnect);
                        }
                        if find_mat
                            && settings::action_button(
                                ui,
                                "Find mat",
                                &format!("find_mat:{id}"),
                                connected,
                            )
                        {
                            action = Some(CardAction::FindMat);
                        }
                        if let Some(still) = still
                            && crate::panels::replay::still_button(ui, id, still, connected)
                        {
                            action = Some(CardAction::Still);
                        }
                    });
                }
            }
        }
        action
    });
    mark(ui, || format!("card:{id}"), &card.response);
    card.inner
}

/// The actor type of card `id`.
fn entry_type(id: &str) -> &str {
    cards::split_actor_id(id).0
}

impl FlighthookApp {
    pub(crate) fn render_devices_panel(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        // Settings load with the tab, and again after a save that reloads them.
        if self.settings.fetch_due(ctx.input(|i| i.time)) {
            net::fetch_settings(ctx, &self.pending);
        }

        if self.camera_list_due() {
            self.refresh_cameras(ctx);
        }

        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                let locked = self.settings.locked();
                let loaded = self.settings.loaded;
                let saving = self.settings.saving();

                // Launch monitors for the routing dropdowns
                let monitors: Vec<(String, String)> = self
                    .settings
                    .actors
                    .iter()
                    .filter_map(|a| match a {
                        ActorFormEntry::Device(d) if !d.is_camera() => {
                            Some((a.global_id(), d.name.clone()))
                        }
                        ActorFormEntry::Device(_) | ActorFormEntry::Integration(_) => None,
                    })
                    .collect();

                let mut clicked: Option<(String, Option<usize>, CardAction)> = None;
                let mut added = None;
                for (group, members) in device_card_order(&self.actors, &self.settings.actors) {
                    // General leads with the Global, Simulation and Replay
                    // cards, once the settings have loaded, and has no (+).
                    let general = if group == CardGroup::General {
                        if loaded { 3 } else { 0 }
                    } else {
                        0
                    };
                    // Without camera support on the server, Cameras has no
                    // (+), and shows only for cameras already configured.
                    let fixed = group == CardGroup::General
                        || (group == CardGroup::Cameras && !self.cameras_supported);
                    if fixed {
                        if general + members.len() == 0 {
                            continue;
                        }
                        cards::group_heading(ui, group);
                    } else if let Some(preset) = cards::add_heading(ui, group, loaded && !locked) {
                        added = Some(preset);
                    }
                    let general_editable = (!locked || self.settings.global_dirty) && !saving;
                    cards::card_grid(ui, general + members.len(), |ui, i| {
                        if i < general {
                            match i {
                                0 => self.render_global_card(ctx, ui, general_editable),
                                1 => self.render_simulation_card(ctx, ui, general_editable),
                                _ => self.render_replay_card(ctx, ui, general_editable),
                            }
                            return;
                        }
                        let card = &members[i - general];
                        if let Some(action) =
                            self.render_device_card(ui, card, locked, saving, &monitors)
                        {
                            clicked = Some((card.id.clone(), card.form, action));
                        }
                    });
                }

                if let Some((id, form, action)) = clicked {
                    self.card_action(ctx, &id, form, action);
                }
                if let Some(preset) = added {
                    self.add_device(preset);
                }

                ui.add_space(12.0);
                if self.settings.saving() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                    });
                }
            });
    }

    /// Device card `card`, with the tab `locked` and a save out while
    /// `saving`. Returns what was clicked.
    fn render_device_card(
        &mut self,
        ui: &mut egui::Ui,
        card: &DeviceCard,
        locked: bool,
        saving: bool,
        monitors: &[(String, String)],
    ) -> Option<CardAction> {
        let error = self
            .settings
            .save_error
            .as_ref()
            .filter(|e| e.target.scope() == Some(card.id.as_str()))
            .map(|e| e.message.clone());
        let form = card.form.and_then(|idx| self.settings.actors.get_mut(idx));
        let dirty = form.as_ref().is_some_and(|f| f.dirty());
        let still = (entry_type(&card.id) == "camera")
            .then(|| self.stills.entry(card.id.clone()).or_default());
        // A failed reconnect request shows as the card's `error`, like an
        // error the actor reports.
        let failed = self.reconnect_errors.get(&card.id).map(|e| {
            let mut shown = self
                .actors
                .get(&card.id)
                .cloned()
                .unwrap_or_else(|| crate::app::new_actor(String::new()));
            shown.telemetry.insert("error".into(), e.clone());
            shown
        });
        let view = CardView {
            telemetry: failed.as_ref().or_else(|| self.actors.get(&card.id)),
            now_ms: self.now_ms,
            mode: self.card_modes.get(&card.id),
            editable: (!locked || dirty) && !saving,
            can_save: form.as_ref().is_some_and(|f| settings::actor_valid(f)),
            saving,
            error: error.as_deref(),
            reconnecting: self.reconnecting.contains(&card.id),
            monitors,
            cameras: &self.camera_list,
        };
        device_card(ui, &card.id, &view, form, still)
    }

    /// Carry out a click on card `id`, whose form entry is at `form`.
    fn card_action(
        &mut self,
        ctx: &egui::Context,
        id: &str,
        form: Option<usize>,
        action: CardAction,
    ) {
        match (action, form) {
            (CardAction::Mode(mode), _) => self.card_modes.set(id, mode),
            (CardAction::FindMat, _) => net::post_find_mat(ctx, id),
            (CardAction::Reconnect, _) => self.reconnect(ctx, id),
            (CardAction::Still, _) => {
                #[cfg(not(test))]
                net::fetch_still(ctx, &self.pending, id);
            }
            (CardAction::RefreshCameras, _) => self.refresh_cameras(ctx),
            (CardAction::Save, Some(_)) => self.post_save(ctx, SaveTarget::Actor(id.to_string())),
            (CardAction::Cancel, Some(idx)) => self.cancel_device(idx),
            (CardAction::Remove, Some(idx)) if cards::removable(entry_type(id)) => {
                let name = self.settings.actors[idx].name().to_string();
                self.confirm_remove = Some(PendingRemoval {
                    id: id.to_string(),
                    name,
                });
            }
            (CardAction::Remove, Some(_)) | (_, None) => {}
        }
    }

    /// Reconnect actor `id`: one request at a time per card, the card's
    /// Reconnect disabled until the new instance has started or the request
    /// has failed.
    pub(crate) fn reconnect(&mut self, ctx: &egui::Context, id: &str) {
        if self.reconnecting.contains(id) {
            return;
        }
        self.reconnect_done(id);
        self.reconnecting.insert(id.to_owned());
        self.reconnect_errors.remove(id);
        #[cfg(not(test))]
        net::post_reconnect(ctx, &self.pending, id);
        #[cfg(test)]
        let _ = ctx;
    }

    /// Ask the server for its camera list again.
    pub(crate) fn refresh_cameras(&mut self, ctx: &egui::Context) {
        if self.camera_list.loading {
            return;
        }
        self.camera_list.loading = true;
        #[cfg(not(test))]
        net::fetch_cameras(ctx, &self.pending);
        #[cfg(test)]
        let _ = ctx;
    }

    /// Fetch the camera list the first time a camera card shows its
    /// settings.
    fn camera_list_due(&self) -> bool {
        self.camera_list.list.is_none()
            && self.camera_list.error.is_none()
            && !self.camera_list.loading
            && self.settings.actors.iter().any(|a| {
                matches!(a, ActorFormEntry::Device(d) if d.is_camera())
                    && (a.dirty() || self.card_modes.get(&a.global_id()) == CardMode::Settings)
            })
    }

    /// Add a new, unsaved card from (+) menu `preset`, showing its settings.
    pub(crate) fn add_device(&mut self, preset: &cards::AddPreset) {
        if let Some(id) = self.settings.add_preset(preset) {
            self.card_modes.set(&id, CardMode::Settings);
        }
    }

    /// Cancel the edits on the card whose form entry is at `idx`: back to
    /// the saved settings, or gone if it was never saved.
    pub(crate) fn cancel_device(&mut self, idx: usize) {
        let Some(id) = self.settings.actors.get(idx).map(ActorFormEntry::global_id) else {
            return;
        };
        if self.settings.cancel_actor(idx) {
            self.card_modes.forget(&id);
        }
    }
}

/// The controls the tests click on, by name: their rect and whether they
/// were enabled, from the last pass.
#[cfg(test)]
pub(crate) type Marks = HashMap<String, (egui::Rect, bool)>;

/// Record a control under `name` for the tests.
#[cfg(test)]
pub(crate) fn mark(ui: &egui::Ui, name: impl FnOnce() -> String, response: &egui::Response) {
    let entry = (response.rect, response.enabled());
    ui.ctx().data_mut(|d| {
        d.get_temp_mut_or_default::<Marks>(egui::Id::NULL)
            .insert(name(), entry);
    });
}

/// Record a control for the tests: nothing outside them.
#[cfg(not(test))]
#[inline]
pub(crate) fn mark(_ui: &egui::Ui, _name: impl FnOnce() -> String, _response: &egui::Response) {}

/// A card body's scroll state, for the tests: its rect, content height and
/// scroll offset, by card ID.
#[cfg(test)]
pub(crate) type Bodies = HashMap<String, (egui::Rect, f32, f32)>;

/// Where [`Bodies`] are kept for the tests.
#[cfg(test)]
fn bodies_id() -> egui::Id {
    egui::Id::new("devices_bodies")
}

/// Record card `key`'s body for the tests.
#[cfg(test)]
fn mark_body<R>(ui: &egui::Ui, key: &str, shown: &egui::scroll_area::ScrollAreaOutput<R>) {
    let entry = (shown.inner_rect, shown.content_size.y, shown.state.offset.y);
    ui.ctx().data_mut(|d| {
        d.get_temp_mut_or_default::<Bodies>(bodies_id())
            .insert(key.to_owned(), entry);
    });
}

/// Record a card body for the tests: nothing outside them.
#[cfg(not(test))]
#[inline]
fn mark_body<R>(_ui: &egui::Ui, _key: &str, _shown: &egui::scroll_area::ScrollAreaOutput<R>) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::settings::{GeneralCard, SettingsForm};
    use crate::panels::{TABS, Tab, next_alert_change, tab_alert};
    use crate::types::{
        ActorStatus, ConnectionState, FlighthookConfig, RECONNECT_ALERT_MS, UnitSystem,
    };
    use egui::{Color32, Event, Key, Modifiers, PointerButton, Pos2, Rect, pos2, vec2};

    /// The Outputs (+) menu entry labelled `label`.
    fn output_preset(label: &str) -> &'static cards::AddPreset {
        cards::add_options(CardGroup::Outputs)
            .iter()
            .find(|p| p.label == label)
            .unwrap_or_else(|| panic!("no {label} preset"))
    }

    /// A saved config with a Mevo and two Open Connect clients: GSPro
    /// (`openconnect_client.0`) and Muni Golf (`openconnect_client.1`).
    fn config() -> FlighthookConfig {
        let mut form = SettingsForm::default();
        form.load_from(&FlighthookConfig::default());
        let _ = form.add_actor("mevo");
        let _ = form.add_preset(output_preset("GSPro (OpenConnect)"));
        let _ = form.add_preset(output_preset("Muni Golf (OpenConnect)"));
        form.to_request()
    }

    /// An app on the Devices tab with `config()` loaded and its actors
    /// running.
    fn app() -> FlighthookApp {
        let mut app = FlighthookApp::with_pending(net::new_pending());
        app.settings.load_from(&config());
        for a in &app.settings.actors {
            let mut status = ActorStatusResponse::new(a.name(), ActorStatus::Connected);
            status.telemetry = HashMap::from([("ready".to_string(), "true".to_string())]);
            app.actors.insert(a.global_id(), status);
        }
        app
    }

    fn form_index(app: &FlighthookApp, id: &str) -> usize {
        app.settings
            .actors
            .iter()
            .position(|a| a.global_id() == id)
            .unwrap()
    }

    /// Edit the name of actor `id`, as its name field does.
    fn edit_name(app: &mut FlighthookApp, id: &str) {
        let idx = form_index(app, id);
        match &mut app.settings.actors[idx] {
            ActorFormEntry::Device(d) => d.name.push_str(" edited"),
            ActorFormEntry::Integration(i) => i.name.push_str(" edited"),
        }
        app.settings.actors[idx].set_dirty(true);
    }

    /// Draws the tab bar and the Devices tab, 1600×1600 unless sized: tall
    /// enough for every card of the tests' configs to be on screen.
    struct Screen(egui::Context, egui::Vec2);

    impl Screen {
        fn new() -> Self {
            Self::sized(1600.0, 1600.0)
        }

        fn sized(width: f32, height: f32) -> Self {
            let ctx = egui::Context::default();
            theme::install(&ctx);
            Self(ctx, vec2(width, height))
        }

        /// One pass with `events`. Returns the marked controls.
        fn frame(&self, app: &mut FlighthookApp, events: Vec<Event>) -> Marks {
            self.painted(app, events).0
        }

        /// One pass with `events`. Returns the marked controls and every
        /// shape painted.
        fn painted(
            &self,
            app: &mut FlighthookApp,
            events: Vec<Event>,
        ) -> (Marks, Vec<egui::Shape>) {
            self.0.data_mut(|d| d.remove_temp::<Marks>(egui::Id::NULL));
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.1)),
                events,
                ..Default::default()
            };
            let output = self.0.run(input, |ctx| {
                egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
                    let _ = app.render_tabs(ui);
                });
                egui::CentralPanel::default().show(ctx, |ui| {
                    let ctx = ui.ctx().clone();
                    app.render_devices_panel(&ctx, ui);
                });
                if app.confirm_remove.is_some() {
                    app.render_remove_confirm(ctx);
                }
            });
            let marks = self
                .0
                .data(|d| d.get_temp::<Marks>(egui::Id::NULL))
                .unwrap_or_default();
            let mut shapes = Vec::new();
            for clipped in output.shapes {
                flatten(clipped.shape, &mut shapes);
            }
            (marks, shapes)
        }

        /// Click at `at`, then draw once more. Returns the marked controls.
        fn click(&self, app: &mut FlighthookApp, at: Pos2) -> Marks {
            let button = |pressed| Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            };
            self.frame(app, vec![Event::PointerMoved(at)]);
            self.frame(app, vec![button(true)]);
            self.frame(app, vec![button(false)]);
            self.frame(app, Vec::new())
        }

        /// The card bodies of the last pass.
        fn bodies(&self) -> Bodies {
            self.0
                .data(|d| d.get_temp::<Bodies>(bodies_id()))
                .unwrap_or_default()
        }

        /// Click the middle of control `name`.
        fn click_mark(&self, app: &mut FlighthookApp, marks: &Marks, name: &str) -> Marks {
            let (rect, _) = marks[name];
            self.click(app, rect.center())
        }
    }

    /// The left (Telemetry) or right (Settings) half of a card's toggle.
    fn toggle_side(marks: &Marks, id: &str, mode: CardMode) -> Pos2 {
        let (rect, _) = marks[&format!("toggle:{id}")];
        let x = match mode {
            CardMode::Telemetry => rect.left() + rect.width() * 0.25,
            CardMode::Settings => rect.left() + rect.width() * 0.75,
        };
        pos2(x, rect.center().y)
    }

    fn enabled(marks: &Marks, name: &str) -> bool {
        marks
            .get(name)
            .unwrap_or_else(|| panic!("{name} not drawn"))
            .1
    }

    #[test]
    fn tabs_are_devices_shots_log_and_the_app_opens_on_devices() {
        assert_eq!(TABS.map(|(_, label)| label), ["Devices", "Shots", "Log"]);
        assert_eq!(
            TABS.map(|(tab, _)| tab),
            [Tab::Devices, Tab::Shots, Tab::Log]
        );
        let app = FlighthookApp::with_pending(net::new_pending());
        assert_eq!(app.active_tab, Tab::Devices);
    }

    /// `shape` and the shapes inside it, into `out`.
    fn flatten(shape: egui::Shape, out: &mut Vec<egui::Shape>) {
        match shape {
            egui::Shape::Vec(shapes) => {
                for s in shapes {
                    flatten(s, out);
                }
            }
            other => out.push(other),
        }
    }

    /// Actor `id`, Connected since it started, then `status` at `at_ms`.
    fn lose(app: &mut FlighthookApp, id: &str, status: ActorStatus, at_ms: u64) {
        let actor = app.actors.get_mut(id).unwrap();
        actor.observe(ActorStatus::Connected, at_ms);
        actor.observe(status, at_ms);
    }

    #[test]
    fn the_alert_dot_shows_only_while_a_card_is_red() {
        let mut app = app();
        app.actors.insert(
            "openconnect_server.0".into(),
            ActorStatusResponse::new("Uneekor", ActorStatus::Connected),
        );
        assert_eq!(tab_alert(&app.actors, 0), None);

        // Not Connected since it started: Connecting, no dot.
        for id in ["mevo.0", "openconnect_client.0", "openconnect_server.0"] {
            for status in [
                ActorStatus::Disconnected,
                ActorStatus::Reconnecting,
                ActorStatus::Starting,
            ] {
                let actor = app.actors.get_mut(id).unwrap();
                actor.observe(ActorStatus::Starting, 0);
                actor.observe(status, 1);
                assert_eq!(actor.state(), ConnectionState::Connecting);
                assert_eq!(tab_alert(&app.actors, 1), None, "{id} {status:?}");
            }
            app.actors
                .get_mut(id)
                .unwrap()
                .observe(ActorStatus::Connected, 2);
        }
        // An `error` alone does not light it.
        app.actors
            .get_mut("openconnect_client.0")
            .unwrap()
            .telemetry
            .insert("error".into(), "OpenConnect Server: refused ...".into());
        assert_eq!(tab_alert(&app.actors, 2), None);

        // A lost connection lights it for ten minutes, whatever the actor:
        // a launch monitor, an output, a server whose peer left.
        for (id, status) in [
            ("mevo.0", ActorStatus::Reconnecting),
            ("openconnect_client.1", ActorStatus::Reconnecting),
            ("openconnect_server.0", ActorStatus::Disconnected),
        ] {
            let at = 1_000;
            lose(&mut app, id, status, at);
            assert_eq!(tab_alert(&app.actors, at), Some(0), "{id}");
            assert_eq!(TABS[0].0, Tab::Devices);
            assert_eq!(
                tab_alert(&app.actors, at + RECONNECT_ALERT_MS - 1),
                Some(0),
                "{id}"
            );
            assert_eq!(
                tab_alert(&app.actors, at + RECONNECT_ALERT_MS),
                None,
                "{id}"
            );
            assert_eq!(
                app.actors[id].state(),
                ConnectionState::Reconnecting,
                "{id}"
            );
            app.actors
                .get_mut(id)
                .unwrap()
                .observe(ActorStatus::Connected, at + 1);
            assert_eq!(tab_alert(&app.actors, at + 1), None, "{id}");
        }
    }

    #[test]
    fn the_next_alert_change_is_when_the_first_red_card_turns_yellow() {
        let mut app = app();
        assert_eq!(next_alert_change(&app.actors, 0), None);
        lose(&mut app, "mevo.0", ActorStatus::Reconnecting, 0);
        lose(
            &mut app,
            "openconnect_client.0",
            ActorStatus::Reconnecting,
            60_000,
        );
        assert_eq!(
            next_alert_change(&app.actors, 1_000),
            Some(RECONNECT_ALERT_MS - 1_000)
        );
        assert_eq!(
            next_alert_change(&app.actors, RECONNECT_ALERT_MS),
            Some(60_000)
        );
        assert_eq!(
            next_alert_change(&app.actors, RECONNECT_ALERT_MS + 60_000),
            None
        );
    }

    /// The text and colour of card `id`'s badge, and the colour of the card
    /// stripe left of it.
    fn badge_look(marks: &Marks, shapes: &[egui::Shape], id: &str) -> (String, Color32, Color32) {
        let (badge, _) = marks[&format!("badge:{id}")];
        let (text, color) = shapes
            .iter()
            .find_map(|s| match s {
                egui::Shape::Text(t) if badge.contains_rect(t.visual_bounding_rect()) => {
                    Some((t.galley.text().to_string(), t.fallback_color))
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("no badge text for {id}"));
        let stripe = shapes
            .iter()
            .filter_map(|s| match s {
                egui::Shape::Rect(r)
                    if (r.rect.width() - 3.0).abs() < 0.1
                        && r.rect.y_range().contains(badge.center().y)
                        && r.rect.right() < badge.left() =>
                {
                    Some((r.rect.left(), r.fill))
                }
                _ => None,
            })
            .max_by(|a, b| a.0.total_cmp(&b.0))
            .unwrap_or_else(|| panic!("no stripe for {id}"))
            .1;
        (text, color, stripe)
    }

    #[test]
    fn a_cards_badge_and_stripe_show_its_state() {
        let mut app = app();
        let lost_at = 2 * 60 * 1000;
        app.actors
            .get_mut("openconnect_client.0")
            .unwrap()
            .observe(ActorStatus::Starting, 0);
        lose(
            &mut app,
            "openconnect_client.1",
            ActorStatus::Reconnecting,
            lost_at,
        );
        let screen = Screen::new();
        let look = |app: &mut FlighthookApp, now: u64, id: &str| {
            app.now_ms = now;
            let (marks, shapes) = screen.painted(app, Vec::new());
            badge_look(&marks, &shapes, id)
        };
        let now = lost_at + 1_000;
        for (id, label, color) in [
            ("mevo.0", "CONNECTED", theme::STATUS_OK),
            ("openconnect_client.0", "CONNECTING", theme::STATUS_PENDING),
            ("openconnect_client.1", "RECONNECTING", theme::STATUS_DOWN),
        ] {
            assert_eq!(
                look(&mut app, now, id),
                (label.into(), color, color),
                "{id}"
            );
        }
        // Yellow once the alert is over.
        let later = lost_at + RECONNECT_ALERT_MS;
        assert_eq!(
            look(&mut app, later, "openconnect_client.1"),
            (
                "RECONNECTING".into(),
                theme::STATUS_PENDING,
                theme::STATUS_PENDING
            )
        );
    }

    #[test]
    fn device_cards_cover_configured_and_running_actors_in_card_order() {
        let mut app = app();
        // Running, not in the form (a config edited by hand since the load).
        app.actors.insert(
            "mock_monitor.0".into(),
            ActorStatusResponse::new("Mock", ActorStatus::Connected),
        );
        // In the form, not running yet.
        app.actors.remove("openconnect_client.1");
        let order = device_card_order(&app.actors, &app.settings.actors);
        let ids = |group: CardGroup| -> Vec<(String, bool)> {
            order
                .iter()
                .find(|(g, _)| *g == group)
                .unwrap()
                .1
                .iter()
                .map(|c| (c.id.clone(), c.form.is_some()))
                .collect()
        };
        assert_eq!(ids(CardGroup::General), [("webserver.0".into(), true)]);
        assert_eq!(
            ids(CardGroup::LaunchMonitors),
            [("mevo.0".into(), true), ("mock_monitor.0".into(), false)]
        );
        assert_eq!(
            ids(CardGroup::Outputs),
            [
                ("openconnect_client.0".into(), true),
                ("openconnect_client.1".into(), true),
            ]
        );
        for (_, cards) in &order {
            for card in cards {
                if let Some(idx) = card.form {
                    assert_eq!(app.settings.actors[idx].global_id(), card.id);
                }
            }
        }
    }

    #[test]
    fn each_card_flips_on_its_own_toggle() {
        let screen = Screen::new();
        let mut app = app();
        let marks = screen.frame(&mut app, Vec::new());
        for id in [
            "mevo.0",
            "openconnect_client.0",
            "openconnect_client.1",
            "webserver.0",
        ] {
            assert_eq!(app.card_modes.get(id), CardMode::Telemetry, "{id}");
            assert!(enabled(&marks, &format!("toggle:{id}")), "{id}");
            assert!(!marks.contains_key(&format!("remove:{id}")), "{id}");
        }

        let marks = screen.click(
            &mut app,
            toggle_side(&marks, "openconnect_client.0", CardMode::Settings),
        );
        assert_eq!(
            app.card_modes.get("openconnect_client.0"),
            CardMode::Settings
        );
        assert_eq!(
            app.card_modes.get("openconnect_client.1"),
            CardMode::Telemetry
        );
        assert_eq!(app.card_modes.get("mevo.0"), CardMode::Telemetry);
        // The settings side ends in Remove; nothing is dirty.
        assert!(enabled(&marks, "remove:openconnect_client.0"));
        assert!(!marks.contains_key("remove:openconnect_client.1"));
        assert!(!app.settings.locked());

        let marks = screen.click(
            &mut app,
            toggle_side(&marks, "openconnect_client.0", CardMode::Telemetry),
        );
        assert_eq!(
            app.card_modes.get("openconnect_client.0"),
            CardMode::Telemetry
        );
        assert!(!marks.contains_key("remove:openconnect_client.0"));
    }

    #[test]
    fn unsaved_edits_swap_the_toggle_for_save_and_cancel() {
        let screen = Screen::new();
        let mut app = app();
        app.card_modes
            .set("openconnect_client.0", CardMode::Settings);
        let marks = screen.frame(&mut app, Vec::new());
        assert!(marks.contains_key("toggle:openconnect_client.0"));
        assert!(!marks.contains_key("save:openconnect_client.0"));

        edit_name(&mut app, "openconnect_client.0");
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!marks.contains_key("toggle:openconnect_client.0"));
        assert!(enabled(&marks, "save:openconnect_client.0"));
        assert!(enabled(&marks, "cancel:openconnect_client.0"));
        assert!(enabled(&marks, "remove:openconnect_client.0"));

        // An invalid form can't be saved, only cancelled.
        let idx = form_index(&app, "openconnect_client.0");
        if let ActorFormEntry::Integration(i) = &mut app.settings.actors[idx] {
            i.address = "127.0.0.1:".into();
        }
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!enabled(&marks, "save:openconnect_client.0"));
        assert!(enabled(&marks, "cancel:openconnect_client.0"));

        // While a save is out, neither.
        if let ActorFormEntry::Integration(i) = &mut app.settings.actors[idx] {
            i.address = "127.0.0.1:921".into();
        }
        let _ = app
            .settings
            .begin_save(SaveTarget::Actor("openconnect_client.0".into()));
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!enabled(&marks, "save:openconnect_client.0"));
        assert!(!enabled(&marks, "cancel:openconnect_client.0"));
        assert!(!enabled(&marks, "remove:openconnect_client.0"));
    }

    #[test]
    fn cancel_puts_the_card_back_to_its_saved_settings() {
        let screen = Screen::new();
        let mut app = app();
        app.card_modes
            .set("openconnect_client.1", CardMode::Settings);
        let saved = app.settings.actors[form_index(&app, "openconnect_client.1")]
            .name()
            .to_string();
        edit_name(&mut app, "openconnect_client.1");
        let marks = screen.frame(&mut app, Vec::new());
        assert!(app.settings.dirty());

        let marks = screen.click_mark(&mut app, &marks, "cancel:openconnect_client.1");
        let entry = &app.settings.actors[form_index(&app, "openconnect_client.1")];
        assert_eq!(entry.name(), saved);
        assert!(!entry.dirty());
        assert!(!app.settings.dirty());
        assert!(!app.settings.locked());
        // Back to the toggle, still on the settings side.
        assert!(enabled(&marks, "toggle:openconnect_client.1"));
        assert_eq!(
            app.card_modes.get("openconnect_client.1"),
            CardMode::Settings
        );
        assert!(enabled(&marks, "tabs"));
    }

    #[test]
    fn unsaved_edits_lock_the_rest_of_the_tab() {
        let screen = Screen::new();
        let mut app = app();

        // Unlocked, a click on the middle of the tab bar switches tab.
        let marks = screen.frame(&mut app, Vec::new());
        let tabs = marks["tabs"].0.center();
        screen.click(&mut app, tabs);
        assert_eq!(app.active_tab, Tab::Shots);
        app.active_tab = Tab::Devices;

        app.card_modes
            .set("openconnect_client.0", CardMode::Settings);
        app.card_modes.set("mevo.0", CardMode::Settings);
        edit_name(&mut app, "openconnect_client.0");
        let marks = screen.frame(&mut app, Vec::new());
        assert!(app.settings.locked());
        assert!(!enabled(&marks, "tabs"));
        for id in ["mevo.0", "openconnect_client.1", "webserver.0"] {
            assert!(!enabled(&marks, &format!("toggle:{id}")), "{id}");
        }
        assert!(!enabled(&marks, "remove:mevo.0"));
        assert!(!enabled(&marks, "add:Launch monitors"));
        assert!(!enabled(&marks, "add:Outputs"));
        assert!(enabled(&marks, "save:openconnect_client.0"));
        assert!(enabled(&marks, "cancel:openconnect_client.0"));

        // Clicks on the tab bar, another card's toggle or a (+) do nothing.
        screen.click(&mut app, tabs);
        assert_eq!(app.active_tab, Tab::Devices);
        screen.click(
            &mut app,
            toggle_side(&marks, "openconnect_client.1", CardMode::Settings),
        );
        assert_eq!(
            app.card_modes.get("openconnect_client.1"),
            CardMode::Telemetry
        );
        let marks = screen.click_mark(&mut app, &marks, "add:Outputs");
        assert!(
            !marks.contains_key("add:Outputs:GSPro (OpenConnect)"),
            "no menu"
        );

        // Escape doesn't discard.
        let escape = Event::Key {
            key: Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        screen.frame(&mut app, vec![escape]);
        assert!(app.settings.actors[form_index(&app, "openconnect_client.0")].dirty());
        assert!(app.settings.locked());
    }

    #[test]
    fn general_cards_show_save_and_cancel_where_edited_and_cancel_together() {
        let screen = Screen::new();
        let mut app = app();
        let marks = screen.frame(&mut app, Vec::new());
        for card in ["global", "simulation"] {
            assert!(!marks.contains_key(&format!("save:{card}")));
        }

        // A Global edit: the pair on Global only, the rest locked.
        app.set_default_units(UnitSystem::Metric);
        assert_eq!(app.units_toggle, UnitSystem::Metric);
        let marks = screen.frame(&mut app, Vec::new());
        assert!(enabled(&marks, "save:global"));
        assert!(enabled(&marks, "cancel:global"));
        assert!(!marks.contains_key("save:simulation"));
        assert!(!enabled(&marks, "tabs"));
        assert!(!enabled(&marks, "toggle:mevo.0"));
        assert!(!enabled(&marks, "add:Outputs"));

        // A Simulation edit too: the pair on both.
        app.settings.simulation.altitude_val = "100".into();
        app.settings.mark_general(GeneralCard::Simulation);
        let marks = screen.frame(&mut app, Vec::new());
        assert!(enabled(&marks, "save:global"));
        assert!(enabled(&marks, "save:simulation"));

        // Cancel on either card reverts both, and the Shots units with them.
        screen.click_mark(&mut app, &marks, "cancel:simulation");
        assert!(!app.settings.global_dirty);
        assert!(!app.settings.locked());
        assert_eq!(app.settings.default_units, UnitSystem::Imperial);
        assert!(app.settings.simulation.fahrenheit);
        assert_eq!(app.settings.simulation.altitude_val, "0");
        assert_eq!(app.units_toggle, UnitSystem::Imperial);
    }

    #[test]
    fn cancel_general_keeps_the_units_the_shots_tab_had_before_the_edit() {
        let screen = Screen::new();
        let mut app = app();
        // Saved Imperial; the Shots tab toggled to Metric for the session.
        app.units_toggle = UnitSystem::Metric;
        app.set_default_units(UnitSystem::Metric);
        app.set_default_units(UnitSystem::Imperial);
        let marks = screen.frame(&mut app, Vec::new());
        screen.click_mark(&mut app, &marks, "cancel:global");
        assert_eq!(app.settings.default_units, UnitSystem::Imperial);
        assert_eq!(app.units_toggle, UnitSystem::Metric);
        assert_eq!(app.units_before_edit, None);
    }

    #[test]
    fn a_new_card_opens_dirty_in_settings_and_cancel_removes_it() {
        let screen = Screen::new();
        let mut app = app();
        let before = app.settings.actors.len();
        let marks = screen.frame(&mut app, Vec::new());
        assert!(enabled(&marks, "add:Outputs"));

        let marks = screen.click_mark(&mut app, &marks, "add:Outputs");
        let mut offered: Vec<&str> = marks
            .keys()
            .filter_map(|k| k.strip_prefix("add:Outputs:"))
            .collect();
        offered.sort_unstable();
        assert_eq!(
            offered,
            [
                "GSPro (OpenConnect)",
                "Muni Golf (OpenConnect)",
                "OpenConnect Client",
            ],
            "the menu offers exactly these"
        );
        let marks = screen.click_mark(&mut app, &marks, "add:Outputs:Muni Golf (OpenConnect)");
        assert_eq!(app.settings.actors.len(), before + 1);
        let new = &app.settings.actors[before];
        assert_eq!(new.global_id(), "openconnect_client.2");
        assert_eq!(new.name(), "Muni Golf");
        assert_eq!(new.type_label(), "OpenConnect Client");
        let ActorFormEntry::Integration(entry) = new else {
            panic!("an Open Connect client is an integration entry");
        };
        assert_eq!(entry.address, "127.0.0.1:9210");
        assert!(new.dirty());
        assert_eq!(
            app.card_modes.get("openconnect_client.2"),
            CardMode::Settings
        );
        assert!(
            !marks.contains_key("add:Outputs:Muni Golf (OpenConnect)"),
            "the menu closes"
        );
        assert!(enabled(&marks, "save:openconnect_client.2"));
        assert!(enabled(&marks, "cancel:openconnect_client.2"));
        assert!(!enabled(&marks, "tabs"));
        assert!(!enabled(&marks, "add:Launch monitors"));

        let marks = screen.click_mark(&mut app, &marks, "cancel:openconnect_client.2");
        assert_eq!(app.settings.actors.len(), before);
        assert!(
            app.settings
                .actors
                .iter()
                .all(|a| a.global_id() != "openconnect_client.2")
        );
        assert_eq!(
            app.card_modes.get("openconnect_client.2"),
            CardMode::Telemetry
        );
        assert!(!marks.keys().any(|k| k.ends_with("openconnect_client.2")));
        assert!(!app.settings.locked());
        assert!(enabled(&marks, "tabs"));
    }

    fn device<'a>(app: &'a mut FlighthookApp, id: &str) -> &'a mut settings::DeviceFormEntry {
        let idx = form_index(app, id);
        match &mut app.settings.actors[idx] {
            ActorFormEntry::Device(d) => d,
            ActorFormEntry::Integration(_) => panic!("{id} is an integration"),
        }
    }

    #[test]
    fn the_generic_openconnect_server_edits_its_bind_address() {
        let screen = Screen::new();
        let mut app = app();
        let marks = screen.frame(&mut app, Vec::new());
        let marks = screen.click_mark(&mut app, &marks, "add:Launch monitors");
        for label in ["Uneekor (OpenConnect)", "OpenConnect Server"] {
            assert!(
                marks.contains_key(&format!("add:Launch monitors:{label}")),
                "the menu offers {label}"
            );
        }
        let marks = screen.click_mark(&mut app, &marks, "add:Launch monitors:OpenConnect Server");
        let id = "openconnect_server.0";
        let entry = &app.settings.actors[form_index(&app, id)];
        assert_eq!(entry.name(), "OpenConnect Server");
        assert_eq!(entry.type_label(), "OpenConnect Server");
        assert_eq!(device(&mut app, id).address, "0.0.0.0:921");
        assert!(enabled(&marks, &format!("bind:{id}")));
        assert!(enabled(&marks, &format!("save:{id}")));
        let bind = |app: &FlighthookApp| {
            app.settings.build_actor_request(id).openconnect_server["0"]
                .bind
                .clone()
        };

        device(&mut app, id).address = "0.0.0.0:".into();
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!enabled(&marks, &format!("save:{id}")), "invalid bind");

        // Blank listens on the default.
        device(&mut app, id).address = String::new();
        let marks = screen.frame(&mut app, Vec::new());
        assert!(enabled(&marks, &format!("save:{id}")));
        assert_eq!(bind(&app), None);

        device(&mut app, id).address = "127.0.0.1:9921".into();
        let marks = screen.frame(&mut app, Vec::new());
        assert!(enabled(&marks, &format!("save:{id}")));
        assert_eq!(bind(&app).as_deref(), Some("127.0.0.1:9921"));
    }

    #[test]
    fn an_added_exputt_takes_an_optional_ip_address() {
        let screen = Screen::new();
        let mut app = app();
        let marks = screen.frame(&mut app, Vec::new());
        let marks = screen.click_mark(&mut app, &marks, "add:Launch monitors");
        let marks = screen.click_mark(&mut app, &marks, "add:Launch monitors:ExPutt");
        let id = "exputt.0";
        let entry = &app.settings.actors[form_index(&app, id)];
        assert_eq!(entry.name(), "ExPutt");
        assert_eq!(entry.type_label(), "ExPutt");
        assert!(enabled(&marks, &format!("ip:{id}")));
        let field_texts = |app: &mut FlighthookApp| {
            let (marks, shapes) = screen.painted(app, Vec::new());
            let (field, _) = marks[&format!("ip:{id}")];
            shapes
                .iter()
                .filter_map(|s| match s {
                    egui::Shape::Text(t) if field.contains(t.visual_bounding_rect().center()) => {
                        Some(t.galley.text().to_string())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        // An empty field shows the placeholder, a filled one only its text.
        assert_eq!(field_texts(&mut app), [settings::EXPUTT_IP_PLACEHOLDER]);
        assert_eq!(settings::EXPUTT_IP_PLACEHOLDER, "empty = auto-detect");
        device(&mut app, id).address = "10.0.0.7".into();
        assert_eq!(field_texts(&mut app), ["10.0.0.7"]);
        device(&mut app, id).address = String::new();
        // Blank finds the device on the network.
        assert!(enabled(&marks, &format!("save:{id}")));
        let address = |app: &FlighthookApp| {
            app.settings.build_actor_request(id).exputt["0"]
                .address
                .clone()
        };
        assert_eq!(address(&app), None);

        device(&mut app, id).address = "192.168.1.50:5000".into();
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!enabled(&marks, &format!("save:{id}")), "an IP, no port");

        device(&mut app, id).address = " 192.168.1.50 ".into();
        let marks = screen.frame(&mut app, Vec::new());
        assert!(enabled(&marks, &format!("save:{id}")));
        assert_eq!(address(&app).as_deref(), Some("192.168.1.50"));
    }

    /// `app()` plus a saved ExPutt, `exputt.0`, with status `status` (not
    /// running when `None`).
    fn app_with_exputt(status: Option<ActorStatus>) -> FlighthookApp {
        let mut app = app();
        let mut config = app.settings.to_request();
        config.exputt.insert(
            "0".into(),
            flighthook::ExPuttSection {
                name: "ExPutt".into(),
                address: None,
                impact_lead_s: None,
            },
        );
        app.settings.load_from(&config);
        if let Some(status) = status {
            let mut actor = ActorStatusResponse::new("ExPutt", status);
            actor.telemetry = HashMap::from([("ready".to_string(), "true".to_string())]);
            app.actors.insert("exputt.0".into(), actor);
        }
        app
    }

    #[test]
    fn find_mat_is_on_the_exputt_telemetry_side_and_needs_a_connection() {
        let screen = Screen::new();
        let mut app = app_with_exputt(Some(ActorStatus::Connected));
        let marks = screen.frame(&mut app, Vec::new());
        assert!(enabled(&marks, "find_mat:exputt.0"));
        let finds: Vec<&String> = marks
            .keys()
            .filter(|k| k.starts_with("find_mat:"))
            .collect();
        assert_eq!(finds, ["find_mat:exputt.0"], "ExPutt cards only");
        // Inside its card, under the telemetry.
        let (card, _) = marks["card:exputt.0"];
        let (button, _) = marks["find_mat:exputt.0"];
        assert!(card.contains_rect(button), "{button:?} in {card:?}");
        let (badge, _) = marks["badge:exputt.0"];
        assert!(button.top() > badge.bottom());

        for status in [
            ActorStatus::Starting,
            ActorStatus::Reconnecting,
            ActorStatus::Disconnected,
        ] {
            let mut app = app_with_exputt(Some(status));
            let marks = screen.frame(&mut app, Vec::new());
            assert!(!enabled(&marks, "find_mat:exputt.0"), "{status:?}");
        }
        let mut app = app_with_exputt(None);
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!enabled(&marks, "find_mat:exputt.0"), "not running");

        // Not on the settings side.
        let mut app = app_with_exputt(Some(ActorStatus::Connected));
        app.card_modes.set("exputt.0", CardMode::Settings);
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!marks.contains_key("find_mat:exputt.0"));
        assert!(enabled(&marks, "remove:exputt.0"));
    }

    #[test]
    fn a_saved_openconnect_server_shows_its_bind_on_the_settings_side() {
        let screen = Screen::new();
        let mut app = app();
        let mut config = app.settings.to_request();
        config.openconnect_server.insert(
            "0".into(),
            flighthook::OpenConnectServerSection {
                name: "Uneekor".into(),
                bind: Some("0.0.0.0:9921".into()),
                impact_lead_s: None,
            },
        );
        app.settings.load_from(&config);
        app.card_modes
            .set("openconnect_server.0", CardMode::Settings);
        let marks = screen.frame(&mut app, Vec::new());
        assert!(enabled(&marks, "bind:openconnect_server.0"));
        assert_eq!(
            device(&mut app, "openconnect_server.0").address,
            "0.0.0.0:9921"
        );
        // Only the server has one.
        assert!(
            !marks
                .keys()
                .any(|k| k.starts_with("bind:") && k != "bind:openconnect_server.0")
        );
    }

    /// The Square Golf Omni telemetry seen live, whose `device_info` is
    /// longer than a card's value column.
    const OMNI_DEVICE_INFO: &str = "Square Golf SGO300A SquareGolf(CD01) (02:00:00:AB:CD:01)";

    /// `app()` plus a Square Golf Omni between the Mevo and an Open Connect
    /// server, reporting [`OMNI_DEVICE_INFO`].
    fn app_with_omni() -> FlighthookApp {
        let mut app = app();
        let mut config = app.settings.to_request();
        config.square.insert(
            "0".into(),
            flighthook::SquareSection {
                name: "Square Golf Omni".into(),
                ..Default::default()
            },
        );
        config
            .openconnect_server
            .insert("0".into(), flighthook::OpenConnectServerSection::default());
        app.settings.load_from(&config);
        for (id, name, telemetry) in [
            (
                "square.0",
                "Square Golf Omni",
                vec![
                    ("device_info", OMNI_DEVICE_INFO),
                    ("ble_address", "02:00:00:AB:CD:01"),
                    ("ready", "true"),
                    ("battery_pct", "85"),
                ],
            ),
            (
                "openconnect_server.0",
                "Uneekor",
                vec![("bind", "0.0.0.0:921")],
            ),
        ] {
            let mut actor = ActorStatusResponse::new(name, ActorStatus::Connected);
            actor.telemetry = telemetry
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            app.actors.insert(id.into(), actor);
        }
        app
    }

    /// Cards `ids`, in card order and one grid row, keep to equal column
    /// widths and stay out of each other's way.
    fn assert_row_fits(marks: &Marks, ids: &[&str], at: &str) {
        let rects: Vec<Rect> = ids
            .iter()
            .map(|id| {
                marks
                    .get(&format!("card:{id}"))
                    .unwrap_or_else(|| panic!("{id} not drawn at {at}"))
                    .0
            })
            .collect();
        let width = rects[0].width();
        for (id, r) in ids.iter().zip(&rects) {
            assert!(
                (r.width() - width).abs() < 0.5,
                "{id} is {} wide, its row {width} at {at}",
                r.width()
            );
            assert!(
                r.width() <= cards::CARD_MAX_WIDTH + 0.5,
                "{id} wider than a column at {at}"
            );
        }
        for pair in rects.windows(2) {
            assert!(
                pair[0].right() + cards::CARD_GAP - 0.5 <= pair[1].left(),
                "cards overlap at {at}: {:?} / {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn a_long_telemetry_value_stays_inside_its_card() {
        for (w, h, rows) in [
            (
                1600.0,
                900.0,
                vec![vec!["mevo.0", "square.0", "openconnect_server.0"]],
            ),
            (
                1024.0,
                768.0,
                vec![vec!["mevo.0", "square.0"], vec!["openconnect_server.0"]],
            ),
        ] {
            let at = format!("{w}x{h}");
            let screen = Screen::sized(w, h);
            let mut app = app_with_omni();
            screen.frame(&mut app, Vec::new());
            let marks = screen.frame(&mut app, Vec::new());
            for row in &rows {
                assert_row_fits(&marks, row, &at);
            }
            // Every card of the group the same width.
            let (mevo, _) = marks["card:mevo.0"];
            let (server, _) = marks["card:openconnect_server.0"];
            assert!((mevo.width() - server.width()).abs() < 0.5, "{at}");

            // The long value is cut short inside the card.
            let (card, _) = marks["card:square.0"];
            let (value, _) = marks["telemetry:square.0:device_info"];
            assert!(
                value.right() <= card.right(),
                "{value:?} past {card:?} at {at}"
            );
            let (ble, _) = marks["telemetry:square.0:ble_address"];
            assert!(ble.right() <= card.right(), "{at}");
        }
    }

    #[test]
    fn a_long_name_stays_inside_its_card_on_both_sides() {
        let long = "Square Golf SGO300A SquareGolf(CD01) in the back bay by the window";
        for (w, h) in [(1600.0, 900.0), (1024.0, 768.0)] {
            let at = format!("{w}x{h}");
            let screen = Screen::sized(w, h);
            let mut app = app_with_omni();
            app.actors.get_mut("square.0").unwrap().name = long.into();
            screen.frame(&mut app, Vec::new());
            let marks = screen.frame(&mut app, Vec::new());
            assert_row_fits(&marks, &["mevo.0", "square.0"], &at);

            // Settings side, the name and BLE address fields holding long text.
            app.card_modes.set("square.0", CardMode::Settings);
            device(&mut app, "square.0").name = long.into();
            device(&mut app, "square.0").address = OMNI_DEVICE_INFO.into();
            device(&mut app, "square.0").dirty = true;
            screen.frame(&mut app, Vec::new());
            let after = screen.frame(&mut app, Vec::new());
            assert_row_fits(&after, &["mevo.0", "square.0"], &at);
            assert!(
                (after["card:square.0"].0.width() - marks["card:square.0"].0.width()).abs() < 0.5,
                "the settings side is as wide as the telemetry side at {at}"
            );
            // Save and Cancel stay in the card.
            let (card, _) = after["card:square.0"];
            for control in ["save:square.0", "cancel:square.0", "remove:square.0"] {
                let (r, _) = after[control];
                assert!(
                    card.contains_rect(r),
                    "{control} {r:?} outside {card:?} at {at}"
                );
            }

            // A client routed to it shows the long name in its dropdown.
            integration(&mut app, "openconnect_client.0").full_monitor = "square.0".into();
            app.card_modes
                .set("openconnect_client.0", CardMode::Settings);
            screen.frame(&mut app, Vec::new());
            let outputs = screen.frame(&mut app, Vec::new());
            assert_row_fits(
                &outputs,
                &["openconnect_client.0", "openconnect_client.1"],
                &at,
            );
        }
    }

    /// The header of card `id` in `marks`: its title, type tag, badge,
    /// toggle, Cancel and Save (those drawn) stay inside the card, left to
    /// right, apart.
    fn assert_header_fits(marks: &Marks, id: &str, at: &str) {
        let (card, _) = marks[&format!("card:{id}")];
        let parts: Vec<(String, Rect)> = ["title", "tag", "badge", "toggle", "cancel", "save"]
            .iter()
            .filter_map(|p| {
                let name = format!("{p}:{id}");
                marks.get(&name).map(|(r, _)| (name, *r))
            })
            .collect();
        for (name, r) in &parts {
            assert!(
                card.contains_rect(*r),
                "{name} {r:?} outside {card:?} at {at}"
            );
        }
        for pair in parts.windows(2) {
            let ((a, ra), (b, rb)) = (&pair[0], &pair[1]);
            assert!(
                ra.right() <= rb.left(),
                "{a} {ra:?} runs into {b} {rb:?} at {at}"
            );
        }
    }

    /// Card `id`'s type tag in `marks` sits right of its title and right
    /// against `next` (the toggle, or Cancel while dirty), on the title's
    /// row.
    fn assert_tag_by_buttons(marks: &Marks, id: &str, next: &str, at: &str) {
        let (title, _) = marks[&format!("title:{id}")];
        let (tag, _) = marks[&format!("tag:{id}")];
        let (next_rect, _) = marks[&format!("{next}:{id}")];
        assert!(
            title.right() <= tag.left(),
            "{id} tag left of its title at {at}"
        );
        assert!(
            tag.right() <= next_rect.left() && next_rect.left() - tag.right() < 12.0,
            "{id} tag {tag:?} not right against {next} {next_rect:?} at {at}"
        );
        assert!(
            tag.center().y > title.top() && tag.center().y < title.bottom(),
            "{id} tag off the title's row at {at}"
        );
    }

    #[test]
    fn the_type_tag_shows_on_the_settings_side_only_and_a_long_name_gives_way_to_it() {
        let long = "Square Golf SGO300A SquareGolf(CD01) in the back bay by the window";
        let ids = ["mevo.0", "square.0", "openconnect_client.0", "webserver.0"];
        for (w, h) in [(1600.0, 900.0), (1024.0, 768.0)] {
            let at = format!("{w}x{h}");
            let screen = Screen::sized(w, h);
            let mut app = app_with_omni();
            app.actors.get_mut("square.0").unwrap().name = "Bay Omni".into();
            app.actors.get_mut("webserver.0").unwrap().name = "Dashboard".into();
            screen.frame(&mut app, Vec::new());
            let telemetry = screen.frame(&mut app, Vec::new());
            for id in ids {
                assert!(
                    !telemetry.contains_key(&format!("tag:{id}")),
                    "{id} at {at}"
                );
                assert!(
                    telemetry.contains_key(&format!("badge:{id}")),
                    "{id} at {at}"
                );
                assert_header_fits(&telemetry, id, &at);
            }

            // Settings side: the tag, no badge.
            for id in ids {
                app.card_modes.set(id, CardMode::Settings);
            }
            device(&mut app, "square.0").name = "Bay Omni".into();
            integration(&mut app, "webserver.0").name = "Dashboard".into();
            screen.frame(&mut app, Vec::new());
            let short = screen.frame(&mut app, Vec::new());
            let tag_width = |m: &Marks| m["tag:square.0"].0.width();
            for id in ids {
                assert!(short.contains_key(&format!("tag:{id}")), "{id} at {at}");
                assert!(!short.contains_key(&format!("badge:{id}")), "{id} at {at}");
                assert_header_fits(&short, id, &at);
                assert_tag_by_buttons(&short, id, "toggle", &at);
            }
            // Right-aligned, apart from a short name.
            let gap = short["tag:square.0"].0.left() - short["title:square.0"].0.right();
            assert!(gap > 100.0, "the tag beside the name at {at}");
            // Where the Telemetry side's badge sits, against the toggle.
            for id in ids {
                let (badge, _) = telemetry[&format!("badge:{id}")];
                let (tag, _) = short[&format!("tag:{id}")];
                assert!((badge.right() - tag.right()).abs() < 0.5, "{id} at {at}");
            }

            // Dirty: the tag right against Cancel, before Save.
            device(&mut app, "square.0").dirty = true;
            screen.frame(&mut app, Vec::new());
            let dirty = screen.frame(&mut app, Vec::new());
            assert!(!dirty.contains_key("toggle:square.0"), "{at}");
            assert_header_fits(&dirty, "square.0", &at);
            assert_tag_by_buttons(&dirty, "square.0", "cancel", &at);
            assert!(
                (tag_width(&dirty) - tag_width(&short)).abs() < 0.5,
                "the tag shrank while dirty at {at}"
            );
            device(&mut app, "square.0").dirty = false;

            // A long name: the name is cut short, the tag keeps its full
            // width.
            device(&mut app, "square.0").name = long.into();
            screen.frame(&mut app, Vec::new());
            let marks = screen.frame(&mut app, Vec::new());
            assert_header_fits(&marks, "square.0", &at);
            assert_tag_by_buttons(&marks, "square.0", "toggle", &at);
            assert!(
                (tag_width(&marks) - tag_width(&short)).abs() < 0.5,
                "the tag shrank at {at}"
            );
            assert!(
                marks["title:square.0"].0.width() < 300.0,
                "the name is cut short at {at}"
            );

            // Back on the Telemetry side, the long name has the header to
            // itself and no tag.
            app.card_modes.set("square.0", CardMode::Telemetry);
            app.actors.get_mut("square.0").unwrap().name = long.into();
            screen.frame(&mut app, Vec::new());
            let back = screen.frame(&mut app, Vec::new());
            assert!(!back.contains_key("tag:square.0"), "{at}");
            assert_header_fits(&back, "square.0", &at);
        }
    }

    #[test]
    fn the_type_tag_is_left_out_when_the_name_says_it() {
        let screen = Screen::new();
        let mut app = app_with_omni();
        // Named as their type: the Omni and the web server at their default
        // names, on their Settings sides.
        for id in ["square.0", "webserver.0", "openconnect_client.0"] {
            app.card_modes.set(id, CardMode::Settings);
        }
        screen.frame(&mut app, Vec::new());
        let marks = screen.frame(&mut app, Vec::new());
        for id in ["square.0", "webserver.0"] {
            assert!(marks.contains_key(&format!("title:{id}")), "{id}");
            assert!(!marks.contains_key(&format!("tag:{id}")), "{id}");
        }
        assert!(marks.contains_key("tag:openconnect_client.0"));

        // Renamed, the web server's tag names its type.
        integration(&mut app, "webserver.0").name = "Dashboard".into();
        screen.frame(&mut app, Vec::new());
        let marks = screen.frame(&mut app, Vec::new());
        assert!(marks.contains_key("tag:webserver.0"));
        let entry = &app.settings.actors[form_index(&app, "webserver.0")];
        assert_eq!(entry.type_label(), "Web Server");

        // The generic presets at their default names, in any case.
        for (menu, preset, id) in [
            (
                "add:Launch monitors",
                "OpenConnect Server",
                "openconnect_server.1",
            ),
            ("add:Outputs", "OpenConnect Client", "openconnect_client.2"),
        ] {
            let marks = screen.frame(&mut app, Vec::new());
            let marks = screen.click_mark(&mut app, &marks, menu);
            screen.click_mark(&mut app, &marks, &format!("{menu}:{preset}"));
            let marks = screen.frame(&mut app, Vec::new());
            assert!(marks.contains_key(&format!("title:{id}")), "{id}");
            assert!(!marks.contains_key(&format!("tag:{id}")), "{id}");
            let idx = form_index(&app, id);
            match &mut app.settings.actors[idx] {
                ActorFormEntry::Device(d) => d.name = preset.to_uppercase(),
                ActorFormEntry::Integration(i) => i.name = preset.to_lowercase(),
            }
            let marks = screen.frame(&mut app, Vec::new());
            assert!(!marks.contains_key(&format!("tag:{id}")), "{id}");
            let marks = screen.click_mark(&mut app, &marks, &format!("cancel:{id}"));
            assert!(!marks.keys().any(|k| k.ends_with(id)), "{id}");
        }
    }

    #[test]
    fn on_a_narrow_window_the_tag_shrinks_before_the_badge_and_toggle() {
        let long = "Square Golf SGO300A SquareGolf(CD01) in the back bay by the window";
        for w in [480.0, 400.0, 360.0] {
            let at = format!("{w}x800");
            let screen = Screen::sized(w, 800.0);
            let mut app = app_with_omni();
            app.actors.get_mut("square.0").unwrap().name = long.into();
            device(&mut app, "square.0").name = long.into();
            // Not named as its type, so the web server's tag shows.
            integration(&mut app, "webserver.0").name = "Dashboard".into();
            let ids = ["square.0", "openconnect_client.0", "webserver.0"];
            // The badge on the Telemetry side; the tag, on the Settings side,
            // may give way altogether.
            for (mode, badge) in [(CardMode::Telemetry, true), (CardMode::Settings, false)] {
                for id in ids {
                    app.card_modes.set(id, mode);
                }
                screen.frame(&mut app, Vec::new());
                let marks = screen.frame(&mut app, Vec::new());
                // On the Telemetry side each card keeps to its column, the
                // web server's under the wider Global and Simulation cards
                // too.
                let width = marks["card:square.0"].0.width();
                for id in ids {
                    let (card, _) = marks[&format!("card:{id}")];
                    assert!(!badge || (card.width() - width).abs() < 0.5, "{id} at {at}");
                    assert_eq!(
                        marks.contains_key(&format!("badge:{id}")),
                        badge,
                        "{id} {mode:?} at {at}"
                    );
                    assert!(marks.contains_key(&format!("toggle:{id}")), "{id} at {at}");
                    assert_header_fits(&marks, id, &at);
                    if marks.contains_key(&format!("tag:{id}")) {
                        assert_tag_by_buttons(&marks, id, "toggle", &at);
                    }
                }
            }
        }
    }

    #[test]
    fn the_web_server_card_sits_in_general_after_global_simulation_and_replay() {
        for (w, h) in [(1600.0, 900.0), (1024.0, 768.0)] {
            let at = format!("{w}x{h}");
            let screen = Screen::sized(w, h);
            let mut app = app();
            screen.frame(&mut app, Vec::new());
            let marks = screen.frame(&mut app, Vec::new());
            let (global, _) = marks["title:global"];
            let (simulation, _) = marks["title:simulation"];
            let (replay, _) = marks["title:replay"];
            let (web, _) = marks["card:webserver.0"];
            let (mevo, _) = marks["card:mevo.0"];
            assert!(global.left() < simulation.left(), "{at}");
            assert!(web.top() < mevo.top(), "above the launch monitors at {at}");
            if w > 1200.0 {
                // One row: Global, Simulation, Replay; the web server under
                // Global.
                assert!(simulation.right() < replay.left(), "{at}");
                assert!(replay.top() < global.bottom(), "the same row at {at}");
                assert!(web.top() > global.bottom(), "{at}");
                assert!(web.left() < simulation.left(), "{at}");
            } else {
                // Two columns: Replay under Global, the web server under
                // Simulation.
                assert!(replay.top() > global.bottom(), "{at}");
                assert!(replay.left() < simulation.left(), "{at}");
                assert!(web.top() > simulation.bottom(), "{at}");
                assert!(web.left() > replay.right(), "{at}");
            }
            // General has no (+).
            assert!(!marks.contains_key("add:General"), "{at}");
            assert!(marks.contains_key("add:Launch monitors"));
            assert!(marks.contains_key("add:Outputs"));
        }
    }

    #[test]
    fn the_web_server_settings_side_has_no_remove_and_its_bind_stays_editable() {
        let screen = Screen::new();
        let mut app = app();
        app.card_modes.set("webserver.0", CardMode::Settings);
        let marks = screen.frame(&mut app, Vec::new());
        assert!(enabled(&marks, "toggle:webserver.0"));
        assert!(!marks.contains_key("remove:webserver.0"));

        // Edited, Save and Cancel, still no Remove.
        integration(&mut app, "webserver.0").address = "0.0.0.0:5881".into();
        integration(&mut app, "webserver.0").dirty = true;
        let marks = screen.frame(&mut app, Vec::new());
        assert!(enabled(&marks, "save:webserver.0"));
        assert!(enabled(&marks, "cancel:webserver.0"));
        assert!(!marks.contains_key("remove:webserver.0"));
        assert_eq!(
            app.settings.build_actor_request("webserver.0").webserver["0"].bind,
            "0.0.0.0:5881"
        );

        // Not even a stray Remove action takes it out.
        let idx = form_index(&app, "webserver.0");
        app.card_action(
            &egui::Context::default(),
            "webserver.0",
            Some(idx),
            CardAction::Remove,
        );
        assert!(app.confirm_remove.is_none());
    }

    /// Deliver a save reply, as the network callback does, and drain it.
    fn reply(app: &mut FlighthookApp, result: Result<(), &str>) {
        let reply = result
            .map(|()| crate::types::PostSettingsResponse {
                restarted: Vec::new(),
                stopped: Vec::new(),
            })
            .map_err(str::to_owned);
        app.pending.lock().unwrap().settings_save = Some(reply);
        app.apply_pending();
    }

    /// Deliver a `GET /api/settings` reply and drain it.
    fn settings_reply(app: &mut FlighthookApp, config: &FlighthookConfig) {
        app.pending.lock().unwrap().settings = Some(config.clone());
        app.apply_pending();
    }

    fn integration<'a>(
        app: &'a mut FlighthookApp,
        id: &str,
    ) -> &'a mut settings::IntegrationFormEntry {
        let idx = form_index(app, id);
        match &mut app.settings.actors[idx] {
            ActorFormEntry::Integration(i) => i,
            ActorFormEntry::Device(_) => panic!("{id} is a device"),
        }
    }

    #[test]
    fn a_failed_save_unlocks_its_card_keeps_the_edits_and_says_why() {
        let screen = Screen::new();
        let mut app = app();
        app.card_modes
            .set("openconnect_client.0", CardMode::Settings);
        edit_name(&mut app, "openconnect_client.0");
        let marks = screen.frame(&mut app, Vec::new());
        let marks = screen.click_mark(&mut app, &marks, "save:openconnect_client.0");
        assert!(app.settings.saving());
        assert!(!enabled(&marks, "save:openconnect_client.0"));
        assert!(!enabled(&marks, "cancel:openconnect_client.0"));

        // A non-2xx reply, a body that isn't a save response, a network
        // error: all the same failure.
        reply(&mut app, Err("504 the settings were not applied in time"));
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!app.settings.saving());
        assert!(app.settings.actors[form_index(&app, "openconnect_client.0")].dirty());
        assert!(enabled(&marks, "save:openconnect_client.0"));
        assert!(enabled(&marks, "cancel:openconnect_client.0"));
        assert!(enabled(&marks, "remove:openconnect_client.0"));
        assert!(marks.contains_key("error:openconnect_client.0"));
        assert!(!marks.contains_key("error:openconnect_client.1"));
        assert!(!enabled(&marks, "tabs"), "still locked to the edits");

        // The saved config did not take the edit: Cancel goes back to it.
        let marks = screen.click_mark(&mut app, &marks, "cancel:openconnect_client.0");
        assert_eq!(
            app.settings.actors[form_index(&app, "openconnect_client.0")].name(),
            "GSPro"
        );
        assert!(!marks.contains_key("error:openconnect_client.0"));
        assert!(enabled(&marks, "tabs"));
    }

    #[test]
    fn a_failed_get_started_unlocks_the_wizard() {
        let mut app = FlighthookApp::with_pending(net::new_pending());
        app.settings.load_from(&FlighthookConfig::default());
        app.wizard_gspro = true;
        app.apply_wizard(&egui::Context::default());
        assert!(app.wizard_saving);

        reply(&mut app, Err("Failed to fetch"));
        assert!(!app.wizard_saving);
        assert!(!app.wizard_dismissed);
        assert_eq!(app.wizard_error.as_deref(), Some("Failed to fetch"));
        assert!(!app.settings.saving());

        // Get Started again: saved, and the form holds what it saved.
        app.apply_wizard(&egui::Context::default());
        assert_eq!(app.wizard_error, None);
        reply(&mut app, Ok(()));
        assert!(app.wizard_dismissed);
        assert!(app.settings.loaded, "no reload window");
        let gspro = &app.settings.actors[form_index(&app, "openconnect_client.0")];
        assert!(!gspro.dirty());
        assert!(!app.settings.locked());
    }

    #[test]
    fn a_lost_reply_is_settled_by_reading_the_settings_again() {
        let screen = Screen::new();
        let mut app = app();
        app.card_modes.set("webserver.0", CardMode::Settings);
        integration(&mut app, "webserver.0").address = "0.0.0.0:5881".into();
        integration(&mut app, "webserver.0").dirty = true;
        let marks = screen.frame(&mut app, Vec::new());
        screen.click_mark(&mut app, &marks, "save:webserver.0");
        let saved_before = app.settings.to_request();

        // The bind change restarts the server answering: the reply is lost.
        reply(&mut app, Err("connection reset"));
        assert!(app.settings.fetch_due(100.0), "the settings are read again");
        assert!(!app.settings.fetch_due(100.5));

        // The server did not apply it: still dirty, still saying why.
        let mut unchanged = saved_before.clone();
        unchanged.webserver.get_mut("0").unwrap().bind = "0.0.0.0:5880".into();
        settings_reply(&mut app, &unchanged);
        assert!(app.settings.actors[form_index(&app, "webserver.0")].dirty());
        assert_eq!(integration(&mut app, "webserver.0").address, "0.0.0.0:5881");
        assert!(app.settings.save_error.is_some());

        // It did: clean, the error gone, the tab unlocked.
        reply_after_retry(&mut app, &screen);
        settings_reply(&mut app, &saved_before);
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!app.settings.locked());
        assert_eq!(app.settings.save_error, None);
        assert!(!marks.contains_key("error:webserver.0"));
        assert!(enabled(&marks, "toggle:webserver.0"));
        assert!(enabled(&marks, "tabs"));
        // The saved config, which Cancel goes back to, has the new bind.
        assert_eq!(
            app.settings.build_actor_request("webserver.0"),
            saved_before
        );
    }

    /// Save the dirty Web Server card again and lose the reply again.
    fn reply_after_retry(app: &mut FlighthookApp, screen: &Screen) {
        let marks = screen.frame(app, Vec::new());
        screen.click_mark(app, &marks, "save:webserver.0");
        reply(app, Err("connection reset"));
    }

    /// The text painted in card `id`'s title.
    fn title_text(marks: &Marks, shapes: &[egui::Shape], id: &str) -> String {
        let (title, _) = marks[&format!("title:{id}")];
        shapes
            .iter()
            .find_map(|s| match s {
                egui::Shape::Text(t) if title.contains(t.visual_bounding_rect().center()) => {
                    Some(t.galley.text().to_string())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("no title text for {id}"))
    }

    #[test]
    fn a_rename_shows_on_the_telemetry_side_and_in_another_window_without_a_reload() {
        let id = "openconnect_client.0";
        let screen = Screen::new();
        // Another window, open on the Telemetry side all along.
        let other_screen = Screen::new();
        let mut other = app();
        let mut app = app();
        let old = app.actors[id].name.clone();
        other_screen.frame(&mut other, Vec::new());
        let (marks, shapes) = other_screen.painted(&mut other, Vec::new());
        assert_eq!(title_text(&marks, &shapes, id), old);

        app.card_modes.set(id, CardMode::Settings);
        integration(&mut app, id).name = "Bay Sim".into();
        let idx = form_index(&app, id);
        app.settings.actors[idx].set_dirty(true);
        let marks = screen.frame(&mut app, Vec::new());
        screen.click_mark(&mut app, &marks, &format!("save:{id}"));
        reply(&mut app, Ok(()));

        // The rename restarts nothing; its outcome asks every window for the
        // status, and the status carries the new name.
        for app in [&mut app, &mut other] {
            app.needs_status_refresh = false;
            app.handle_bus_event(
                crate::types::FlighthookMessage::new(
                    crate::types::FlighthookEvent::ConfigOutcome {
                        request_id: None,
                        restarted: Vec::new(),
                        stopped: Vec::new(),
                        started: Vec::new(),
                    },
                )
                .actor("system"),
            );
            assert!(app.needs_status_refresh);
            let mut actors = app.actors.clone();
            actors.get_mut(id).unwrap().name = "Bay Sim".into();
            app.pending.lock().unwrap().status = Some(crate::types::StatusResponse {
                actors,
                mode: None,
                club: None,
                handed: None,
                cameras_supported: false,
                clip_dir: None,
            });
            app.apply_pending();
        }

        let marks = screen.frame(&mut app, Vec::new());
        let marks = screen.click(&mut app, toggle_side(&marks, id, CardMode::Telemetry));
        assert_eq!(app.card_modes.get(id), CardMode::Telemetry);
        assert!(marks.contains_key(&format!("badge:{id}")));
        let (marks, shapes) = screen.painted(&mut app, Vec::new());
        assert_eq!(title_text(&marks, &shapes, id), "Bay Sim");
        let (marks, shapes) = other_screen.painted(&mut other, Vec::new());
        assert_eq!(title_text(&marks, &shapes, id), "Bay Sim");
    }

    #[test]
    fn a_save_locks_every_card_and_the_reply_cleans_only_what_it_posted() {
        let screen = Screen::new();
        let mut app = app();
        app.card_modes
            .set("openconnect_client.0", CardMode::Settings);
        app.card_modes
            .set("openconnect_client.1", CardMode::Settings);
        edit_name(&mut app, "openconnect_client.0");
        let marks = screen.frame(&mut app, Vec::new());
        let marks = screen.click_mark(&mut app, &marks, "save:openconnect_client.0");
        // Out: nothing takes input, the saving card included.
        assert!(!enabled(&marks, "tabs"));
        assert!(!enabled(&marks, "add:Launch monitors"));
        assert!(!enabled(&marks, "add:Outputs"));
        assert!(!enabled(&marks, "remove:openconnect_client.0"));
        for id in ["mevo.0", "openconnect_client.1", "webserver.0"] {
            assert!(!enabled(&marks, &format!("toggle:{id}")), "{id}");
        }
        let posted = app.settings.actors[form_index(&app, "openconnect_client.0")]
            .name()
            .to_string();

        // An edit that lands while the save is out stays unsaved.
        edit_name(&mut app, "openconnect_client.0");
        reply(&mut app, Ok(()));
        let entry = &app.settings.actors[form_index(&app, "openconnect_client.0")];
        assert!(entry.dirty());
        assert_eq!(entry.name(), format!("{posted} edited"));
        app.cancel_device(form_index(&app, "openconnect_client.0"));
        assert_eq!(
            app.settings.actors[form_index(&app, "openconnect_client.0")].name(),
            posted
        );
        assert!(!app.settings.locked());
    }

    #[test]
    fn a_removal_waits_for_its_reply_and_a_card_added_meanwhile_stays_unsaved() {
        let screen = Screen::new();
        let mut app = app();
        app.card_modes
            .set("openconnect_client.0", CardMode::Settings);
        let marks = screen.frame(&mut app, Vec::new());
        let marks = screen.click_mark(&mut app, &marks, "remove:openconnect_client.0");
        assert!(enabled(&marks, "confirm:remove"));
        let marks = screen.click_mark(&mut app, &marks, "confirm:remove");
        assert!(app.confirm_remove.is_none());
        assert!(app.settings.saving());
        assert!(!enabled(&marks, "add:Outputs"));
        assert!(!enabled(&marks, "tabs"));
        // The card stays until the server has removed it.
        assert!(app.settings.position("openconnect_client.0").is_some());

        // A card that slips in while the removal is out.
        app.add_device(output_preset("GSPro (OpenConnect)"));
        reply(&mut app, Ok(()));
        assert_eq!(app.settings.position("openconnect_client.0"), None);
        assert_eq!(
            app.card_modes.get("openconnect_client.0"),
            CardMode::Telemetry
        );
        let new = &app.settings.actors[form_index(&app, "openconnect_client.2")];
        assert!(new.dirty(), "never saved");
        assert!(app.settings.locked());
    }

    #[test]
    fn a_failed_removal_keeps_the_card_and_says_why() {
        let screen = Screen::new();
        let mut app = app();
        app.card_modes
            .set("openconnect_client.1", CardMode::Settings);
        let _ = app
            .settings
            .begin_save(SaveTarget::Remove("openconnect_client.1".into()));
        reply(&mut app, Err("Failed to fetch"));
        let marks = screen.frame(&mut app, Vec::new());
        assert!(app.settings.position("openconnect_client.1").is_some());
        assert!(marks.contains_key("error:openconnect_client.1"));
        assert!(enabled(&marks, "remove:openconnect_client.1"));
        assert!(enabled(&marks, "tabs"));
    }

    #[test]
    fn the_removal_dialog_removes_by_id_and_waits_for_a_save_out() {
        let screen = Screen::new();
        let mut app = app();
        app.confirm_remove = Some(PendingRemoval {
            id: "openconnect_client.1".into(),
            name: "GSPro".into(),
        });
        // Entries move under the open dialog.
        let moved = app.settings.actors.remove(form_index(&app, "mevo.0"));
        app.settings.actors.push(moved);

        let _ = app
            .settings
            .begin_save(SaveTarget::Actor("openconnect_client.0".into()));
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!enabled(&marks, "confirm:remove"));
        reply(&mut app, Ok(()));

        let marks = screen.frame(&mut app, Vec::new());
        screen.click_mark(&mut app, &marks, "confirm:remove");
        reply(&mut app, Ok(()));
        assert_eq!(app.settings.position("openconnect_client.1"), None);
        for id in ["mevo.0", "openconnect_client.0", "webserver.0"] {
            assert!(app.settings.position(id).is_some(), "{id}");
        }
    }

    #[test]
    fn an_invalid_card_blocks_only_its_own_save() {
        let screen = Screen::new();
        let mut app = app();
        // A hand-edited config: openconnect_client.1 without an address, never opened.
        integration(&mut app, "openconnect_client.1").address = String::new();
        app.card_modes
            .set("openconnect_client.0", CardMode::Settings);
        edit_name(&mut app, "openconnect_client.0");
        let marks = screen.frame(&mut app, Vec::new());
        assert!(enabled(&marks, "save:openconnect_client.0"));

        app.settings.simulation.altitude_val = "99999".into();
        app.settings.mark_general(GeneralCard::Simulation);
        let marks = screen.frame(&mut app, Vec::new());
        assert!(enabled(&marks, "save:openconnect_client.0"));
        assert!(!enabled(&marks, "save:simulation"));
    }

    #[test]
    fn add_is_offered_only_once_the_settings_have_loaded() {
        let screen = Screen::new();
        let mut app = FlighthookApp::with_pending(net::new_pending());
        app.settings.fetched_at = Some(f64::MAX);
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!enabled(&marks, "add:Launch monitors"));
        assert!(!enabled(&marks, "add:Outputs"));
        assert!(!marks.contains_key("save:global"));
    }

    /// The camera list a server with one USB camera, in use by `camera.1`,
    /// and the test pattern returns.
    fn camera_list() -> crate::types::CameraListResponse {
        crate::types::CameraListResponse {
            supported: true,
            cameras: vec![
                crate::types::CameraListEntry {
                    id: "/dev/v4l/by-id/usb-c920".into(),
                    name: "HD Pro Webcam C920".into(),
                    usb: Some("046d:085e".into()),
                    modes: vec!["1920x1080@30".into(), "1280x720@30".into()],
                    in_use_by: Some("camera.1".into()),
                    error: None,
                },
                crate::types::CameraListEntry {
                    id: "test-pattern".into(),
                    name: "Test pattern".into(),
                    modes: vec!["640x360@30".into()],
                    ..Default::default()
                },
            ],
        }
    }

    #[test]
    fn cameras_get_a_group_whose_add_needs_server_support() {
        let screen = Screen::new();
        let mut app = app();
        let marks = screen.frame(&mut app, Vec::new());
        assert!(
            !marks.contains_key("add:Cameras"),
            "no (+) while the server can't record"
        );
        app.cameras_supported = true;
        let marks = screen.frame(&mut app, Vec::new());
        let (cameras, _) = marks["add:Cameras"];
        let (monitors, _) = marks["add:Launch monitors"];
        let (outputs, _) = marks["add:Outputs"];
        assert!(monitors.top() < cameras.top() && cameras.top() < outputs.top());

        // A camera already configured keeps its card without support.
        let id = app.settings.add_actor("camera").unwrap();
        assert_eq!(id, "camera.0");
        device(&mut app, &id).cam.id = "test-pattern".into();
        let config = app.settings.to_request();
        app.settings = SettingsForm::default();
        app.settings.load_from(&config);
        app.cameras_supported = false;
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!marks.contains_key("add:Cameras"));
        assert!(marks.contains_key(&format!("card:{id}")));
    }

    /// The launch monitor kinds and the default camera timing each shows in
    /// its empty field.
    const MONITOR_TIMINGS: [(&str, &str); 6] = [
        ("mevo", "0"),
        ("r10", "2"),
        ("square", "1"),
        ("exputt", "1.5"),
        ("mock_monitor", "0"),
        ("openconnect_server", "2.5"),
    ];

    /// An app with one saved `kind.0` launch monitor, its card showing
    /// `mode`.
    fn app_with_monitor(kind: &str, mode: CardMode) -> FlighthookApp {
        let mut config = FlighthookConfig::default();
        match kind {
            "mevo" => drop(config.mevo.insert("0".into(), Default::default())),
            "r10" => drop(config.r10.insert("0".into(), Default::default())),
            "square" => drop(config.square.insert("0".into(), Default::default())),
            "exputt" => drop(config.exputt.insert("0".into(), Default::default())),
            "mock_monitor" => drop(config.mock_monitor.insert(
                "0".into(),
                flighthook::MockMonitorSection {
                    name: "Mock".into(),
                    impact_lead_s: None,
                },
            )),
            _ => drop(
                config
                    .openconnect_server
                    .insert("0".into(), Default::default()),
            ),
        }
        let mut app = FlighthookApp::with_pending(net::new_pending());
        app.settings.load_from(&config);
        app.card_modes.set(&format!("{kind}.0"), mode);
        app
    }

    #[test]
    fn camera_timing_is_on_every_launch_monitors_settings_side_only() {
        let screen = Screen::new();
        for (kind, default) in MONITOR_TIMINGS {
            let id = format!("{kind}.0");
            let mark = format!("camera_timing:{id}");
            let mut app = app_with_monitor(kind, CardMode::Telemetry);
            let marks = screen.frame(&mut app, Vec::new());
            assert!(!marks.contains_key(&mark), "{kind}: telemetry side");

            let mut app = app_with_monitor(kind, CardMode::Settings);
            let (marks, shapes) = screen.painted(&mut app, Vec::new());
            let (field, _) = marks[&mark];
            let (card, _) = marks[&format!("card:{id}")];
            // Mevo's long settings scroll inside the card; the others fit.
            assert!(
                card.x_range().contains(field.left()) && card.x_range().contains(field.right()),
                "{kind}: {field:?} in {card:?}"
            );
            if kind != "mevo" {
                assert!(card.contains_rect(field), "{kind}: {field:?} in {card:?}");
            }
            // Empty: the device's default is shown in the field itself.
            let texts: Vec<String> = shapes
                .iter()
                .filter_map(|s| match s {
                    egui::Shape::Text(t) if field.contains(t.visual_bounding_rect().center()) => {
                        Some(t.galley.text().to_string())
                    }
                    _ => None,
                })
                .collect();
            // (Mevo's field is scrolled out of the card, so not painted.)
            if kind != "mevo" {
                assert_eq!(texts, [default], "{kind}");
            }
        }
        // Not on a camera or an output.
        let mut app = app();
        for a in ["mevo.0", "openconnect_client.0"] {
            app.card_modes.set(a, CardMode::Settings);
        }
        let marks = screen.frame(&mut app, Vec::new());
        assert!(marks.contains_key("camera_timing:mevo.0"));
        assert!(
            !marks
                .keys()
                .any(|k| k.starts_with("camera_timing:") && !k.contains("mevo")),
            "{:?}",
            marks.keys().collect::<Vec<_>>()
        );
        let mut app = app_with_monitor("mevo", CardMode::Settings);
        app.cameras_supported = true;
        app.camera_list.list = Some(camera_list());
        let marks = screen.frame(&mut app, Vec::new());
        let marks = screen.click_mark(&mut app, &marks, "add:Cameras");
        let _ = screen.click_mark(&mut app, &marks, "add:Cameras:Camera");
        let marks = screen.frame(&mut app, Vec::new());
        assert!(marks.contains_key("camera_pick:camera.0"));
        assert!(!marks.contains_key("camera_timing:camera.0"));
    }

    #[test]
    fn camera_timing_saves_and_is_validated() {
        let screen = Screen::new();
        for (kind, _) in MONITOR_TIMINGS {
            let id = format!("{kind}.0");
            let mut app = app_with_monitor(kind, CardMode::Settings);
            let lead = |app: &FlighthookApp| {
                app.settings
                    .build_actor_request(&id)
                    .configured_impact_lead_s(&id)
            };
            // Blank: the device's default, nothing saved.
            assert_eq!(lead(&app), None, "{kind}");

            for (typed, saved) in [("0", 0.0), (" 1.25 ", 1.25), ("5", 5.0)] {
                device(&mut app, &id).impact_lead = typed.into();
                device(&mut app, &id).dirty = true;
                let marks = screen.frame(&mut app, Vec::new());
                assert!(enabled(&marks, &format!("save:{id}")), "{kind} {typed}");
                assert_eq!(lead(&app), Some(saved), "{kind} {typed}");
            }
            for bad in ["-0.1", "5.1", "NaN", "inf", "soon"] {
                device(&mut app, &id).impact_lead = bad.into();
                let marks = screen.frame(&mut app, Vec::new());
                assert!(!enabled(&marks, &format!("save:{id}")), "{kind} {bad}");
            }
            device(&mut app, &id).impact_lead = String::new();
            let marks = screen.frame(&mut app, Vec::new());
            assert!(enabled(&marks, &format!("save:{id}")), "{kind}: blank");
            assert_eq!(lead(&app), None);
        }
        // A saved value loads back into the field.
        let mut config = FlighthookConfig::default();
        config.square.insert(
            "0".into(),
            flighthook::SquareSection {
                impact_lead_s: Some(0.5),
                ..Default::default()
            },
        );
        let mut app = FlighthookApp::with_pending(net::new_pending());
        app.settings.load_from(&config);
        assert_eq!(device(&mut app, "square.0").impact_lead, "0.5");
    }

    /// An app with one saved test-pattern `camera.0`, its card showing `mode`.
    fn app_with_camera(mode: CardMode) -> FlighthookApp {
        let mut config = FlighthookConfig::default();
        config.camera.insert(
            "0".into(),
            flighthook::CameraSection {
                camera: flighthook::CameraSection::TEST_PATTERN.into(),
                ..Default::default()
            },
        );
        let mut app = FlighthookApp::with_pending(net::new_pending());
        app.settings.load_from(&config);
        app.card_modes.set("camera.0", mode);
        app
    }

    #[test]
    fn latency_is_on_a_camera_cards_settings_side_only() {
        let screen = Screen::new();
        let mark = "camera_latency:camera.0";
        let mut cam = app_with_camera(CardMode::Telemetry);
        let marks = screen.frame(&mut cam, Vec::new());
        assert!(!marks.contains_key(mark), "telemetry side");

        let mut cam = app_with_camera(CardMode::Settings);
        let (marks, shapes) = screen.painted(&mut cam, Vec::new());
        let (field, _) = marks[mark];
        let (card, _) = marks["card:camera.0"];
        assert!(card.contains_rect(field), "{field:?} in {card:?}");
        // Empty: the hint `0` is shown in the field itself.
        let texts: Vec<String> = shapes
            .iter()
            .filter_map(|s| match s {
                egui::Shape::Text(t) if field.contains(t.visual_bounding_rect().center()) => {
                    Some(t.galley.text().to_string())
                }
                _ => None,
            })
            .collect();
        assert_eq!(texts, ["0"]);

        // Not on a launch monitor or an output.
        let mut others = app();
        for a in ["mevo.0", "openconnect_client.0"] {
            others.card_modes.set(a, CardMode::Settings);
        }
        let marks = screen.frame(&mut others, Vec::new());
        assert!(
            !marks.keys().any(|k| k.starts_with("camera_latency:")),
            "{:?}",
            marks.keys().collect::<Vec<_>>()
        );
        for (kind, _) in MONITOR_TIMINGS {
            let mut app = app_with_monitor(kind, CardMode::Settings);
            let marks = screen.frame(&mut app, Vec::new());
            assert!(
                !marks.keys().any(|k| k.starts_with("camera_latency:")),
                "{kind}"
            );
        }
    }

    #[test]
    fn camera_latency_saves_and_is_validated() {
        let screen = Screen::new();
        let id = "camera.0";
        let mut app = app_with_camera(CardMode::Settings);
        let latency =
            |app: &FlighthookApp| app.settings.build_actor_request(id).camera["0"].latency_s;
        // Blank: no latency, nothing saved.
        assert_eq!(latency(&app), None);

        for (typed, saved) in [("0", 0.0), (" 1.25 ", 1.25), ("10", 10.0)] {
            device(&mut app, id).cam.latency = typed.into();
            device(&mut app, id).dirty = true;
            let marks = screen.frame(&mut app, Vec::new());
            assert!(enabled(&marks, &format!("save:{id}")), "{typed}");
            assert_eq!(latency(&app), Some(saved), "{typed}");
        }
        for bad in ["-0.1", "10.1", "NaN", "inf", "soon"] {
            device(&mut app, id).cam.latency = bad.into();
            let marks = screen.frame(&mut app, Vec::new());
            assert!(!enabled(&marks, &format!("save:{id}")), "{bad}");
        }
        device(&mut app, id).cam.latency = String::new();
        let marks = screen.frame(&mut app, Vec::new());
        assert!(enabled(&marks, &format!("save:{id}")), "blank");
        assert_eq!(latency(&app), None);

        // A saved value loads back into the field.
        let mut config = FlighthookConfig::default();
        config.camera.insert(
            "0".into(),
            flighthook::CameraSection {
                camera: flighthook::CameraSection::TEST_PATTERN.into(),
                latency_s: Some(2.5),
                ..Default::default()
            },
        );
        let mut app = FlighthookApp::with_pending(net::new_pending());
        app.settings.load_from(&config);
        assert_eq!(device(&mut app, id).cam.latency, "2.5");
    }

    #[test]
    fn an_added_camera_needs_a_camera_from_the_list() {
        let screen = Screen::new();
        let mut app = app();
        app.cameras_supported = true;
        app.camera_list.list = Some(camera_list());
        let marks = screen.frame(&mut app, Vec::new());
        let marks = screen.click_mark(&mut app, &marks, "add:Cameras");
        let marks = screen.click_mark(&mut app, &marks, "add:Cameras:Camera");
        let id = "camera.0";
        let idx = form_index(&app, id);
        assert_eq!(app.settings.actors[idx].name(), "Camera");
        assert_eq!(app.settings.actors[idx].type_label(), "Camera");
        assert!(marks.contains_key(&format!("camera_pick:{id}")));
        assert!(marks.contains_key(&format!("camera_mode:{id}")));
        assert!(!enabled(&marks, &format!("save:{id}")), "no camera yet");

        // Picking from the list, as the picker does.
        let entry = camera_list().cameras[0].clone();
        let dev = device(&mut app, id);
        dev.cam.id = entry.id.clone();
        dev.cam.name = Some(entry.name.clone());
        dev.cam.position = crate::types::CameraPosition::TargetLine;
        dev.cam.mode = "1280x720@30".into();
        let marks = screen.frame(&mut app, Vec::new());
        assert!(enabled(&marks, &format!("save:{id}")));
        let section = &app.settings.build_actor_request(id).camera["0"];
        assert_eq!(section.camera, "/dev/v4l/by-id/usb-c920");
        assert_eq!(section.camera_name.as_deref(), Some("HD Pro Webcam C920"));
        assert_eq!(section.position, crate::types::CameraPosition::TargetLine);
        assert_eq!(section.mode, "1280x720@30");

        // A mode that doesn't parse can't be saved.
        device(&mut app, id).cam.mode = "fast".into();
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!enabled(&marks, &format!("save:{id}")));
    }

    #[test]
    fn the_camera_list_loads_with_a_camera_cards_settings_and_on_refresh() {
        let screen = Screen::new();
        let mut app = app();
        let _ = app.settings.add_actor("camera");
        screen.frame(&mut app, Vec::new());
        assert!(app.camera_list.loading, "asked for once a card needs it");
        // The reply lands.
        app.pending.lock().unwrap().cameras = Some(Ok(camera_list()));
        app.apply_pending();
        assert!(!app.camera_list.loading);
        assert_eq!(app.camera_list.list, Some(camera_list()));
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!app.camera_list.loading, "loaded: not asked again");
        let marks = screen.click_mark(&mut app, &marks, "camera_refresh:camera.0");
        let _ = marks;
        assert!(app.camera_list.loading, "refresh asks again");
        app.pending.lock().unwrap().cameras = Some(Err("504 Gateway Timeout".into()));
        app.apply_pending();
        assert_eq!(
            app.camera_list.error.as_deref(),
            Some("504 Gateway Timeout")
        );
        assert_eq!(
            app.camera_list.list,
            Some(camera_list()),
            "the last list stays"
        );
    }

    #[test]
    fn cameras_are_not_routing_monitors() {
        let screen = Screen::new();
        let mut app = app();
        let _ = app.settings.add_actor("camera");
        let mut config = app.settings.to_request();
        config.camera.get_mut("0").unwrap().camera = "test-pattern".into();
        app.settings = SettingsForm::default();
        app.settings.load_from(&config);
        app.card_modes
            .set("openconnect_client.0", CardMode::Settings);
        // The routing dropdowns list the Mevo only.
        let monitors: Vec<String> = app
            .settings
            .actors
            .iter()
            .filter_map(|a| match a {
                ActorFormEntry::Device(d) if !d.is_camera() => Some(a.global_id()),
                _ => None,
            })
            .collect();
        assert_eq!(monitors, ["mevo.0"]);
        let marks = screen.frame(&mut app, Vec::new());
        assert!(marks.contains_key("card:camera.0"));
    }

    #[test]
    fn a_running_cameras_card_fetches_and_shows_a_still_frame() {
        use stance::{Source, TestPattern, TestPatternConfig};

        let screen = Screen::new();
        let mut app = app();
        let _ = app.settings.add_actor("camera");
        let mut config = app.settings.to_request();
        config.camera.get_mut("0").unwrap().camera = "test-pattern".into();
        app.settings = SettingsForm::default();
        app.settings.load_from(&config);
        let id = "camera.0";
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!enabled(&marks, &format!("still:{id}")), "not running");

        app.actors.insert(
            id.into(),
            ActorStatusResponse::new("Camera", ActorStatus::Connected),
        );
        let marks = screen.frame(&mut app, Vec::new());
        let marks = screen.click_mark(&mut app, &marks, &format!("still:{id}"));
        assert!(
            !enabled(&marks, &format!("still:{id}")),
            "waiting for the frame"
        );
        assert!(app.stills[id].loading);

        let mut pattern = TestPattern::new(TestPatternConfig {
            width: 160,
            height: 90,
            paced: false,
            ..TestPatternConfig::default()
        })
        .unwrap();
        let frame = pattern
            .next_frame(std::time::Duration::ZERO)
            .unwrap()
            .unwrap();
        app.pending
            .lock()
            .unwrap()
            .stills
            .push((id.into(), Ok(frame.jpeg.to_vec())));
        app.apply_pending();
        let marks = screen.frame(&mut app, Vec::new());
        assert!(enabled(&marks, &format!("still:{id}")));
        let (image, _) = marks[&format!("still_image:{id}")];
        let (card, _) = marks[&format!("card:{id}")];
        assert!(image.width() > card.width() * 0.8, "at the card's width");
        assert!((image.height() / image.width() - 90.0 / 160.0).abs() < 0.02);
        // In the card's body, which scrolls to show it all; the card keeps
        // its height.
        let (body, content, _) = screen.bodies()[id];
        assert!(card.contains_rect(body));
        assert!(image.left() >= body.left() && image.right() <= body.right() + 0.5);
        assert!(image.top() >= body.top() && image.top() < body.bottom());
        assert!(content > body.height(), "the body scrolls");
        assert!((card.height() - CARD_HEIGHT).abs() < 0.5);
    }

    #[test]
    fn the_replay_card_saves_with_the_global_settings() {
        let screen = Screen::new();
        let mut app = app();
        app.clip_dir = Some("/home/golfer/.local/share/flighthook/clips".into());
        let marks = screen.frame(&mut app, Vec::new());
        assert!(marks.contains_key("replay_folder"));
        assert!(!marks.contains_key("save:replay"));

        app.settings.replay.keep_shots = "50".into();
        app.settings.mark_general(GeneralCard::Replay);
        let marks = screen.frame(&mut app, Vec::new());
        assert!(enabled(&marks, "save:replay"));
        assert!(!marks.contains_key("save:simulation"));
        assert!(!marks.contains_key("save:global"));
        let request = app.settings.build_global_request();
        assert_eq!(request.replay.keep_shots, 50);
        assert_eq!(request.replay.pre_roll_s, 4.0);
        assert_eq!(request.replay.start_before_impact_s, 2.0);
        assert!(marks.contains_key("replay_keep"));

        // The start field saves with the rest and rejects out-of-range text.
        app.settings.replay.start_before_impact_s = "3.5".into();
        let request = app.settings.build_global_request();
        assert_eq!(request.replay.start_before_impact_s, 3.5);
        app.settings.replay.start_before_impact_s = "11".into();
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!enabled(&marks, "save:replay"));
        app.settings.replay.start_before_impact_s = "2".into();

        // Out of range: no save.
        app.settings.replay.pre_roll_s = "12".into();
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!enabled(&marks, "save:replay"));
        // Cancel puts every General card back.
        let marks = screen.click_mark(&mut app, &marks, "cancel:replay");
        assert!(!marks.contains_key("cancel:replay"));
        assert_eq!(app.settings.replay.keep_shots, "20");
        assert_eq!(app.settings.replay.pre_roll_s, "4");
        assert_eq!(app.settings.replay.start_before_impact_s, "2");
        assert!(!app.settings.global_dirty);
    }

    // ----- Fixed-height cards and Reconnect -----

    /// Every card on the Devices tab, in card order.
    const EVERY_CARD: [&str; 13] = [
        "global",
        "simulation",
        "replay",
        "webserver.0",
        "mevo.0",
        "r10.0",
        "square.0",
        "exputt.0",
        "mock_monitor.0",
        "openconnect_server.0",
        "camera.0",
        "openconnect_client.0",
        "random_club.0",
    ];

    /// The cards with a Reconnect: every launch monitor and output.
    const RECONNECTABLE: [&str; 8] = [
        "mevo.0",
        "r10.0",
        "square.0",
        "exputt.0",
        "mock_monitor.0",
        "openconnect_server.0",
        "openconnect_client.0",
        "random_club.0",
    ];

    /// An app with a card of every type, each actor connected with the
    /// telemetry its device reports.
    fn app_with_every_card() -> FlighthookApp {
        let mut app = FlighthookApp::with_pending(net::new_pending());
        let mut config = FlighthookConfig::default();
        config.mevo.insert(
            "0".into(),
            serde_json::from_str(r#"{"name":"Mevo+"}"#).expect("mevo"),
        );
        config.r10.insert(
            "0".into(),
            serde_json::from_str(r#"{"name":"R10"}"#).expect("r10"),
        );
        config.square.insert(
            "0".into(),
            flighthook::SquareSection {
                name: "Square Golf Omni".into(),
                ..Default::default()
            },
        );
        config
            .exputt
            .insert("0".into(), flighthook::ExPuttSection::default());
        config.mock_monitor.insert(
            "0".into(),
            flighthook::MockMonitorSection {
                name: "Mock".into(),
                impact_lead_s: None,
            },
        );
        config
            .openconnect_server
            .insert("0".into(), flighthook::OpenConnectServerSection::default());
        config
            .camera
            .insert("0".into(), flighthook::CameraSection::default());
        config.openconnect_client.insert(
            "0".into(),
            flighthook::OpenConnectClientSection {
                name: "GSPro".into(),
                address: Some("127.0.0.1:921".into()),
                ..Default::default()
            },
        );
        config.random_club.insert(
            "0".into(),
            flighthook::RandomClubSection {
                name: "Random club".into(),
            },
        );
        app.settings.load_from(&config);
        let telemetry: [(&str, &[(&str, &str)]); 9] = [
            (
                "mevo.0",
                &[
                    ("ready", "true"),
                    ("battery_pct", "85"),
                    ("tilt", "0.4"),
                    ("roll", "-1.2"),
                    ("temp_c", "41.5"),
                    ("external_power", "false"),
                ],
            ),
            (
                "square.0",
                &[
                    ("device_info", OMNI_DEVICE_INFO),
                    ("ble_address", "02:00:00:AB:CD:01"),
                    ("shot_count", "12"),
                    ("ready", "true"),
                    ("device_state", "ready"),
                    ("battery_pct", "85"),
                    ("zero_spin_discards", "2"),
                ],
            ),
            (
                "exputt.0",
                &[
                    ("device_info", "ExPutt RG (192.168.1.40)"),
                    ("phase", "measuring"),
                    ("ready", "true"),
                ],
            ),
            ("mock_monitor.0", &[("shot_count", "3"), ("ready", "true")]),
            ("openconnect_server.0", &[("bind", "0.0.0.0:921")]),
            ("camera.0", &[("fps", "119.8"), ("dropped", "0")]),
            (
                "openconnect_client.0",
                &[
                    ("address", "127.0.0.1:921"),
                    ("club", "7I"),
                    ("handed", "rh"),
                    ("last_shot", "#12 at 16:41:07"),
                    ("monitor_ready", "true"),
                    ("response", "200 Shot received successfully"),
                    ("shots_sent", "3"),
                ],
            ),
            ("random_club.0", &[]),
            ("webserver.0", &[("ws_clients", "1")]),
        ];
        for (id, pairs) in telemetry {
            let mut actor = ActorStatusResponse::new(id, ActorStatus::Connected);
            actor.telemetry = pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect();
            app.actors.insert(id.into(), actor);
        }
        app
    }

    /// Every card's rect, by ID.
    fn card_rects(marks: &Marks) -> HashMap<String, Rect> {
        EVERY_CARD
            .iter()
            .map(|id| {
                let (rect, _) = marks
                    .get(&format!("card:{id}"))
                    .unwrap_or_else(|| panic!("{id} not drawn"));
                ((*id).to_owned(), *rect)
            })
            .collect()
    }

    /// Draw twice (layouts settle in one pass) and return the marks.
    fn settled(screen: &Screen, app: &mut FlighthookApp) -> Marks {
        screen.frame(app, Vec::new());
        screen.frame(app, Vec::new())
    }

    #[test]
    fn every_card_is_one_height_on_both_sides() {
        for (w, h) in [(1600.0, 2400.0), (1024.0, 4000.0)] {
            let screen = Screen::sized(w, h);
            let mut app = app_with_every_card();
            for side in [CardMode::Telemetry, CardMode::Settings] {
                for id in EVERY_CARD {
                    app.card_modes.set(id, side);
                }
                let rects = card_rects(&settled(&screen, &mut app));
                for (id, rect) in &rects {
                    assert!(
                        (rect.height() - CARD_HEIGHT).abs() < 0.5,
                        "{id} is {} tall on the {side:?} side at {w}",
                        rect.height()
                    );
                }
                // Rows line up: cards side by side share their top.
                let mevo = rects["mevo.0"];
                let beside = rects
                    .values()
                    .filter(|r| (r.top() - mevo.top()).abs() < 0.5)
                    .count();
                assert!(beside >= 2 || w < 1100.0, "a row of cards at {w}");
            }
        }
    }

    #[test]
    fn the_common_cards_fit_without_scrolling() {
        let screen = Screen::sized(1600.0, 2400.0);
        let mut app = app_with_every_card();
        settled(&screen, &mut app);
        let fits = |screen: &Screen, id: &str| {
            let (body, content, _) = screen.bodies()[id];
            content <= body.height()
        };
        for id in ["mevo.0", "square.0", "openconnect_client.0"] {
            assert!(fits(&screen, id), "{id}'s telemetry");
        }
        app.card_modes.set("global", CardMode::Settings);
        settled(&screen, &mut app);
        assert!(fits(&screen, "global"), "the Global settings");
    }

    #[test]
    fn telemetry_a_flip_a_validation_message_or_an_error_moves_no_card() {
        let screen = Screen::sized(1600.0, 2400.0);
        let mut app = app_with_every_card();
        let before = card_rects(&settled(&screen, &mut app));
        let unchanged = |app: &mut FlighthookApp, what: &str| {
            let after = card_rects(&settled(&screen, app));
            for id in EVERY_CARD {
                assert_eq!(after[id], before[id], "{id} after {what}");
            }
        };

        // Telemetry discovered: many more rows than the body holds.
        let mevo = app.actors.get_mut("mevo.0").unwrap();
        for n in 0..25 {
            mevo.telemetry
                .insert(format!("extra_{n:02}"), n.to_string());
        }
        unchanged(&mut app, "new telemetry");
        let (body, content, _) = screen.bodies()["mevo.0"];
        assert!(content > body.height(), "the body scrolls instead");

        // A side flip.
        app.card_modes.set("mevo.0", CardMode::Settings);
        unchanged(&mut app, "a flip to Settings");

        // An edit with a validation message, and the lock hint.
        let idx = form_index(&app, "mevo.0");
        if let ActorFormEntry::Device(d) = &mut app.settings.actors[idx] {
            d.address = "not an address".into();
        }
        app.settings.actors[idx].set_dirty(true);
        unchanged(&mut app, "an invalid address");

        // A failed save's error line.
        app.settings.save_error = Some(settings::SaveError {
            target: SaveTarget::Actor("mevo.0".into()),
            message: "504 the settings were not applied in time".into(),
        });
        unchanged(&mut app, "a save error");

        // A General card's edits.
        app.settings.save_error = None;
        app.settings.actors[idx].set_dirty(false);
        app.settings.chipping_clubs.clear();
        app.settings.mark_general(GeneralCard::Global);
        unchanged(&mut app, "a Global edit");
    }

    /// `n` wheel notches over `at`, then one more pass.
    fn wheel(screen: &Screen, app: &mut FlighthookApp, at: Pos2, down: f32, n: usize) {
        screen.frame(app, vec![Event::PointerMoved(at)]);
        for _ in 0..n {
            let events = vec![
                Event::PointerMoved(at),
                Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: vec2(0.0, -down),
                    modifiers: Modifiers::NONE,
                },
            ];
            screen.frame(app, events);
        }
        screen.frame(app, Vec::new());
    }

    #[test]
    fn a_long_body_scrolls_inside_its_card_before_the_page() {
        // The Mevo card on screen, and the tab long enough to scroll too.
        let screen = Screen::sized(1600.0, 1100.0);
        let mut app = app_with_every_card();
        app.card_modes.set("mevo.0", CardMode::Settings);
        let marks = settled(&screen, &mut app);
        let card = marks["card:mevo.0"].0;
        let title = marks["title:mevo.0"].0;
        let remove = marks["remove:mevo.0"].0;
        let (body, content, offset) = screen.bodies()["mevo.0"];
        assert!(body.bottom() < 1100.0, "on screen");
        assert!(content > body.height(), "the Mevo settings overflow");
        assert_eq!(offset, 0.0);

        // The wheel over the body scrolls the card, not the page.
        wheel(&screen, &mut app, body.center(), 7.0, 6);
        let marks = screen.frame(&mut app, Vec::new());
        let (_, _, scrolled) = screen.bodies()["mevo.0"];
        assert!(scrolled > 0.0, "the card scrolled");
        assert_eq!(marks["card:mevo.0"].0, card, "the page did not move");
        // Header and action row stay put; another card keeps its own place.
        assert_eq!(marks["title:mevo.0"].0, title);
        assert_eq!(marks["remove:mevo.0"].0, remove);
        assert_eq!(screen.bodies()["square.0"].2, 0.0);

        // Once the card is at its end, the page takes the wheel.
        wheel(&screen, &mut app, body.center(), 7.0, 120);
        let marks = screen.frame(&mut app, Vec::new());
        let (body_after, content, end) = screen.bodies()["mevo.0"];
        assert!(
            (end - (content - body_after.height())).abs() < 1.0,
            "the card reached its end: {end}"
        );
        assert!(
            marks["card:mevo.0"].0.top() < card.top(),
            "the page scrolled"
        );

        // The card keeps its position through later passes.
        for _ in 0..3 {
            screen.frame(&mut app, Vec::new());
        }
        assert_eq!(screen.bodies()["mevo.0"].2, end);
    }

    #[test]
    fn reconnect_is_on_every_launch_monitor_and_output_telemetry_side_and_no_other() {
        let screen = Screen::sized(1600.0, 2400.0);
        let mut app = app_with_every_card();
        let marks = settled(&screen, &mut app);
        for id in EVERY_CARD {
            let shown = marks.contains_key(&format!("reconnect:{id}"));
            assert_eq!(shown, RECONNECTABLE.contains(&id), "{id}");
        }
        // In the card's action row, at its bottom right.
        for id in RECONNECTABLE {
            let (button, enabled) = marks[&format!("reconnect:{id}")];
            let card = marks[&format!("card:{id}")].0;
            assert!(enabled, "{id}");
            assert!(card.contains_rect(button), "{id}");
            assert!(card.bottom() - button.bottom() < 16.0, "{id} at the bottom");
            assert!(card.right() - button.right() < 18.0, "{id} at the right");
        }
        // The ExPutt's Find mat sits beside it.
        let (find, _) = marks["find_mat:exputt.0"];
        let (reconnect, _) = marks["reconnect:exputt.0"];
        assert!(find.right() <= reconnect.left());
        assert!((find.center().y - reconnect.center().y).abs() < 0.5);

        // None on the Settings side.
        for id in EVERY_CARD {
            app.card_modes.set(id, CardMode::Settings);
        }
        let marks = settled(&screen, &mut app);
        assert!(!marks.keys().any(|k| k.starts_with("reconnect:")));
    }

    #[test]
    fn reconnect_posts_once_and_waits_for_its_answer() {
        let screen = Screen::sized(1600.0, 2400.0);
        let mut app = app_with_every_card();
        let marks = settled(&screen, &mut app);
        let marks = screen.click_mark(&mut app, &marks, "reconnect:mevo.0");
        assert!(app.reconnecting.contains("mevo.0"), "the request is out");
        assert!(!enabled(&marks, "reconnect:mevo.0"), "in flight");
        assert!(enabled(&marks, "reconnect:square.0"), "others are free");

        // A second click while it is out sends nothing more.
        app.reconnect(&screen.0, "mevo.0");
        assert_eq!(app.reconnecting.len(), 1);

        app.pending
            .lock()
            .unwrap()
            .reconnects
            .push(("mevo.0".into(), Ok(net::Reconnected::Started)));
        app.apply_pending();
        let marks = screen.frame(&mut app, Vec::new());
        assert!(app.reconnecting.is_empty());
        assert!(enabled(&marks, "reconnect:mevo.0"));

        // A refusal frees it too; the card says why through its telemetry.
        let marks = screen.click_mark(&mut app, &marks, "reconnect:square.0");
        assert!(!enabled(&marks, "reconnect:square.0"));
        app.pending
            .lock()
            .unwrap()
            .reconnects
            .push(("square.0".into(), Err("409 still closing".into())));
        app.apply_pending();
        let marks = screen.frame(&mut app, Vec::new());
        assert!(enabled(&marks, "reconnect:square.0"));
    }

    /// A reconnect request that fails (an error status, a network error, a
    /// 504) shows on its card as the card's `error`, the same row an error
    /// the actor reports takes, until the actor's next status or the next
    /// press. A request still running (202) shows nothing.
    #[test]
    fn a_failed_reconnect_shows_on_its_card_until_the_next_status_or_press() {
        let screen = Screen::sized(1600.0, 2400.0);
        let mut app = app_with_every_card();
        let marks = settled(&screen, &mut app);
        let height = marks["card:mevo.0"].0.height();
        assert!(!marks.contains_key("telemetry:mevo.0:error"));
        let reply = |app: &mut FlighthookApp, reply: Result<net::Reconnected, String>| {
            app.pending
                .lock()
                .unwrap()
                .reconnects
                .push(("mevo.0".into(), reply));
            app.apply_pending();
        };

        screen.click_mark(&mut app, &marks, "reconnect:mevo.0");
        reply(&mut app, Err("the reconnect did not finish".into()));
        let marks = screen.frame(&mut app, Vec::new());
        assert!(marks.contains_key("telemetry:mevo.0:error"), "on the card");
        assert_eq!(
            app.reconnect_errors.get("mevo.0").map(String::as_str),
            Some("the reconnect did not finish")
        );
        assert!(
            (marks["card:mevo.0"].0.height() - height).abs() < 0.5,
            "the card keeps its height"
        );
        assert!(enabled(&marks, "reconnect:mevo.0"));
        assert!(
            !marks.contains_key("telemetry:square.0:error"),
            "its card only"
        );

        // The actor's next status replaces it.
        app.handle_bus_event(
            crate::types::FlighthookMessage::new(crate::types::FlighthookEvent::ActorStatus {
                status: ActorStatus::Connected,
                telemetry: HashMap::new(),
            })
            .actor("mevo.0"),
        );
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!marks.contains_key("telemetry:mevo.0:error"));

        // So does pressing again; a 202 adds nothing.
        screen.click_mark(&mut app, &marks, "reconnect:mevo.0");
        reply(&mut app, Err("connection refused".into()));
        let marks = screen.frame(&mut app, Vec::new());
        assert!(marks.contains_key("telemetry:mevo.0:error"));
        screen.click_mark(&mut app, &marks, "reconnect:mevo.0");
        let marks = screen.frame(&mut app, Vec::new());
        assert!(
            !marks.contains_key("telemetry:mevo.0:error"),
            "cleared by the press"
        );
        reply(&mut app, Ok(net::Reconnected::Started));
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!marks.contains_key("telemetry:mevo.0:error"));
        assert!(app.reconnect_errors.is_empty());
    }

    fn reconnect_reply(app: &mut FlighthookApp, id: &str, reply: Result<net::Reconnected, String>) {
        app.pending
            .lock()
            .unwrap()
            .reconnects
            .push((id.into(), reply));
        app.apply_pending();
    }

    fn status_of(app: &mut FlighthookApp, id: &str, status: ActorStatus) {
        app.handle_bus_event(
            crate::types::FlighthookMessage::new(crate::types::FlighthookEvent::ActorStatus {
                status,
                telemetry: HashMap::new(),
            })
            .actor(id),
        );
    }

    /// A reconnect that starts later (202: its old connection is still
    /// closing, or it is still running) keeps Reconnect disabled until the
    /// actor starts again: its next Starting status, or an outcome listing
    /// it restarted, the one with no request ID included. Other statuses,
    /// Connected included, leave it waiting.
    #[test]
    fn a_reconnect_that_starts_later_waits_for_the_restart() {
        let screen = Screen::sized(1600.0, 2400.0);
        let mut app = app_with_every_card();
        let marks = settled(&screen, &mut app);

        // Freed by the next Starting status.
        screen.click_mark(&mut app, &marks, "reconnect:mevo.0");
        reconnect_reply(&mut app, "mevo.0", Ok(net::Reconnected::Later));
        status_of(&mut app, "mevo.0", ActorStatus::Disconnected);
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!enabled(&marks, "reconnect:mevo.0"), "still closing");
        // The instance being replaced may still report Connected.
        status_of(&mut app, "mevo.0", ActorStatus::Connected);
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!enabled(&marks, "reconnect:mevo.0"), "only Starting frees");
        status_of(&mut app, "mevo.0", ActorStatus::Starting);
        let marks = screen.frame(&mut app, Vec::new());
        assert!(enabled(&marks, "reconnect:mevo.0"), "started again");
        assert!(app.reconnect_errors.is_empty());

        // Freed by the self-completed outcome.
        screen.click_mark(&mut app, &marks, "reconnect:square.0");
        reconnect_reply(&mut app, "square.0", Ok(net::Reconnected::Later));
        app.handle_bus_event(
            crate::types::FlighthookMessage::new(crate::types::FlighthookEvent::ConfigOutcome {
                request_id: None,
                restarted: vec!["mevo.0".into()],
                stopped: Vec::new(),
                started: Vec::new(),
            })
            .actor("system"),
        );
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!enabled(&marks, "reconnect:square.0"), "another actor's");
        app.handle_bus_event(
            crate::types::FlighthookMessage::new(crate::types::FlighthookEvent::ConfigOutcome {
                request_id: None,
                restarted: vec!["square.0".into()],
                stopped: Vec::new(),
                started: Vec::new(),
            })
            .actor("system"),
        );
        let marks = screen.frame(&mut app, Vec::new());
        assert!(enabled(&marks, "reconnect:square.0"));

        // Started before the answer came: freed by the answer.
        screen.click_mark(&mut app, &marks, "reconnect:mevo.0");
        status_of(&mut app, "mevo.0", ActorStatus::Starting);
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!enabled(&marks, "reconnect:mevo.0"), "not answered yet");
        reconnect_reply(&mut app, "mevo.0", Ok(net::Reconnected::Later));
        let marks = screen.frame(&mut app, Vec::new());
        assert!(enabled(&marks, "reconnect:mevo.0"));
    }

    /// A reconnect that starts later but never does is given up on after
    /// a while: Reconnect takes clicks again and the card says why.
    #[test]
    fn a_reconnect_that_never_starts_is_given_up_on() {
        let screen = Screen::sized(1600.0, 2400.0);
        let mut app = app_with_every_card();
        let marks = settled(&screen, &mut app);
        app.now_ms = 1_000;
        screen.click_mark(&mut app, &marks, "reconnect:mevo.0");
        reconnect_reply(&mut app, "mevo.0", Ok(net::Reconnected::Later));
        app.now_ms += crate::app::RECONNECT_WAIT_MS - 1;
        app.apply_pending();
        assert!(app.reconnecting.contains("mevo.0"));
        app.now_ms += 1;
        app.apply_pending();
        let marks = screen.frame(&mut app, Vec::new());
        assert!(enabled(&marks, "reconnect:mevo.0"));
        assert_eq!(
            app.reconnect_errors.get("mevo.0").map(String::as_str),
            Some(crate::app::RECONNECT_UNFINISHED)
        );
        assert!(marks.contains_key("telemetry:mevo.0:error"), "on the card");
    }

    /// The give-up leaves the actor's own `error` row (the old connection is
    /// still closing) on the card instead of replacing it.
    #[test]
    fn giving_up_keeps_the_actors_own_error() {
        let screen = Screen::sized(1600.0, 2400.0);
        let mut app = app_with_every_card();
        let marks = settled(&screen, &mut app);
        app.now_ms = 1_000;
        screen.click_mark(&mut app, &marks, "reconnect:mevo.0");
        reconnect_reply(&mut app, "mevo.0", Ok(net::Reconnected::Later));
        let closing = "The previous connection has not closed yet. It reconnects when it does.";
        app.handle_bus_event(
            crate::types::FlighthookMessage::new(crate::types::FlighthookEvent::ActorStatus {
                status: ActorStatus::Disconnected,
                telemetry: HashMap::from([("error".into(), closing.into())]),
            })
            .actor("mevo.0"),
        );
        app.now_ms += crate::app::RECONNECT_WAIT_MS;
        app.apply_pending();
        let marks = screen.frame(&mut app, Vec::new());
        assert!(enabled(&marks, "reconnect:mevo.0"), "given up on");
        assert!(app.reconnect_errors.is_empty(), "the actor's row stays");
        assert_eq!(
            app.actors["mevo.0"]
                .telemetry
                .get("error")
                .map(String::as_str),
            Some(closing)
        );
        assert!(marks.contains_key("telemetry:mevo.0:error"), "on the card");
    }

    /// The repaint the last pass asked for: how soon.
    fn repaint_after(screen: &Screen, app: &mut FlighthookApp) -> std::time::Duration {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, screen.1)),
            ..Default::default()
        };
        let output = screen.0.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let ctx = ui.ctx().clone();
                app.render_devices_panel(&ctx, ui);
            });
        });
        output.viewport_output[&egui::ViewportId::ROOT].repaint_delay
    }

    /// Set `id`'s statuses to `statuses` in turn, with no telemetry.
    fn without_telemetry(app: &mut FlighthookApp, id: &str, statuses: &[ActorStatus]) {
        let actor = app.actors.get_mut(id).expect("running");
        actor.telemetry.clear();
        for status in statuses {
            actor.observe(*status, 0);
        }
    }

    /// A card whose Telemetry side has no row to show shows a spinner in
    /// the middle of its body while its actor is starting, connecting or
    /// reconnecting, and a check in a ring once it is connected; either
    /// goes as soon as a row arrives. A spinner asks for repaints
    /// (`monitoring::tests` checks nothing else does).
    #[test]
    fn an_empty_telemetry_side_shows_a_spinner_or_the_connected_mark() {
        let screen = Screen::sized(1600.0, 2400.0);
        let mut app = app_with_every_card();
        let marks = settled(&screen, &mut app);
        assert!(!marks.keys().any(|k| k.starts_with("spinner:")));
        assert!(
            marks.contains_key("connected_icon:random_club.0"),
            "no rows"
        );
        assert!(!marks.contains_key("connected_icon:mevo.0"), "rows");

        let cases: [(&str, &[ActorStatus]); 3] = [
            ("mevo.0", &[ActorStatus::Starting]),
            ("square.0", &[ActorStatus::Disconnected]),
            (
                "openconnect_client.0",
                &[ActorStatus::Connected, ActorStatus::Reconnecting],
            ),
        ];
        for (id, statuses) in cases {
            without_telemetry(&mut app, id, statuses);
        }
        let marks = settled(&screen, &mut app);
        for (id, _) in cases {
            let (spinner, _) = marks[&format!("spinner:{id}")];
            let (body, content, _) = screen.bodies()[id];
            assert!(
                (spinner.center().x - body.center().x).abs() < 1.0
                    && (spinner.center().y - body.center().y).abs() < 2.0,
                "{id}: centred in the body"
            );
            assert!(content <= body.height(), "{id}: nothing to scroll");
            assert!(!marks.contains_key(&format!("connected_icon:{id}")));
        }
        assert!(
            repaint_after(&screen, &mut app) <= theme::LOADING_FRAME,
            "a spinner turns"
        );

        // A card cut off by the bottom of the window keeps its spinner in
        // the middle of its whole body, not of the part on screen.
        let (body, _, _) = screen.bodies()["mevo.0"];
        let cut = Screen::sized(1600.0, (body.top() + body.height() * 0.3).round());
        let marks = settled(&cut, &mut app);
        let (spinner, _) = marks["spinner:mevo.0"];
        let (cut_body, _, _) = cut.bodies()["mevo.0"];
        assert!((cut_body.height() - body.height()).abs() < 0.5);
        assert!(
            (spinner.center().y - cut_body.center().y).abs() < 2.0,
            "{spinner:?} in {cut_body:?}"
        );

        // Connected with no rows: the mark, not the spinner.
        without_telemetry(&mut app, "mevo.0", &[ActorStatus::Connected]);
        let marks = settled(&screen, &mut app);
        assert!(marks.contains_key("connected_icon:mevo.0"));
        assert!(!marks.contains_key("spinner:mevo.0"));
        let (body, content, _) = screen.bodies()["mevo.0"];
        assert!(content <= body.height(), "nothing to scroll");

        // A row arrives: both go.
        for id in ["mevo.0", "square.0", "openconnect_client.0"] {
            app.actors
                .get_mut(id)
                .expect("running")
                .telemetry
                .insert("ready".into(), "false".into());
        }
        let marks = settled(&screen, &mut app);
        for id in ["mevo.0", "square.0", "openconnect_client.0"] {
            assert!(!marks.contains_key(&format!("spinner:{id}")), "{id}");
            assert!(!marks.contains_key(&format!("connected_icon:{id}")), "{id}");
            assert!(marks.contains_key(&format!("telemetry:{id}:ready")), "{id}");
        }
    }

    #[test]
    fn reconnect_waits_while_the_tab_is_locked_or_a_save_is_out() {
        let screen = Screen::sized(1600.0, 2400.0);
        let mut app = app_with_every_card();
        app.card_modes
            .set("openconnect_client.0", CardMode::Settings);
        edit_name(&mut app, "openconnect_client.0");
        let marks = settled(&screen, &mut app);
        for id in RECONNECTABLE {
            if id != "openconnect_client.0" {
                assert!(!enabled(&marks, &format!("reconnect:{id}")), "{id} locked");
            }
        }
        // Clicking a disabled one sends nothing.
        screen.click_mark(&mut app, &marks, "reconnect:mevo.0");
        assert!(app.reconnecting.is_empty());

        let marks = screen.click_mark(&mut app, &marks, "save:openconnect_client.0");
        assert!(app.settings.saving());
        for id in RECONNECTABLE {
            if id != "openconnect_client.0" {
                assert!(!enabled(&marks, &format!("reconnect:{id}")), "{id} saving");
            }
        }
    }
}
