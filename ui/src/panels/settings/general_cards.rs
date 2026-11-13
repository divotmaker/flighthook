//! Rendering the General and Cameras sections' settings-only cards (Global, Simulation, Replay,
//! Body tracking) and their save/cancel (`impl FlighthookApp`).

use super::*;

impl FlighthookApp {
    /// Post a save of `target`.
    pub(crate) fn post_save(&mut self, ctx: &egui::Context, target: SaveTarget) {
        let req = self.settings.begin_save(target.clone());
        net::post_settings(ctx, &self.pending, &req, target.scope());
    }

    /// Save the Global and Simulation cards: one request for both.
    pub(crate) fn save_general(&mut self, ctx: &egui::Context) {
        self.post_save(ctx, SaveTarget::Global);
    }

    /// Cancel the Global and Simulation cards together. The Shots tab's
    /// units go back with the setting.
    pub(crate) fn cancel_general(&mut self) {
        self.settings.cancel_general();
    }

    /// The unit system every Shots view and the exports use: the Units
    /// setting, which the Shots tab follows as soon as it is changed on the
    /// Global card (Cancel puts the saved one back).
    pub(crate) fn units(&self) -> UnitSystem {
        self.settings.default_units
    }

    /// Change the Units setting on the Global card.
    pub(crate) fn set_default_units(&mut self, units: UnitSystem) {
        self.settings.default_units = units;
        self.settings
            .simulation
            .set_fahrenheit(units == UnitSystem::Imperial);
        self.settings
            .golfers
            .set_inches(units == UnitSystem::Imperial);
        self.settings.mark_general(GeneralCard::Global);
    }

    /// A General card's header buttons, right to left: Save and Cancel
    /// while `card` has unsaved edits.
    pub(super) fn general_buttons(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        card: GeneralCard,
    ) -> bool {
        if !self.settings.general_card_dirty(card) {
            return false;
        }
        let key = match card {
            GeneralCard::Global => "global",
            GeneralCard::Simulation => "simulation",
            GeneralCard::Replay => "replay",
            GeneralCard::BodyTracking => "body_tracking",
        };
        let can_save = self.settings.general_valid();
        match save_cancel(ui, key, can_save, self.settings.saving()) {
            Some(Pending::Save) => self.save_general(ctx),
            Some(Pending::Cancel) => self.cancel_general(),
            None => {}
        }
        true
    }

    /// A failed Global save's reason, on a dirty General card.
    pub(super) fn general_error(&self, ui: &mut egui::Ui, key: &str) {
        if let Some(error) = &self.settings.save_error
            && error.target == SaveTarget::Global
        {
            save_error_line(ui, key, &error.message);
        }
    }

    /// The Global card: units, default shot view and the Auto-Chip UI
    /// helper. Its edit controls are disabled unless `editable`.
    pub(crate) fn render_global_card(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        editable: bool,
        height: f32,
    ) {
        let card = devices::fixed_card(ui, "global", theme::HEADING, height, |card| {
            let ui = card.top();
            let dirty = card_header(ui, "global", "Global", None, None, |ui| {
                let dirty = self.general_buttons(ctx, ui, GeneralCard::Global);
                if ui
                    .button(egui::RichText::new("API Docs").size(11.0))
                    .clicked()
                {
                    self.show_api_docs = true;
                }
                dirty
            });
            if dirty {
                lock_hint(ui);
                self.general_error(ui, "global");
            }
            card.body(|ui| {
                ui.add_space(6.0);
                form_rows(ui, |ui| {
                    ui.add_enabled_ui(editable, |ui| self.global_fields(ui))
                });
            });
        });
        devices::mark(ui, || "card:global".to_owned(), &card.response);
    }

