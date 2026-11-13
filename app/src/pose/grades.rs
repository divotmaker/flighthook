//! Grades of a camera's swing numbers (alpha): each metric at each key
//! position, and the tempo, placed against stance's bands from published
//! tour averages (`stance::golf::norms`), for the tour and the amateur
//! reference. Served by `GET /api/shots/{shot_id}/grades/{camera}`,
//! computed per request from the clip's swing analysis and the Player card
//! as it is now, never stored.
//!
//! The club the shot was hit with picks the club class ([`Club::class`]);
//! a putter or an unknown club is not graded. The Player card's sex picks
//! the men's or women's bands, and its height puts distances into inches
//! (an assumed height for the sex without one). Every stance metric is
//! measured relative to the target, so the hand changes nothing.

use std::path::Path;

use flighthook::ClubExt;
use flighthook::{
    BodyGrades, Club, ClubClass, Fit, Handedness, MetricGrade, PlayerSection, ReferenceGrades,
    ReferenceKind, Sex, Side,
};
use stance::golf::norms::{self, Grade, NormUnit, Scale};
use stance::golf::{Context, Norms, SwingReport, TORSO_PER_HEIGHT};

/// What a shot's grades are for: its club, the golfer's hand, and the
/// Player card.
#[derive(Debug, Clone)]
pub struct GradeInput {
    pub club: Option<Club>,
    pub hand: Option<Handedness>,
    pub player: PlayerSection,
}

/// The grades of the swing analysis at `path` (`<camera>.swing.json`) for
/// shot `shot_id`'s clip from `camera`. `None` when it can't be read.
pub fn grade_file(
    path: &Path,
    shot_id: &str,
    camera: &str,
    input: GradeInput,
) -> Option<BodyGrades> {
    let report = SwingReport::read(path)
        .map_err(|e| tracing::debug!("grades: {}: {e}", path.display()))
        .ok()?;
    Some(grade(&report, shot_id, camera, input))
}

/// The grades of `report` against the built-in bands, for every reference.
#[must_use]
pub fn grade(report: &SwingReport, shot_id: &str, camera: &str, input: GradeInput) -> BodyGrades {
    let player = input.player.in_effect();
    let club_class = input.club.and_then(|c| c.class());
    let report = with_height(report, player.height);
    let references = match (club_class, Norms::builtin()) {
        (Some(class), Ok(norms)) => ReferenceKind::ALL
            .iter()
            .map(|&reference| {
                let ctx = Context::new(
                    stance_class(class),
                    stance_sex(player.sex),
                    stance_reference(reference),
                );
                let found = report.events.impact_source != stance::golf::ImpactSource::Given;
                let grades = if found {
                    norms.grade_report(&report, &ctx)
                } else {
                    Vec::new()
                };
                let tempo = report
                    .tempo
                    .map(|t| norms.grade_tempo(&t, &ctx))
                    .unwrap_or_default();
                ReferenceGrades {
                    reference,
                    grades: grades.iter().map(metric_grade).collect(),
                    tempo: tempo.iter().map(metric_grade).collect(),
                }
            })
            .collect(),
        (Some(_), Err(e)) => {
            tracing::warn!("grades: {e}");
            Vec::new()
        }
        (None, _) => Vec::new(),
    };
    BodyGrades {
        shot_id: shot_id.to_owned(),
        camera: camera.to_owned(),
        club_class,
        sex: player.sex,
        hand: input.hand.unwrap_or(Handedness::Right),
        references,
    }
}

/// `report` with its distances scaled by the player's `height` now: the
/// body scale's centimeters per pixel from the height (none without one),
/// the analysis-time centimeters dropped. A report without the upright
/// torso length keeps the centimeters it was written with.
fn with_height(report: &SwingReport, height: Option<f64>) -> SwingReport {
    let mut report = report.clone();
    let Some(scale) = report.scale.as_mut() else {
        return report;
    };
    let Some(upright) = scale.upright_px.filter(|u| *u > 0.0) else {
        return report;
    };
    scale.cm_per_px = height.map(|h| TORSO_PER_HEIGHT * h / upright);
    for metric in &mut report.metrics {
        metric.cm = None;
    }
    report
}

fn stance_class(class: ClubClass) -> norms::ClubClass {
    match class {
        ClubClass::Driver => norms::ClubClass::Driver,
        ClubClass::MidIron => norms::ClubClass::MidIron,
        ClubClass::Wedge => norms::ClubClass::Wedge,
    }
}

