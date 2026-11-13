//! Swing analysis shown under a clip: tiles, tempo, metric labels and values, the metrics table,
//! frame tiles, grade colors and the miss line.

use super::*;

/// Milliseconds of a duration in ns, rounded.
pub(super) fn ms(ns: u64) -> String {
    format!("{} ms", (ns as f64 / 1e6).round())
}

/// The color of a value graded `fit`: aqua inside its band (inside both
/// the fade and the draw band of a shot-shape band), blue inside the fade
/// band only, green inside the draw band only, yellow near, red far.
pub(crate) fn grade_color(fit: Fit) -> egui::Color32 {
    match fit {
        Fit::Both => theme::GRADE_BOTH,
        Fit::Fade => theme::GRADE_FADE,
        Fit::Draw => theme::GRADE_DRAW,
        Fit::Near => theme::GRADE_NEAR,
        Fit::Far => theme::GRADE_FAR,
    }
}

/// One camera's swing analysis as the shot's tiles read it: its report,
/// the frame it shows (its time after the clip's first frame, `None`
/// outside the clip) and its grades against the picked reference.
#[derive(Clone, Copy)]
pub(crate) struct Swing<'a> {
    pub(crate) report: &'a SwingReportFile,
    pub(crate) t_ns: Option<u64>,
    pub(crate) grades: Option<&'a ReferenceGrades>,
}

/// The key position whose event is at the frame of `report` at `t_ns`, or
/// one frame either side of it; `None` elsewhere.
pub(crate) fn key_position(report: &SwingReportFile, t_ns: u64) -> Option<SwingPosition> {
    let at = report.series.frame_at(t_ns)?;
    SwingPosition::ALL
        .into_iter()
        .filter_map(|p| {
            let frame = report.series.frame_at(report.events.at(p)?.t_ns)?;
            let apart = frame.abs_diff(at);
            (apart <= 1).then_some((apart, p))
        })
        .min_by_key(|(apart, _)| *apart)
        .map(|(_, p)| p)
}

/// The one grade a tile shows for several cameras' `grades` of it: theirs
/// when every camera has one and they agree, else none.
pub(super) fn shared_grade<'a>(
    mut grades: impl Iterator<Item = Option<&'a MetricGrade>>,
) -> Option<MetricGrade> {
    let first = grades.next()??;
    grades
        .all(|g| g.is_some_and(|g| g.fit == first.fit))
        .then(|| first.clone())
}

/// Whether metric `id` is a turn: it reads `Open` or `Closed` when graded.
pub(super) fn is_turn(id: &str) -> bool {
    matches!(id, "hip_turn" | "shoulder_turn")
}

/// A graded value's tile on screen: its own text and direction word; a
/// graded turn reads `Open` or `Closed`, and any other value off its band
/// that has no direction word of its own takes the word of the side it is
/// off on ("29° Less").
pub(crate) fn graded_shown(
    id: &str,
    unit: &str,
    v: f64,
    cm: Option<f64>,
    imperial: bool,
    grade: Option<&MetricGrade>,
) -> Shown {
    let mut shown = metric_shown(id, unit, v, cm, imperial);
    let Some(grade) = grade else {
        return shown;
    };
    shown.color = grade_color(grade.fit);
    if direction_word(id).is_none() {
        shown.word = if is_turn(id) {
            signs::turn(grade.value)
        } else if grade.fit == Fit::Both {
            None
        } else {
            grade.direction.as_deref().and_then(signs::direction)
        };
    }
    shown
}

/// The tempo tiles of a shot: the ratio, the backswing and the downswing,
/// averaged over the `reports` that carry a tempo (the cameras see one
/// swing). The backswing and downswing are the means of the cameras'
/// durations and the ratio is the mean backswing over the mean downswing, so
/// the three agree; one camera shows its own numbers. Dashes when no report
/// has a tempo.
pub(crate) fn tempo_tiles(reports: &[&SwingReportFile]) -> Vec<(&'static str, String)> {
    let tempos: Vec<SwingTempo> = reports.iter().filter_map(|r| r.tempo).collect();
    match average_tempo(&tempos) {
        Some(t) => vec![
            ("Tempo", format!("{:.1} : 1", t.ratio)),
            ("Backswing", ms(t.backswing_ns)),
            ("Downswing", ms(t.downswing_ns)),
        ],
        None => vec![
            ("Tempo", NONE.to_owned()),
            ("Backswing", NONE.to_owned()),
            ("Downswing", NONE.to_owned()),
        ],
    }
}