    /// The Global card's fields.
    pub(super) fn global_fields(&mut self, ui: &mut egui::Ui) {
        let (field_width, _) = field_widths(ui);

        ui.horizontal(|ui| {
            field_label(ui, "Units:").on_hover_text(
                "Unit system for shot display and exports. The Shots tab follows it.",
            );
            let units_label = match self.settings.default_units {
                UnitSystem::Imperial => "Imperial",
                UnitSystem::Metric => "Metric",
            };
            let combo = egui::ComboBox::from_id_salt("default_units")
                .selected_text(units_label)
                .width(field_width)
                .show_ui(ui, |ui| {
                    for (units, label) in [
                        (UnitSystem::Imperial, "Imperial"),
                        (UnitSystem::Metric, "Metric"),
                    ] {
                        let item = ui.selectable_label(self.settings.default_units == units, label);
                        devices::mark(ui, || format!("units:{}", label.to_lowercase()), &item);
                        if item.clicked() {
                            self.set_default_units(units);
                        }
                    }
                });
            devices::mark(ui, || "units".to_owned(), &combo.response);
        });
        // A fixed row height, the segmented control's, so the label
        // centers on it.
        let row = egui::vec2(ui.available_width(), 36.0);
        ui.allocate_ui_with_layout(row, egui::Layout::left_to_right(egui::Align::Center), |ui| {
                field_label(ui, "Default shot view:").on_hover_text("Which view the Shots tab opens in. Sets are always recorded; this only changes how they're shown.");
                let modes = crate::groups::GROUPING_MODES;
                let selected = modes
                    .iter()
                    .position(|(m, _)| *m == self.settings.shot_grouping)
                    .unwrap_or(0);
                let labels = modes.map(|(_, label)| label);
                if let Some(i) = theme::segmented(ui, &labels, selected, None).inner {
                    self.settings.shot_grouping = modes[i].0;
                    self.settings.mark_general(GeneralCard::Global);
                }
            });
        ui.horizontal(|ui| {
            field_label(ui, "Auto-Chip Helper:").on_hover_text(AUTO_CHIP_SETTING_HOVER);
            let shown = &mut self.settings.simulation.auto_chip;
            let mut changed = false;
            egui::ComboBox::from_id_salt("auto_chip_helper")
                .selected_text(if *shown { "Show" } else { "Hide" })
                .width(field_width)
                .show_ui(ui, |ui| {
                    for (value, label) in [(true, "Show"), (false, "Hide")] {
                        if ui.selectable_label(*shown == value, label).clicked() && *shown != value
                        {
                            *shown = value;
                            changed = true;
                        }
                    }
                });
            if changed {
                self.settings.mark_general(GeneralCard::Global);
            }
        });
    }

    /// The Simulation card: simulated values (beta), saved with the Global
    /// card. Its edit controls are disabled unless `editable`.
    pub(crate) fn render_simulation_card(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        editable: bool,
        height: f32,
    ) {
        let card = devices::fixed_card(ui, "simulation", theme::HEADING, height, |card| {
            let ui = card.top();
            let dirty = card_header(
                ui,
                "simulation",
                "Simulation",
                None,
                Some(SIMULATION_HOVER),
                |ui| self.general_buttons(ctx, ui, GeneralCard::Simulation),
            );
            if dirty {
                lock_hint(ui);
                self.general_error(ui, "simulation");
            }
            card.body(|ui| {
                ui.add_space(6.0);
                form_rows(ui, |ui| {
                    ui.add_enabled_ui(editable, |ui| self.simulation_fields(ui))
                });
            });
        });
        devices::mark(ui, || "card:simulation".to_owned(), &card.response);
    }

    /// The Replay card: the cameras' clips, saved with the Global
    /// card. Its edit controls are disabled unless `editable`.
    pub(crate) fn render_replay_card(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        editable: bool,
        height: f32,
    ) {
        let card = devices::fixed_card(ui, "replay", theme::HEADING, height, |card| {
            let ui = card.top();
            let dirty = card_header(ui, "replay", "Replay", None, Some(REPLAY_HOVER), |ui| {
                self.general_buttons(ctx, ui, GeneralCard::Replay)
            });
            if dirty {
                lock_hint(ui);
                self.general_error(ui, "replay");
            }
            card.body(|ui| {
                ui.add_space(6.0);
                form_rows(ui, |ui| {
                    ui.add_enabled_ui(editable, |ui| self.replay_fields(ui))
                });
            });
        });
        devices::mark(ui, || "card:replay".to_owned(), &card.response);
    }

