use crate::app::FlighthookApp;
use crate::net;
use crate::types::{Distance, UnitSystem};

/// Format an optional f64 with the given precision, or "-" if None.
fn opt_f(v: Option<f64>, prec: usize) -> String {
    match v {
        Some(v) => format!("{v:.prec$}"),
        None => "-".into(),
    }
}

/// Hover text on every simulated cell.
const SIMULATED_HINT: &str = "Simulated (beta), not measured by the device";

/// Hover text on the JSONL export link.
const EXPORT_HINT: &str = "Every shot this session (the last 1000), including shots the device \
     sent no data for, with the raw device frames behind each one. \
     Contains no serial numbers, addresses or network names.";

/// Number of ball columns: launch, results and spin.
const BALL_COLUMNS: usize = 12;

/// Render a value that is either measured or simulated: the measured value
/// plainly, else the simulated one in italics, else "-". `signed` prints an
/// explicit `+` (offlines, roll).
fn cell(ui: &mut egui::Ui, measured: Option<f64>, derived: Option<f64>, prec: usize, signed: bool) {
    let fmt = |v: f64| {
        if signed {
            format!("{v:+.prec$}")
        } else {
            format!("{v:.prec$}")
        }
    };
    match (measured, derived) {
        (Some(v), _) => {
            ui.label(fmt(v));
        }
        (None, Some(v)) => {
            ui.label(egui::RichText::new(fmt(v)).italics())
                .on_hover_text(SIMULATED_HINT);
        }
        (None, None) => {
            ui.label("-");
        }
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
            ui.add_space(12.0);
            ui.label(
                egui::RichText::new("italic = simulated (beta)")
                    .italics()
                    .weak()
                    .size(11.0),
            )
            .on_hover_text(
                "Values the launch monitor did not report, simulated from the measured launch. \
                 Offline is + right of the target line.",
            );

            let units = self.units_toggle;
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.hyperlink_to("CSV", net::export_url("csv", units))
                    .on_hover_text("Decoded shots, one row each. No raw frames.");
                ui.hyperlink_to("JSONL (with raw)", net::export_url("jsonl", units))
                    .on_hover_text(EXPORT_HINT);
                ui.label(egui::RichText::new("Export session:").strong().size(11.0));
            });
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
                        hdr(ui, &format!("Offline\n{dist_label}"));
                        hdr(ui, "Desc\ndeg");
                        hdr(ui, &format!("Roll\n{dist_label}"));
                        hdr(ui, &format!("Total\n{dist_label}"));
                        hdr(ui, &format!("T.Offline\n{dist_label}"));
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
                                .get(&shot.actor)
                                .map(|a| a.name.as_str())
                                .unwrap_or(&shot.actor);
                            if dev_display.is_empty() {
                                ui.label("-");
                            } else {
                                ui.label(dev_display);
                            }
                            ui.label(format!("{}", shot.shot_number));

                            if let Some(ref f) = converted.ball {
                                let d = converted.derived.clone().unwrap_or_default();
                                let v = |x: Option<Distance>| x.map(|x| x.value());
                                ui.label(opt_f(f.launch_speed.map(|v| v.value()), 1));
                                ui.label(opt_f(f.launch_elevation, 1));
                                ui.label(opt_f(f.launch_azimuth, 1));
                                cell(ui, v(f.carry_distance), v(d.carry_distance), 1, false);
                                cell(ui, v(f.max_height), v(d.max_height), 1, false);
                                cell(ui, None, v(d.carry_offline), 1, true);
                                cell(ui, None, d.descent_angle, 1, false);
                                cell(ui, v(f.roll_distance), v(d.roll_distance), 1, true);
                                cell(ui, v(f.total_distance), v(d.total_distance), 1, false);
                                cell(ui, None, v(d.total_offline), 1, true);
                                ui.label(opt_i(f.backspin_rpm));
                                ui.label(opt_i(f.sidespin_rpm));
                            } else {
                                for _ in 0..BALL_COLUMNS {
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
