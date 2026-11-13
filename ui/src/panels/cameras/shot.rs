//! A shot's cameras: the inline player per camera with its swing data, retracking turned clips,
//! the yaw warning, the clip status and no-swing lines, and the tile helpers (number, worded,
//! posture tiles).

use super::*;

impl FlighthookApp {
    /// Shot row `i`'s cameras: its header, then its clips playing side by
    /// side with each camera's swing analysis under its clip; for a shot
    /// without clips, each camera's status. The pane scrolls when they
    /// don't fit.
    pub(super) fn render_shot_cameras(&mut self, ui: &mut egui::Ui, i: usize) {
        let ctx = ui.ctx().clone();
        let following = self.following();
        let header = ui
            .allocate_ui_with_layout(
                egui::vec2(ui.available_width(), TITLE_H),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| self.render_shot_header(ui, i, following),
            )
            .inner;
        if header.back {
            self.shot_focus.follow();
        }
        if header.replay {
            self.open_replay(&ctx, i);
        }
        let max_picture = (ui.available_height() * PLAYER_SHARE).max(120.0);
        if self.shots[i].video.is_empty() {
            self.shot_player = None;
            self.render_clip_status(ui, i);
            return;
        }
        self.sync_shot_player(&ctx, i);
        self.retrack_turned_clips(&ctx, i);
        let imperial = self.units() == UnitSystem::Imperial;
        let reference = self.body_reference;
        // A solid bar, beside the columns rather than over them.
        ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
        egui::ScrollArea::vertical()
            .id_salt("shot_cameras")
            .auto_shrink(false)
            .show(ui, |ui| {
                let Some(layout) = self.render_shot_player(ui, max_picture) else {
                    return;
                };
                ui.add_space(12.0);
                // Each camera's swing, at the frame it shows, graded
                // against the picked reference.
                let grades = |c| column_grades(c, reference);
                let swings: Vec<Swing> = layout
                    .columns
                    .iter()
                    .filter_map(|c| {
                        Some(Swing {
                            report: c.swing.as_deref()?,
                            t_ns: c.frame_t_ns,
                            grades: grades(c),
                        })
                    })
                    .filter(|s| s.report.swing_found())
                    .collect();
                // In the portrait layout the cameras are stacked, each the
                // full width.
                let stacked = self.portrait;
                if !swings.is_empty() {
                    let n = if stacked {
                        1.0
                    } else {
                        layout.columns.len() as f32
                    };
                    let row = layout.columns.first().map_or(0.0, |c| c.width) * n
                        + layout.gap * (n - 1.0);
                    // One tempo for the shot, across its cameras' columns.
                    let tempo = tempo_values(&swings);
                    ui.horizontal_top(|ui| {
                        ui.add_space(layout.pad);
                        tempo_row(ui, &tempo, row);
                    });
                    // The body at the frame each camera shows.
                    let frame = frame_tiles(&swings, imperial);
                    ui.horizontal_top(|ui| {
                        ui.add_space(layout.pad);
                        frame_row(ui, &frame, row);
                    });
                    let mut misses = miss_entries(&tempo);
                    misses.extend(miss_entries(&frame));
                    let graded = swings.iter().any(|s| {
                        s.grades
                            .is_some_and(|g| !g.grades.is_empty() || !g.tempo.is_empty())
                    });
                    ui.horizontal_top(|ui| {
                        ui.add_space(layout.pad);
                        miss_line(ui, &misses, row, graded);
                    });
                }
                let body = |ui: &mut egui::Ui| {
                    // The same offset as the clips' row (`pad`, then the
                    // row's item spacing), then the clips' gap.
                    ui.add_space(layout.pad);
                    ui.spacing_mut().item_spacing.x = layout.gap;
                    for column in &layout.columns {
                        ui.allocate_ui_with_layout(
                            egui::vec2(column.width, 0.0),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| {
                                ui.set_width(column.width);
                                if let Some(report) = &column.swing {
                                    let panel = if report.swing_found() {
                                        if let Some(text) = self.yaw_warning(&column.camera) {
                                            let line = muted_line(ui, &text, column.width);
                                            devices::mark(
                                                ui,
                                                || format!("yaw_warning:{}", column.camera),
                                                &line,
                                            );
                                        }
                                        ui.scope(|ui| {
                                            swing_metrics(
                                                ui,
                                                &column.camera,
                                                report,
                                                imperial,
                                                column.width,
                                                grades(column),
                                                reference,
                                            );
                                        })
                                        .response
                                    } else {
                                        no_swing_line(ui, column.width)
                                    };
                                    devices::mark(
                                        ui,
                                        || format!("swing:{}", column.camera),
                                        &panel,
                                    );
                                }
                            },
                        );
                    }
                };
                if stacked {
                    ui.vertical(body);
                } else {
                    ui.horizontal_top(body);
                }
                ui.add_space(8.0);
            });
    }