    /// The Body tracking card (alpha): every body tracking setting, saved
    /// with the Global card (turning it on from Off saves at once, after the
    /// model download prompt). Its edit controls are disabled unless
    /// `editable`.
    pub(crate) fn render_body_tracking_card(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        editable: bool,
        height: f32,
    ) {
        let card = devices::fixed_card(ui, "body_tracking", theme::HEADING, height, |card| {
            let ui = card.top();
            let dirty = card_header(
                ui,
                "body_tracking",
                "Body tracking",
                Some(("alpha", TagPlace::Title)),
                None,
                |ui| self.general_buttons(ctx, ui, GeneralCard::BodyTracking),
            );
            if dirty {
                lock_hint(ui);
                self.general_error(ui, "body_tracking");
            }
            // Six rows: no space above the first and a point less between
            // them than on the other cards, so they fit the card's height.
            card.body(|ui| {
                ui.spacing_mut().item_spacing.y -= 1.0;
                form_rows(ui, |ui| {
                    ui.add_enabled_ui(editable, |ui| self.body_tracking_fields(ui))
                });
            });
        });
        devices::mark(ui, || "card:body_tracking".to_owned(), &card.response);
    }

    /// The Body tracking card's fields: Mode, Model, Show skeleton, Minimum
    /// score, Smoothing and Detect every, each with its range
    /// under it while out of range. Turning body tracking on from Off goes
    /// through the model download
    /// prompt ([`FlighthookApp::turn_body_tracking_on`]), which saves the
    /// mode itself; every other change is an edit saved with Save.
    pub(super) fn body_tracking_fields(&mut self, ui: &mut egui::Ui) {
        let (field_width, _) = field_widths(ui);
        let off = self.body_tracking().mode == BodyTrackingMode::Off;
        let mut turn_on = None;
        let form = &mut self.settings.body_tracking;
        let mut changed = false;
        ui.horizontal(|ui| {
            field_label(ui, "Mode:");
            let modes = BodyTrackingMode::all();
            let labels = modes.map(BodyTrackingMode::label);
            let selected = modes.iter().position(|m| *m == form.mode).unwrap_or(1);
            let toggle = theme::segmented_pills(ui, &labels, selected, None, theme::PILL_FONT);
            devices::mark(ui, || "body_tracking_mode".to_owned(), &toggle.response);
            let (clicked, pills) = toggle.inner;
            for (mode, pill) in modes.iter().zip(&pills) {
                devices::mark(ui, || format!("body_tracking_mode:{}", mode.key()), pill);
            }
            if let Some(i) = clicked {
                if off && modes[i].is_on() {
                    turn_on = Some(modes[i]);
                } else {
                    changed |= modes[i] != form.mode;
                    form.mode = modes[i];
                }
            }
        });
        ui.horizontal(|ui| {
            field_label(ui, "Model:");
            let models = PoseModel::all();
            let labels = models.map(PoseModel::label);
            let selected = models.iter().position(|m| *m == form.model).unwrap_or(0);
            let picker = theme::segmented(ui, &labels, selected, None);
            devices::mark(ui, || "body_tracking_model".to_owned(), &picker.response);
            if let Some(i) = picker.inner {
                changed |= models[i] != form.model;
                form.model = models[i];
            }
        });
        ui.horizontal(|ui| {
            field_indent(ui);
            let check = ui.checkbox(&mut form.show_skeleton, "Show skeleton");
            devices::mark(ui, || "body_tracking_skeleton".to_owned(), &check);
            changed |= check.changed();
        });
        let mut number_field =
            |ui: &mut egui::Ui, label: &str, mark: &str, text: &mut String, unit: Option<&str>| {
                ui.horizontal(|ui| {
                    field_label(ui, label);
                    let field = ui.add(
                        egui::TextEdit::singleline(text)
                            .desired_width(field_width - TEXT_EDIT_MARGIN),
                    );
                    devices::mark(ui, || mark.to_owned(), &field);
                    changed |= field.changed();
                    if let Some(unit) = unit {
                        ui.label(unit);
                    }
                });
            };
        number_field(
            ui,
            "Minimum score:",
            "body_tracking_min_score",
            &mut form.min_score,
            None,
        );
        if form.min_score_value().is_none() {
            field_error(ui, "0 to 1");
        }
        number_field(
            ui,
            "Smoothing:",
            "body_tracking_smoothing",
            &mut form.smoothing,
            Some("Hz"),
        );
        if form.smoothing_value().is_none() {
            field_error(
                ui,
                &format!(
                    "0, or {} to {} Hz",
                    BodyTrackingSection::MIN_SMOOTHING_HZ,
                    BodyTrackingSection::MAX_SMOOTHING_HZ
                ),
            );
        }
        number_field(
            ui,
            "Detect every:",
            "body_tracking_detect_every",
            &mut form.detect_every,
            Some("frames"),
        );
        if form.detect_every_value().is_none() {
            field_error(
                ui,
                &format!("1 to {}", BodyTrackingSection::MAX_DETECT_EVERY),
            );
        }
        if changed {
            self.settings.mark_general(GeneralCard::BodyTracking);
        }
        if let Some(mode) = turn_on {
            let ctx = ui.ctx().clone();
            self.turn_body_tracking_on(&ctx, mode);
        }
    }

