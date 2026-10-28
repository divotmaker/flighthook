//! The Devices tab: the settings-only Global and Simulation cards, then one
//! card per launch monitor and output that flips between its live telemetry
//! and its settings.
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
use crate::types::ActorStatusResponse;

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

/// What a click on a device card asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CardAction {
    Mode(CardMode),
    Save,
    Cancel,
    Remove,
}

/// How a device card is drawn.
struct CardView<'a> {
    telemetry: Option<&'a ActorStatusResponse>,
    /// The side picked on the toggle.
    mode: CardMode,
    /// Its toggle and edit controls take input: nothing else is dirty and
    /// no save is out.
    editable: bool,
    can_save: bool,
    saving: bool,
    /// Why its last save failed.
    error: Option<&'a str>,
    /// Launch monitors for the routing dropdowns: (actor ID, name).
    monitors: &'a [(String, String)],
}

/// One device card. Its header holds the name, then the Telemetry |
/// Settings toggle, or Save and Cancel while its settings have unsaved
/// edits. The left stripe is the actor's status colour, dim while it is not
/// running.
fn device_card(
    ui: &mut egui::Ui,
    id: &str,
    view: &CardView<'_>,
    form: Option<&mut ActorFormEntry>,
) -> Option<CardAction> {
    let accent = view
        .telemetry
        .map_or(theme::TEXT_DIM, |a| monitoring::status_color(a.status));
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
    let tag = form
        .as_ref()
        .filter(|_| mode == CardMode::Settings)
        .map(|f| f.type_label().to_string());
    let hover = form.as_ref().map(|f| f.type_tooltip().to_string());

    theme::accent_card(ui, accent, |ui| {
        let mut action = None;
        settings::card_header(ui, &title, tag.as_deref(), hover.as_deref(), |ui| {
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
                theme::badge(ui, &actor.status.to_string().to_uppercase(), accent);
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
                ui.add_space(6.0);
                ui.add_enabled_ui(view.editable, |ui| {
                    settings::actor_fields(ui, entry, view.monitors);
                });
                if settings::remove_row(ui, id, view.editable) {
                    action = Some(CardAction::Remove);
                }
            }
            _ => {
                if let Some(actor) = view.telemetry {
                    monitoring::telemetry_body(ui, id, actor);
                }
            }
        }
        action
    })
    .inner
}

impl FlighthookApp {
    pub(crate) fn render_devices_panel(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        // Settings load with the tab, and again after a save that reloads them.
        if self.settings.fetch_due(ctx.input(|i| i.time)) {
            net::fetch_settings(ctx, &self.pending);
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
                        ActorFormEntry::Device(d) => Some((a.global_id(), d.name.clone())),
                        ActorFormEntry::Integration(_) => None,
                    })
                    .collect();

                let mut clicked: Option<(String, Option<usize>, CardAction)> = None;
                let mut added = None;
                for (group, members) in device_card_order(&self.actors, &self.settings.actors) {
                    if group == CardGroup::General {
                        if !loaded {
                            continue;
                        }
                        cards::group_heading(ui, group);
                        let editable = (!locked || self.settings.global_dirty) && !saving;
                        cards::card_grid(ui, 2, |ui, i| {
                            if i == 0 {
                                self.render_global_card(ctx, ui, editable);
                            } else {
                                self.render_simulation_card(ctx, ui, editable);
                            }
                        });
                        continue;
                    }
                    if let Some(type_key) = cards::add_heading(ui, group, loaded && !locked) {
                        added = Some(type_key);
                    }
                    cards::card_grid(ui, members.len(), |ui, i| {
                        let card = &members[i];
                        let error = self
                            .settings
                            .save_error
                            .as_ref()
                            .filter(|e| e.target.scope() == Some(card.id.as_str()))
                            .map(|e| e.message.clone());
                        let form = card.form.and_then(|idx| self.settings.actors.get_mut(idx));
                        let dirty = form.as_ref().is_some_and(|f| f.dirty());
                        let view = CardView {
                            telemetry: self.actors.get(&card.id),
                            mode: self.card_modes.get(&card.id),
                            editable: (!locked || dirty) && !saving,
                            can_save: form.as_ref().is_some_and(|f| settings::actor_valid(f)),
                            saving,
                            error: error.as_deref(),
                            monitors: &monitors,
                        };
                        if let Some(action) = device_card(ui, &card.id, &view, form) {
                            clicked = Some((card.id.clone(), card.form, action));
                        }
                    });
                }

