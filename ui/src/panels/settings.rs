use crate::app::FlighthookApp;
use crate::net;
use crate::types::{
    Distance, FlighthookConfig, GsProSection, MevoSection, MockMonitorSection, PartialMode,
    RandomClubSection, UnitSystem, WebserverSection,
};

const PARTIAL_OPTIONS: &[(&str, &str)] = &[
    ("never", "Never"),
    ("chipping_only", "Chipping only"),
    ("always", "Always"),
];

const DISTANCE_UNITS: &[(&str, &str)] = &[
    ("inches", "in"),
    ("feet", "ft"),
    ("yards", "yd"),
    ("meters", "m"),
    ("centimeters", "cm"),
];

/// Format a distance value: strip trailing zeros but keep at least one decimal.
fn format_distance_value(v: f64) -> String {
    let s = format!("{:.4}", v);
    let s = s.trim_end_matches('0');
    let s = s.trim_end_matches('.');
    s.to_string()
}

fn unit_suffix(key: &str) -> &str {
    DISTANCE_UNITS
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, suffix)| *suffix)
        .unwrap_or("in")
}

fn partial_mode_to_str(m: PartialMode) -> &'static str {
    match m {
        PartialMode::Never => "never",
        PartialMode::ChippingOnly => "chipping_only",
        PartialMode::Always => "always",
    }
}

fn str_to_partial_mode(s: &str) -> PartialMode {
    match s {
        "never" => PartialMode::Never,
        "always" => PartialMode::Always,
        _ => PartialMode::ChippingOnly,
    }
}

fn combo_label<'a>(opts: &[(&str, &'a str)], value: &str) -> &'a str {
    opts.iter()
        .find(|(v, _)| *v == value)
        .map(|(_, label)| *label)
        .unwrap_or(opts[0].1)
}

/// Find the lowest unused integer key (starting at "0") in a set of existing keys.
fn next_index(existing: &[&str]) -> String {
    let mut i = 0u32;
    loop {
        let key = i.to_string();
        if !existing.iter().any(|k| *k == key) {
            return key;
        }
        i += 1;
    }
}

/// Per-device form entry. The `id` is the map index (e.g. "0"),
/// and `monitor_type` encodes which section map it belongs to.
/// Distance fields are `(value_string, unit_key)` pairs where unit_key
/// is one of "inches", "feet", "meters", "centimeters".
#[derive(Clone)]
pub(crate) struct DeviceFormEntry {
    pub(crate) id: String,
    pub(crate) monitor_type: String, // "mevo" | "mock_monitor"
    pub(crate) name: String,
    pub(crate) address: String,
    pub(crate) ball_type: u8,
    pub(crate) tee_height_val: String,
    pub(crate) tee_height_unit: String,
    pub(crate) range_val: String,
    pub(crate) range_unit: String,
    pub(crate) surface_height_val: String,
    pub(crate) surface_height_unit: String,
    pub(crate) track_pct: String,
    pub(crate) use_partial: String,
    pub(crate) dirty: bool,
}

impl DeviceFormEntry {
    pub(crate) fn from_mevo(id: &str, s: &MevoSection) -> Self {
        let tee = s.tee_height.unwrap_or(Distance::Inches(1.5));
        let rng = s.range.unwrap_or(Distance::Feet(8.0));
        let surf = s.surface_height.unwrap_or(Distance::Inches(0.0));
        let partial = s.use_partial.unwrap_or_default();
        Self {
            id: id.into(),
            monitor_type: "mevo".into(),
            name: s.name.clone(),
            address: s.address.clone().unwrap_or_default(),
            ball_type: s.ball_type.unwrap_or(0),
            tee_height_val: format_distance_value(tee.value()),
            tee_height_unit: tee.unit_key().into(),
            range_val: format_distance_value(rng.value()),
            range_unit: rng.unit_key().into(),
            surface_height_val: format_distance_value(surf.value()),
            surface_height_unit: surf.unit_key().into(),
            track_pct: format!("{:.0}", s.track_pct.unwrap_or(80.0)),
            use_partial: partial_mode_to_str(partial).into(),
            dirty: false,
        }
    }