/// The tempo tiles of a shot ([`tempo_tiles`]) with the ratio's and the
/// downswing's grades. The tiles average the cameras, the grades are per
/// camera: a tile takes the face-on cameras' grade (none when two disagree),
/// or the first camera's in column order when no face-on camera has one.
pub(crate) fn tempo_values<'a>(swings: &[Swing<'a>]) -> Vec<FrameValue> {
    let reports: Vec<&SwingReportFile> = swings.iter().map(|s| s.report).collect();
    let timed: Vec<&Swing> = swings.iter().filter(|s| s.report.tempo.is_some()).collect();
    let grade = |id: &str| {
        let of = |s: &&Swing<'a>| s.grades.and_then(move |g| g.grade(id, "swing"));
        let face_on: Vec<&&Swing> = timed
            .iter()
            .filter(|s| s.report.view == Some(SwingView::FaceOn))
            .collect();
        if face_on.iter().any(|s| of(s).is_some()) {
            shared_grade(face_on.iter().map(|s| of(s)))
        } else {
            timed.iter().find_map(of).cloned()
        }
    };
    tempo_tiles(&reports)
        .into_iter()
        .map(|(label, text)| {
            let id = match label {
                "Tempo" => "tempo_ratio",
                "Downswing" => "downswing_time",
                _ => "backswing",
            };
            let grade = (text != NONE).then(|| grade(id)).flatten();
            FrameValue {
                id,
                label,
                color: grade.as_ref().map_or(theme::TEXT, |g| grade_color(g.fit)),
                text,
                word: None,
                grade,
            }
        })
        .collect()
}

/// The tempo of one swing seen by several cameras: the mean durations and
/// their ratio. A single tempo is returned as it is.
pub(super) fn average_tempo(tempos: &[SwingTempo]) -> Option<SwingTempo> {
    let [first, rest @ ..] = tempos else {
        return None;
    };
    if rest.is_empty() {
        return Some(*first);
    }
    let n = tempos.len() as u128;
    let mean = |pick: fn(&SwingTempo) -> u64| {
        let sum: u128 = tempos.iter().map(|t| u128::from(pick(t))).sum();
        replay::rounded_mean(sum, n)
    };
    let (backswing_ns, downswing_ns) = (mean(|t| t.backswing_ns), mean(|t| t.downswing_ns));
    (downswing_ns > 0).then(|| SwingTempo {
        backswing_ns,
        downswing_ns,
        ratio: backswing_ns as f64 / downswing_ns as f64,
    })
}

/// A metric's name on screen: what it measures.
pub(crate) fn metric_label(id: &str) -> String {
    let known = match id {
        "hip_sway" => "Hip sway",
        "head_lateral" => "Head sway",
        "head_vertical" => "Head height",
        "lateral_spine_tilt" => "Side bend",
        "shoulder_tilt" => "Shoulder tilt",
        "lead_arm_angle" => "Lead elbow",
        "stance_width" => "Stance width",
        "hip_turn" => "Hip turn",
        "shoulder_turn" => "Shoulder turn",
        "spine_forward_bend" => "Forward bend",
        "early_extension" => "Hip depth",
        "lead_knee_flex" => "Lead knee flex",
        "trail_knee_flex" => "Trail knee flex",
        "trail_elbow_angle" => "Trail elbow",
        "hand_height" => "Hand height",
        other => {
            let words = other.replace('_', " ");
            let mut chars = words.chars();
            return chars.next().map_or_else(String::new, |c| {
                c.to_uppercase().collect::<String>() + chars.as_str()
            });
        }
    };
    known.to_owned()
}

/// A value `v` of a metric in `unit` (and its centimeters `cm`, when the
/// golfer's height was given) on screen: degrees whole, distances in inches
/// or centimeters when known, else as a share of the torso length, ratios
/// to 0.01.
pub(crate) fn metric_text(unit: &str, v: f64, cm: Option<f64>, imperial: bool) -> String {
    match unit {
        "degrees" => degrees(v),
        "torso" => match cm {
            Some(cm) if imperial => format!("{} in", fixed(cm / 2.54, 1)),
            Some(cm) => format!("{} cm", fixed(cm, 1)),
            None => format!("{}% torso", fixed(v * 100.0, 0)),
        },
        _ => fixed(v, 2),
    }
}

