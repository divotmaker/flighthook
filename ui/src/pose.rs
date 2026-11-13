//! Body tracking drawing (alpha): the golfer's skeleton over a camera
//! picture, live and on a shot's clips in the Cameras tab,
//! and in the replay. Pure geometry, and
//! one function that paints it. The lead side (the left of a right-handed
//! golfer) and the trail side get their own colors.

use egui::{Color32, Pos2, Rect, Stroke, vec2};

use crate::theme;
use crate::types::Handedness;
use flighthook::{
    BodySide, HALPE26_BONES, HALPE26_KEYPOINTS, Keypoint, halpe26_bone_side, halpe26_side,
};

/// The lead side's color.
pub(crate) const LEAD: Color32 = theme::TEAL;
/// The trail side's color.
pub(crate) const TRAIL: Color32 = Color32::from_rgb(255, 146, 60);
/// The midline's color (head, neck, hips).
pub(crate) const MIDLINE: Color32 = theme::TEXT;

/// Where frame pixels land in a picture rect: the frame fitted inside it
/// at its own aspect, centered.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Fit {
    origin: Pos2,
    scale: f32,
    /// The frame's width on screen.
    width: f32,
}

impl Fit {
    /// A `width` × `height` pixel frame shown in `rect`; `None` when either
    /// has no area.
    pub(crate) fn new(width: u32, height: u32, rect: Rect) -> Option<Self> {
        if width == 0 || height == 0 || rect.width() <= 0.0 || rect.height() <= 0.0 {
            return None;
        }
        let (w, h) = (width as f32, height as f32);
        let scale = (rect.width() / w).min(rect.height() / h);
        let origin = rect.center() - vec2(w * scale, h * scale) / 2.0;
        Some(Self {
            origin,
            scale,
            width: w * scale,
        })
    }

    /// Keypoint `k` on screen.
    pub(crate) fn map(&self, k: &Keypoint) -> Pos2 {
        self.origin + vec2(k[0], k[1]) * self.scale
    }
}

/// Whether a camera rotation of `rotation` clockwise degrees is a quarter
/// turn (90 or 270), which swaps the picture's width and height.
pub(crate) fn quarter_turn(rotation: u16) -> bool {
    rotation % 180 == 90
}

/// The `[width, height]` of a `width × height` frame turned clockwise by
/// `rotation` degrees (0, 90, 180 or 270).
pub(crate) fn turned(width: u32, height: u32, rotation: u16) -> [u32; 2] {
    if quarter_turn(rotation) {
        [height, width]
    } else {
        [width, height]
    }
}

/// The size of a picture of `size` turned clockwise by `rotation` degrees.
pub(crate) fn turned_size(size: egui::Vec2, rotation: u16) -> egui::Vec2 {
    if quarter_turn(rotation) {
        vec2(size.y, size.x)
    } else {
        size
    }
}

/// The rect the unturned picture is painted in so that, turned clockwise by
/// `rotation` degrees about its center, it fills `rect`.
pub(crate) fn unturned_rect(rect: Rect, rotation: u16) -> Rect {
    Rect::from_center_size(rect.center(), turned_size(rect.size(), rotation))
}

/// Paint texture `texture` turned clockwise by `rotation` degrees so that
/// it fills `rect`, whose aspect is the turned picture's. An unturned
/// picture keeps `corners`; a turned one has square corners.
pub(crate) fn paint_turned(
    ui: &egui::Ui,
    texture: egui::TextureId,
    rect: Rect,
    rotation: u16,
    corners: egui::CornerRadius,
) {
    let at = unturned_rect(rect, rotation);
    let image = egui::Image::new((texture, at.size()));
    match rotation % 360 {
        0 => image.corner_radius(corners).paint_at(ui, at),
        degrees => image
            .rotate(f32::from(degrees).to_radians(), vec2(0.5, 0.5))
            .paint_at(ui, at),
    }
}

