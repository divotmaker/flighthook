//! Words for the sign of a shot's directional values, as the Cards view
//! shows them next to the value's magnitude.
//!
//! Signs follow FRP and are absolute: + is right of the target line for
//! every golfer, whatever their hand (launch direction, offline, sidespin,
//! club path and face angle), and + attack angle is up. Words that name a
//! shape (open, in-to-out, fade) depend on the golfer's hand: a face
//! pointing right of the target is open for a right-hander and closed for
//! a left-hander. Left, right, up and down do not.
//!
//! A value of exactly 0 has no word.

use crate::types::Handedness;

/// `positive` for a value above 0, `negative` below, none at 0.
fn by_sign(v: f64, positive: &'static str, negative: &'static str) -> Option<&'static str> {
    if v > 0.0 {
        Some(positive)
    } else if v < 0.0 {
        Some(negative)
    } else {
        None
    }
}

/// `right` for a right-hander, `left` mirrored: the word for a value right
/// of the target line.
fn for_hand(handed: Handedness, right: &'static str, left: &'static str) -> &'static str {
    match handed {
        Handedness::Right => right,
        Handedness::Left => left,
    }
}

/// Launch direction, offline and lateral: `R` right of the target line,
/// `L` left of it.
pub(crate) fn side(v: f64) -> Option<&'static str> {
    by_sign(v, "R", "L")
}

/// Face angle: open or closed to the target for a `handed` golfer.
pub(crate) fn face(v: f64, handed: Handedness) -> Option<&'static str> {
    let (right, left) = (
        for_hand(handed, "Open", "Closed"),
        for_hand(handed, "Closed", "Open"),
    );
    by_sign(v, right, left)
}

/// Club path: in-to-out or out-to-in for a `handed` golfer.
pub(crate) fn path(v: f64, handed: Handedness) -> Option<&'static str> {
    let (right, left) = (
        for_hand(handed, "In-to-out", "Out-to-in"),
        for_hand(handed, "Out-to-in", "In-to-out"),
    );
    by_sign(v, right, left)
}

/// Attack angle: hitting up or down.
pub(crate) fn attack(v: f64) -> Option<&'static str> {
    by_sign(v, "Up", "Down")
}

/// Sidespin: the curve it puts on the ball for a `handed` golfer. A ball
/// curving right fades for a right-hander and draws for a left-hander.
pub(crate) fn sidespin(v: f64, handed: Handedness) -> Option<&'static str> {
    let (right, left) = (
        for_hand(handed, "Fade", "Draw"),
        for_hand(handed, "Draw", "Fade"),
    );
    by_sign(v, right, left)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RH: Handedness = Handedness::Right;
    const LH: Handedness = Handedness::Left;

    #[test]
    fn side_is_the_same_for_both_hands() {
        assert_eq!(side(2.4), Some("R"));
        assert_eq!(side(-2.4), Some("L"));
        assert_eq!(side(0.0), None);
        assert_eq!(side(-0.0), None);
    }

    #[test]
    fn face_is_open_right_of_the_target_for_a_right_hander() {
        assert_eq!(face(1.8, RH), Some("Open"));
        assert_eq!(face(-1.8, RH), Some("Closed"));
        assert_eq!(face(1.8, LH), Some("Closed"));
        assert_eq!(face(-1.8, LH), Some("Open"));
        assert_eq!(face(0.0, RH), None);
        assert_eq!(face(0.0, LH), None);
    }

    #[test]
    fn path_right_of_the_target_is_in_to_out_for_a_right_hander() {
        assert_eq!(path(1.2, RH), Some("In-to-out"));
        assert_eq!(path(-1.2, RH), Some("Out-to-in"));
        assert_eq!(path(1.2, LH), Some("Out-to-in"));
        assert_eq!(path(-1.2, LH), Some("In-to-out"));
        assert_eq!(path(0.0, RH), None);
    }

    #[test]
    fn attack_is_up_or_down_for_both_hands() {
        assert_eq!(attack(3.0), Some("Up"));
        assert_eq!(attack(-0.9), Some("Down"));
        assert_eq!(attack(0.0), None);
    }

    #[test]
    fn sidespin_curving_right_fades_for_a_right_hander() {
        assert_eq!(sidespin(644.0, RH), Some("Fade"));
        assert_eq!(sidespin(-644.0, RH), Some("Draw"));
        assert_eq!(sidespin(644.0, LH), Some("Draw"));
        assert_eq!(sidespin(-644.0, LH), Some("Fade"));
        assert_eq!(sidespin(0.0, LH), None);
    }
}