/// The word for the direction of a metric shown as a distance and a word
/// instead of a sign: the body's shifts from address. Hip sway and head
/// sway are measured toward the target ([`signs::target`]), hip depth
/// toward the ball ([`signs::ball`]) and head height up ([`signs::rise`]).
pub(super) fn direction_word(id: &str) -> Option<fn(f64) -> Option<&'static str>> {
    match id {
        "hip_sway" | "head_lateral" => Some(signs::target),
        "early_extension" => Some(signs::ball),
        "head_vertical" => Some(signs::rise),
        _ => None,
    }
}

/// A metric's value on screen: its text, for a shift from address the word
/// for its direction, and its color (its grade's, else plain).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Shown {
    pub(crate) text: String,
    pub(crate) word: Option<&'static str>,
    pub(crate) color: egui::Color32,
}

impl Shown {
    /// A dash: not measured.
    pub(super) fn none() -> Self {
        Self {
            text: NONE.to_owned(),
            word: None,
            color: theme::TEXT,
        }
    }
}

/// Metric `id`'s value `v` in `unit` (and its centimeters `cm`) on screen,
/// as [`metric_text`] writes it; a shift from address ([`direction_word`])
/// shows its distance with the word for its direction instead of a sign
/// (`Toward` / `Away`, `Toward ball` / `Back`, `Up` / `Down`), and no word
/// when the distance shows as 0. Plain (ungraded).
pub(crate) fn metric_shown(id: &str, unit: &str, v: f64, cm: Option<f64>, imperial: bool) -> Shown {
    let Some(direction) = direction_word(id) else {
        return Shown {
            text: metric_text(unit, v, cm, imperial),
            word: None,
            color: theme::TEXT,
        };
    };
    let text = metric_text(unit, v.abs(), cm.map(f64::abs), imperial);
    let zero = text.chars().filter(char::is_ascii_digit).all(|c| c == '0');
    Shown {
        word: if zero { None } else { direction(v) },
        text,
        color: theme::TEXT,
    }
}

/// Metrics the table leaves out: the hip width ratio (the hip turn shows
/// it as an angle) and the loss of posture (the forward bend's change from
/// address, which its own row shows).
pub(super) const NOT_IN_TABLE: &[&str] = &["hip_width_proxy", "loss_of_posture"];

/// The metrics table's positions in a column `width` wide: address, top,
/// impact and finish, and from [`WIDE_TABLE`] the lead arm parallel back
/// and down too.
pub(crate) fn table_positions(width: f32) -> &'static [SwingPosition] {
    const NARROW: [SwingPosition; 4] = [
        SwingPosition::Address,
        SwingPosition::Top,
        SwingPosition::Impact,
        SwingPosition::Finish,
    ];
    if width >= WIDE_TABLE {
        &SwingPosition::ALL
    } else {
        &NARROW
    }
}

/// Column width from which the metrics table shows the lead arm
/// positions.
pub(crate) const WIDE_TABLE: f32 = 560.0;

/// A cell of the metrics table: its value on screen (colored by its
/// grade), its grade, and the value as a tile reads it ([`graded_shown`],
/// for the hover text).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Cell {
    pub(crate) shown: Shown,
    pub(crate) grade: Option<MetricGrade>,
    pub(crate) worded: Shown,
}