/// Whether keypoint `k` is drawn: finite, and scoring at least `min_score`.
pub(crate) fn shown(k: &Keypoint, min_score: f32) -> bool {
    k.iter().all(|v| v.is_finite()) && k[2] >= min_score
}

/// The side a golfer playing with `hand` leads with: a right-hander's left.
pub(crate) fn lead_side(hand: Handedness) -> BodySide {
    match hand {
        Handedness::Left => BodySide::Right,
        Handedness::Right => BodySide::Left,
    }
}

/// The color of a part on `side` of a golfer who leads with `lead`.
pub(crate) fn color(side: BodySide, lead: BodySide) -> Color32 {
    match side {
        BodySide::Center => MIDLINE,
        s if s == lead => LEAD,
        _ => TRAIL,
    }
}

/// A skeleton laid out on screen: its bones and joints, with colors.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Layout {
    pub(crate) bones: Vec<([Pos2; 2], Color32)>,
    pub(crate) joints: Vec<(Pos2, Color32)>,
    /// Line width, points.
    pub(crate) width: f32,
}

/// Halpe-26 `keypoints` laid out with `fit`: the joints scoring at least
/// `min_score`, and the bones between two such joints. Nothing for another
/// number of keypoints.
pub(crate) fn layout(
    fit: &Fit,
    keypoints: &[Keypoint],
    min_score: f32,
    hand: Handedness,
) -> Layout {
    if keypoints.len() != HALPE26_KEYPOINTS {
        return Layout::default();
    }
    let lead = lead_side(hand);
    let bones = HALPE26_BONES
        .iter()
        .filter(|(a, b)| shown(&keypoints[*a], min_score) && shown(&keypoints[*b], min_score))
        .map(|&(a, b)| {
            (
                [fit.map(&keypoints[a]), fit.map(&keypoints[b])],
                color(halpe26_bone_side((a, b)), lead),
            )
        })
        .collect();
    let joints = keypoints
        .iter()
        .enumerate()
        .filter(|(_, k)| shown(k, min_score))
        .map(|(i, k)| (fit.map(k), color(halpe26_side(i), lead)))
        .collect();
    Layout {
        bones,
        joints,
        width: (fit.width / 320.0).clamp(1.5, 4.0),
    }
}

