//! The settings fields of one launch monitor or output card's page (`actor_fields`), including
//! the Open Connect client's club mode lists.

use super::*;

/// The fields of one actor card, by type. `cameras` is the camera list a
/// camera card picks from. Returns whether the camera list was asked to
/// refresh.
pub(crate) fn actor_fields(
    ui: &mut egui::Ui,
    actor: &mut ActorFormEntry,
    device_monitor_options: &[(String, String)],
    cameras: &CameraList,
) -> bool {
    let (field_width, wide_width) = field_widths(ui);
    let mut refresh = false;
    match actor {
        ActorFormEntry::Device(dev) => {
            // Name
            ui.horizontal(|ui| {
                field_label(ui, "Name:")
                    .on_hover_text("Display name for this device in the UI and logs.");
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut dev.name)
                            .desired_width(field_width - TEXT_EDIT_MARGIN),
                    )
                    .changed()
                {
                    dev.dirty = true;
                }
            });
            if dev.name.is_empty() {
                field_error(ui, "Name is required");
            }

            if dev.is_camera() {
                refresh = camera_fields(ui, dev, cameras, field_width, wide_width);
            }

            if dev.has_network_address() {
                // Address
                ui.horizontal(|ui| {
                    field_label(ui, "Address:").on_hover_text(
                        "TCP address of the launch monitor. Connect to the device WiFi AP first.",
                    );
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut dev.address)
                                .desired_width(field_width - TEXT_EDIT_MARGIN)
                                .hint_text(MEVO_ADDRESS),
                        )
                        .on_hover_text("ip:port (e.g. 192.168.2.1:5100)")
                        .changed()
                    {
                        dev.dirty = true;
                    }
                });
            }

            if dev.has_bind_address() {
                // Bind address — optional; blank listens on the default.
                ui.horizontal(|ui| {
                    field_label(ui, "Bind Address:").on_hover_text(format!(
                        "Address and port this server listens on. 0.0.0.0 accepts \
                         connections from other computers too; 127.0.0.1 only from \
                         this one. Leave blank for {OPENCONNECT_BIND}. Point the \
                         launch monitor's Open Connect connector at this port."
                    ));
                    let field = ui
                        .add(
                            egui::TextEdit::singleline(&mut dev.address)
                                .desired_width(field_width - TEXT_EDIT_MARGIN)
                                .hint_text(OPENCONNECT_BIND),
                        )
                        .on_hover_text("ip:port (e.g. 0.0.0.0:921)");
                    devices::mark(
                        ui,
                        || format!("bind:{}.{}", dev.monitor_type, dev.id),
                        &field,
                    );
                    if field.changed() {
                        dev.dirty = true;
                    }
                });
                if !dev.address.is_empty() && dev.address.parse::<std::net::SocketAddr>().is_err() {
                    field_error(ui, "Invalid bind address");
                }
            }

            if dev.has_pureplay_address() {
                // Pureplay's address — optional; blank is the default.
                ui.horizontal(|ui| {
                    field_label(ui, "Address:").on_hover_text(format!(
                        "Pureplay's address and Accessory port, from Pureplay's \
                         Settings > Accessories. Leave blank for \
                         {PUREPLAY_ACCESSORY_ADDRESS} (Pureplay on this computer). \
                         Changing it pairs again."
                    ));
                    let field = ui
                        .add(
                            egui::TextEdit::singleline(&mut dev.address)
                                .desired_width(field_width - TEXT_EDIT_MARGIN)
                                .hint_text(PUREPLAY_ACCESSORY_ADDRESS),
                        )
                        .on_hover_text("ip:port (e.g. 127.0.0.1:9770)");
                    devices::mark(
                        ui,
                        || format!("address:{}.{}", dev.monitor_type, dev.id),
                        &field,
                    );
                    if field.changed() {
                        dev.dirty = true;
                    }
                });
                if !dev.address.is_empty() && dev.address.parse::<std::net::SocketAddr>().is_err() {
                    field_error(ui, "Invalid address");
                }
            }

            if dev.monitor_type == "pureplay_accessory" {
                // Which of Pureplay's shots the card takes.
                ui.horizontal(|ui| {
                    field_label(ui, "Shots:").on_hover_text(
                        "Auto: shots of a player at this computer, except the ones \
                         flighthook sent to Pureplay (Pureplay's results still reach \
                         those). Auto assumes one launch monitor, on either side.\n\
                         From Pureplay: every shot of a player at this computer (the \
                         launch monitor is connected to Pureplay).\n\
                         From flighthook: none (the launch monitor is connected to \
                         flighthook, which sends its shots to Pureplay).",
                    );
                    let picker = egui::ComboBox::from_id_salt(format!("pureplay_shots_{}", dev.id))
                        .selected_text(pureplay_shots_label(dev.pureplay_shots))
                        .width(wide_width)
                        .show_ui(ui, |ui| {
                            for shots in PUREPLAY_SHOTS {
                                if ui
                                    .selectable_label(
                                        dev.pureplay_shots == shots,
                                        pureplay_shots_label(shots),
                                    )
                                    .clicked()
                                {
                                    dev.pureplay_shots = shots;
                                    dev.dirty = true;
                                }
                            }
                        });
                    devices::mark(
                        ui,
                        || format!("shots:{}.{}", dev.monitor_type, dev.id),
                        &picker.response,
                    );
                });
            }

            if dev.has_ble_address() {
                // BLE address — optional; blank auto-discovers.
                ui.horizontal(|ui| {
                    field_label(ui, "BLE Address:").on_hover_text("Advertised name of the device, e.g. SquareGolf(CD01) — the same on every OS. A Bluetooth address (a UUID on macOS) also works. Leave blank to auto-discover.");
                    if ui
                        .add(egui::TextEdit::singleline(&mut dev.address).desired_width(field_width - TEXT_EDIT_MARGIN))
                        .on_hover_text("optional, e.g. SquareGolf(CD01)")
                        .changed()
                    {
                        dev.dirty = true;
                    }
                });
                let a = dev.address.trim();
                if !a.is_empty() && !is_ble_address(a) {
                    field_error(ui, "Invalid BLE address");
                }
            }

            if dev.has_ip_address() {
                // IP address — optional; blank finds the device.
                ui.horizontal(|ui| {
                    field_label(ui, "IP Address:").on_hover_text(
                        "IP address of the device on your network. Leave blank to \
                         find it automatically.",
                    );
                    let field = ui
                        .add(
                            egui::TextEdit::singleline(&mut dev.address)
                                .desired_width(field_width - TEXT_EDIT_MARGIN)
                                .hint_text(EXPUTT_IP_PLACEHOLDER),
                        )
                        .on_hover_text("optional, e.g. 192.168.1.50");
                    devices::mark(ui, || format!("ip:{}.{}", dev.monitor_type, dev.id), &field);
                    if field.changed() {
                        dev.dirty = true;
                    }
                });
            }

            if dev.is_square() {
                // Zero-spin rejection (putts always exempt).
                ui.horizontal(|ui| {
                    field_indent(ui);
                    if ui
                        .checkbox(&mut dev.discard_zero_spin, "Discard zero-spin shots")
                        .on_hover_text(
                            "A struck ball always spins, so a zero-spin reading \
                             is a failed read — usually a ball too far forward in \
                             the hitting zone. Such shots fly far too long in the \
                             sim, so they are discarded and you re-hit.\n\n\
                             Putts are never discarded: there is no airborne \
                             flight to measure spin over, so a putt reads zero \
                             every time.",
                        )
                        .changed()
                    {
                        dev.dirty = true;
                    }
                });

                // Per-club face impact calibration override.
                {
                    egui::CollapsingHeader::new("Face impact calibration (beta)")
                        .id_salt(format!("impact_cal_{}", dev.id))
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new(
                                    "Distance from the bottom edge of the club \
                                     sticker's dot down to face center, in mm. \
                                     Blank uses the built-in default for that club.",
                                )
                                .size(11.0)
                                .weak(),
                            );
                            for &club in SQUARE_CALIBRATED_CLUBS {
                                let mut val =
                                    dev.square_impact_mm.get(&club).cloned().unwrap_or_default();
                                ui.horizontal(|ui| {
                                    ui.label(format!("{club}:"));
                                    let response = ui.add(
                                        egui::TextEdit::singleline(&mut val)
                                            .desired_width(80.0)
                                            .hint_text("default"),
                                    );
                                    let invalid = !is_valid_impact_mm(&val);
                                    if response.changed() {
                                        dev.square_impact_mm.insert(club, val);
                                        dev.dirty = true;
                                    }
                                    ui.label(egui::RichText::new("mm").size(11.0).weak());
                                    if invalid {
                                        ui.label(
                                            egui::RichText::new("Invalid")
                                                .color(theme::DANGER)
                                                .size(11.0),
                                        );
                                    }
                                });
                            }
                        });
                }
            }

            if dev.is_r10() {
                // Tee distance, pushed to the device on wake-up.
                ui.horizontal(|ui| {
                    field_label(ui, "Monitor-to-Ball:").on_hover_text("Distance from the R10 to the ball, sent to the device as its tee distance.\nGarmin recommends 6-8 ft behind the ball.\n\nLeave blank to keep whatever distance is already set on the device.");
                    if ui
                        .add(egui::TextEdit::singleline(&mut dev.range_val).desired_width(field_width - TEXT_EDIT_MARGIN))
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
                // Correction for a unit that is not aimed at the target.
                ui.horizontal(|ui| {
                    field_label(ui, "Aim offset:").on_hover_text("Added to the launch direction (and to the face angle and path of a tracked club), in degrees; + is right.\n\nTo set it, hit a short straight chip and enter the negative of the launch direction it shows.\n\nBlank is 0.");
                    let field = ui.add(
                        egui::TextEdit::singleline(&mut dev.aim_offset)
                            .desired_width(field_width - TEXT_EDIT_MARGIN)
                            .hint_text("0"),
                    );
                    devices::mark(ui, || format!("aim_offset:{}", dev.id), &field);
                    if field.changed() {
                        dev.dirty = true;
                    }
                    ui.label("°");
                });
            }

            if dev.has_mevo_tuning() {
                // Ball Type
                ui.horizontal(|ui| {
                    field_label(ui, "Ball Type:").on_hover_text(
                        "RCT = Radar Capture Technology.\nStandard = any regular golf ball.",
                    );
                    let ball_text = if dev.ball_type == 1 {
                        "RCT"
                    } else {
                        "Standard"
                    };
                    egui::ComboBox::from_id_salt(format!("ball_type_{}", dev.id))
                        .selected_text(ball_text)
                        .width(field_width)
                        .show_ui(ui, |ui| {
                            if ui.selectable_label(dev.ball_type == 1, "RCT").clicked() {
                                dev.ball_type = 1;
                                dev.dirty = true;
                            }
                            if ui
                                .selectable_label(dev.ball_type == 0, "Standard")
                                .clicked()
                            {
                                dev.ball_type = 0;
                                dev.dirty = true;
                            }
                        });
                });

                // Tee Height
                ui.horizontal(|ui| {
                    field_label(ui, "Tee Height:")
                        .on_hover_text("Height of the tee above the hitting surface.");
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut dev.tee_height_val)
                                .desired_width(field_width - TEXT_EDIT_MARGIN),
                        )
                        .changed()
                    {
                        dev.dirty = true;
                    }
                    egui::ComboBox::from_id_salt(format!("tee_unit_{}", dev.id))
                        .selected_text(unit_suffix(&dev.tee_height_unit))
                        .width(50.0)
                        .show_ui(ui, |ui| {
                            for &(key, label) in DISTANCE_UNITS {
                                if ui
                                    .selectable_label(dev.tee_height_unit == key, label)
                                    .clicked()
                                {
                                    dev.tee_height_unit = key.to_string();
                                    dev.dirty = true;
                                }
                            }
                        });
                });

                // Monitor-to-Ball
                ui.horizontal(|ui| {
                    field_label(ui, "Monitor-to-Ball:").on_hover_text("Distance from the front of the launch monitor to the ball.\nMevo+ recommended range: 7-9 ft.");
                    if ui
                        .add(egui::TextEdit::singleline(&mut dev.range_val).desired_width(field_width - TEXT_EDIT_MARGIN))
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
                    field_label(ui, "Surface Height:").on_hover_text("Height of the hitting surface above the monitor.\nSet to 0 if the ball and monitor are on the same level.");
                    if ui
                        .add(egui::TextEdit::singleline(&mut dev.surface_height_val).desired_width(field_width - TEXT_EDIT_MARGIN))
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
                    field_label(ui, "Track %:").on_hover_text("Minimum outdoor tracking percentage (0-100).\nLower values accept shorter-tracked shots. Default: 80.");
                    if ui
                        .add(egui::TextEdit::singleline(&mut dev.track_pct).desired_width(field_width - TEXT_EDIT_MARGIN))
                        .changed()
                    {
                        dev.dirty = true;
                    }
                    ui.label("%");
                });

                // Use Estimated
                ui.horizontal(|ui| {
                    field_indent(ui);
                    if ui.checkbox(&mut dev.use_estimated, "Use Estimated Shots")
                        .on_hover_text("Include estimated (E8 fallback) shots. Off by default.\nWith it off, a shot the device only estimated (a flight too short\nto read in Outdoor mode, a very short chip) is skipped.\nEstimated shots may lack sidespin and carry less data.")
                        .changed()
                    {
                        dev.dirty = true;
                    }
                });

                // Environment
                ui.horizontal(|ui| {
                    field_label(ui, "Environment:").on_hover_text(
                        "Limited flight is for nets and sim screens.\nOutdoor needs about 40 yd of unobstructed flight;\nshots into a screen won't read.",
                    );
                    egui::ComboBox::from_id_salt(format!("environment_{}", dev.id))
                        .selected_text(environment_label(dev.environment))
                        .width(wide_width)
                        .show_ui(ui, |ui| {
                            for env in ENVIRONMENTS {
                                if ui
                                    .selectable_label(
                                        dev.environment == env,
                                        environment_label(env),
                                    )
                                    .clicked()
                                {
                                    dev.environment = env;
                                    dev.dirty = true;
                                }
                            }
                        });
                });

                // Spin source (Xi)
                ui.horizontal(|ui| {
                    field_label(ui, "Spin source (Xi):").on_hover_text(
                        "FlightScope Xi only. The Xi sends two backspin values per shot.\nD4 is its final spin, the value FS Golf shows.\nE8 is the second value, often higher on full swings; on chips and\npitches E8 isn't measured, so choose knowingly. Auto uses E8 only\nwhile flighthook is in Full mode, and D4 otherwise -- a chip hit\nwhile still in Full mode gets E8's speed-scaled value, so set\nchipping clubs or switch modes first. A Mevo+ or Gen2 reports the\nsame spin either way.",
                    );
                    egui::ComboBox::from_id_salt(format!("spin_source_{}", dev.id))
                        .selected_text(spin_source_label(dev.spin_source))
                        .width(wide_width)
                        .show_ui(ui, |ui| {
                            for src in SPIN_SOURCES {
                                if ui
                                    .selectable_label(
                                        dev.spin_source == src,
                                        spin_source_label(src),
                                    )
                                    .clicked()
                                {
                                    dev.spin_source = src;
                                    dev.dirty = true;
                                }
                            }
                        });
                });

                // Launch-only spin
                ui.horizontal(|ui| {
                    let hint = "When the device measures launch but no spin (very short chips on the Xi), use this many rpm of backspin per mph of ball speed, 0 to 1000. 0 sends the shot as the device read it.";
                    field_label(ui, "Launch-only spin:").on_hover_text(hint);
                    let unit = "rpm per mph";
                    let unit_width = ui
                        .painter()
                        .layout_no_wrap(
                            unit.into(),
                            egui::TextStyle::Body.resolve(ui.style()),
                            egui::Color32::PLACEHOLDER,
                        )
                        .size()
                        .x;
                    let gap = ui.spacing().item_spacing.x;
                    let edit_width = (wide_width - gap - unit_width - TEXT_EDIT_MARGIN).max(40.0);
                    if ui
                        .add(egui::TextEdit::singleline(&mut dev.launch_only_spin).desired_width(edit_width))
                        .on_hover_text(hint)
                        .changed()
                    {
                        dev.dirty = true;
                    }
                    ui.label(unit);
                });
                if launch_only_spin_value(&dev.launch_only_spin).is_none() {
                    field_error(ui, "Must be a number, 0 to 1000");
                }

                // Camera Mode
                ui.horizontal(|ui| {
                    field_label(ui, "Camera Mode:").on_hover_text(
                        "Standard reports ball flight and radar club data.\nFusion modes add the full club data set (path, face angle,\nattack angle, dynamic loft, smash factor, swing planes) and\nneed the Pro Package enabled on the device.\n\nWhich Fusion mode depends on firmware: Raw Fusion for\nBM17.04 and newer, Fusion for older. The wrong one yields\nno Fusion club data. Fusion is applied after a 15s camera warmup.",
                    );
                    egui::ComboBox::from_id_salt(format!("cam_mode_{}", dev.id))
                        .selected_text(camera_mode_label(dev.camera_mode))
                        .width(wide_width)
                        .show_ui(ui, |ui| {
                            for mode in CAMERA_MODES {
                                if ui
                                    .selectable_label(
                                        dev.camera_mode == mode,
                                        camera_mode_label(mode),
                                    )
                                    .clicked()
                                {
                                    dev.camera_mode = mode;
                                    dev.dirty = true;
                                }
                            }
                        });
                });
            }

            if dev.is_trackball() {
                trackball::fields(ui, dev, field_width, wide_width);
            }

            if dev.has_camera_timing() {
                camera_timing_field(ui, dev, field_width);
            }
            if let Some(error) = dev.driver_error() {
                let line = ui.label(egui::RichText::new(error).color(theme::DANGER).size(11.0));
                devices::mark(
                    ui,
                    || format!("driver_error:{}.{}", dev.monitor_type, dev.id),
                    &line,
                );
            }
        }
        ActorFormEntry::Integration(entry) => {
            // Name
            ui.horizontal(|ui| {
                field_label(ui, "Name:")
                    .on_hover_text("Display name for this integration in the UI and logs.");
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut entry.name)
                            .desired_width(field_width - TEXT_EDIT_MARGIN),
                    )
                    .changed()
                {
                    entry.dirty = true;
                }
            });
            if entry.name.is_empty() {
                field_error(ui, "Name is required");
            }

            // Address field (skip for mock): a web server's bind address, a
            // client's simulator address.
            if entry.integration_type != "random_club" {
                let (label, hover, example) = if entry.integration_type == "webserver" {
                    (
                        "Bind Address:",
                        "Address and port the web server listens on, for the dashboard \
                         and the API. 0.0.0.0 accepts connections from other computers \
                         too; 127.0.0.1 only from this one.",
                        "ip:port (e.g. 0.0.0.0:5880)",
                    )
                } else if entry.integration_type == "pureplay" {
                    (
                        "Address:",
                        "TCP address of Pureplay. Shot data is sent here over Pureplay Connect.",
                        "ip:port (e.g. 127.0.0.1:9760)",
                    )
                } else {
                    (
                        "Address:",
                        "TCP address of the simulator. Shot data is forwarded here as JSON.",
                        "ip:port (e.g. 127.0.0.1:921)",
                    )
                };
                ui.horizontal(|ui| {
                    field_label(ui, label).on_hover_text(hover);
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut entry.address)
                                .desired_width(field_width - TEXT_EDIT_MARGIN),
                        )
                        .on_hover_text(example)
                        .changed()
                    {
                        entry.dirty = true;
                    }
                });
                if entry.address.parse::<std::net::SocketAddr>().is_err() {
                    field_error(ui, "Invalid address");
                }
            }

            // Routing dropdowns (Open Connect clients and Pureplay)
            if matches!(
                entry.integration_type.as_str(),
                "openconnect_client" | "pureplay"
            ) {
                for (label_text, field_val, salt) in [
                    ("Full Monitor:", &mut entry.full_monitor, "full"),
                    ("Chipping Monitor:", &mut entry.chipping_monitor, "chipping"),
                    ("Putting Monitor:", &mut entry.putting_monitor, "putting"),
                ] {
                    ui.horizontal(|ui| {
                        field_label(ui, label_text).on_hover_text(
                            "Which launch monitor to accept shots from for this mode.\n\"Any\" accepts shots from all monitors."
                        );
                        // A routed launch monitor no card is any more.
                        let display = if field_val.is_empty() {
                            "Any"
                        } else {
                            device_monitor_options.iter()
                                .find(|(id, _)| id == field_val.as_str())
                                .map_or("Missing monitor", |(_, name)| name.as_str())
                        };
                        egui::ComboBox::from_id_salt(format!("{}_{}", salt, entry.id))
                            .selected_text(display)
                            .width(field_width)
                            .truncate()
                            .show_ui(ui, |ui| {
                                if ui.selectable_label(field_val.is_empty(), "Any").clicked() {
                                    field_val.clear();
                                    entry.dirty = true;
                                }
                                for (monitor_id, monitor_name) in device_monitor_options {
                                    if ui.selectable_label(field_val.as_str() == monitor_id, monitor_name).clicked() {
                                        *field_val = monitor_id.clone();
                                        entry.dirty = true;
                                    }
                                }
                            });
                    });
                }
            }
            if entry.integration_type == "openconnect_client" {
                club_mode_fields(ui, entry);
            }
        }
    }
    refresh
}