/// The metrics table: each metric measured at any of `positions`, with its
/// value at each (a dash where it was not measured), colored by its grade
/// in `grades`.
pub(crate) fn metric_rows(
    report: &SwingReportFile,
    imperial: bool,
    positions: &[SwingPosition],
    grades: Option<&ReferenceGrades>,
) -> Vec<(String, Vec<Cell>)> {
    report
        .metrics
        .iter()
        .filter(|m| !NOT_IN_TABLE.contains(&m.id.as_str()))
        .filter_map(|m| {
            let cells: Vec<Option<Cell>> = positions
                .iter()
                .map(|&p| {
                    let v = m.values.at(p)?;
                    let cm = m.cm.as_ref().and_then(|c| c.at(p));
                    let grade = grades.and_then(|g| g.grade(&m.id, p.key())).cloned();
                    let worded = graded_shown(&m.id, &m.unit, v, cm, imperial, grade.as_ref());
                    let shown = Shown {
                        color: worded.color,
                        ..metric_shown(&m.id, &m.unit, v, cm, imperial)
                    };
                    Some(Cell {
                        shown,
                        grade,
                        worded,
                    })
                })
                .collect();
            cells.iter().any(Option::is_some).then(|| {
                (
                    metric_label(&m.id),
                    cells
                        .into_iter()
                        .map(|c| {
                            c.unwrap_or_else(|| Cell {
                                shown: Shown::none(),
                                grade: None,
                                worded: Shown::none(),
                            })
                        })
                        .collect(),
                )
            })
        })
        .collect()
}

/// A band `low`–`high` in `unit` on screen: degrees whole, inches to 0.1
/// (centimeters to 0.1 in metric units, rounded after the conversion),
/// seconds to 0.01, ratios to 0.1.
pub(crate) fn band_text(low: f64, high: f64, unit: &str, imperial: bool) -> String {
    match unit {
        "degrees" => format!("{}\u{2013}{}\u{b0}", fixed(low, 0), fixed(high, 0)),
        "inches" if imperial => format!("{}\u{2013}{} in", fixed(low, 1), fixed(high, 1)),
        "inches" => format!(
            "{}\u{2013}{} cm",
            fixed(low * 2.54, 1),
            fixed(high * 2.54, 1)
        ),
        "seconds" => format!("{}\u{2013}{} s", fixed(low, 2), fixed(high, 2)),
        _ => format!("{}\u{2013}{}", fixed(low, 1), fixed(high, 1)),
    }
}

/// A graded cell's hover text: the reference and its band (the fade and
/// the draw band of a shot-shape band), the value as its tile reads it
/// (`shown`), and the words for a value off the band.
pub(crate) fn band_tip(
    reference: ReferenceKind,
    grade: &MetricGrade,
    shown: &Shown,
    imperial: bool,
) -> String {
    let band = |b: [f64; 2]| band_text(b[0], b[1], &grade.unit, imperial);
    let mut parts = vec![match (grade.fade, grade.draw) {
        (None, None) => format!("{} {}", reference.label(), band([grade.low, grade.high])),
        (fade, draw) => {
            let mut s = reference.label().to_owned();
            if let Some(f) = fade {
                s.push_str(&format!(" \u{b7} fade {}", band(f)));
            }
            if let Some(d) = draw {
                s.push_str(&format!(" \u{b7} draw {}", band(d)));
            }
            s
        }
    }];
    parts.push(match shown.word {
        Some(word) => format!("{} {word}", shown.text),
        None => shown.text.clone(),
    });
    if let Some(word) = &grade.word {
        parts.push(word.clone());
    }
    parts.join(" \u{b7} ")
}

/// Height of a row of the swing tables.
pub(super) const TABLE_ROW_H: f32 = 22.0;

/// A shot's tempo tiles across `width`, once for all its cameras
/// ([`tempo_values`]).
pub(super) fn tempo_row(ui: &mut egui::Ui, tiles: &[FrameValue], width: f32) {
    let tempo = value_tiles(ui, tiles, width);
    devices::mark(ui, || "swing_tempo".to_owned(), &tempo);
}

/// A tile of the selected-frame row: its name and the metric it shows,
/// from the cameras of which view.
pub(super) struct FrameTile {
    pub(super) label: &'static str,
    pub(super) view: SwingView,
    pub(super) id: &'static str,
}

/// The selected-frame row, left to right.
pub(super) const FRAME_TILES: [FrameTile; 6] = [
    FrameTile {
        label: "Hip turn",
        view: SwingView::FaceOn,
        id: "hip_turn",
    },
    FrameTile {
        label: "Shoulder turn",
        view: SwingView::FaceOn,
        id: "shoulder_turn",
    },
    FrameTile {
        label: "Hip sway",
        view: SwingView::FaceOn,
        id: "hip_sway",
    },
    FrameTile {
        label: "Head sway",
        view: SwingView::FaceOn,
        id: "head_lateral",
    },
    FrameTile {
        label: "Forward bend",
        view: SwingView::DownTheLine,
        id: "spine_forward_bend",
    },
    FrameTile {
        label: "Hip depth",
        view: SwingView::DownTheLine,
        id: "early_extension",
    },
];

