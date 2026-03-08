use crate::app::FlighthookApp;
use crate::types::UnitSystem;

/// Format an optional f64 with the given precision, or "-" if None.
fn opt_f(v: Option<f64>, prec: usize) -> String {
    match v {
        Some(v) => format!("{v:.prec$}"),
        None => "-".into(),
    }
}

/// Format an optional i32, or "-" if None.
fn opt_i(v: Option<i32>) -> String {
    match v {
        Some(v) => format!("{v}"),
        None => "-".into(),
    }
}

impl FlighthookApp {
    pub(crate) fn render_shots_panel(&mut self, ui: &mut egui::Ui) {
        let imperial = self.units_toggle == UnitSystem::Imperial;
        let (speed_label, dist_label, height_label) = if imperial {
            ("mph", "yd", "yd")
        } else {
            ("m/s", "m", "m")
        };

        // Unit toggle (outside ScrollArea to avoid borrow conflict)
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Units:").strong().size(11.0));
            if ui.selectable_label(imperial, "Imperial").clicked() && !imperial {
                self.units_toggle = UnitSystem::Imperial;
            }
            if ui.selectable_label(!imperial, "Metric").clicked() && imperial {
                self.units_toggle = UnitSystem::Metric;
            }
        });

        let units = self.units_toggle;

        egui::ScrollArea::both()
            .auto_shrink(false)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                egui::Grid::new("shots_grid")
                    .striped(true)
                    .min_col_width(45.0)
                    .show(ui, |ui| {
                        // Header
                        let hdr = |ui: &mut egui::Ui, text: &str| {
                            ui.label(egui::RichText::new(text).strong().size(11.0));
                        };
                        hdr(ui, "Device");
                        hdr(ui, "#");
                        hdr(ui, &format!("Ball\n{speed_label}"));
                        hdr(ui, "VLA\ndeg");
                        hdr(ui, "HLA\ndeg");
                        hdr(ui, &format!("Carry\n{dist_label}"));
                        hdr(ui, &format!("Height\n{height_label}"));
                        hdr(ui, "Back\nrpm");
                        hdr(ui, "Side\nrpm");
                        hdr(ui, &format!("Club\n{speed_label}"));
                        hdr(ui, "Path\ndeg");
                        hdr(ui, "AoA\ndeg");
                        hdr(ui, "Face\ndeg");
                        hdr(ui, "Loft\ndeg");
                        hdr(ui, "Smash");
                        ui.end_row();

                        for shot in &self.shots {
                            let converted = shot.to_unit_system(units);
                            // Device display name (look up from actor key)
                            let dev_display = self
                                .actors
                                .get(&shot.source)
                                .map(|a| a.name.as_str())
                                .unwrap_or(&shot.source);
                            if dev_display.is_empty() {
                                ui.label("-");
                            } else {
                                ui.label(dev_display);
                            }
                            ui.label(format!("{}", shot.shot_number));

                            if let Some(ref f) = converted.ball {
                                ui.label(opt_f(f.launch_speed.map(|v| v.value()), 1));
                                ui.label(opt_f(f.launch_elevation, 1));
                                ui.label(opt_f(f.launch_azimuth, 1));
                                ui.label(opt_f(f.carry_distance.map(|d| d.value()), 1));
                                ui.label(opt_f(f.max_height.map(|d| d.value()), 1));
                                ui.label(opt_i(f.backspin_rpm));
                                ui.label(opt_i(f.sidespin_rpm));
                            } else {
                                for _ in 0..7 {
                                    ui.label("-");
                                }
                            }

                            if let Some(ref c) = converted.club {
                                ui.label(opt_f(c.club_speed.map(|v| v.value()), 1));
                                ui.label(opt_f(c.path, 1));
                                ui.label(opt_f(c.attack_angle, 1));
                                ui.label(opt_f(c.face_angle, 1));
                                ui.label(opt_f(c.dynamic_loft, 1));
                                ui.label(opt_f(c.smash_factor, 2));
                            } else {
                                for _ in 0..6 {
                                    ui.label("-");
                                }
                            }
                            ui.end_row();
                        }
                    });
            });
    }
}
