//! The Cameras pane's **Last shot** strip (alpha): while Live is on, the
//! newest shot's swing under the live pictures, as the shot's own page
//! shows it at impact: the tempo tiles, the six body tiles at each
//! camera's impact, colored against the picked reference, and the line
//! naming the tiles off their band.
//!
//! The strip fetches only the newest shot's swing analyses and grades (no
//! clip), once the server has analyzed them (`video[].swing`, which body
//! tracking's Live mode does for every clip it records). It appears once a
//! camera's swing and its grades are in, and the next shot's replaces it
//! when that shot's are.

use std::sync::Arc;

use crate::app::FlighthookApp;
use crate::net::ReplayReply;
use crate::panels::cameras::{self, FrameValue, Swing};
use crate::panels::{devices, shots};
use crate::theme;
use crate::types::{ShotRow, SwingReportFile, UnitSystem};
use flighthook::{BodyGrades, ReferenceKind};

/// Height of the strip: its title, the tempo tiles, the body tiles and two
/// lines of misses.
pub(crate) const LAST_SHOT_H: f32 = 182.0;

/// One camera's swing and grades of the shot.
#[derive(Default)]
struct Camera {
    camera: String,
    asked: bool,
    report: Option<Arc<SwingReportFile>>,
    /// The grades key the grades were last asked for with.
    graded_for: Option<String>,
    grades: Option<Arc<BodyGrades>>,
}

/// A request the strip needs sent.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Fetch {
    Swing(String),
    Grades(String),
}

/// One shot's swings and grades, fetched by request number `seq`.
pub(crate) struct LastShot {
    seq: u64,
    shot_id: String,
    title: String,
    cameras: Vec<Camera>,
}

impl LastShot {
    pub(crate) fn new(seq: u64, shot: &ShotRow) -> Self {
        let mut title = format!("Last shot \u{b7} #{}", shot.shot_number);
        if let Some(club) = shot.club_selected {
            title.push_str(&format!(" \u{b7} {club}"));
        }
        Self {
            seq,
            shot_id: shot.shot_id.clone(),
            title,
            cameras: Vec::new(),
        }
    }

    /// What to ask for now: each camera's swing analysis once `shot` says
    /// it is done, then its grades for `grades_key`, again when the key
    /// changes.
    pub(crate) fn due(&mut self, shot: &ShotRow, grades_key: &str) -> Vec<Fetch> {
        for video in shot.video.iter().filter(|v| v.swing) {
            if !self.cameras.iter().any(|c| c.camera == video.camera) {
                self.cameras.push(Camera {
                    camera: video.camera.clone(),
                    ..Camera::default()
                });
            }
        }
        let mut due = Vec::new();
        for cam in &mut self.cameras {
            if !cam.asked {
                cam.asked = true;
                due.push(Fetch::Swing(cam.camera.clone()));
            } else if cam.report.is_some() && cam.graded_for.as_deref() != Some(grades_key) {
                cam.graded_for = Some(grades_key.to_owned());
                due.push(Fetch::Grades(cam.camera.clone()));
            }
        }
        due
    }

    /// Take a reply; false when it is not this strip's.
    pub(crate) fn reply(&mut self, reply: ReplayReply) -> bool {
        match reply {
            ReplayReply::Swing {
                replay,
                camera,
                result,
            } if replay == self.seq => {
                if let Some(cam) = self.cameras.iter_mut().find(|c| c.camera == camera) {
                    cam.report = result.ok().map(Arc::new);
                }
                true
            }
            ReplayReply::Grades {
                replay,
                camera,
                result,
            } if replay == self.seq => {
                if let Some(cam) = self.cameras.iter_mut().find(|c| c.camera == camera) {
                    cam.grades = result.ok().map(Arc::new);
                }
                true
            }
            _ => false,
        }
    }

    /// Whether there is a swing to show: a camera found one and its grades
    /// are in.
    pub(crate) fn ready(&self) -> bool {
        self.cameras
            .iter()
            .any(|c| c.grades.is_some() && c.report.as_ref().is_some_and(|r| r.swing_found()))
    }