/// The spine tile when no camera is down the line.
pub(super) const SPINE_FACE_ON: FrameTile = FrameTile {
    label: "Side bend",
    view: SwingView::FaceOn,
    id: "lateral_spine_tilt",
};

/// One tile's content: the metric's id, its name, its value on screen
/// (with a direction word) in its grade's color, and its grade.
pub(crate) struct FrameValue {
    pub(crate) id: &'static str,
    pub(crate) label: &'static str,
    pub(crate) text: String,
    pub(crate) word: Option<&'static str>,
    pub(crate) color: egui::Color32,
    pub(crate) grade: Option<MetricGrade>,
}

/// The body at the replay's position: per tile, the metric from each
/// camera whose report is of the tile's view, at the frame that camera
/// shows, the mean when several have a value. A dash where none has. The
/// spine is the down-the-line forward bend, or the face-on side bend when
/// no report is down the line. Values read as in the metrics table.
///
/// A tile is graded only where the frame shown is a key position's (or
/// next to it, [`key_position`]): with that position's grade, the grade the
/// cameras showing it share ([`shared_grade`]). Elsewhere it is plain.
pub(crate) fn frame_tiles(swings: &[Swing], imperial: bool) -> Vec<FrameValue> {
    let down_the_line = swings
        .iter()
        .any(|s| s.report.view == Some(SwingView::DownTheLine));
    FRAME_TILES
        .iter()
        .map(|tile| {
            let tile = if tile.id == "spine_forward_bend" && !down_the_line {
                &SPINE_FACE_ON
            } else {
                tile
            };
            let samples: Vec<_> = swings
                .iter()
                .filter(|s| s.report.view == Some(tile.view))
                .filter_map(|s| {
                    let t = s.t_ns?;
                    let sample = s.report.sample(tile.id, t)?;
                    let grade =
                        key_position(s.report, t).and_then(|p| s.grades?.grade(tile.id, p.key()));
                    Some((sample, grade))
                })
                .collect();
            let (shown, grade) = match samples.as_slice() {
                [] => (Shown::none(), None),
                [(first, _), ..] => {
                    let n = samples.len() as f64;
                    let value = samples.iter().map(|(s, _)| s.value).sum::<f64>() / n;
                    let cm = samples
                        .iter()
                        .map(|(s, _)| s.cm)
                        .sum::<Option<f64>>()
                        .map(|sum| sum / n);
                    let grade = shared_grade(samples.iter().map(|(_, g)| *g));
                    let shown =
                        graded_shown(tile.id, &first.unit, value, cm, imperial, grade.as_ref());
                    (shown, grade)
                }
            };
            FrameValue {
                id: tile.id,
                label: tile.label,
                text: shown.text,
                word: shown.word,
                color: shown.color,
                grade,
            }
        })
        .collect()
}

/// The selected-frame tiles across `width`, under the tempo row (marks
/// `frame_metrics`, and `frame_metric:<id>` per tile).
pub(super) fn frame_row(ui: &mut egui::Ui, tiles: &[FrameValue], width: f32) {
    let row = value_tiles(ui, tiles, width);
    devices::mark(ui, || "frame_metrics".to_owned(), &row);
    if cfg!(test) {
        for (tile, rect) in tiles.iter().zip(tile_rects(row.rect, tiles.len())) {
            let shown = ui.interact(rect, ui.id().with(tile.id), egui::Sense::hover());
            devices::mark(ui, || format!("frame_metric:{}", tile.id), &shown);
        }
    }
}

/// `tiles` across `width`, each value in its color.
pub(crate) fn value_tiles(ui: &mut egui::Ui, tiles: &[FrameValue], width: f32) -> egui::Response {
    let shown: Vec<(&str, &str, Option<&str>, egui::Color32)> = tiles
        .iter()
        .map(|t| (t.label, t.text.as_str(), t.word, t.color))
        .collect();
    worded_tiles(ui, &shown, width)
}