    pub(crate) fn from_mock(id: &str, s: &MockMonitorSection) -> Self {
        Self {
            id: id.into(),
            monitor_type: "mock_monitor".into(),
            name: s.name.clone(),
            address: String::new(),
            ball_type: 0,
            tee_height_val: "1.5".into(),
            tee_height_unit: "inches".into(),
            range_val: "8".into(),
            range_unit: "feet".into(),
            surface_height_val: "0".into(),
            surface_height_unit: "inches".into(),
            track_pct: "80".into(),
            use_partial: "chipping_only".into(),
            dirty: false,
        }
    }

    pub(crate) fn is_mock(&self) -> bool {
        self.monitor_type == "mock_monitor"
    }
}

/// Per-integration form entry. The `id` is the map index,
/// `integration_type` encodes which section map it belongs to.
#[derive(Clone)]
pub(crate) struct IntegrationFormEntry {
    pub(crate) id: String,
    pub(crate) integration_type: String, // "gspro" | "random_club"
    pub(crate) name: String,
    pub(crate) address: String,
    /// Routing: actor ID for full-swing monitor, or empty = "Any".
    pub(crate) full_monitor: String,
    /// Routing: actor ID for chipping monitor, or empty = "Any".
    pub(crate) chipping_monitor: String,
    /// Routing: actor ID for putting monitor, or empty = "Any".
    pub(crate) putting_monitor: String,
    pub(crate) dirty: bool,
}

/// Unified actor form entry wrapping both device and integration entries.
#[derive(Clone)]
pub(crate) enum ActorFormEntry {
    Device(DeviceFormEntry),
    Integration(IntegrationFormEntry),
}

impl ActorFormEntry {
    pub(crate) fn name(&self) -> &str {
        match self {
            ActorFormEntry::Device(d) => &d.name,
            ActorFormEntry::Integration(i) => &i.name,
        }
    }

    pub(crate) fn dirty(&self) -> bool {
        match self {
            ActorFormEntry::Device(d) => d.dirty,
            ActorFormEntry::Integration(i) => i.dirty,
        }
    }

    pub(crate) fn set_dirty(&mut self, v: bool) {
        match self {
            ActorFormEntry::Device(d) => d.dirty = v,
            ActorFormEntry::Integration(i) => i.dirty = v,
        }
    }

    pub(crate) fn type_label(&self) -> &str {
        match self {
            ActorFormEntry::Device(d) => match d.monitor_type.as_str() {
                "mevo" => "Mevo",
                "mock_monitor" => "Mock",
                _ => &d.monitor_type,
            },
            ActorFormEntry::Integration(i) => match i.integration_type.as_str() {
                "gspro" => "GSPro",
                "random_club" => "Random Club",
                "webserver" => "Web",
                _ => &i.integration_type,
            },
        }
    }

    /// Sort key: (name, type_label, id) for stable ordering.
    fn sort_key(&self) -> (&str, &str, &str) {
        match self {
            ActorFormEntry::Device(d) => (&d.name, self.type_label(), &d.id),
            ActorFormEntry::Integration(i) => (&i.name, self.type_label(), &i.id),
        }
    }
}

/// Pending removal confirmation: (index in actors Vec, display name).
pub(crate) struct PendingRemoval(pub(crate) usize, pub(crate) String);

/// Which section is being saved.
#[derive(Clone, Debug)]
pub(crate) enum SaveTarget {
    /// Global settings (default units).
    Global,
    /// Actor at the given index in the actors Vec.
    Actor(usize),
    /// Full config (e.g. after removal).
    Full,
}

/// Settings form state.
#[derive(Clone)]
pub(crate) struct SettingsForm {
    pub(crate) default_units: UnitSystem,
    pub(crate) global_dirty: bool,
    pub(crate) actors: Vec<ActorFormEntry>,
    pub(crate) loaded: bool,
    pub(crate) dirty: bool,
    pub(crate) saving: bool,
    pub(crate) save_target: Option<SaveTarget>,
    /// Snapshot of the config at last load/save — used to build scoped requests.
    original_config: Option<FlighthookConfig>,
}

impl Default for SettingsForm {
    fn default() -> Self {
        Self {
            default_units: UnitSystem::default(),
            global_dirty: false,
            actors: Vec::new(),
            loaded: false,
            dirty: false,
            saving: false,
            save_target: None,
            original_config: None,
        }
    }
}