    /// Ask once per session for each of shot row `i`'s clips tracked with
    /// a rotation other than its camera's now to be tracked again
    /// ([`retrack_due`]); its old keypoints and analysis are dropped
    /// meanwhile. Nothing while body tracking is off.
    pub(super) fn retrack_turned_clips(&mut self, ctx: &egui::Context, i: usize) {
        if !self.body_tracking().mode.is_on() {
            return;
        }
        let Some(shot) = self.shots.get(i) else {
            return;
        };
        let shot_id = shot.shot_id.clone();
        let due = retrack_due(
            shot,
            |camera| self.settings.saved_rotation(camera),
            &self.retracked,
        );
        for camera in due {
            self.retracked.insert((shot_id.clone(), camera.clone()));
            #[cfg(not(test))]
            crate::net::post_retrack(ctx, &shot_id, &camera);
            #[cfg(test)]
            let _ = ctx;
            if crate::types::forget_analysis(&mut self.shots, &shot_id, &camera) {
                self.shots_rev += 1;
            }
            if let Some(player) = self.shot_player.as_mut().filter(|p| p.shot_id() == shot_id) {
                player.forget_analysis(&camera);
            }
        }
    }

    /// The line over camera `camera`'s swing analysis when it is a Golfer
    /// Front camera pointing [`YAW_WARNING_DEG`] or more off square
    /// ([`yaw_warning`]).
    pub(super) fn yaw_warning(&self, camera: &str) -> Option<String> {
        let (_, section) = self
            .settings
            .saved_cameras()
            .into_iter()
            .find(|(actor, _)| actor == camera)?;
        yaw_warning(section.position, section.yaw_deg)
    }

    /// For shot row `i`, which has no clips: each saved camera's column
    /// with its status where the picture would be, for the cameras that are
    /// on.
    pub(super) fn render_clip_status(&mut self, ui: &mut egui::Ui, i: usize) {
        let cameras = self.settings.cameras_on();
        if cameras.is_empty() {
            return;
        }
        let latest = crate::panels::shots::latest_index(&self.shots, self.filter()) == Some(i);
        let aspect = cameras
            .iter()
            .map(|(actor, _)| self.camera_aspect(actor))
            .fold(DEFAULT_ASPECT, f32::max);
        let available = ui.available_size();
        let n = cameras.len();
        let width = column_width(n, available, aspect);
        let row = width * n as f32 + GAP * (n as f32 - 1.0);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            ui.add_space(((available.x - row) / 2.0 - GAP).max(0.0));
            for (actor, section) in &cameras {
                ui.allocate_ui_with_layout(
                    egui::vec2(width, 0.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_width(width);
                        ui.allocate_ui_with_layout(
                            egui::vec2(width, HEADING_H),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| replay::camera_heading(ui, &section.name, section.position),
                        );
                        let size = egui::vec2(width, width * aspect);
                        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
                        ui.painter().rect_filled(rect, 6.0, theme::SURFACE_RAISED);
                        let status = self
                            .actors
                            .get(actor.as_str())
                            .cloned()
                            .unwrap_or_else(|| new_actor(section.name.clone()));
                        if status.state() == ConnectionState::Connected {
                            let text = clip_status_text(latest);
                            ui.painter().text(
                                rect.center(),
                                egui::Align2::CENTER_CENTER,
                                text,
                                egui::FontId::proportional(14.0),
                                theme::TEXT_MUTED,
                            );
                        } else {
                            status_over(ui, rect, &status, self.now_ms);
                        }
                        devices::mark(ui, || format!("clip_status:{actor}"), &response);
                    },
                );
            }
        });
    }
}

/// What a camera's column says under its clip when the swing analysis
/// found no swing near the shot's impact.
pub(crate) const NO_SWING: &str = "No swing found in this clip";

/// The [`NO_SWING`] line, `width` wide, in the clip status lines' style.
/// Degrees off square from which a Golfer Front camera's one-camera turn
/// and sway are flagged as approximate.
pub(crate) const YAW_WARNING_DEG: f64 = 5.0;