fn stance_sex(sex: Sex) -> norms::Sex {
    match sex {
        Sex::Male => norms::Sex::Male,
        Sex::Female => norms::Sex::Female,
    }
}

fn stance_reference(reference: ReferenceKind) -> norms::Reference {
    match reference {
        ReferenceKind::Amateur => norms::Reference::Amateur,
        _ => norms::Reference::Tour,
    }
}

/// A serde enum's snake_case name (`"arm_parallel_back"`).
fn key(value: impl serde::Serialize) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(s)) => s,
        _ => String::new(),
    }
}

/// `grade` on the wire.
fn metric_grade(grade: &Grade) -> MetricGrade {
    let id = key(grade.metric);
    // stance's words start with its own name for the quantity; the UI puts
    // its own name first.
    let word = grade.fit_word().map(|w| {
        w.strip_prefix(grade.metric.name())
            .unwrap_or(&w)
            .trim_start()
            .to_owned()
    });
    let range = |r: norms::Range| [r.center - r.sd, r.center + r.sd];
    MetricGrade {
        id,
        position: key(grade.position),
        value: grade.value,
        unit: match grade.unit {
            NormUnit::Degrees => "degrees",
            NormUnit::Inches => "inches",
            NormUnit::Seconds => "seconds",
            _ => "ratio",
        }
        .to_owned(),
        low: grade.center - grade.sd,
        high: grade.center + grade.sd,
        fade: grade.fade.map(range),
        draw: grade.draw.map(range),
        z: grade.z,
        z_fade: grade.z_fade,
        z_draw: grade.z_draw,
        fit: match grade.fit {
            norms::Fit::Both => Fit::Both,
            norms::Fit::Fade => Fit::Fade,
            norms::Fit::Draw => Fit::Draw,
            norms::Fit::Near => Fit::Near,
            _ => Fit::Far,
        },
        side: match grade.side {
            norms::Side::Low => Side::Low,
            norms::Side::High => Side::High,
            _ => Side::On,
        },
        direction: grade.direction.map(key),
        word,
        proxy_sd: grade.proxy_sd(),
        assumed_scale: grade.scale == Scale::Assumed,
        club_fallback: grade.club_fallback,
        sex_fallback: grade.sex_fallback,
        confidence: grade.confidence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Inches of hip sway as stance's torso units, for the report's scale
    /// (torso 200 px, upright 220 px) and a 177 cm golfer.
    fn torso(inches: f64) -> f64 {
        inches * 2.54 / (200.0 * TORSO_PER_HEIGHT * 177.0 / 220.0)
    }

    /// A face-on report: hips 44° open at impact (38° at the top), the
    /// shoulders 31° open, the shoulder line 55° at impact, the side bend
    /// 18° at impact and 30° at the finish, the hips 4 in toward the
    /// target, a 3:1 tempo with a 0.25 s downswing.
    fn report(impact_source: &str) -> SwingReport {
        let json = format!(
            r#"{{"version":1,"view":"face_on","handed":"right",
            "scale":{{"torso_px":200.0,"upright_px":220.0}},
            "events":{{"address":{{"t_ns":0,"frame":0,"confidence":0.9}},
              "impact":{{"t_ns":1000000000,"frame":60,"confidence":0.9}},
              "impact_source":"{impact_source}"}},
            "tempo":{{"backswing_ns":750000000,"downswing_ns":250000000,"ratio":3.0}},
            "metrics":[
              {{"id":"hip_sway","view":"face_on","unit":"torso","values":{{"impact":{sway}}},"cm":{{"impact":99.0}},"confidence":0.9}},
              {{"id":"lateral_spine_tilt","view":"face_on","unit":"degrees","values":{{"impact":18.0,"finish":30.0}},"confidence":0.9}},
              {{"id":"shoulder_tilt","view":"face_on","unit":"degrees","values":{{"impact":55.0}},"confidence":0.9}},
              {{"id":"hip_turn","view":"face_on","unit":"degrees","values":{{"top":38.0,"impact":44.0}},"confidence":0.9}},
              {{"id":"shoulder_turn","view":"face_on","unit":"degrees","values":{{"impact":31.0}},"confidence":0.9}}]}}"#,
            sway = torso(4.0),
        );
        SwingReport::from_json(&json).expect("report")
    }

    fn input(club: Option<Club>, sex: Sex, height: Option<f64>) -> GradeInput {
        GradeInput {
            club,
            hand: Some(Handedness::Right),
            player: PlayerSection { sex, height },
        }
    }

    fn driver() -> GradeInput {
        input(Some(Club::Driver), Sex::Male, Some(177.0))
    }

    fn tour(g: &BodyGrades) -> &ReferenceGrades {
        g.reference(ReferenceKind::Tour).expect("tour")
    }

    #[test]
    fn each_band_gives_its_fit_and_its_words() {
        let g = grade(&report("refined"), "s1", "camera.0", driver());
        assert_eq!((g.shot_id.as_str(), g.camera.as_str()), ("s1", "camera.0"));
        assert_eq!(g.club_class, Some(ClubClass::Driver));
        assert_eq!(
            g.references.iter().map(|r| r.reference).collect::<Vec<_>>(),
            [ReferenceKind::Tour, ReferenceKind::Amateur]
        );
        let t = tour(&g);
        // Hips 44° open: inside the fade band (36–46) only.
        let hips = t.grade("hip_turn", "impact").expect("hips");
        assert_eq!((hips.fit, hips.side), (Fit::Fade, Side::High));
        assert_eq!(hips.word.as_deref(), Some("more open: fade side"));
        assert_eq!(hips.direction.as_deref(), Some("open"));
        assert_eq!((hips.low, hips.high), (33.0, 43.0));
        assert_eq!(
            (hips.fade, hips.draw),
            (Some([36.0, 46.0]), Some([30.0, 40.0]))
        );
        assert_eq!((hips.unit.as_str(), hips.value), ("degrees", 44.0));
        assert!(hips.proxy_sd);
        // Shoulders 31° open: inside both bands.
        let shoulders = t.grade("shoulder_turn", "impact").expect("shoulders");
        assert_eq!((shoulders.fit, shoulders.side), (Fit::Both, Side::On));
        assert_eq!(shoulders.word, None);
        // The shoulder line far past both bands: the miss and its pattern.
        let tilt = t.grade("shoulder_tilt", "impact").expect("tilt");
        assert_eq!((tilt.fit, tilt.side), (Fit::Far, Side::High));
        assert_eq!(
            tilt.word.as_deref(),
            Some("more than tour: hanging back, fat or push-draw")
        );
        // Shape-neutral: one band, never fade or draw.
        let side = t.grade("lateral_spine_tilt", "impact").expect("side bend");
        assert_eq!((side.fit, side.fade, side.draw), (Fit::Both, None, None));
        // A distance in inches from the Player card's height.
        let sway = t.grade("hip_sway", "impact").expect("sway");
        assert_eq!(sway.unit, "inches");
        assert!((sway.value - 4.0).abs() < 1e-9, "{}", sway.value);
        assert_eq!(sway.fit, Fit::Both);
        assert!(!sway.assumed_scale);
        // Tempo against its bands.
        let ratio = t.grade("tempo_ratio", "swing").expect("ratio");
        assert_eq!((ratio.fit, ratio.value), (Fit::Both, 3.0));
        let down = t.grade("downswing_time", "swing").expect("downswing");
        assert_eq!((down.unit.as_str(), down.value), ("seconds", 0.25));
    }

    #[test]
    fn a_turn_short_of_the_center_reads_less_of_its_direction() {
        // Shoulders 15° open against the tour's 30° open.
        let mut r = report("refined");
        let shoulders = r
            .metrics
            .iter_mut()
            .find(|m| m.id == stance::golf::MetricId::ShoulderTurn)
            .expect("shoulders");
        shoulders.values.impact = Some(15.0);
        let g = grade(&r, "s1", "camera.0", driver());
        let s = tour(&g)
            .grade("shoulder_turn", "impact")
            .expect("shoulders");
        assert_eq!((s.fit, s.side), (Fit::Far, Side::Low));
        assert_eq!(s.direction.as_deref(), Some("closed"));
        assert_eq!(
            s.word.as_deref(),
            Some("less open than tour: shoulders behind, push or hook")
        );
    }

    #[test]
    fn a_one_camera_turn_is_graded_at_impact_only_and_ungraded_rows_are_left_out() {
        let g = grade(&report("refined"), "s1", "camera.0", driver());
        let t = tour(&g);
        assert!(
            t.grade("hip_turn", "top").is_none(),
            "one-camera turn at the top"
        );
        assert!(t.grade("hip_turn", "impact").is_some());
        assert!(
            t.grade("lateral_spine_tilt", "finish").is_none(),
            "the finish back bend has a band but is not graded"
        );
    }

    #[test]
    fn the_reference_picks_the_bands() {
        let g = grade(&report("refined"), "s1", "camera.0", driver());
        let amateur = g.reference(ReferenceKind::Amateur).expect("amateur");
        // No amateur band for the impact side bend; the amateur one-camera
        // hip turn's direction is unknowable (its center is within 2 SD).
        assert!(amateur.grade("lateral_spine_tilt", "impact").is_none());
        assert!(amateur.grade("hip_turn", "impact").is_none());
        let tilt = amateur.grade("shoulder_tilt", "impact").expect("tilt");
        assert_eq!(
            tilt.word.as_deref().map(|w| w.contains("than amateurs")),
            Some(true)
        );
        assert_ne!(amateur.grades, tour(&g).grades);
    }

    #[test]
    fn the_player_card_picks_the_sex_and_the_height() {
        // The women's hip turn at impact is 42 ± 5: 44° is inside both bands.
        let women = grade(
            &report("refined"),
            "s1",
            "camera.0",
            input(Some(Club::Driver), Sex::Female, Some(177.0)),
        );
        assert_eq!(women.sex, Sex::Female);
        let hips = tour(&women).grade("hip_turn", "impact").unwrap();
        assert_eq!(hips.fit, Fit::Both);
        // Without a height, an assumed one; a taller golfer's hips moved
        // further. The centimeters written with the analysis are not used.
        let none = grade(
            &report("refined"),
            "s1",
            "camera.0",
            input(Some(Club::Driver), Sex::Male, None),
        );
        let sway = tour(&none).grade("hip_sway", "impact").unwrap();
        assert!(sway.assumed_scale && (sway.value - 4.0).abs() < 1e-9);
        let tall = grade(
            &report("refined"),
            "s1",
            "camera.0",
            input(Some(Club::Driver), Sex::Male, Some(200.0)),
        );
        let sway = tour(&tall).grade("hip_sway", "impact").unwrap();
        assert!(
            (sway.value - 4.0 * 200.0 / 177.0).abs() < 1e-9,
            "{}",
            sway.value
        );
        // A height out of range is no height.
        let wrong = grade(
            &report("refined"),
            "s1",
            "camera.0",
            input(Some(Club::Driver), Sex::Male, Some(40.0)),
        );
        assert!(
            tour(&wrong)
                .grade("hip_sway", "impact")
                .unwrap()
                .assumed_scale
        );
    }

    #[test]
    fn the_club_picks_the_class_and_a_putter_or_no_club_grades_nothing() {
        let r = report("refined");
        let wedge = grade(
            &r,
            "s1",
            "camera.0",
            input(Some(Club::SandWedge), Sex::Male, None),
        );
        assert_eq!(wedge.club_class, Some(ClubClass::Wedge));
        let hips = tour(&wedge).grade("hip_turn", "impact").unwrap();
        assert!(hips.club_fallback, "wedges use the mid-iron bands");
        for club in [Some(Club::Putter), None] {
            let g = grade(&r, "s1", "camera.0", input(club, Sex::Male, None));
            assert!(
                g.club_class.is_none() && g.references.is_empty(),
                "{club:?}"
            );
        }
    }

    #[test]
    fn a_left_handed_golfer_grades_the_same() {
        let r = report("refined");
        let right = grade(&r, "s1", "camera.0", driver());
        let mut lh = driver();
        lh.hand = Some(Handedness::Left);
        let left = grade(&r, "s1", "camera.0", lh);
        assert_eq!(left.hand, Handedness::Left);
        assert_eq!(left.references, right.references);
    }

    #[test]
    fn no_swing_found_grades_only_the_tempo() {
        let g = grade(&report("given"), "s1", "camera.0", driver());
        let t = tour(&g);
        assert!(t.grades.is_empty());
        assert!(!t.tempo.is_empty());
    }

    #[test]
    fn a_file_is_read_and_graded() {
        let dir = crate::actors::web::tests::TempDir(std::env::temp_dir().join(format!(
            "fh-grades-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        )));
        std::fs::create_dir_all(&dir.0).unwrap();
        let path = dir.0.join("camera.0.swing.json");
        assert!(
            grade_file(&path, "s1", "camera.0", driver()).is_none(),
            "missing"
        );
        std::fs::write(&path, report("refined").to_json().unwrap()).unwrap();
        let g = grade_file(&path, "s1", "camera.0", driver()).expect("graded");
        assert_eq!(g, grade(&report("refined"), "s1", "camera.0", driver()));
    }
}
