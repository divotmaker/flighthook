//! The Cameras section's camera card fields: camera picker, mode, position, timing, latency and rotation.

use super::*;

/// How a camera entry reads in the picker: its name, and the card using it
/// when that is another one.
pub(super) fn camera_choice_label(entry: &CameraListEntry, own: &str) -> String {
    match entry.in_use_by.as_deref() {
        Some(user) if user != own => format!("{} (in use by {user})", entry.name),
        _ => entry.name.clone(),
    }
}

/// A launch monitor card's Camera timing field: seconds from impact to the
/// monitor's report, blank for the device's default (shown in the field).
pub(super) fn camera_timing_field(ui: &mut egui::Ui, dev: &mut DeviceFormEntry, field_width: f32) {
    ui.horizontal(|ui| {
        field_label(ui, "Camera timing:");
        let hint = flighthook::impact_lead_default_s(&dev.monitor_type)
            .map(format_distance_value)
            .unwrap_or_default();
        let field = ui.add(
            egui::TextEdit::singleline(&mut dev.impact_lead)
                .desired_width(field_width - TEXT_EDIT_MARGIN)
                .hint_text(hint),
        );
        devices::mark(
            ui,
            || format!("camera_timing:{}.{}", dev.monitor_type, dev.id),
            &field,
        );
        if field.changed() {
            dev.dirty = true;
        }
        ui.label("s");
    });
    if impact_lead_value(&dev.impact_lead).is_none() {
        field_error(
            ui,
            &format!("Must be a number, 0 to {}", flighthook::MAX_IMPACT_LEAD_S),
        );
    }
}