/// The line flagging a Golfer Front camera `yaw` degrees off square, at
/// [`YAW_WARNING_DEG`] or more either way; `None` for any other camera or a
/// smaller (or invalid) yaw.
pub(crate) fn yaw_warning(position: CameraPosition, yaw: Option<f64>) -> Option<String> {
    let yaw = flighthook::camera_angle_in_effect_deg(yaw).abs();
    (position == CameraPosition::GolferFront && yaw >= YAW_WARNING_DEG).then(|| {
        format!(
            "Camera {}° off square: one-camera turn and sway are approximate",
            yaw.round()
        )
    })
}

/// The cameras of `shot` whose clip was tracked with a rotation other than
/// the camera's now (`rotation_of`; `None` for a camera no longer
/// configured, which is left alone) and not asked about yet (`asked`, by
/// (shot ID, camera)).
pub(crate) fn retrack_due(
    shot: &crate::types::ShotRow,
    rotation_of: impl Fn(&str) -> Option<u16>,
    asked: &std::collections::HashSet<(String, String)>,
) -> Vec<String> {
    shot.video
        .iter()
        .filter(|v| v.pose)
        .filter(|v| rotation_of(&v.camera).is_some_and(|r| r != v.rotation))
        .filter(|v| !asked.contains(&(shot.shot_id.clone(), v.camera.clone())))
        .map(|v| v.camera.clone())
        .collect()
}

/// A muted line of `text`, `width` wide, wrapped.
pub(super) fn muted_line(ui: &mut egui::Ui, text: &str, width: f32) -> egui::Response {
    ui.allocate_ui_with_layout(
        egui::vec2(width, 0.0),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(text)
                        .size(14.0)
                        .color(theme::TEXT_MUTED),
                )
                .wrap(),
            )
        },
    )
    .inner
}

pub(super) fn no_swing_line(ui: &mut egui::Ui, width: f32) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width, TABLE_ROW_H), egui::Sense::hover());
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        NO_SWING,
        egui::FontId::proportional(14.0),
        theme::TEXT_MUTED,
    );
    response
}

/// What a connected camera's column says for a shot without its clip: the
/// latest shot's may still be on its way.
pub(crate) fn clip_status_text(latest: bool) -> &'static str {
    if latest {
        "Waiting for the clip"
    } else {
        "No clip of this shot"
    }
}

/// The pane's title row: `title` large, as a shot's header reads.
pub(super) fn pane_title(ui: &mut egui::Ui, title: &str) -> egui::Response {
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), TITLE_H),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.label(
                egui::RichText::new(title)
                    .size(22.0)
                    .strong()
                    .color(theme::TEXT),
            )
        },
    )
    .inner
}

/// A row of number tiles across `width`: each a name over its value.
/// Returns the row.
pub(super) fn number_tiles(
    ui: &mut egui::Ui,
    tiles: &[(&str, String)],
    width: f32,
) -> egui::Response {
    let tiles: Vec<(&str, &str, Option<&str>, egui::Color32)> = tiles
        .iter()
        .map(|(name, value)| (*name, value.as_str(), None, theme::TEXT))
        .collect();
    worded_tiles(ui, &tiles, width)
}

/// A row of number tiles across `width`: each a name over its value, and
/// the value's direction word after it when it has one, both in the
/// value's color. Returns the row.
pub(super) fn worded_tiles(
    ui: &mut egui::Ui,
    tiles: &[(&str, &str, Option<&str>, egui::Color32)],
    width: f32,
) -> egui::Response {
    let height = TILES_H - 8.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    if !ui.is_rect_visible(rect) || tiles.is_empty() {
        return response;
    }
    let painter = ui.painter_at(rect);
    for ((name, value, word, color), tile) in tiles.iter().zip(tile_rects(rect, tiles.len())) {
        painter.rect_filled(tile, 6.0, theme::SURFACE);
        let name = painter.layout_no_wrap(
            name.to_uppercase(),
            egui::FontId::proportional(10.0),
            theme::TEXT_MUTED,
        );
        painter.galley(
            egui::pos2(tile.left() + 8.0, tile.top() + 5.0),
            name,
            theme::TEXT_MUTED,
        );
        let (value, word) = worded_value(&painter, value, *word, 18.0, tile.width() - 16.0);
        let at = egui::pos2(tile.left() + 8.0, tile.bottom() - value.size().y - 4.0);
        painter.galley(at, value, *color);
        if let Some((word, offset)) = word {
            painter.galley(at + offset, word, *color);
        }
    }
    response
}