    /// Each camera that found a swing, at its own impact, graded against
    /// `reference`.
    fn swings(&self, reference: ReferenceKind) -> Vec<Swing<'_>> {
        self.cameras
            .iter()
            .filter_map(|c| {
                let report = c.report.as_deref().filter(|r| r.swing_found())?;
                Some(Swing {
                    report,
                    t_ns: report.events.impact.map(|e| e.t_ns),
                    grades: c.grades.as_deref().and_then(|g| g.reference(reference)),
                })
            })
            .collect()
    }

    /// The strip's tiles: the tempo, then the body at impact.
    pub(crate) fn tiles(
        &self,
        reference: ReferenceKind,
        imperial: bool,
    ) -> (Vec<FrameValue>, Vec<FrameValue>) {
        let swings = self.swings(reference);
        (
            cameras::tempo_values(&swings),
            cameras::frame_tiles(&swings, imperial),
        )
    }
}

/// The newest shot's strip being fetched (`next`) and the one shown
/// (`shown`).
#[derive(Default)]
pub(crate) struct LiveStrip {
    shown: Option<LastShot>,
    next: Option<LastShot>,
}

impl LiveStrip {
    /// The strip asking with request number `seq`, if any.
    pub(crate) fn find_mut(&mut self, seq: u64) -> Option<&mut LastShot> {
        [&mut self.shown, &mut self.next]
            .into_iter()
            .flatten()
            .find(|s| s.seq == seq)
    }

    /// The request number of the strip being fetched.
    #[cfg(test)]
    pub(crate) fn next_seq(&self) -> Option<u64> {
        self.next.as_ref().map(|s| s.seq)
    }

    /// The strip on screen, once one is ready.
    pub(crate) fn shown(&self) -> Option<&LastShot> {
        self.shown.as_ref()
    }
}

impl FlighthookApp {
    /// Follow the newest shot (under the device filter): fetch its swings
    /// and grades, and show it once it is ready, keeping the strip shown
    /// until then. Nothing while body tracking is off.
    pub(crate) fn sync_last_shot(&mut self, ctx: &egui::Context) {
        if !self.body_tracking().mode.is_on() {
            self.live_strip = LiveStrip::default();
            return;
        }
        let Some(i) = shots::latest_index(&self.shots, self.filter()) else {
            return;
        };
        let shot = &self.shots[i];
        let key = self.settings.grades_key();
        let shown_is_newest = self
            .live_strip
            .shown
            .as_ref()
            .is_some_and(|s| s.shot_id == shot.shot_id);
        if shown_is_newest {
            self.live_strip.next = None;
        } else if self
            .live_strip
            .next
            .as_ref()
            .is_none_or(|s| s.shot_id != shot.shot_id)
        {
            self.replay_seq += 1;
            self.live_strip.next = Some(LastShot::new(self.replay_seq, shot));
        }
        let mut sends = Vec::new();
        for strip in [&mut self.live_strip.shown, &mut self.live_strip.next]
            .into_iter()
            .flatten()
        {
            if let Some(row) = self.shots.iter().rev().find(|s| s.shot_id == strip.shot_id) {
                for fetch in strip.due(row, &key) {
                    sends.push((strip.seq, strip.shot_id.clone(), fetch));
                }
            }
        }
        // Tests answer for the server.
        #[cfg(not(test))]
        for (seq, shot_id, fetch) in sends {
            match fetch {
                Fetch::Swing(camera) => {
                    crate::net::fetch_swing(ctx, &self.pending, seq, &shot_id, &camera);
                }
                Fetch::Grades(camera) => {
                    crate::net::fetch_grades(ctx, &self.pending, seq, &shot_id, &camera);
                }
            }
        }
        #[cfg(test)]
        let _ = (ctx, sends);
        if self.live_strip.next.as_ref().is_some_and(LastShot::ready) {
            self.live_strip.shown = self.live_strip.next.take();
        }
    }