                if let Some((id, form, action)) = clicked {
                    self.card_action(ctx, &id, form, action);
                }
                if let Some(type_key) = added {
                    self.add_device(type_key);
                }

                ui.add_space(12.0);
                if self.settings.saving() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                    });
                }
            });
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
            (CardAction::Save, Some(_)) => self.post_save(ctx, SaveTarget::Actor(id.to_string())),
            (CardAction::Cancel, Some(idx)) => self.cancel_device(idx),
            (CardAction::Remove, Some(idx)) => {
                let name = self.settings.actors[idx].name().to_string();
                self.confirm_remove = Some(PendingRemoval {
                    id: id.to_string(),
                    name,
                });
            }
            (_, None) => {}
        }
    }

    /// Add a new, unsaved card of `type_key`, showing its settings.
    pub(crate) fn add_device(&mut self, type_key: &str) {
        if let Some(id) = self.settings.add_actor(type_key) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::settings::{GeneralCard, SettingsForm};
    use crate::panels::{TABS, Tab, tab_alert};
    use crate::types::{ActorStatus, FlighthookConfig, UnitSystem};
    use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect, pos2, vec2};

    /// A saved config with a Mevo and two GSPro instances.
    fn config() -> FlighthookConfig {
        let mut form = SettingsForm::default();
        form.load_from(&FlighthookConfig::default());
        for type_key in ["mevo", "gspro", "gspro"] {
            let _ = form.add_actor(type_key);
        }
        form.to_request()
    }

    /// An app on the Devices tab with `config()` loaded and its actors
    /// running.
    fn app() -> FlighthookApp {
        let mut app = FlighthookApp::with_pending(net::new_pending());
        app.settings.load_from(&config());
        for a in &app.settings.actors {
            let status = ActorStatusResponse {
                name: a.name().to_string(),
                status: ActorStatus::Connected,
                telemetry: HashMap::from([("ready".to_string(), "true".to_string())]),
            };
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

    /// Draws the tab bar and the Devices tab, 1600×900.
    struct Screen(egui::Context);

    impl Screen {
        fn new() -> Self {
            let ctx = egui::Context::default();
            theme::install(&ctx);
            Self(ctx)
        }

        /// One pass with `events`. Returns the marked controls.
        fn frame(&self, app: &mut FlighthookApp, events: Vec<Event>) -> Marks {
            self.0.data_mut(|d| d.remove_temp::<Marks>(egui::Id::NULL));
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1600.0, 900.0))),
                events,
                ..Default::default()
            };
            let _ = self.0.run(input, |ctx| {
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
            self.0
                .data(|d| d.get_temp::<Marks>(egui::Id::NULL))
                .unwrap_or_default()
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

    #[test]
    fn the_alert_dot_sits_on_devices_while_a_connection_has_an_issue() {
        let mut app = app();
        assert_eq!(tab_alert(&app.actors), None);
        for status in [
            ActorStatus::Disconnected,
            ActorStatus::Reconnecting,
            ActorStatus::Starting,
        ] {
            app.actors.get_mut("mevo.0").unwrap().status = status;
            assert_eq!(tab_alert(&app.actors), Some(0), "{status:?}");
            assert_eq!(TABS[0].0, Tab::Devices);
        }
    }

    #[test]
    fn the_alert_dot_stays_off_while_gspro_is_not_running() {
        let mut app = app();
        for status in [ActorStatus::Reconnecting, ActorStatus::Starting] {
            app.actors.get_mut("gspro.1").unwrap().status = status;
            assert_eq!(tab_alert(&app.actors), None, "{status:?}");
        }
        app.actors.get_mut("gspro.1").unwrap().status = ActorStatus::Disconnected;
        assert_eq!(tab_alert(&app.actors), Some(0));
    }

    #[test]
    fn device_cards_cover_configured_and_running_actors_in_card_order() {
        let mut app = app();
        // Running, not in the form (a config edited by hand since the load).
        app.actors.insert(
            "mock_monitor.0".into(),
            ActorStatusResponse {
                name: "Mock".into(),
                status: ActorStatus::Connected,
                telemetry: HashMap::new(),
            },
        );
        // In the form, not running yet.
        app.actors.remove("gspro.1");
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
        assert!(ids(CardGroup::General).is_empty());
        assert_eq!(
            ids(CardGroup::LaunchMonitors),
            [("mevo.0".into(), true), ("mock_monitor.0".into(), false)]
        );
        assert_eq!(
            ids(CardGroup::Outputs),
            [
                ("gspro.0".into(), true),
                ("gspro.1".into(), true),
                ("webserver.0".into(), true),
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
        for id in ["mevo.0", "gspro.0", "gspro.1", "webserver.0"] {
            assert_eq!(app.card_modes.get(id), CardMode::Telemetry, "{id}");
            assert!(enabled(&marks, &format!("toggle:{id}")), "{id}");
            assert!(!marks.contains_key(&format!("remove:{id}")), "{id}");
        }

        let marks = screen.click(&mut app, toggle_side(&marks, "gspro.0", CardMode::Settings));
        assert_eq!(app.card_modes.get("gspro.0"), CardMode::Settings);
        assert_eq!(app.card_modes.get("gspro.1"), CardMode::Telemetry);
        assert_eq!(app.card_modes.get("mevo.0"), CardMode::Telemetry);
        // The settings side ends in Remove; nothing is dirty.
        assert!(enabled(&marks, "remove:gspro.0"));
        assert!(!marks.contains_key("remove:gspro.1"));
        assert!(!app.settings.locked());

        let marks = screen.click(
            &mut app,
            toggle_side(&marks, "gspro.0", CardMode::Telemetry),
        );
        assert_eq!(app.card_modes.get("gspro.0"), CardMode::Telemetry);
        assert!(!marks.contains_key("remove:gspro.0"));
    }

    #[test]
    fn unsaved_edits_swap_the_toggle_for_save_and_cancel() {
        let screen = Screen::new();
        let mut app = app();
        app.card_modes.set("gspro.0", CardMode::Settings);
        let marks = screen.frame(&mut app, Vec::new());
        assert!(marks.contains_key("toggle:gspro.0"));
        assert!(!marks.contains_key("save:gspro.0"));

        edit_name(&mut app, "gspro.0");
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!marks.contains_key("toggle:gspro.0"));
        assert!(enabled(&marks, "save:gspro.0"));
        assert!(enabled(&marks, "cancel:gspro.0"));
        assert!(enabled(&marks, "remove:gspro.0"));

        // An invalid form can't be saved, only cancelled.
        let idx = form_index(&app, "gspro.0");
        if let ActorFormEntry::Integration(i) = &mut app.settings.actors[idx] {
            i.address = "127.0.0.1:".into();
        }
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!enabled(&marks, "save:gspro.0"));
        assert!(enabled(&marks, "cancel:gspro.0"));

        // While a save is out, neither.
        if let ActorFormEntry::Integration(i) = &mut app.settings.actors[idx] {
            i.address = "127.0.0.1:921".into();
        }
        let _ = app.settings.begin_save(SaveTarget::Actor("gspro.0".into()));
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!enabled(&marks, "save:gspro.0"));
        assert!(!enabled(&marks, "cancel:gspro.0"));
        assert!(!enabled(&marks, "remove:gspro.0"));
    }

    #[test]
    fn cancel_puts_the_card_back_to_its_saved_settings() {
        let screen = Screen::new();
        let mut app = app();
        app.card_modes.set("gspro.1", CardMode::Settings);
        let saved = app.settings.actors[form_index(&app, "gspro.1")]
            .name()
            .to_string();
        edit_name(&mut app, "gspro.1");
        let marks = screen.frame(&mut app, Vec::new());
        assert!(app.settings.dirty());

        let marks = screen.click_mark(&mut app, &marks, "cancel:gspro.1");
        let entry = &app.settings.actors[form_index(&app, "gspro.1")];
        assert_eq!(entry.name(), saved);
        assert!(!entry.dirty());
        assert!(!app.settings.dirty());
        assert!(!app.settings.locked());
        // Back to the toggle, still on the settings side.
        assert!(enabled(&marks, "toggle:gspro.1"));
        assert_eq!(app.card_modes.get("gspro.1"), CardMode::Settings);
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

        app.card_modes.set("gspro.0", CardMode::Settings);
        app.card_modes.set("mevo.0", CardMode::Settings);
        edit_name(&mut app, "gspro.0");
        let marks = screen.frame(&mut app, Vec::new());
        assert!(app.settings.locked());
        assert!(!enabled(&marks, "tabs"));
        for id in ["mevo.0", "gspro.1", "webserver.0"] {
            assert!(!enabled(&marks, &format!("toggle:{id}")), "{id}");
        }
        assert!(!enabled(&marks, "remove:mevo.0"));
        assert!(!enabled(&marks, "add:Launch monitors"));
        assert!(!enabled(&marks, "add:Outputs"));
        assert!(enabled(&marks, "save:gspro.0"));
        assert!(enabled(&marks, "cancel:gspro.0"));

        // Clicks on the tab bar, another card's toggle or a (+) do nothing.
        screen.click(&mut app, tabs);
        assert_eq!(app.active_tab, Tab::Devices);
        screen.click(&mut app, toggle_side(&marks, "gspro.1", CardMode::Settings));
        assert_eq!(app.card_modes.get("gspro.1"), CardMode::Telemetry);
        let marks = screen.click_mark(&mut app, &marks, "add:Outputs");
        assert!(!marks.contains_key("add:Outputs:gspro"), "no menu");

        // Escape doesn't discard.
        let escape = Event::Key {
            key: Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        screen.frame(&mut app, vec![escape]);
        assert!(app.settings.actors[form_index(&app, "gspro.0")].dirty());
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
        assert!(marks.contains_key("add:Outputs:gspro"), "the menu opens");
        assert!(!marks.contains_key("add:Outputs:mevo"));
        let marks = screen.click_mark(&mut app, &marks, "add:Outputs:gspro");
        assert_eq!(app.settings.actors.len(), before + 1);
        let new = &app.settings.actors[before];
        assert_eq!(new.global_id(), "gspro.2");
        assert!(new.dirty());
        assert_eq!(app.card_modes.get("gspro.2"), CardMode::Settings);
        assert!(!marks.contains_key("add:Outputs:gspro"), "the menu closes");
        assert!(enabled(&marks, "save:gspro.2"));
        assert!(enabled(&marks, "cancel:gspro.2"));
        assert!(!enabled(&marks, "tabs"));
        assert!(!enabled(&marks, "add:Launch monitors"));

        let marks = screen.click_mark(&mut app, &marks, "cancel:gspro.2");
        assert_eq!(app.settings.actors.len(), before);
        assert!(
            app.settings
                .actors
                .iter()
                .all(|a| a.global_id() != "gspro.2")
        );
        assert_eq!(app.card_modes.get("gspro.2"), CardMode::Telemetry);
        assert!(!marks.keys().any(|k| k.ends_with("gspro.2")));
        assert!(!app.settings.locked());
        assert!(enabled(&marks, "tabs"));
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
        app.card_modes.set("gspro.0", CardMode::Settings);
        edit_name(&mut app, "gspro.0");
        let marks = screen.frame(&mut app, Vec::new());
        let marks = screen.click_mark(&mut app, &marks, "save:gspro.0");
        assert!(app.settings.saving());
        assert!(!enabled(&marks, "save:gspro.0"));
        assert!(!enabled(&marks, "cancel:gspro.0"));

        // A non-2xx reply, a body that isn't a save response, a network
        // error: all the same failure.
        reply(&mut app, Err("504 the settings were not applied in time"));
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!app.settings.saving());
        assert!(app.settings.actors[form_index(&app, "gspro.0")].dirty());
        assert!(enabled(&marks, "save:gspro.0"));
        assert!(enabled(&marks, "cancel:gspro.0"));
        assert!(enabled(&marks, "remove:gspro.0"));
        assert!(marks.contains_key("error:gspro.0"));
        assert!(!marks.contains_key("error:gspro.1"));
        assert!(!enabled(&marks, "tabs"), "still locked to the edits");

        // The saved config did not take the edit: Cancel goes back to it.
        let marks = screen.click_mark(&mut app, &marks, "cancel:gspro.0");
        assert_eq!(
            app.settings.actors[form_index(&app, "gspro.0")].name(),
            "Local GSPro"
        );
        assert!(!marks.contains_key("error:gspro.0"));
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
        let gspro = &app.settings.actors[form_index(&app, "gspro.0")];
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

    #[test]
    fn a_save_locks_every_card_and_the_reply_cleans_only_what_it_posted() {
        let screen = Screen::new();
        let mut app = app();
        app.card_modes.set("gspro.0", CardMode::Settings);
        app.card_modes.set("gspro.1", CardMode::Settings);
        edit_name(&mut app, "gspro.0");
        let marks = screen.frame(&mut app, Vec::new());
        let marks = screen.click_mark(&mut app, &marks, "save:gspro.0");
        // Out: nothing takes input, the saving card included.
        assert!(!enabled(&marks, "tabs"));
        assert!(!enabled(&marks, "add:Launch monitors"));
        assert!(!enabled(&marks, "add:Outputs"));
        assert!(!enabled(&marks, "remove:gspro.0"));
        for id in ["mevo.0", "gspro.1", "webserver.0"] {
            assert!(!enabled(&marks, &format!("toggle:{id}")), "{id}");
        }
        let posted = app.settings.actors[form_index(&app, "gspro.0")]
            .name()
            .to_string();

        // An edit that lands while the save is out stays unsaved.
        edit_name(&mut app, "gspro.0");
        reply(&mut app, Ok(()));
        let entry = &app.settings.actors[form_index(&app, "gspro.0")];
        assert!(entry.dirty());
        assert_eq!(entry.name(), format!("{posted} edited"));
        app.cancel_device(form_index(&app, "gspro.0"));
        assert_eq!(
            app.settings.actors[form_index(&app, "gspro.0")].name(),
            posted
        );
        assert!(!app.settings.locked());
    }

    #[test]
    fn a_removal_waits_for_its_reply_and_a_card_added_meanwhile_stays_unsaved() {
        let screen = Screen::new();
        let mut app = app();
        app.card_modes.set("gspro.0", CardMode::Settings);
        let marks = screen.frame(&mut app, Vec::new());
        let marks = screen.click_mark(&mut app, &marks, "remove:gspro.0");
        assert!(enabled(&marks, "confirm:remove"));
        let marks = screen.click_mark(&mut app, &marks, "confirm:remove");
        assert!(app.confirm_remove.is_none());
        assert!(app.settings.saving());
        assert!(!enabled(&marks, "add:Outputs"));
        assert!(!enabled(&marks, "tabs"));
        // The card stays until the server has removed it.
        assert!(app.settings.position("gspro.0").is_some());

        // A card that slips in while the removal is out.
        app.add_device("gspro");
        reply(&mut app, Ok(()));
        assert_eq!(app.settings.position("gspro.0"), None);
        assert_eq!(app.card_modes.get("gspro.0"), CardMode::Telemetry);
        let new = &app.settings.actors[form_index(&app, "gspro.2")];
        assert!(new.dirty(), "never saved");
        assert!(app.settings.locked());
    }

    #[test]
    fn a_failed_removal_keeps_the_card_and_says_why() {
        let screen = Screen::new();
        let mut app = app();
        app.card_modes.set("gspro.1", CardMode::Settings);
        let _ = app
            .settings
            .begin_save(SaveTarget::Remove("gspro.1".into()));
        reply(&mut app, Err("Failed to fetch"));
        let marks = screen.frame(&mut app, Vec::new());
        assert!(app.settings.position("gspro.1").is_some());
        assert!(marks.contains_key("error:gspro.1"));
        assert!(enabled(&marks, "remove:gspro.1"));
        assert!(enabled(&marks, "tabs"));
    }

    #[test]
    fn the_removal_dialog_removes_by_id_and_waits_for_a_save_out() {
        let screen = Screen::new();
        let mut app = app();
        app.confirm_remove = Some(PendingRemoval {
            id: "gspro.1".into(),
            name: "Local GSPro".into(),
        });
        // Entries move under the open dialog.
        let moved = app.settings.actors.remove(form_index(&app, "mevo.0"));
        app.settings.actors.push(moved);

        let _ = app.settings.begin_save(SaveTarget::Actor("gspro.0".into()));
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!enabled(&marks, "confirm:remove"));
        reply(&mut app, Ok(()));

        let marks = screen.frame(&mut app, Vec::new());
        screen.click_mark(&mut app, &marks, "confirm:remove");
        reply(&mut app, Ok(()));
        assert_eq!(app.settings.position("gspro.1"), None);
        for id in ["mevo.0", "gspro.0", "webserver.0"] {
            assert!(app.settings.position(id).is_some(), "{id}");
        }
    }

    #[test]
    fn an_invalid_card_blocks_only_its_own_save() {
        let screen = Screen::new();
        let mut app = app();
        // A hand-edited config: gspro.1 without an address, never opened.
        integration(&mut app, "gspro.1").address = String::new();
        app.card_modes.set("gspro.0", CardMode::Settings);
        edit_name(&mut app, "gspro.0");
        let marks = screen.frame(&mut app, Vec::new());
        assert!(enabled(&marks, "save:gspro.0"));

        app.settings.simulation.altitude_val = "99999".into();
        app.settings.mark_general(GeneralCard::Simulation);
        let marks = screen.frame(&mut app, Vec::new());
        assert!(enabled(&marks, "save:gspro.0"));
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
}