impl SettingsForm {
    pub(crate) fn load_from(&mut self, s: &FlighthookConfig) {
        self.original_config = Some(s.clone());
        self.default_units = s.default_units;
        self.global_dirty = false;

        self.actors.clear();
        for (id, section) in &s.mevo {
            self.actors
                .push(ActorFormEntry::Device(DeviceFormEntry::from_mevo(
                    id, section,
                )));
        }
        for (id, section) in &s.mock_monitor {
            self.actors
                .push(ActorFormEntry::Device(DeviceFormEntry::from_mock(
                    id, section,
                )));
        }
        for (id, section) in &s.gspro {
            self.actors
                .push(ActorFormEntry::Integration(IntegrationFormEntry {
                    id: id.clone(),
                    integration_type: "gspro".into(),
                    name: section.name.clone(),
                    address: section.address.clone().unwrap_or_default(),
                    full_monitor: section.full_monitor.clone().unwrap_or_default(),
                    chipping_monitor: section.chipping_monitor.clone().unwrap_or_default(),
                    putting_monitor: section.putting_monitor.clone().unwrap_or_default(),
                    dirty: false,
                }));
        }
        for (id, section) in &s.random_club {
            self.actors
                .push(ActorFormEntry::Integration(IntegrationFormEntry {
                    id: id.clone(),
                    integration_type: "random_club".into(),
                    name: section.name.clone(),
                    address: String::new(),
                    full_monitor: String::new(),
                    chipping_monitor: String::new(),
                    putting_monitor: String::new(),
                    dirty: false,
                }));
            let _ = section;
        }
        for (id, section) in &s.webserver {
            self.actors
                .push(ActorFormEntry::Integration(IntegrationFormEntry {
                    id: id.clone(),
                    integration_type: "webserver".into(),
                    name: section.name.clone(),
                    address: section.bind.clone(),
                    full_monitor: String::new(),
                    chipping_monitor: String::new(),
                    putting_monitor: String::new(),
                    dirty: false,
                }));
        }
        self.actors.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));

        self.loaded = true;
        self.dirty = false;
    }

    pub(crate) fn is_valid(&self) -> bool {
        for actor in &self.actors {
            match actor {
                ActorFormEntry::Device(dev) => {
                    if dev.name.is_empty() {
                        return false;
                    }
                    if !dev.is_mock() && dev.address.parse::<std::net::SocketAddr>().is_err() {
                        return false;
                    }
                }
                ActorFormEntry::Integration(entry) => {
                    if entry.name.is_empty() {
                        return false;
                    }
                    if entry.integration_type != "random_club"
                        && entry.address.parse::<std::net::SocketAddr>().is_err()
                    {
                        return false;
                    }
                }
            }
        }
        true
    }

    pub(crate) fn to_request(&self) -> FlighthookConfig {
        let mut webserver = std::collections::HashMap::new();
        let mut mevo = std::collections::HashMap::new();
        let mut mock_monitor = std::collections::HashMap::new();
        let mut gspro = std::collections::HashMap::new();
        let mut random_club = std::collections::HashMap::new();

        for actor in &self.actors {
            match actor {
                ActorFormEntry::Device(dev) => match dev.monitor_type.as_str() {
                    "mevo" => {
                        mevo.insert(
                            dev.id.clone(),
                            MevoSection {
                                name: dev.name.clone(),
                                address: if dev.address.is_empty() {
                                    None
                                } else {
                                    Some(dev.address.clone())
                                },
                                ball_type: Some(dev.ball_type),
                                tee_height: dev.tee_height_val.parse::<f64>().ok().map(|v| {
                                    Distance::from_value_and_unit(v, &dev.tee_height_unit)
                                }),
                                range: dev
                                    .range_val
                                    .parse::<f64>()
                                    .ok()
                                    .map(|v| Distance::from_value_and_unit(v, &dev.range_unit)),
                                surface_height: dev.surface_height_val.parse::<f64>().ok().map(
                                    |v| Distance::from_value_and_unit(v, &dev.surface_height_unit),
                                ),
                                track_pct: dev.track_pct.parse().ok(),
                                use_partial: Some(str_to_partial_mode(&dev.use_partial)),
                            },
                        );
                    }
                    "mock_monitor" => {
                        mock_monitor.insert(
                            dev.id.clone(),
                            MockMonitorSection {
                                name: dev.name.clone(),
                            },
                        );
                    }
                    _ => {}
                },
                ActorFormEntry::Integration(entry) => match entry.integration_type.as_str() {
                    "gspro" => {
                        gspro.insert(
                            entry.id.clone(),
                            GsProSection {
                                name: entry.name.clone(),
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
                            },
                        );
                    }
                    "random_club" => {
                        random_club.insert(
                            entry.id.clone(),
                            RandomClubSection {
                                name: entry.name.clone(),
                            },
                        );
                    }
                    "webserver" => {
                        webserver.insert(
                            entry.id.clone(),
                            WebserverSection {
                                name: entry.name.clone(),
                                bind: entry.address.clone(),
                            },
                        );
                    }
                    _ => {}
                },
            }
        }

        FlighthookConfig {
            default_units: self.default_units,
            webserver,
            mevo,
            mock_monitor,
            gspro,
            random_club,
        }
    }

    /// Build a config that applies only the global settings change on top of the
    /// original config.
    pub(crate) fn build_global_request(&self) -> FlighthookConfig {
        let mut config = self
            .original_config
            .clone()
            .unwrap_or_else(|| self.to_request());
        config.default_units = self.default_units;
        config
    }

    /// Build a config that applies only the actor at `idx` on top of the
    /// original config. Returns `(config, global_id)`.
    pub(crate) fn build_actor_request(&self, idx: usize) -> (FlighthookConfig, String) {
        let mut config = self
            .original_config
            .clone()
            .unwrap_or_else(|| self.to_request());
        let actor = &self.actors[idx];
        let global_id = apply_actor_to_config(&mut config, actor);
        (config, global_id)
    }

    /// After a successful scoped save, update the original config to reflect
    /// the saved values for the given target.
    pub(crate) fn update_original_after_save(&mut self, target: &SaveTarget) {
        match target {
            SaveTarget::Global => {
                if let Some(ref mut orig) = self.original_config {
                    orig.default_units = self.default_units;
                }
            }
            SaveTarget::Actor(idx) => {
                // Clone the actor to avoid borrow conflict
                let actor = match self.actors.get(*idx) {
                    Some(a) => a.clone(),
                    None => return,
                };
                if let Some(ref mut orig) = self.original_config {
                    apply_actor_to_config(orig, &actor);
                }
            }
            SaveTarget::Full => {
                // Full save — replace original with current form state
                let new_config = self.to_request();
                self.original_config = Some(new_config);
            }
        }
    }
}

