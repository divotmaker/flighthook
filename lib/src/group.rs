//! How the UI shows stored shots: by set (practice block), by club, or the
//! last shot only.

use serde::{Deserialize, Serialize};

/// How the UI shows stored shots. Persisted in
/// [`FlighthookConfig::shot_grouping`](crate::FlighthookConfig::shot_grouping)
/// as the default view. Sets are recorded the same way in every view: a
/// club change or "New set" starts a new set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShotGrouping {
    /// The last shot only: no sets or club sections.
    None,
    /// One section per club, combining that club's sets since the last
    /// "New session".
    ByClub,
    /// One section per set.
    #[default]
    BySet,
}

impl ShotGrouping {
    /// True for the default view ([`ShotGrouping::BySet`]).
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouping_serde_names() {
        for (mode, name) in [
            (ShotGrouping::None, "none"),
            (ShotGrouping::ByClub, "by_club"),
            (ShotGrouping::BySet, "by_set"),
        ] {
            let json = serde_json::to_string(&mode).expect("ser");
            assert_eq!(json, format!("\"{name}\""));
            let back: ShotGrouping = serde_json::from_str(&json).expect("de");
            assert_eq!(back, mode);
        }
        assert!(serde_json::from_str::<ShotGrouping>("\"by_golfer\"").is_err());
    }

    #[test]
    fn grouping_default_is_by_set() {
        assert_eq!(ShotGrouping::default(), ShotGrouping::BySet);
        assert!(ShotGrouping::BySet.is_default());
        assert!(!ShotGrouping::None.is_default());
        assert!(!ShotGrouping::ByClub.is_default());
    }
}