    /// The Replay card's fields.
    pub(super) fn replay_fields(&mut self, ui: &mut egui::Ui) {
        let (field_width, _) = field_widths(ui);
        let replay = &mut self.settings.replay;
        let mut changed = false;
        ui.horizontal(|ui| {
            field_label(ui, "Keep clips:").on_hover_text(format!(
                "How many clips stay on disk, 1 to {}. Each camera's clip of a 5.5 s \
                 window at 1280×720, 120 fps is about 55 MB.",
                ReplaySection::MAX_KEEP_SHOTS
            ));
            let field = ui.add(
                egui::TextEdit::singleline(&mut replay.keep_shots)
                    .desired_width(field_width - TEXT_EDIT_MARGIN),
            );
            devices::mark(ui, || "replay_keep".to_owned(), &field);
            changed |= field.changed();
        });
        if !replay
            .keep_shots
            .trim()
            .parse::<u32>()
            .is_ok_and(|n| (1..=ReplaySection::MAX_KEEP_SHOTS).contains(&n))
        {
            field_error(ui, &format!("1 to {}", ReplaySection::MAX_KEEP_SHOTS));
        }
        for (label, hover, text, loaded) in [
            (
                "Pre-roll:",
                Some("Seconds recorded before the estimated impact, 0 to 10."),
                &mut replay.pre_roll_s,
                replay.loaded.pre_roll_s,
            ),
            (
                "Post-roll:",
                Some("Seconds recorded after the launch monitor reports the shot, 0 to 10."),
                &mut replay.post_roll_s,
                replay.loaded.post_roll_s,
            ),
            (
                "Start before impact:",
                None,
                &mut replay.start_before_impact_s,
                replay.loaded.start_before_impact_s,
            ),
        ] {
            ui.horizontal(|ui| {
                let label = field_label(ui, label);
                if let Some(hover) = hover {
                    label.on_hover_text(hover);
                }
                changed |= ui
                    .add(
                        egui::TextEdit::singleline(text)
                            .desired_width(field_width - TEXT_EDIT_MARGIN),
                    )
                    .changed();
                ui.label("s");
            });
            if ReplayForm::seconds(text, loaded)
                .is_none_or(|v| !(v.is_finite() && (0.0..=ReplaySection::MAX_ROLL_S).contains(&v)))
            {
                field_error(ui, "0 to 10 s");
            }
        }
        ui.horizontal(|ui| {
            field_indent(ui);
            let check = ui.checkbox(&mut replay.shot_cards, "Show video on shot cards");
            devices::mark(ui, || "replay_shot_cards".to_owned(), &check);
            changed |= check.changed();
        });
        let folder = replay
            .clip_dir()
            .map(str::to_owned)
            .or_else(|| self.clip_dir.clone());
        ui.horizontal(|ui| {
            field_label(ui, "Clip folder:")
                .on_hover_text("Where the clips are written: an AVI file and its index per camera, in a folder per clip.");
            let text = folder.as_deref().unwrap_or("-");
            let shown = ui.add(
                egui::Label::new(egui::RichText::new(text).color(theme::TEXT_MUTED)).truncate(),
            );
            devices::mark(ui, || "replay_folder".to_owned(), &shown);
        });
        if changed {
            self.settings.mark_general(GeneralCard::Replay);
        }
    }