/// Apply a single actor form entry's data into a FlighthookConfig.
/// Returns the actor's global ID (e.g. "mevo.0").
fn apply_actor_to_config(config: &mut FlighthookConfig, actor: &ActorFormEntry) -> String {
    match actor {
        ActorFormEntry::Device(dev) => {
            let global_id = format!("{}.{}", dev.monitor_type, dev.id);
            match dev.monitor_type.as_str() {
                "mevo" => {
                    config.mevo.insert(
                        dev.id.clone(),
                        MevoSection {
                            name: dev.name.clone(),
                            address: if dev.address.is_empty() {
                                None
                            } else {
                                Some(dev.address.clone())
                            },
                            ball_type: Some(dev.ball_type),
                            tee_height: dev
                                .tee_height_val
                                .parse::<f64>()
                                .ok()
                                .map(|v| Distance::from_value_and_unit(v, &dev.tee_height_unit)),
                            range: dev
                                .range_val
                                .parse::<f64>()
                                .ok()
                                .map(|v| Distance::from_value_and_unit(v, &dev.range_unit)),
                            surface_height: dev.surface_height_val.parse::<f64>().ok().map(|v| {
                                Distance::from_value_and_unit(v, &dev.surface_height_unit)
                            }),
                            track_pct: dev.track_pct.parse().ok(),
                            use_partial: Some(str_to_partial_mode(&dev.use_partial)),
                        },
                    );
                }
                "mock_monitor" => {
                    config.mock_monitor.insert(
                        dev.id.clone(),
                        MockMonitorSection {
                            name: dev.name.clone(),
                        },
                    );
                }
                _ => {}
            }
            global_id
        }
        ActorFormEntry::Integration(entry) => {
            let global_id = format!("{}.{}", entry.integration_type, entry.id);
            match entry.integration_type.as_str() {
                "gspro" => {
                    config.gspro.insert(
                        entry.id.clone(),
                        GsProSection {
                            name: entry.name.clone(),
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
                        },
                    );
                }
                "random_club" => {
                    config.random_club.insert(
                        entry.id.clone(),
                        RandomClubSection {
                            name: entry.name.clone(),
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

impl FlighthookApp {
    pub(crate) fn render_settings_panel(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        // Lazy-load settings on first render of this tab
        if !self.settings.loaded {
            net::fetch_settings(ctx, &self.pending);
        }

        let field_width = 200.0;

        egui::ScrollArea::both()
            .auto_shrink(false)
            .show(ui, |ui| {
                // --- GLOBAL ---
                ui.label(
                    egui::RichText::new("Global")
                        .strong()
                        .color(egui::Color32::from_rgb(180, 200, 255)),
                );
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label("Default Units:").on_hover_text("Default unit system for shot display. Can be toggled per-session in the Shots tab.");
                    let units_label = match self.settings.default_units {
                        UnitSystem::Imperial => "Imperial",
                        UnitSystem::Metric => "Metric",
                    };
                    egui::ComboBox::from_id_salt("default_units")
                        .selected_text(units_label)
                        .width(field_width)
                        .show_ui(ui, |ui| {
                            if ui.selectable_label(self.settings.default_units == UnitSystem::Imperial, "Imperial").clicked() {
                                self.settings.default_units = UnitSystem::Imperial;
                                self.settings.global_dirty = true;
                                self.settings.dirty = true;
                                self.units_toggle = UnitSystem::Imperial;
                            }
                            if ui.selectable_label(self.settings.default_units == UnitSystem::Metric, "Metric").clicked() {
                                self.settings.default_units = UnitSystem::Metric;
                                self.settings.global_dirty = true;
                                self.settings.dirty = true;
                                self.units_toggle = UnitSystem::Metric;
                            }
                        });
                    let save_btn = if self.settings.global_dirty {
                        egui::Button::new(
                            egui::RichText::new("Save").size(11.0).color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(200, 50, 50))
                    } else {
                        egui::Button::new(egui::RichText::new("Save").size(11.0))
                    };
                    if ui.add_enabled(self.settings.global_dirty && self.settings.is_valid() && !self.settings.saving, save_btn).clicked() {
                        self.settings.saving = true;
                        self.settings.save_target = Some(crate::panels::settings::SaveTarget::Global);
                        let req = self.settings.build_global_request();
                        net::post_settings(ctx, &self.pending, &req, None);
                    }
                    if ui.button(egui::RichText::new("API Docs").size(11.0)).clicked() {
                        self.show_api_docs = true;
                    }
                });

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);

                // --- ACTORS (devices + integrations) ---
                let mut save_idx = None;
                let settings_valid = self.settings.is_valid();
                let settings_saving = self.settings.saving;

                // Collect device actor IDs for routing dropdowns (before mutable iteration)
                let device_monitor_options: Vec<(String, String)> = self.settings.actors.iter()
                    .filter_map(|a| match a {
                        ActorFormEntry::Device(d) => {
                            let global_id = format!("{}.{}", d.monitor_type, d.id);
                            Some((global_id, d.name.clone()))
                        }
                        _ => None,
                    })
                    .collect();

                for (idx, actor) in self.settings.actors.iter_mut().enumerate() {
                    let type_label = actor.type_label().to_string();
                    let dirty = actor.dirty();

                    // Header: name + type badge + Remove + Save
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(actor.name())
                                .strong()
                                .color(egui::Color32::from_rgb(180, 200, 255)),
                        );
                        egui::Frame::new()
                            .fill(egui::Color32::from_rgb(60, 80, 120))
                            .corner_radius(4.0)
                            .inner_margin(egui::Margin::symmetric(6, 2))
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(&type_label)
                                        .size(11.0)
                                        .color(egui::Color32::from_rgb(200, 220, 255)),
                                );
                            });
                        if ui
                            .button(egui::RichText::new("Remove").size(11.0))
                            .clicked()
                        {
                            self.confirm_remove = Some(PendingRemoval(idx, actor.name().to_string()));
                        }
                        let save_btn = if dirty {
                            egui::Button::new(
                                egui::RichText::new("Save").size(11.0).color(egui::Color32::WHITE),
                            )
                            .fill(egui::Color32::from_rgb(200, 50, 50))
                        } else {
                            egui::Button::new(egui::RichText::new("Save").size(11.0))
                        };
                        if ui.add_enabled(dirty && settings_valid && !settings_saving, save_btn).clicked() {
                            save_idx = Some(idx);
                        }
                    });

                    // Type-specific fields
                    match actor {
                        ActorFormEntry::Device(dev) => {
                            // Name
                            ui.horizontal(|ui| {
                                ui.add_space(16.0);
                                ui.label("Name:").on_hover_text("Display name for this device in the UI and logs.");
                                if ui
                                    .add(egui::TextEdit::singleline(&mut dev.name).desired_width(field_width))
                                    .changed()
                                {
                                    dev.dirty = true;
                                }
                            });

                            if !dev.is_mock() {
                                // Address
                                ui.horizontal(|ui| {
                                    ui.add_space(16.0);
                                    ui.label("Address:").on_hover_text("TCP address of the launch monitor. Connect to the device WiFi AP first.");
                                    if ui
                                        .add(egui::TextEdit::singleline(&mut dev.address).desired_width(field_width))
                                        .on_hover_text("ip:port (e.g. 192.168.2.1:5100)")
                                        .changed()
                                    {
                                        dev.dirty = true;
                                    }
                                    if dev.address.parse::<std::net::SocketAddr>().is_err() {
                                        ui.label(
                                            egui::RichText::new("Invalid address")
                                                .color(egui::Color32::from_rgb(255, 80, 80))
                                                .size(11.0),
                                        );
                                    }
                                });

                                // Ball Type
                                ui.horizontal(|ui| {
                                    ui.add_space(16.0);
                                    ui.label("Ball Type:").on_hover_text("RCT = Radar Capture Technology.\nStandard = any regular golf ball.");
                                    let ball_text = if dev.ball_type == 0 { "RCT" } else { "Standard" };
                                    egui::ComboBox::from_id_salt(format!("ball_type_{}", dev.id))
                                        .selected_text(ball_text)
                                        .width(field_width)
                                        .show_ui(ui, |ui| {
                                            if ui.selectable_label(dev.ball_type == 0, "RCT").clicked() {
                                                dev.ball_type = 0;
                                                dev.dirty = true;
                                            }
                                            if ui.selectable_label(dev.ball_type == 1, "Standard").clicked() {
                                                dev.ball_type = 1;
                                                dev.dirty = true;
                                            }
                                        });
                                });

                                // Tee Height
                                ui.horizontal(|ui| {
                                    ui.add_space(16.0);
                                    ui.label("Tee Height:").on_hover_text("Height of the tee above the hitting surface.");
                                    if ui
                                        .add(egui::TextEdit::singleline(&mut dev.tee_height_val).desired_width(field_width))
                                        .changed()
                                    {
                                        dev.dirty = true;
                                    }
                                    egui::ComboBox::from_id_salt(format!("tee_unit_{}", dev.id))
                                        .selected_text(unit_suffix(&dev.tee_height_unit))
                                        .width(50.0)
                                        .show_ui(ui, |ui| {
                                            for &(key, label) in DISTANCE_UNITS {
                                                if ui.selectable_label(dev.tee_height_unit == key, label).clicked() {
                                                    dev.tee_height_unit = key.to_string();
                                                    dev.dirty = true;
                                                }
                                            }
                                        });
                                });

                                // Monitor-to-Ball
                                ui.horizontal(|ui| {
                                    ui.add_space(16.0);
                                    ui.label("Monitor-to-Ball:").on_hover_text("Distance from the front of the launch monitor to the ball.\nMevo+ recommended range: 7-9 ft.");
                                    if ui
                                        .add(egui::TextEdit::singleline(&mut dev.range_val).desired_width(field_width))
                                        .changed()
                                    {
                                        dev.dirty = true;
                                    }
                                    egui::ComboBox::from_id_salt(format!("range_unit_{}", dev.id))
                                        .selected_text(unit_suffix(&dev.range_unit))
                                        .width(50.0)
                                        .show_ui(ui, |ui| {
                                            for &(key, label) in DISTANCE_UNITS {
                                                if ui.selectable_label(dev.range_unit == key, label).clicked() {
                                                    dev.range_unit = key.to_string();
                                                    dev.dirty = true;
                                                }
                                            }
                                        });
                                });

                                // Surface Height
                                ui.horizontal(|ui| {
                                    ui.add_space(16.0);
                                    ui.label("Surface Height:").on_hover_text("Height of the hitting surface above the monitor.\nSet to 0 if the ball and monitor are on the same level.");
                                    if ui
                                        .add(egui::TextEdit::singleline(&mut dev.surface_height_val).desired_width(field_width))
                                        .changed()
                                    {
                                        dev.dirty = true;
                                    }
                                    egui::ComboBox::from_id_salt(format!("surface_unit_{}", dev.id))
                                        .selected_text(unit_suffix(&dev.surface_height_unit))
                                        .width(50.0)
                                        .show_ui(ui, |ui| {
                                            for &(key, label) in DISTANCE_UNITS {
                                                if ui.selectable_label(dev.surface_height_unit == key, label).clicked() {
                                                    dev.surface_height_unit = key.to_string();
                                                    dev.dirty = true;
                                                }
                                            }
                                        });
                                });

                                // Track %
                                ui.horizontal(|ui| {
                                    ui.add_space(16.0);
                                    ui.label("Track %:").on_hover_text("Minimum outdoor tracking percentage (0-100).\nLower values accept shorter-tracked shots. GSPro default: 100, FS Golf default: 60.");
                                    if ui
                                        .add(egui::TextEdit::singleline(&mut dev.track_pct).desired_width(field_width))
                                        .changed()
                                    {
                                        dev.dirty = true;
                                    }
                                    ui.label("%");
                                });

                                // Partial
                                ui.horizontal(|ui| {
                                    ui.add_space(16.0);
                                    ui.label("Partial:").on_hover_text("Accept partial (E8-only) shot results when the device\ncannot compute a full flight result (D4).\nCommon for short chips and marginal shots.\n\nNever: only accept full results.\nChipping only: allow partial in chipping mode.\nAlways: allow partial in any mode.");
                                    egui::ComboBox::from_id_salt(format!("partial_{}", dev.id))
                                        .selected_text(combo_label(PARTIAL_OPTIONS, &dev.use_partial))
                                        .width(field_width)
                                        .show_ui(ui, |ui| {
                                            for &(value, label) in PARTIAL_OPTIONS {
                                                if ui
                                                    .selectable_label(dev.use_partial == value, label)
                                                    .clicked()
                                                {
                                                    dev.use_partial = value.to_string();
                                                    dev.dirty = true;
                                                }
                                            }
                                        });
                                });
                            }
                        }
                        ActorFormEntry::Integration(entry) => {
                            // Name
                            ui.horizontal(|ui| {
                                ui.add_space(16.0);
                                ui.label("Name:").on_hover_text("Display name for this integration in the UI and logs.");
                                if ui
                                    .add(egui::TextEdit::singleline(&mut entry.name).desired_width(field_width))
                                    .changed()
                                {
                                    entry.dirty = true;
                                }
                            });

                            // Address field (skip for mock)
                            if entry.integration_type != "random_club" {
                                ui.horizontal(|ui| {
                                    ui.add_space(16.0);
                                    ui.label("Address:").on_hover_text("TCP address of the simulator. Shot data is forwarded here as JSON.");
                                    if ui
                                        .add(egui::TextEdit::singleline(&mut entry.address).desired_width(field_width))
                                        .on_hover_text("ip:port (e.g. 127.0.0.1:921)")
                                        .changed()
                                    {
                                        entry.dirty = true;
                                    }
                                    if entry.address.parse::<std::net::SocketAddr>().is_err() {
                                        ui.label(
                                            egui::RichText::new("Invalid address")
                                                .color(egui::Color32::from_rgb(255, 80, 80))
                                                .size(11.0),
                                        );
                                    }
                                });
                            }

                            // Routing dropdowns (GSPro only)
                            if entry.integration_type == "gspro" {
                                for (field_label, field_val, salt) in [
                                    ("Full Monitor:", &mut entry.full_monitor, "full"),
                                    ("Chipping Monitor:", &mut entry.chipping_monitor, "chipping"),
                                    ("Putting Monitor:", &mut entry.putting_monitor, "putting"),
                                ] {
                                    ui.horizontal(|ui| {
                                        ui.add_space(16.0);
                                        ui.label(field_label).on_hover_text(
                                            "Which launch monitor to accept shots from for this mode.\n\"Any\" accepts shots from all monitors."
                                        );
                                        let display = if field_val.is_empty() {
                                            "Any"
                                        } else {
                                            device_monitor_options.iter()
                                                .find(|(id, _)| id == field_val.as_str())
                                                .map(|(_, name)| name.as_str())
                                                .unwrap_or(field_val.as_str())
                                        };
                                        egui::ComboBox::from_id_salt(format!("{}_{}", salt, entry.id))
                                            .selected_text(display)
                                            .width(field_width)
                                            .show_ui(ui, |ui| {
                                                if ui.selectable_label(field_val.is_empty(), "Any").clicked() {
                                                    field_val.clear();
                                                    entry.dirty = true;
                                                }
                                                for (monitor_id, monitor_name) in &device_monitor_options {
                                                    if ui.selectable_label(field_val.as_str() == monitor_id, monitor_name).clicked() {
                                                        *field_val = monitor_id.clone();
                                                        entry.dirty = true;
                                                    }
                                                }
                                            });
                                    });
                                }
                            }
                        }
                    }
                    ui.add_space(4.0);
                }

                // Recalculate global dirty from per-entry flags
                self.settings.dirty = self.settings.global_dirty
                    || self.settings.actors.iter().any(|a| a.dirty());

                if let Some(idx) = save_idx {
                    self.settings.saving = true;
                    let (req, scope) = self.settings.build_actor_request(idx);
                    self.settings.save_target = Some(crate::panels::settings::SaveTarget::Actor(idx));
                    net::post_settings(ctx, &self.pending, &req, Some(&scope));
                }

                // Add dropdown (all actor types)
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("add_actor")
                        .selected_text("+ Add")
                        .show_ui(ui, |ui| {
                            if ui.selectable_label(false, "Mevo").clicked() {
                                let existing: Vec<&str> = self.settings.actors.iter()
                                    .filter_map(|a| match a {
                                        ActorFormEntry::Device(d) if d.monitor_type == "mevo" => Some(d.id.as_str()),
                                        _ => None,
                                    })
                                    .collect();
                                let id = next_index(&existing);
                                self.settings.actors.push(ActorFormEntry::Device(DeviceFormEntry {
                                    id,
                                    monitor_type: "mevo".into(),
                                    name: "Mevo WiFi".into(),
                                    address: "192.168.2.1:5100".into(),
                                    ball_type: 0,
                                    tee_height_val: "1.5".into(),
                                    tee_height_unit: "inches".into(),
                                    range_val: "8".into(),
                                    range_unit: "feet".into(),
                                    surface_height_val: "0".into(),
                                    surface_height_unit: "inches".into(),
                                    track_pct: "80".into(),
                                    use_partial: "chipping_only".into(),
                                    dirty: true,
                                }));
                                self.settings.dirty = true;
                            }
                            if ui.selectable_label(false, "GSPro").clicked() {
                                let existing: Vec<&str> = self.settings.actors.iter()
                                    .filter_map(|a| match a {
                                        ActorFormEntry::Integration(i) if i.integration_type == "gspro" => Some(i.id.as_str()),
                                        _ => None,
                                    })
                                    .collect();
                                let id = next_index(&existing);
                                self.settings.actors.push(ActorFormEntry::Integration(IntegrationFormEntry {
                                    id,
                                    integration_type: "gspro".into(),
                                    name: "Local GSPro".into(),
                                    address: "127.0.0.1:921".into(),
                                    full_monitor: String::new(),
                                    chipping_monitor: String::new(),
                                    putting_monitor: String::new(),
                                    dirty: true,
                                }));
                                self.settings.dirty = true;
                            }
                            if ui.selectable_label(false, "Web Server").clicked() {
                                let existing: Vec<&str> = self.settings.actors.iter()
                                    .filter_map(|a| match a {
                                        ActorFormEntry::Integration(i) if i.integration_type == "webserver" => Some(i.id.as_str()),
                                        _ => None,
                                    })
                                    .collect();
                                let id = next_index(&existing);
                                self.settings.actors.push(ActorFormEntry::Integration(IntegrationFormEntry {
                                    id,
                                    integration_type: "webserver".into(),
                                    name: "Web Server".into(),
                                    address: "0.0.0.0:3030".into(),
                                    full_monitor: String::new(),
                                    chipping_monitor: String::new(),
                                    putting_monitor: String::new(),
                                    dirty: true,
                                }));
                                self.settings.dirty = true;
                            }
                        });
                });

                ui.add_space(12.0);

                // --- Status indicators ---
                if self.settings.saving {
                    ui.horizontal(|ui| {
                        ui.spinner();
                    });
                }
            });
    }
}
