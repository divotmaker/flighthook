//! The `[player]` section and the golfer records (`[[golfers]]`): sex, height, golfer ids and
//! the own-id rule.

use super::*;

/// The player's sex: picks the men's or the women's tour averages.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sex {
    /// Men's tour averages. The default.
    #[default]
    Male,
    /// Women's tour averages.
    Female,
}

impl Sex {
    /// True for the default ([`Sex::Male`]).
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Stable key, as the config file holds it.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Male => "male",
            Self::Female => "female",
        }
    }

    /// User-facing label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Male => "Male",
            Self::Female => "Female",
        }
    }

    /// Both, in display order.
    #[must_use]
    pub fn all() -> [Self; 2] {
        [Self::Male, Self::Female]
    }
}

/// Facts about the player that the swing analysis uses. Every field is
/// read live. A field at its default is left out of the file and the API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PlayerSection {
    /// Whose tour averages the player is compared with. Defaults to
    /// [`Sex::Male`].
    #[serde(default, skip_serializing_if = "Sex::is_default")]
    pub sex: Sex,
    /// The player's height in centimeters,
    /// [`MIN_HEIGHT_CM`](Self::MIN_HEIGHT_CM) to
    /// [`MAX_HEIGHT_CM`](Self::MAX_HEIGHT_CM). With it, swing metrics that
    /// are distances are also given in centimeters (inches in imperial
    /// units); without it they stay fractions of the golfer's torso length.
    /// Not set by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<f64>,
}

impl PlayerSection {
    /// Shortest height accepted, cm.
    pub const MIN_HEIGHT_CM: f64 = 100.0;
    /// Tallest height accepted, cm.
    pub const MAX_HEIGHT_CM: f64 = 250.0;

    /// True when every setting is at its default.
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Why a setting is out of range, or `None` when all are valid:
    /// `height` (when set) [`MIN_HEIGHT_CM`](Self::MIN_HEIGHT_CM) to
    /// [`MAX_HEIGHT_CM`](Self::MAX_HEIGHT_CM).
    #[must_use]
    pub fn error(&self) -> Option<String> {
        self.height.filter(|h| !height_valid(*h)).map(|h| {
            format!(
                "height {h} is outside {} to {} cm",
                Self::MIN_HEIGHT_CM,
                Self::MAX_HEIGHT_CM
            )
        })
    }

    /// The settings in effect: a height out of range is no height.
    #[must_use]
    pub fn in_effect(&self) -> Self {
        Self {
            sex: self.sex,
            height: self.height.filter(|h| height_valid(*h)),
        }
    }
}

/// The namespace of flighthook's own golfer ids (`flighthook.<uuid>`).
pub const GOLFER_NAMESPACE: &str = "flighthook";

/// A golfer (`[[golfers]]`).
///
/// [`identifiers`](Self::identifiers) lists every golfer id that is this
/// golfer: its own `flighthook.<uuid>`, written when the golfer is created,
/// and any game ids linked to it (`pureplay.local-3`). Each id belongs to one
/// golfer only.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GolferRecord {
    /// The golfer ids that are this golfer, its own `flighthook.*` id first.
    #[serde(default)]
    pub identifiers: Vec<String>,
    /// Display name.
    #[serde(default)]
    pub name: String,
    /// The golfer's hand. Omitted while unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handed: Option<Handedness>,
    /// The golfer's clubs, in display order. Omitted while empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bag: Vec<Club>,
    /// Whose tour averages the golfer is compared with. Omitted while unset
    /// (men's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sex: Option<Sex>,
    /// Height in centimeters,
    /// [`PlayerSection::MIN_HEIGHT_CM`] to [`PlayerSection::MAX_HEIGHT_CM`].
    /// Omitted while unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<f64>,
}

impl GolferRecord {
    /// A golfer named `name` whose own id is `id`.
    #[must_use]
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            identifiers: vec![id.into()],
            name: name.into(),
            ..Self::default()
        }
    }

    /// The golfer's own id: its first `flighthook.*` identifier.
    #[must_use]
    pub fn own_id(&self) -> Option<&str> {
        self.identifiers
            .iter()
            .map(String::as_str)
            .find(|id| is_own_golfer_id(id))
    }

    /// Whether `id` is one of this golfer's identifiers.
    #[must_use]
    pub fn is(&self, id: &str) -> bool {
        self.identifiers.iter().any(|own| own == id)
    }

    /// The identifiers that are game ids (not `flighthook.*`), in order.
    pub fn game_ids(&self) -> impl Iterator<Item = &str> {
        self.identifiers
            .iter()
            .map(String::as_str)
            .filter(|id| !is_own_golfer_id(id))
    }

    /// The facts the swing analysis uses: sex and height, a height out of
    /// range being no height.
    #[must_use]
    pub fn facts(&self) -> PlayerSection {
        PlayerSection {
            sex: self.sex.unwrap_or_default(),
            height: self.height,
        }
        .in_effect()
    }

    /// Why a setting is out of range, or `None` when all are valid.
    #[must_use]
    pub fn error(&self) -> Option<String> {
        PlayerSection {
            sex: Sex::default(),
            height: self.height,
        }
        .error()
    }
}

/// Whether `id` is one of flighthook's own golfer ids (`flighthook.*`).
#[must_use]
pub fn is_own_golfer_id(id: &str) -> bool {
    id.split_once('.')
        .is_some_and(|(namespace, local)| namespace == GOLFER_NAMESPACE && !local.is_empty())
}

/// One golfer id held by two golfers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateGolferId {
    /// The id.
    pub id: String,
    /// The name of the golfer that holds it first.
    pub first: String,
    /// The name of the golfer that holds it again.
    pub second: String,
}

impl std::fmt::Display for DuplicateGolferId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "'{}' is in two golfers: {} and {}",
            self.id, self.first, self.second
        )
    }
}

/// Whether `cm` is an acceptable player height: finite and from
/// [`PlayerSection::MIN_HEIGHT_CM`] to [`PlayerSection::MAX_HEIGHT_CM`].
pub(super) fn height_valid(cm: f64) -> bool {
    cm.is_finite() && (PlayerSection::MIN_HEIGHT_CM..=PlayerSection::MAX_HEIGHT_CM).contains(&cm)
}