/// A camera card's fields: Camera, then Rotation (in sight without
/// scrolling, for a quick setup), Position, Video mode, Latency, Yaw and
/// Pitch. Returns whether the camera list was asked to refresh.
pub(super) fn camera_fields(
    ui: &mut egui::Ui,
    dev: &mut DeviceFormEntry,
    cameras: &CameraList,
    field_width: f32,
    wide_width: f32,
) -> bool {
    let own = format!("{}.{}", dev.monitor_type, dev.id);
    let entries: &[CameraListEntry] = cameras.list.as_ref().map_or(&[], |l| &l.cameras);
    let picked = entries.iter().find(|e| e.id == dev.cam.id);
    let mut refresh = false;
    ui.horizontal(|ui| {
        field_label(ui, "Camera:").on_hover_text(
            "The USB camera to record from. It is found again after a replug into the \
             same port. The test pattern is a generated picture, for trying replay \
             without a camera.",
        );
        let shown = match (picked, &dev.cam.name) {
            (Some(e), _) => e.name.clone(),
            (None, Some(name)) => format!("{name} (not connected)"),
            (None, None) if dev.cam.id.is_empty() => "Pick a camera".to_owned(),
            (None, None) => format!("{} (not connected)", dev.cam.id),
        };
        let combo = egui::ComboBox::from_id_salt(format!("camera_{}", dev.id))
            .selected_text(shown)
            .width(field_width)
            .truncate()
            .show_ui(ui, |ui| {
                for entry in entries {
                    let label = camera_choice_label(entry, &own);
                    let item = ui.selectable_label(entry.id == dev.cam.id, label);
                    if item.clicked() && entry.id != dev.cam.id {
                        dev.cam.id.clone_from(&entry.id);
                        dev.cam.name = Some(entry.name.clone());
                        if !entry.modes.contains(&dev.cam.mode) {
                            dev.cam.mode = CameraSection::AUTO_MODE.to_owned();
                        }
                        dev.dirty = true;
                    }
                }
                if entries.is_empty() {
                    ui.label(
                        egui::RichText::new(if cameras.loading {
                            "Looking for cameras…"
                        } else {
                            "No cameras found"
                        })
                        .color(theme::TEXT_MUTED),
                    );
                }
            });
        devices::mark(ui, || format!("camera_pick:{own}"), &combo.response);
        if cameras.loading {
            ui.spinner();
        } else {
            let again = theme::icon(ui, theme::Icon::Refresh, 20.0, theme::ACCENT, true)
                .on_hover_text("Look for cameras again");
            devices::mark(ui, || format!("camera_refresh:{own}"), &again);
            refresh = again.clicked();
        }
    });
    if let Some(error) = &cameras.error {
        field_error(ui, &format!("Camera list: {error}"));
    }
    if dev.cam.id.trim().is_empty() {
        field_error(ui, "Pick a camera");
    }
    if let Some(error) = picked.and_then(|e| e.error.as_deref()) {
        field_error(ui, error);
    }

    ui.horizontal(|ui| {
        field_label(ui, "Rotation:").on_hover_text(
            "Turns the camera's picture clockwise before it is shown and tracked, for a \
             camera mounted on its side. Clips are stored as the camera sends them.",
        );
        let rotations = flighthook::CAMERA_ROTATIONS;
        let labels = rotations.map(|r| format!("{r}°"));
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        let selected = rotations
            .iter()
            .position(|r| *r == dev.cam.rotation)
            .unwrap_or(usize::MAX);
        let toggle = theme::segmented_pills(ui, &labels, selected, None, theme::PILL_FONT);
        devices::mark(ui, || format!("camera_rotation:{own}"), &toggle.response);
        let (clicked, pills) = toggle.inner;
        for (rotation, pill) in rotations.iter().zip(&pills) {
            devices::mark(ui, || format!("camera_rotation:{own}:{rotation}"), pill);
        }
        if let Some(rotation) = clicked.map(|i| rotations[i])
            && rotation != dev.cam.rotation
        {
            dev.cam.rotation = rotation;
            dev.dirty = true;
        }
    });
    if !flighthook::rotation_valid(dev.cam.rotation) {
        field_error(ui, "0°, 90°, 180° or 270°");
    }

    ui.horizontal(|ui| {
        field_label(ui, "Position:").on_hover_text("Where the camera stands.");
        egui::ComboBox::from_id_salt(format!("camera_position_{}", dev.id))
            .selected_text(dev.cam.position.label())
            .width(wide_width)
            .show_ui(ui, |ui| {
                for position in CameraPosition::all() {
                    if ui
                        .selectable_label(dev.cam.position == position, position.label())
                        .clicked()
                    {
                        dev.cam.position = position;
                        dev.dirty = true;
                    }
                }
            });
    });

    ui.horizontal(|ui| {
        field_label(ui, "Video mode:").on_hover_text(
            "Auto picks the camera's highest frame rate, then its largest size at that \
             rate. A faster mode shows more of the swing in slow motion and takes more \
             disk space.",
        );
        let auto = CameraSection::AUTO_MODE;
        let label = |mode: &str| {
            if mode == auto {
                "Auto".to_owned()
            } else {
                mode.replace('x', "×").replace('@', " @ ")
            }
        };
        let mut modes: Vec<&str> = vec![auto];
        if dev.cam.mode != auto && !picked.is_some_and(|e| e.modes.contains(&dev.cam.mode)) {
            modes.push(dev.cam.mode.as_str());
        }
        let listed: Vec<String> = picked.map(|e| e.modes.clone()).unwrap_or_default();
        modes.extend(listed.iter().map(String::as_str));
        let mut chosen = None;
        let combo = egui::ComboBox::from_id_salt(format!("camera_mode_{}", dev.id))
            .selected_text(label(&dev.cam.mode))
            .width(wide_width)
            .show_ui(ui, |ui| {
                for mode in &modes {
                    if ui
                        .selectable_label(dev.cam.mode == *mode, label(mode))
                        .clicked()
                    {
                        chosen = Some((*mode).to_owned());
                    }
                }
            });
        devices::mark(ui, || format!("camera_mode:{own}"), &combo.response);
        if let Some(mode) = chosen.filter(|m| *m != dev.cam.mode) {
            dev.cam.mode = mode;
            dev.dirty = true;
        }
    });
    if !video_mode_valid(&dev.cam.mode) {
        field_error(ui, "Auto or WIDTHxHEIGHT@FPS");
    }

    ui.horizontal(|ui| {
        field_label(ui, "Latency:");
        let field = ui.add(
            egui::TextEdit::singleline(&mut dev.cam.latency)
                .desired_width(field_width - TEXT_EDIT_MARGIN)
                .hint_text("0"),
        );
        devices::mark(ui, || format!("camera_latency:{own}"), &field);
        if field.changed() {
            dev.dirty = true;
        }
        ui.label("s");
    });
    if latency_value(&dev.cam.latency).is_none() {
        field_error(
            ui,
            &format!("Must be a number, 0 to {}", flighthook::MAX_LATENCY_S),
        );
    }

    let max = flighthook::MAX_CAMERA_ANGLE_DEG;
    for (label, key, text, hint) in [
        (
            "Yaw:",
            "camera_yaw",
            &mut dev.cam.yaw,
            "How far the camera points off square, in degrees.",
        ),
        (
            "Pitch:",
            "camera_pitch",
            &mut dev.cam.pitch,
            "How far the camera tilts down from level, in degrees.",
        ),
    ] {
        ui.horizontal(|ui| {
            field_label(ui, label).on_hover_text(hint);
            let field = ui.add(
                egui::TextEdit::singleline(text)
                    .desired_width(field_width - TEXT_EDIT_MARGIN)
                    .hint_text("0"),
            );
            devices::mark(ui, || format!("{key}:{own}"), &field);
            if field.changed() {
                dev.dirty = true;
            }
            ui.label("°");
        });
        if angle_value(text).is_none() {
            field_error(ui, &format!("Must be a number, -{max} to {max}"));
        }
        if key == "camera_yaw" {
            field_hint(
                ui,
                "Positive: the camera points to the right of the target line for a right-hander",
            );
        }
    }
    refresh
}