    /// The Simulation card's fields.
    pub(super) fn simulation_fields(&mut self, ui: &mut egui::Ui) {
        let (field_width, _) = field_widths(ui);
        let sim = &mut self.settings.simulation;
        let mut sim_changed = false;
        ui.horizontal(|ui| {
            field_label(ui, "Simulation:").on_hover_text(sim_mode_hover(sim.mode));
            egui::ComboBox::from_id_salt("sim_mode")
                .selected_text(sim.mode.label())
                .width(field_width)
                .show_ui(ui, |ui| {
                    for mode in SimMode::all() {
                        if ui
                            .selectable_label(sim.mode == mode, mode.label())
                            .on_hover_text(sim_mode_hover(mode))
                            .clicked()
                        {
                            sim.mode = mode;
                            sim_changed = true;
                        }
                    }
                });
        });
        ui.add_enabled_ui(sim.mode != SimMode::Off, |ui| {
            ui.horizontal(|ui| {
                field_label(ui, "Altitude:")
                    .on_hover_text("Altitude above sea level, 0 to 5000 m (16,404 ft).");
                sim_changed |= ui
                    .add(
                        egui::TextEdit::singleline(&mut sim.altitude_val)
                            .desired_width(field_width - TEXT_EDIT_MARGIN),
                    )
                    .changed();
                egui::ComboBox::from_id_salt("sim_altitude_unit")
                    .selected_text(unit_suffix(&sim.altitude_unit))
                    .width(UNIT_WIDTH)
                    .show_ui(ui, |ui| {
                        for &(key, label) in ALTITUDE_UNITS {
                            if ui
                                .selectable_label(sim.altitude_unit == key, label)
                                .clicked()
                            {
                                sim.altitude_unit = key.to_string();
                                sim_changed = true;
                            }
                        }
                    });
            });
            if sim.altitude().is_none_or(|a| {
                let m = a.as_meters();
                !(m.is_finite() && (0.0..=SimulationSection::MAX_ALTITUDE_M).contains(&m))
            }) {
                field_error(ui, "0 to 5000 m");
            }
            ui.horizontal(|ui| {
                field_label(ui, "Temperature:").on_hover_text(
                    "Air temperature, -30 to 50 °C (-22 to 122 °F). Defaults to 25 °C (77 °F).",
                );
                sim_changed |= ui
                    .add(
                        egui::TextEdit::singleline(&mut sim.temperature_val)
                            .desired_width(field_width - TEXT_EDIT_MARGIN),
                    )
                    .changed();
                ui.label(sim.temperature_unit());
            });
            if sim.temperature_c().is_none_or(|c| {
                !(SimulationSection::MIN_TEMPERATURE_C..=SimulationSection::MAX_TEMPERATURE_C)
                    .contains(&c)
            }) {
                let range = if sim.fahrenheit {
                    "-22 to 122 °F"
                } else {
                    "-30 to 50 °C"
                };
                field_error(ui, range);
            }
            ui.horizontal(|ui| {
                field_label(ui, "Surface:")
                    .on_hover_text("Flat ground the ball lands and rolls on.");
                egui::ComboBox::from_id_salt("sim_surface")
                    .selected_text(sim.surface.label())
                    .width(field_width)
                    .show_ui(ui, |ui| {
                        for surface in SimSurface::all() {
                            if ui
                                .selectable_label(sim.surface == surface, surface.label())
                                .clicked()
                            {
                                sim.surface = surface;
                                sim_changed = true;
                            }
                        }
                    });
            });
        });
        if sim_changed {
            self.settings.mark_general(GeneralCard::Simulation);
        }
    }
}