    /// The strip, `width` wide (mark `live_last_shot`), when one is shown:
    /// its title, the tempo tiles, the body tiles at impact and the misses,
    /// in [`LAST_SHOT_H`].
    pub(crate) fn render_last_shot(&self, ui: &mut egui::Ui, width: f32) {
        let Some(strip) = self.live_strip.shown() else {
            return;
        };
        let imperial = self.units() == UnitSystem::Imperial;
        let (tempo, frame) = strip.tiles(self.body_reference, imperial);
        let shown = ui
            .allocate_ui_with_layout(
                egui::vec2(width, LAST_SHOT_H),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_min_size(egui::vec2(width, LAST_SHOT_H));
                    ui.label(
                        egui::RichText::new(&strip.title)
                            .size(16.0)
                            .strong()
                            .color(theme::TEXT),
                    );
                    cameras::value_tiles(ui, &tempo, width);
                    cameras::value_tiles(ui, &frame, width);
                    let mut misses = cameras::miss_entries(&tempo);
                    misses.extend(cameras::miss_entries(&frame));
                    cameras::miss_line(ui, &misses, width, true);
                },
            )
            .response;
        devices::mark(ui, || "live_last_shot".to_owned(), &shown);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shot(id: &str, swings: &[&str]) -> ShotRow {
        ShotRow {
            actor: "mock_monitor.0".into(),
            shot_id: id.into(),
            shot_number: 3,
            launch: Some(flighthook::Launch::default()),
            flight: None,
            club: None,
            impact: None,
            derived: None,
            club_selected: Some(flighthook::Club::Driver),
            handed: None,
            group: None,
            finished: true,
            video: swings
                .iter()
                .map(|c| flighthook::ShotVideo {
                    camera: (*c).to_owned(),
                    position: flighthook::CameraPosition::GolferFront,
                    duration_ms: 1000,
                    fps: 60.0,
                    trigger_offset_ms: 500,
                    impact_offset_ms: 500,
                    complete: true,
                    dropped: 0,
                    pose: true,
                    swing: true,
                    rotation: 0,
                    analyzed_with: None,
                })
                .collect(),
        }
    }

    fn report(source: &str) -> SwingReportFile {
        serde_json::from_str(&format!(
            r#"{{"version":1,"view":"face_on","events":{{"impact":{{"t_ns":5,"confidence":0.9}},"impact_source":"{source}"}},"metrics":[]}}"#
        ))
        .unwrap()
    }

    fn grades() -> BodyGrades {
        BodyGrades {
            shot_id: "a".into(),
            camera: "camera.0".into(),
            club_class: Some(flighthook::ClubClass::Driver),
            sex: flighthook::Sex::Male,
            hand: crate::types::Handedness::Right,
            references: Vec::new(),
        }
    }

    #[test]
    fn the_strip_fetches_each_swing_then_its_grades_and_is_ready_with_both() {
        let row = shot("a", &["camera.0"]);
        let mut strip = LastShot::new(7, &row);
        assert_eq!(strip.title, "Last shot \u{b7} #3 \u{b7} DR");
        assert_eq!(strip.due(&row, "k"), [Fetch::Swing("camera.0".into())]);
        assert!(strip.due(&row, "k").is_empty(), "once");
        assert!(!strip.ready());
        // Another request number's reply is not taken.
        let swing = |replay, source| ReplayReply::Swing {
            replay,
            camera: "camera.0".into(),
            result: Ok(report(source)),
        };
        assert!(!strip.reply(swing(8, "refined")));
        assert!(strip.reply(swing(7, "refined")));
        assert_eq!(strip.due(&row, "k"), [Fetch::Grades("camera.0".into())]);
        assert!(!strip.ready(), "not before the grades");
        assert!(strip.reply(ReplayReply::Grades {
            replay: 7,
            camera: "camera.0".into(),
            result: Ok(grades()),
        }));
        assert!(strip.ready());
        // A changed Player card: the grades again.
        assert!(strip.due(&row, "k").is_empty());
        assert_eq!(strip.due(&row, "k2"), [Fetch::Grades("camera.0".into())]);
        // A clip that found no swing is not one to show.
        let mut none = LastShot::new(9, &row);
        none.due(&row, "k");
        none.reply(swing(9, "given"));
        none.due(&row, "k");
        none.reply(ReplayReply::Grades {
            replay: 9,
            camera: "camera.0".into(),
            result: Ok(grades()),
        });
        assert!(!none.ready());
        // A shot whose clips are not analyzed yet asks for nothing.
        let mut early = LastShot::new(10, &shot("b", &[]));
        assert!(early.due(&shot("b", &[]), "k").is_empty());
    }
}