/// The miss line's entries: each tile off its band (graded, not
/// [`Fit::Both`]) with its name, its words and its fit.
pub(crate) fn miss_entries(tiles: &[FrameValue]) -> Vec<(&'static str, String, Fit)> {
    tiles
        .iter()
        .filter_map(|t| {
            let grade = t.grade.as_ref().filter(|g| g.fit != Fit::Both)?;
            Some((t.label, grade.word.clone()?, grade.fit))
        })
        .collect()
}

/// Size of the miss line's text.
pub(super) const MISS_SIZE: f32 = 13.0;

/// The miss line's text: each entry's name muted and its words in its fit's
/// color, joined by middle dots.
pub(crate) fn miss_job(entries: &[(&str, String, Fit)], width: f32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let format = |color| egui::TextFormat {
        font_id: egui::FontId::proportional(MISS_SIZE),
        color,
        ..Default::default()
    };
    for (k, (label, words, fit)) in entries.iter().enumerate() {
        if k > 0 {
            job.append(" \u{b7} ", 0.0, format(theme::TEXT_MUTED));
        }
        job.append(label, 0.0, format(theme::TEXT_MUTED));
        job.append(&format!(" {words}"), 0.0, format(grade_color(*fit)));
    }
    job.wrap.max_width = width;
    job.wrap.max_rows = 2;
    job
}

/// The miss line under the tiles, `width` wide: the tiles off their band
/// ([`miss_entries`]), at most two lines. With none, a line's height is
/// kept empty while the shot has grades (`reserve`), so the tables below
/// hold still as the playhead passes a key position. Mark `frame_misses`.
pub(crate) fn miss_line(
    ui: &mut egui::Ui,
    entries: &[(&str, String, Fit)],
    width: f32,
    reserve: bool,
) {
    if entries.is_empty() && !reserve {
        return;
    }
    let galley = ui.fonts_mut(|f| f.layout_job(miss_job(entries, width)));
    let line = ui.fonts_mut(|f| f.row_height(&egui::FontId::proportional(MISS_SIZE)));
    let height = galley.size().y.max(line) + 6.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    if !entries.is_empty() {
        ui.painter()
            .galley(rect.left_top() + egui::vec2(0.0, 4.0), galley, theme::TEXT);
    }
    devices::mark(ui, || "frame_misses".to_owned(), &response);
}

/// One camera's swing metrics, `width` wide: its body measurements at the
/// key positions the width has room for ([`table_positions`]), colored by
/// their grades against `reference` (`grades`), each graded cell's band and
/// words on hover. Nothing when none was measured.
pub(super) fn swing_metrics(
    ui: &mut egui::Ui,
    camera: &str,
    report: &SwingReportFile,
    imperial: bool,
    width: f32,
    grades: Option<&ReferenceGrades>,
    reference: ReferenceKind,
) {
    let positions = table_positions(width);
    let metrics = metric_rows(report, imperial, positions, grades);
    if metrics.is_empty() {
        return;
    }
    ui.add_space(8.0);
    let n = positions.len() as f32;
    let share = if positions.len() > 4 { 0.22 } else { 0.27 };
    let name_w = (width * share).max(100.0);
    let value_w = ((width - name_w) / n).max(1.0);
    let columns: Vec<(f32, f32)> = (0..positions.len())
        .map(|i| (name_w + i as f32 * value_w, value_w))
        .collect();
    let rows: Vec<TableRow> = metrics
        .iter()
        .map(|(name, cells)| {
            (
                Some(name.as_str()),
                cells
                    .iter()
                    .map(|c| TableCell {
                        text: c.shown.text.as_str(),
                        word: c.shown.word,
                        color: c.shown.color,
                        tip: c
                            .grade
                            .as_ref()
                            .map(|g| band_tip(reference, g, &c.worded, imperial)),
                    })
                    .collect(),
            )
        })
        .collect();
    let heading: Vec<&str> = positions.iter().map(|p| p.label()).collect();
    table(ui, camera, width, &columns, Some(&heading), &rows);
}

/// A cell of [`table`]: its value, its direction word (if any), its color
/// and its hover text (if any).
pub(super) struct TableCell<'a> {
    pub(super) text: &'a str,
    pub(super) word: Option<&'a str>,
    pub(super) color: egui::Color32,
    pub(super) tip: Option<String>,
}

