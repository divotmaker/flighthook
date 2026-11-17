//! The card's Shots setting ([`PureplayShots`]): what becomes of a candidate
//! shot, a shot of a player at this machine (`isLocal`):
//!
//! - **Auto**: a candidate that echoes a shot a simulator output sent (the
//!   send registry,
//!   [`SentShots::take_echo`](crate::state::sent_shots::SentShots)) is not
//!   taken; Pureplay's result goes to the shot flighthook sent. Any other
//!   candidate is taken.
//! - **From Pureplay**: every candidate is taken, with no matching
//!   ([`matches_echoes`]).
//! - **From flighthook**: none is taken; echoes are still matched, so their
//!   results reach flighthook's shots.

use flighthook::{PureplayShots, ShotKey};

/// Whether the card matches a candidate against the shots the simulator
/// outputs sent, in mode `shots`. "From Pureplay" takes every local shot
/// as its own, so it does not.
#[must_use]
pub fn matches_echoes(shots: PureplayShots) -> bool {
    shots != PureplayShots::FromPureplay
}

/// What becomes of a candidate shot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// The card takes it: a flighthook shot of its own.
    Take,
    /// It echoes flighthook's shot `ShotKey`: not taken; its result goes
    /// to that shot.
    Echo(ShotKey),
    /// Not taken, and no shot of flighthook's: "From flighthook" with no
    /// matching send.
    Ignore,
}

/// What becomes of a candidate shot in mode `shots`. `echo` finds (and
/// consumes) the send the shot echoes; it is asked only when the mode
/// matches echoes.
pub fn decide(shots: PureplayShots, echo: impl FnOnce() -> Option<ShotKey>) -> Decision {
    if !matches_echoes(shots) {
        return Decision::Take;
    }
    match echo() {
        Some(key) => Decision::Echo(key),
        None if shots == PureplayShots::FromFlighthook => Decision::Ignore,
        None => Decision::Take,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u32) -> ShotKey {
        ShotKey::new(format!("k-{n}"), n)
    }

    /// Auto takes what is no echo; From Pureplay takes everything without
    /// asking; From flighthook takes nothing but still matches echoes.
    #[test]
    fn the_shots_setting_decides() {
        use PureplayShots::{Auto, FromFlighthook, FromPureplay};
        assert_eq!(decide(Auto, || None), Decision::Take);
        assert_eq!(decide(Auto, || Some(key(1))), Decision::Echo(key(1)));
        assert_eq!(decide(FromFlighthook, || None), Decision::Ignore);
        assert_eq!(
            decide(FromFlighthook, || Some(key(2))),
            Decision::Echo(key(2))
        );
        let asked = std::cell::Cell::new(false);
        let d = decide(FromPureplay, || {
            asked.set(true);
            Some(key(3))
        });
        assert_eq!(d, Decision::Take);
        assert!(!asked.get(), "From Pureplay does not match echoes");
        assert!(matches_echoes(Auto) && matches_echoes(FromFlighthook));
        assert!(!matches_echoes(FromPureplay));
    }
}
