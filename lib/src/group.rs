//! Shot groups ("sets"): runs of stored shots hit with one club.

use serde::{Deserialize, Serialize};

use crate::{Club, ShotKey};

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

/// What a group is keyed on. `golfer` is reserved: nothing sets it (GSPro
/// Open Connect v1 carries no golfer identity), so it is always `None`, and a
/// golfer picker can fill it without a wire change.
///
/// Not serialised; derived from a [`ShotGroupInfo`] with
/// [`ShotGroupInfo::key`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct GroupKey {
    pub golfer: Option<String>,
    pub club: Option<Club>,
}

/// A run of shots hit with one club: a "set".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShotGroupInfo {
    /// UUID v4, unique across restarts.
    pub id: String,
    /// Display ordinal ("SET 3"): starts at 1 each run, assigned at
    /// creation. Not part of the shot export.
    pub number: u32,
    /// Session the set belongs to: starts at 0 each run and counts up with
    /// every "New session". Omitted when 0.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub session: u32,
    /// Reserved for a golfer dimension; omitted until something sets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub golfer: Option<String>,
    /// Club the group was hit with; absent when no club was selected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub club: Option<Club>,
    /// RFC 3339 UTC, host clock: when the group was created.
    pub started_at: String,
    /// Started by the "New set" command (`new_shot_group`) rather than by a
    /// club change or a first shot.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub manual: bool,
    /// Still receiving shots. A closed group never reopens.
    #[serde(default)]
    pub open: bool,
    /// Shots with data in the stored history that belong to this group.
    #[serde(default)]
    pub shots: u32,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's skip_serializing_if passes a reference
fn is_zero(v: &u32) -> bool {
    *v == 0
}

impl ShotGroupInfo {
    /// The key this group is filed under.
    #[must_use]
    pub fn key(&self) -> GroupKey {
        GroupKey {
            golfer: self.golfer.clone(),
            club: self.club,
        }
    }
}

/// Which shot joined a group (on the `shot_group` event).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupedShot {
    /// Actor ID of the device that produced the shot (`"mevo.0"`).
    pub actor: String,
    pub key: ShotKey,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> ShotGroupInfo {
        ShotGroupInfo {
            id: "6f1c2a3b-4d5e-4f60-8a71-92b3c4d5e6f7".into(),
            number: 3,
            session: 0,
            golfer: None,
            club: Some(Club::Iron7),
            started_at: "2026-10-26T13:58:02.117Z".into(),
            manual: false,
            open: true,
            shots: 10,
        }
    }

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

    #[test]
    fn group_info_wire_shape() {
        let json = serde_json::to_string(&info()).expect("ser");
        assert_eq!(
            json,
            r#"{"id":"6f1c2a3b-4d5e-4f60-8a71-92b3c4d5e6f7","number":3,"club":"7I","started_at":"2026-10-26T13:58:02.117Z","open":true,"shots":10}"#
        );
        let back: ShotGroupInfo = serde_json::from_str(&json).expect("de");
        assert_eq!(back, info());
    }

    #[test]
    fn group_info_round_trips_with_golfer_and_manual() {
        let g = ShotGroupInfo {
            golfer: Some("Eric".into()),
            club: None,
            manual: true,
            ..info()
        };
        let v = serde_json::to_value(&g).expect("ser");
        assert_eq!(v["golfer"], "Eric");
        assert_eq!(v["manual"], true);
        assert!(v.get("club").is_none());
        let back: ShotGroupInfo = serde_json::from_value(v).expect("de");
        assert_eq!(back, g);
    }

    #[test]
    fn group_info_carries_a_later_session() {
        let g = ShotGroupInfo {
            session: 2,
            ..info()
        };
        let json = serde_json::to_string(&g).expect("ser");
        assert!(json.contains(r#""number":3,"session":2,"#), "{json}");
        let back: ShotGroupInfo = serde_json::from_str(&json).expect("de");
        assert_eq!(back, g);
    }

    #[test]
    fn group_info_flags_default_false() {
        let g: ShotGroupInfo = serde_json::from_str(
            r#"{"id":"a","number":1,"started_at":"2026-10-26T13:58:02.117Z"}"#,
        )
        .expect("de");
        assert!(!g.manual);
        assert!(!g.open);
        assert_eq!(g.shots, 0);
        assert_eq!(g.session, 0);
        assert_eq!(g.club, None);
        assert_eq!(g.golfer, None);
    }

    #[test]
    fn key_comes_from_the_record() {
        assert_eq!(
            info().key(),
            GroupKey {
                golfer: None,
                club: Some(Club::Iron7),
            }
        );
        assert_eq!(
            GroupKey::default(),
            GroupKey {
                golfer: None,
                club: None
            }
        );
    }

    #[test]
    fn grouped_shot_round_trips() {
        let s = GroupedShot {
            actor: "mevo.0".into(),
            key: ShotKey {
                shot_id: "550e8400".into(),
                shot_number: 12,
            },
        };
        let json = serde_json::to_string(&s).expect("ser");
        assert_eq!(
            json,
            r#"{"actor":"mevo.0","key":{"shot_id":"550e8400","shot_number":12}}"#
        );
        let back: GroupedShot = serde_json::from_str(&json).expect("de");
        assert_eq!(back, s);
    }
}