/// A row of [`table`]: its name, if any, and its cells.
pub(super) type TableRow<'a> = (Option<&'a str>, Vec<TableCell<'a>>);

/// A heading over a column `room` wide, in small capitals: one line, or two
/// split at a space when it doesn't fit on one.
pub(super) fn heading_lines(painter: &egui::Painter, heading: &str, room: f32) -> Vec<String> {
    let upper = heading.to_uppercase();
    let width = |s: &str| {
        painter
            .layout_no_wrap(
                s.to_owned(),
                egui::FontId::proportional(10.0),
                theme::TEXT_MUTED,
            )
            .size()
            .x
    };
    if width(&upper) <= room {
        return vec![upper];
    }
    match upper.rsplit_once(' ') {
        Some((first, second)) => vec![first.to_owned(), second.to_owned()],
        None => vec![upper],
    }
}

/// A table `width` wide: an optional heading row of small capitals over
/// `columns` (left edge and width of each value column), then `rows`, each
/// an optional name at the left edge and its cells (each with its direction
/// word, if any, in its color, and its hover text), on alternate stripes.
/// `salt` keeps its hover areas apart from another table's.
pub(super) fn table(
    ui: &mut egui::Ui,
    salt: &str,
    width: f32,
    columns: &[(f32, f32)],
    heading: Option<&[&str]>,
    rows: &[TableRow],
) {
    let painter = ui.painter().clone();
    let headings: Vec<Vec<String>> = heading.map_or_else(Vec::new, |h| {
        columns
            .iter()
            .zip(h)
            .map(|((_, w), h)| heading_lines(&painter, h, w - 8.0))
            .collect()
    });
    let lines = headings.iter().map(Vec::len).max().unwrap_or(0);
    let head_h = match lines {
        0 => 0.0,
        1 => 16.0,
        _ => 28.0,
    };
    let height = head_h + rows.len() as f32 * TABLE_ROW_H;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter_at(rect);
    let text = |at: egui::Pos2, s: &str, size: f32, color: egui::Color32| {
        let galley = painter.layout_no_wrap(s.to_owned(), egui::FontId::proportional(size), color);
        let y = at.y - galley.size().y / 2.0;
        painter.galley(egui::pos2(at.x, y), galley, color);
    };
    for ((left, _), lines) in columns.iter().zip(&headings) {
        let line_h = 12.0;
        let top = rect.top() + head_h / 2.0 - (lines.len() as f32 - 1.0) * line_h / 2.0;
        for (k, line) in lines.iter().enumerate() {
            let at = egui::pos2(rect.left() + left + 8.0, top + k as f32 * line_h);
            text(at, line, 10.0, theme::TEXT_MUTED);
        }
    }
    for (k, (name, cells)) in rows.iter().enumerate() {
        let top = rect.top() + head_h + k as f32 * TABLE_ROW_H;
        let row =
            egui::Rect::from_min_size(egui::pos2(rect.left(), top), egui::vec2(width, TABLE_ROW_H));
        if k % 2 == 0 && name.is_some() {
            painter.rect_filled(row, 4.0, theme::SURFACE);
        }
        let y = row.center().y;
        if let Some(name) = name {
            text(
                egui::pos2(rect.left() + 8.0, y),
                name,
                13.0,
                theme::TEXT_MUTED,
            );
        }
        for (j, ((left, column_w), cell)) in columns.iter().zip(cells).enumerate() {
            let (value, word) = worded_value(&painter, cell.text, cell.word, 14.0, column_w - 12.0);
            let at = egui::pos2(rect.left() + left + 8.0, y - value.size().y / 2.0);
            painter.galley(at, value, cell.color);
            if let Some((word, offset)) = word {
                painter.galley(at + offset, word, cell.color);
            }
            if let Some(tip) = &cell.tip {
                let area = egui::Rect::from_min_size(
                    egui::pos2(rect.left() + left, top),
                    egui::vec2(*column_w, TABLE_ROW_H),
                );
                let hover = ui.interact(area, ui.id().with((salt, k, j)), egui::Sense::hover());
                devices::mark(ui, || format!("cell:{salt}:{k}:{j}"), &hover);
                hover.on_hover_text(tip);
            }
        }
    }
}