/// Paint `keypoints` (a `frame` = `[width, height]` pixel picture shown in
/// `rect`) with `painter`, as [`layout`] lays them out.
pub(crate) fn paint(
    painter: &egui::Painter,
    frame: [u32; 2],
    rect: Rect,
    keypoints: &[Keypoint],
    min_score: f32,
    hand: Handedness,
) {
    let Some(fit) = Fit::new(frame[0], frame[1], rect) else {
        return;
    };
    let layout = layout(&fit, keypoints, min_score, hand);
    let shadow = Color32::from_black_alpha(140);
    for ([a, b], _) in &layout.bones {
        painter.line_segment([*a, *b], Stroke::new(layout.width + 2.0, shadow));
    }
    for ([a, b], color) in &layout.bones {
        painter.line_segment([*a, *b], Stroke::new(layout.width, *color));
    }
    for (at, color) in &layout.joints {
        painter.circle(*at, layout.width * 1.4, *color, Stroke::new(1.0, shadow));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    fn person(score: f32) -> Vec<Keypoint> {
        (0..HALPE26_KEYPOINTS)
            .map(|i| [100.0 + i as f32 * 10.0, 50.0 + i as f32 * 5.0, score])
            .collect()
    }

    #[test]
    fn a_quarter_turn_swaps_the_size_and_a_half_turn_keeps_it() {
        assert_eq!(turned(1280, 720, 0), [1280, 720]);
        assert_eq!(turned(1280, 720, 90), [720, 1280]);
        assert_eq!(turned(1280, 720, 180), [1280, 720]);
        assert_eq!(turned(1280, 720, 270), [720, 1280]);
        assert_eq!(turned_size(vec2(16.0, 9.0), 90), vec2(9.0, 16.0));
        assert_eq!(turned_size(vec2(16.0, 9.0), 180), vec2(16.0, 9.0));
        // Turned about its center, the unturned rect covers the target.
        let target = Rect::from_min_size(pos2(10.0, 20.0), vec2(90.0, 160.0));
        let at = unturned_rect(target, 270);
        assert_eq!(at.center(), target.center());
        assert_eq!(at.size(), vec2(160.0, 90.0));
        assert_eq!(unturned_rect(target, 180), target);
    }

    #[test]
    fn a_frame_fits_its_rect_at_its_aspect() {
        // A 1280×720 frame in a rect of the same aspect: plain scaling.
        let rect = Rect::from_min_size(pos2(20.0, 30.0), vec2(640.0, 360.0));
        let fit = Fit::new(1280, 720, rect).unwrap();
        assert_eq!(fit.map(&[0.0, 0.0, 1.0]), pos2(20.0, 30.0));
        assert_eq!(fit.map(&[1280.0, 720.0, 1.0]), pos2(660.0, 390.0));
        assert_eq!(fit.map(&[640.0, 360.0, 1.0]), rect.center());
        // A 4:3 frame in a 16:9 rect: bars left and right, height filled.
        let fit = Fit::new(640, 480, rect).unwrap();
        assert_eq!(fit.map(&[0.0, 0.0, 1.0]), pos2(20.0 + 80.0, 30.0));
        assert_eq!(fit.map(&[640.0, 480.0, 1.0]), pos2(20.0 + 560.0, 390.0));
        // A wide frame in a tall rect: bars above and below.
        let tall = Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 400.0));
        let fit = Fit::new(200, 100, tall).unwrap();
        assert_eq!(fit.map(&[0.0, 0.0, 1.0]), pos2(0.0, 175.0));
        assert_eq!(fit.map(&[200.0, 100.0, 1.0]), pos2(100.0, 225.0));
        assert!(Fit::new(0, 720, rect).is_none());
        assert!(Fit::new(1280, 720, Rect::NOTHING).is_none());
    }

    #[test]
    fn keypoints_below_the_minimum_score_are_left_out_with_their_bones() {
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(640.0, 360.0));
        let fit = Fit::new(1280, 720, rect).unwrap();
        let all = layout(&fit, &person(0.9), 0.3, Handedness::Right);
        assert_eq!(all.joints.len(), 26);
        assert_eq!(all.bones.len(), HALPE26_BONES.len());
        let mut kps = person(0.9);
        kps[9][2] = 0.29; // left wrist
        let some = layout(&fit, &kps, 0.3, Handedness::Right);
        assert_eq!(some.joints.len(), 25);
        // The left forearm (7-9) goes with it.
        assert_eq!(some.bones.len(), HALPE26_BONES.len() - 1);
        assert_eq!(layout(&fit, &kps, 0.29, Handedness::Right).joints.len(), 26);
        assert!(
            layout(&fit, &person(0.2), 0.3, Handedness::Right)
                .joints
                .is_empty()
        );
        assert_eq!(
            layout(&fit, &kps[..17], 0.0, Handedness::Right),
            Layout::default()
        );
        let mut broken = person(0.9);
        broken[0][0] = f32::NAN;
        assert_eq!(
            layout(&fit, &broken, 0.3, Handedness::Right).joints.len(),
            25
        );
    }

    #[test]
    fn the_lead_side_follows_the_hand() {
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(640.0, 360.0));
        let fit = Fit::new(1280, 720, rect).unwrap();
        let joint = |l: &Layout, i: usize| l.joints[i].1;
        let rh = layout(&fit, &person(0.9), 0.0, Handedness::Right);
        assert_eq!(joint(&rh, 5), LEAD, "a right-hander's left shoulder");
        assert_eq!(joint(&rh, 6), TRAIL);
        assert_eq!(joint(&rh, 18), MIDLINE, "neck");
        let lh = layout(&fit, &person(0.9), 0.0, Handedness::Left);
        assert_eq!(joint(&lh, 5), TRAIL);
        assert_eq!(joint(&lh, 6), LEAD);
        assert_ne!(LEAD, TRAIL);
    }
}