/// An Open Connect client's Chipping Clubs and Putting Clubs toggles: the
/// clubs of the simulator that set chipping or putting. With both empty
/// the card sets no mode.
pub(super) fn club_mode_fields(ui: &mut egui::Ui, entry: &mut IntegrationFormEntry) {
    for (label, tooltip, putting, options) in [
        (
            "Chipping Clubs:",
            "When the simulator selects one of these wedges, the launch monitor is set to chipping mode. With no chipping or putting club, this card does not set the mode.",
            false,
            flighthook::chipping_club_options(),
        ),
        (
            "Putting Clubs:",
            "When the simulator selects the putter, the launch monitor is set to putting mode. With no chipping or putting club, this card does not set the mode.",
            true,
            flighthook::putting_club_options(),
        ),
    ] {
        ui.horizontal_wrapped(|ui| {
            field_label(ui, label).on_hover_text(tooltip);
            for &club in options {
                let in_this = if putting {
                    entry.putting_clubs.contains(&club)
                } else {
                    entry.chipping_clubs.contains(&club)
                };
                let text = egui::RichText::new(format!("{club}")).size(12.0);
                let btn = if in_this {
                    egui::Button::new(text.strong().color(theme::ON_ACCENT)).fill(theme::ACCENT)
                } else {
                    egui::Button::new(text)
                };
                let resp = ui.add(btn);
                devices::mark(
                    ui,
                    || {
                        format!(
                            "{}_club:{}:{club}",
                            if putting { "putting" } else { "chipping" },
                            entry.id
                        )
                    },
                    &resp,
                );
                if resp.clicked() {
                    // A club sets one mode: it leaves the other list.
                    entry.chipping_clubs.retain(|c| *c != club);
                    entry.putting_clubs.retain(|c| *c != club);
                    if !in_this {
                        if putting {
                            entry.putting_clubs.push(club);
                        } else {
                            entry.chipping_clubs.push(club);
                        }
                    }
                    entry.dirty = true;
                }
            }
        });
    }
}