/// A value laid out at `size` with its direction `word` (if any) after it
/// on the same baseline, fitted into `room` as the Cards view fits its sign
/// words: the word starts at the cards' small size and shrinks to their
/// floor first, and only then does the numeral shrink. Returns the value
/// and the word with its offset from the value's top-left corner, to paint
/// in their color.
pub(super) fn worded_value(
    painter: &egui::Painter,
    text: &str,
    word: Option<&str>,
    size: f32,
    room: f32,
) -> (
    std::sync::Arc<egui::Galley>,
    Option<(std::sync::Arc<egui::Galley>, egui::Vec2)>,
) {
    // Laid out without a color: the painter gives each its own.
    let layout = |text: &str, size: f32| {
        painter.layout_no_wrap(
            text.to_owned(),
            egui::FontId::proportional(size),
            egui::Color32::PLACEHOLDER,
        )
    };
    let mut size = size;
    loop {
        let value = layout(text, size);
        let Some(word) = word else {
            return (value, None);
        };
        let gap = (size * 0.16).max(4.0);
        let mut word_size = shots::small_size(size);
        loop {
            let galley = layout(word, word_size);
            let fits = value.size().x + gap + galley.size().x <= room;
            if fits || (word_size <= shots::WORD_MIN && size <= shots::NUMERAL_FLOOR) {
                let baseline_gap = shots::DESCENT * (size - word_size);
                let offset = egui::vec2(
                    value.size().x + gap,
                    value.size().y - baseline_gap - galley.size().y,
                );
                return (value, Some((galley, offset)));
            }
            if word_size <= shots::WORD_MIN {
                break;
            }
            word_size = (word_size - 0.5).max(shots::WORD_MIN);
        }
        size = (size - 1.0).max(shots::NUMERAL_FLOOR);
    }
}

/// The rects of `n` tiles side by side across a row's `rect`.
pub(super) fn tile_rects(rect: egui::Rect, n: usize) -> Vec<egui::Rect> {
    let gap = 6.0;
    let count = n as f32;
    let tile_w = ((rect.width() - gap * (count - 1.0)) / count).max(1.0);
    (0..n)
        .map(|k| {
            let left = rect.left() + k as f32 * (tile_w + gap);
            egui::Rect::from_min_size(
                egui::pos2(left, rect.top()),
                egui::vec2(tile_w, rect.height()),
            )
        })
        .collect()
}

/// An angle in whole degrees.
pub(super) fn degrees(v: f64) -> String {
    format!("{}\u{b0}", fixed(v, 0))
}

/// `v` to `places` decimals, without the sign of a value that rounds to
/// zero ("0", never "-0").
pub(super) fn fixed(v: f64, places: usize) -> String {
    let text = format!("{v:.places$}");
    match text.strip_prefix('-') {
        Some(rest) if rest.chars().all(|c| c == '0' || c == '.') => rest.to_owned(),
        _ => text,
    }
}

/// The live posture tiles of a camera at `position`, from `posture` (the
/// one that came with the frame shown): face-on the spine's side bend, the
/// shoulders' tilt and the lead elbow; down the line the spine's forward
/// bend, both knees' flex and the trail elbow. None for a camera behind or
/// above the golfer.
pub(crate) fn posture_tiles(
    position: CameraPosition,
    posture: Option<&LivePosture>,
) -> Vec<(&'static str, String)> {
    let value = |f: fn(&LivePosture) -> Option<f32>| {
        posture
            .and_then(f)
            .map_or_else(|| NONE.to_owned(), |v| degrees(f64::from(v)))
    };
    let view = match position {
        CameraPosition::GolferFront => SwingView::FaceOn,
        CameraPosition::TargetLine => SwingView::DownTheLine,
        CameraPosition::GolferBack | CameraPosition::Overhead => return Vec::new(),
    };
    match view {
        SwingView::FaceOn => vec![
            ("Side bend", value(|p| p.spine_angle)),
            ("Shoulder tilt", value(|p| p.shoulder_tilt)),
            ("Lead elbow", value(|p| p.lead_arm_angle)),
        ],
        SwingView::DownTheLine => vec![
            ("Forward bend", value(|p| p.spine_angle)),
            ("Lead knee flex", value(|p| p.lead_knee_flex)),
            ("Trail knee flex", value(|p| p.trail_knee_flex)),
            ("Trail elbow", value(|p| p.trail_elbow_angle)),
        ],
    }
}
