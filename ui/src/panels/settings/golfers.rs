//! The Golfers card: the golfer records (`[[golfers]]`), each with its name,
//! sex, hand, height and bag, and the game ids linked to it (added from the
//! profiles games publish on the bus). The card has its own Save
//! (`?scope=golfers`).

use flighthook::{
    Club, GolferRecord, Handedness, PlayerSection, Sex, UnitSystem, is_own_golfer_id,
};

use super::{
    SaveTarget, TEXT_EDIT_MARGIN, card_header, field_error, field_label, field_widths, form_rows,
    format_distance_value, lock_hint, save_cancel, save_error_line,
};
use crate::app::FlighthookApp;
use crate::panels::devices;
use crate::panels::settings::Pending;
use crate::theme;

/// A height typed in inches (`inches`) or centimeters, stored in
/// centimeters rounded to 0.1. Empty is no height; a height left as loaded
/// saves its loaded value exactly.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct HeightField {
    pub(crate) text: String,
    pub(crate) inches: bool,
    loaded: Option<f64>,
}

impl HeightField {
    pub(crate) fn new(cm: Option<f64>, inches: bool) -> Self {
        Self {
            text: Self::text_of(cm, inches),
            inches,
            loaded: cm,
        }
    }

    fn text_of(cm: Option<f64>, inches: bool) -> String {
        cm.map_or_else(String::new, |cm| {
            let v = if inches { cm / 2.54 } else { cm };
            format_distance_value((v * 10.0).round() / 10.0)
        })
    }

    /// The unit as the card shows it.
    pub(crate) fn unit(&self) -> &'static str {
        if self.inches { "in" } else { "cm" }
    }

    /// Switch the unit, converting the typed value.
    pub(crate) fn set_inches(&mut self, inches: bool) {
        if inches == self.inches {
            return;
        }
        if let Some(cm) = self.cm() {
            self.text = Self::text_of(cm, inches);
        }
        self.inches = inches;
    }

    /// The height to save in centimeters (`Some(None)`: no height), or
    /// `None` while the text is not a number.
    pub(crate) fn cm(&self) -> Option<Option<f64>> {
        if self.text == Self::text_of(self.loaded, self.inches) {
            return Some(self.loaded);
        }
        let text = self.text.trim();
        if text.is_empty() {
            return Some(None);
        }
        let v: f64 = text.parse().ok()?;
        let cm = if self.inches { v * 2.54 } else { v };
        Some(Some((cm * 10.0).round() / 10.0))
    }

    /// The height to save while it is empty or a number in range.
    pub(crate) fn valid_cm(&self) -> Option<Option<f64>> {
        self.cm().filter(|&height| {
            PlayerSection {
                height,
                ..PlayerSection::default()
            }
            .error()
            .is_none()
        })
    }
}

/// One golfer on the card: its record (everything but the height, which is
/// typed in [`height`](Self::height)).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GolferForm {
    pub(crate) record: GolferRecord,
    pub(crate) height: HeightField,
}

impl GolferForm {
    fn new(record: &GolferRecord, inches: bool) -> Self {
        Self {
            record: record.clone(),
            height: HeightField::new(record.height, inches),
        }
    }

    /// The record to save, `None` while the height is not valid.
    fn to_record(&self) -> Option<GolferRecord> {
        Some(GolferRecord {
            height: self.height.valid_cm()?,
            ..self.record.clone()
        })
    }
}

/// The Golfers card's form.
#[derive(Clone, Debug, Default)]
pub(crate) struct GolfersForm {
    pub(crate) golfers: Vec<GolferForm>,
    /// The golfer the card shows, by position.
    pub(crate) selected: usize,
    pub(crate) inches: bool,
    /// The card has unsaved edits.
    pub(crate) dirty: bool,
    /// The card's body goes back to its top on the next pass: another
    /// golfer is shown.
    pub(crate) to_top: bool,
    loaded: Vec<GolferRecord>,
}

impl GolfersForm {
    pub(crate) fn from_records(golfers: &[GolferRecord], units: UnitSystem) -> Self {
        let inches = units == UnitSystem::Imperial;
        Self {
            golfers: golfers.iter().map(|g| GolferForm::new(g, inches)).collect(),
            selected: 0,
            inches,
            dirty: false,
            to_top: false,
            loaded: golfers.to_vec(),
        }
    }

    /// Take the saved `golfers`, keeping the golfer shown.
    pub(crate) fn reload(&mut self, golfers: &[GolferRecord], units: UnitSystem) {
        let selected = self.selected;
        *self = Self::from_records(golfers, units);
        self.selected = selected.min(self.golfers.len().saturating_sub(1));
    }

    /// The records to save, `None` while a height is not valid.
    pub(crate) fn to_records(&self) -> Option<Vec<GolferRecord>> {
        self.golfers.iter().map(GolferForm::to_record).collect()
    }

    /// The records to save, the loaded ones while a height is not valid.
    pub(crate) fn to_records_or_loaded(&self) -> Vec<GolferRecord> {
        self.to_records().unwrap_or_else(|| self.loaded.clone())
    }

    /// Switch every height's unit.
    pub(crate) fn set_inches(&mut self, inches: bool) {
        self.inches = inches;
        for golfer in &mut self.golfers {
            golfer.height.set_inches(inches);
        }
    }

    /// Add a golfer named "Golfer N" (N from the golfer count up, the
    /// first no golfer has) and show it. Its own id is given by the server
    /// when it is saved.
    pub(crate) fn add(&mut self) {
        let name = (self.golfers.len() + 1..)
            .map(|n| format!("Golfer {n}"))
            .find(|name| !self.golfers.iter().any(|g| g.record.name == *name))
            .unwrap_or_default();
        let record = GolferRecord {
            name,
            ..GolferRecord::default()
        };
        self.golfers.push(GolferForm::new(&record, self.inches));
        self.selected = self.golfers.len() - 1;
        self.dirty = true;
        self.to_top = true;
    }

    /// Remove the golfer shown.
    pub(crate) fn remove_selected(&mut self) {
        if self.selected < self.golfers.len() {
            self.golfers.remove(self.selected);
            self.selected = self.selected.min(self.golfers.len().saturating_sub(1));
            self.dirty = true;
            self.to_top = true;
        }
    }

    /// The game profiles of `profiles` no golfer on the card is linked to.
    pub(crate) fn linkable<'a>(&self, profiles: &'a [GameProfile]) -> Vec<&'a GameProfile> {
        profiles
            .iter()
            .filter(|p| !self.golfers.iter().any(|g| g.record.is(&p.id)))
            .collect()
    }

    /// Link game id `id` to the golfer shown.
    pub(crate) fn link(&mut self, id: &str) {
        if let Some(golfer) = self.golfers.get_mut(self.selected) {
            golfer.record.identifiers.push(id.to_owned());
            self.dirty = true;
        }
    }

    /// Remove game id `id` from the golfer shown.
    pub(crate) fn unlink(&mut self, id: &str) {
        if let Some(golfer) = self.golfers.get_mut(self.selected) {
            golfer.record.identifiers.retain(|i| i != id);
            self.dirty = true;
        }
    }
}

/// A game's profile on the bus: its game id and the name it publishes.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GameProfile {
    pub(crate) id: String,
    pub(crate) name: String,
}

impl GameProfile {
    /// How the picker shows it: "Pureplay: Sam".
    fn label(&self) -> String {
        game_id_label(&self.id, &self.name)
    }
}

/// The game profiles on the bus: every `golfer.profile` under an id that
/// is not a flighthook golfer's, in id order.
pub(crate) fn game_profiles(
    states: &std::collections::BTreeMap<flighthook::StateKey, flighthook::BusMessage>,
) -> Vec<GameProfile> {
    let mut profiles: Vec<GameProfile> = states
        .values()
        .filter_map(|m| match &m.body {
            flighthook::FrpBody::GolferProfile(p) if !is_own_golfer_id(p.id.as_str()) => {
                Some(GameProfile {
                    id: p.id.as_str().to_owned(),
                    name: p.name.clone(),
                })
            }
            _ => None,
        })
        .collect();
    profiles.sort_by(|a, b| a.id.cmp(&b.id));
    profiles.dedup_by(|a, b| a.id == b.id);
    profiles
}

/// `profiles` plus the game golfers the Golfer card lists (a game's
/// golfer this run, or one with shots in the history), each once, in id
/// order: a game player can be linked while the game is between rounds.
pub(crate) fn with_seen(
    mut profiles: Vec<GameProfile>,
    list: &crate::topbar::GolferList,
) -> Vec<GameProfile> {
    let known = list.known.iter().map(|g| (g.id.as_str(), g.name.as_str()));
    let past = list
        .past
        .iter()
        .filter_map(|p| Some((p.key.as_deref()?, p.name.as_str())));
    for (id, name) in known.chain(past) {
        if id.contains('.') && !is_own_golfer_id(id) && !profiles.iter().any(|p| p.id == id) {
            profiles.push(GameProfile {
                id: id.to_owned(),
                name: name.to_owned(),
            });
        }
    }
    profiles.sort_by(|a, b| a.id.cmp(&b.id));
    profiles
}

/// The game a game id belongs to, named by its namespace: "Pureplay" for
/// `pureplay.local-3`, the namespace itself for any other game. `None`
/// for an id with no namespace.
pub(crate) fn game_name(id: &str) -> Option<String> {
    match id.split_once('.') {
        Some(("pureplay", _)) => Some("Pureplay".to_owned()),
        Some((game, _)) => Some(game.to_owned()),
        None => None,
    }
}

/// How game id `id` reads with the player name `name` the game gave it:
/// "Pureplay: thill"; the id's own part when there is no name.
pub(crate) fn game_id_label(id: &str, name: &str) -> String {
    match (game_name(id), id.split_once('.')) {
        (Some(game), Some((_, local))) => {
            let shown = if name.trim().is_empty() || name == id {
                local
            } else {
                name
            };
            format!("{game}: {shown}")
        }
        _ => id.to_owned(),
    }
}

/// The name a golfer shows with in the list and on its page: its name,
/// else "Golfer N".
pub(crate) fn shown_name(golfer: &GolferRecord, index: usize) -> String {
    if golfer.name.trim().is_empty() {
        format!("Golfer {}", index + 1)
    } else {
        golfer.name.clone()
    }
}

impl FlighthookApp {
    /// The page of the golfer shown (`selected`): its name, Save and
    /// Cancel while the golfers have unsaved edits, the trash icon that
    /// removes it, then its fields as grouped rows. Its edit controls are
    /// disabled unless `editable`.
    pub(crate) fn render_golfers_card(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        editable: bool,
        height: f32,
    ) {
        let form = &self.settings.golfers;
        let title = form.golfers.get(form.selected).map_or_else(
            || "Golfers".to_owned(),
            |g| shown_name(&g.record, form.selected),
        );
        let card = devices::fixed_card(ui, "golfers", theme::HEADING, height, |card| {
            let ui = card.top();
            let dirty = card_header(ui, "golfers", &title, None, None, |ui| {
                let form = &mut self.settings.golfers;
                if !form.golfers.is_empty()
                    && devices::header_icon(
                        ui,
                        theme::Icon::Trash,
                        "Remove",
                        "golfer_remove",
                        editable,
                    )
                {
                    form.remove_selected();
                }
                if !self.settings.golfers.dirty {
                    return false;
                }
                let can_save = self.settings.golfers.to_records().is_some();
                match save_cancel(ui, "golfers", can_save, self.settings.saving()) {
                    Some(Pending::Save) => self.post_save(ctx, SaveTarget::Golfers),
                    Some(Pending::Cancel) => self.settings.cancel_golfers(),
                    None => {}
                }
                true
            });
            if dirty {
                lock_hint(ui);
                if let Some(error) = &self.settings.save_error
                    && error.target == SaveTarget::Golfers
                {
                    save_error_line(ui, "golfers", &error.message);
                }
            }
            card.body(|ui| {
                ui.add_space(6.0);
                form_rows(ui, |ui| {
                    ui.add_enabled_ui(editable, |ui| self.golfer_fields(ui));
                });
            });
        });
        devices::mark(ui, || "card:golfers".to_owned(), &card.response);
    }

    /// The shown golfer's fields: Name, Sex, Hand, Height, linked game ids
    /// and Bag.
    fn golfer_fields(&mut self, ui: &mut egui::Ui) {
        let (field_width, _) = field_widths(ui);
        let profiles = with_seen(game_profiles(&self.bus_states), &self.golfer_list);
        let (states, heard) = (&self.bus_states, &self.golfer_names);
        let player = |id: &str| crate::app::golfer_name(None, states, heard, id);
        let form = &mut self.settings.golfers;
        if std::mem::take(&mut form.to_top) {
            ui.scroll_to_cursor(Some(egui::Align::TOP));
        }
        let Some(golfer) = form.golfers.get_mut(form.selected) else {
            return;
        };
        let mut changed = false;
        ui.horizontal(|ui| {
            field_label(ui, "Name:");
            let field = ui.add(
                egui::TextEdit::singleline(&mut golfer.record.name)
                    .desired_width(field_width - TEXT_EDIT_MARGIN),
            );
            devices::mark(ui, || "golfer_name".to_owned(), &field);
            changed |= field.changed();
        });
        ui.horizontal(|ui| {
            field_label(ui, "Gender:").on_hover_text("Men's or women's tour averages");
            let all = Sex::all();
            let labels = all.map(Sex::label);
            let sex = golfer.record.sex.unwrap_or_default();
            let selected = all.iter().position(|s| *s == sex).unwrap_or(0);
            let toggle = theme::segmented_pills(ui, &labels, selected, None, theme::PILL_FONT);
            let (clicked, pills) = toggle.inner;
            for (sex, pill) in all.iter().zip(&pills) {
                devices::mark(ui, || format!("golfer_sex:{}", sex.key()), pill);
            }
            if let Some(i) = clicked {
                changed |= all[i] != sex;
                golfer.record.sex = Some(all[i]);
            }
        });
        ui.horizontal(|ui| {
            field_label(ui, "Hand:");
            let all = [Handedness::Right, Handedness::Left];
            let labels = ["Right-handed", "Left-handed"];
            let hand = golfer.record.handed.unwrap_or(Handedness::Right);
            let selected = all.iter().position(|h| *h == hand).unwrap_or(0);
            let toggle = theme::segmented_pills(ui, &labels, selected, None, theme::PILL_FONT);
            let (clicked, pills) = toggle.inner;
            for (hand, pill) in all.iter().zip(&pills) {
                devices::mark(ui, || format!("golfer_hand:{hand}"), pill);
            }
            if let Some(i) = clicked {
                changed |= all[i] != hand;
                golfer.record.handed = Some(all[i]);
            }
        });
        ui.horizontal(|ui| {
            field_label(ui, "Height:").on_hover_text(
                "With the height, swing measurements that are distances are shown in \
                 inches or centimeters.",
            );
            let field = ui.add(
                egui::TextEdit::singleline(&mut golfer.height.text)
                    .desired_width(field_width - TEXT_EDIT_MARGIN),
            );
            devices::mark(ui, || "golfer_height".to_owned(), &field);
            changed |= field.changed();
            ui.label(golfer.height.unit());
        });
        if golfer.height.valid_cm().is_none() {
            let (min, max) = (PlayerSection::MIN_HEIGHT_CM, PlayerSection::MAX_HEIGHT_CM);
            field_error(
                ui,
                &if golfer.height.inches {
                    format!(
                        "{:.0} to {:.0} in",
                        (min / 2.54).ceil(),
                        (max / 2.54).floor()
                    )
                } else {
                    format!("{min:.0} to {max:.0} cm")
                },
            );
        }
        let linked: Vec<String> = golfer
            .record
            .identifiers
            .iter()
            .filter(|id| !is_own_golfer_id(id))
            .cloned()
            .collect();
        let mut unlink = None;
        for (i, id) in linked.iter().enumerate() {
            ui.horizontal(|ui| {
                if i == 0 {
                    field_label(ui, "Linked Profiles:");
                } else {
                    super::field_continue(ui);
                }
                let label = ui.label(game_id_label(id, &player(id)));
                devices::mark(ui, || format!("golfer_link:{id}"), &label);
                let remove = ui.small_button("Remove");
                devices::mark(ui, || format!("golfer_unlink:{id}"), &remove);
                if remove.clicked() {
                    unlink = Some(id.clone());
                }
            });
        }
        let linkable = form.linkable(&profiles);
        let mut link = None;
        ui.horizontal(|ui| {
            if linked.is_empty() {
                field_label(ui, "Linked Profiles:");
            } else {
                super::field_continue(ui);
            }
            let reason = if profiles.is_empty() {
                Some("No game players yet: start a round in the game")
            } else if linkable.is_empty() {
                Some("Every game profile is linked")
            } else {
                None
            };
            let combo = ui.add_enabled_ui(reason.is_none(), |ui| {
                egui::ComboBox::from_id_salt("golfer_link_pick")
                    .selected_text("Add a game profile")
                    .width(field_width * 0.6)
                    .show_ui(ui, |ui| {
                        for profile in &linkable {
                            let option = ui.selectable_label(false, profile.label());
                            devices::mark(
                                ui,
                                || format!("golfer_link_option:{}", profile.id),
                                &option,
                            );
                            if option.clicked() {
                                link = Some(profile.id.clone());
                            }
                        }
                    })
                    .response
            });
            let mut response = combo.inner;
            if let Some(reason) = reason {
                response = response.on_disabled_hover_text(reason);
            }
            devices::mark(ui, || "golfer_link_pick".to_owned(), &response);
        });
        if let Some(id) = unlink {
            form.unlink(&id);
        }
        if let Some(id) = link {
            form.link(&id);
        }
        let Some(golfer) = form.golfers.get_mut(form.selected) else {
            return;
        };
        // The clubs wrap within the field column, under each other.
        ui.horizontal_top(|ui| {
            field_label(ui, "Bag:")
                .on_hover_text("The clubs the header's club picker offers for this golfer");
            ui.vertical(|ui| {
                ui.horizontal_wrapped(|ui| {
                    for &club in Club::ALL {
                        let in_bag = golfer.record.bag.contains(&club);
                        let text = egui::RichText::new(format!("{club}")).size(12.0);
                        let button = if in_bag {
                            egui::Button::new(text.strong().color(theme::ON_ACCENT))
                                .fill(theme::ACCENT)
                        } else {
                            egui::Button::new(text)
                        };
                        let resp = ui.add(button);
                        devices::mark(ui, || format!("golfer_bag:{club}"), &resp);
                        if resp.clicked() {
                            if in_bag {
                                golfer.record.bag.retain(|c| *c != club);
                            } else {
                                // Kept in the picker's order.
                                golfer.record.bag.push(club);
                                golfer
                                    .record
                                    .bag
                                    .sort_by_key(|c| Club::ALL.iter().position(|x| x == c));
                            }
                            changed = true;
                        }
                    }
                });
            });
        });
        if changed {
            form.dirty = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_golfers_form_round_trips_its_records() {
        let mut eric = GolferRecord::new("flighthook.e", "Eric");
        eric.identifiers.push("pureplay.local-3".into());
        eric.height = Some(182.5);
        eric.bag = vec![Club::Driver];
        let records = vec![eric, GolferRecord::new("flighthook.s", "Sam")];
        let form = GolfersForm::from_records(&records, UnitSystem::Imperial);
        assert_eq!(form.golfers[0].height.text, "71.9");
        assert_eq!(form.to_records().expect("valid"), records, "exact");
        let mut form = form;
        form.golfers[0].height.text = "300".into();
        assert!(form.to_records().is_none());
        assert_eq!(form.to_records_or_loaded(), records);
        form.golfers[0].height.text = "180".into();
        form.set_inches(false);
        assert_eq!(form.golfers[0].height.text, "457.2");
    }

    #[test]
    fn add_and_remove_change_the_golfer_shown() {
        let mut form = GolfersForm::from_records(&[], UnitSystem::Metric);
        form.add();
        form.add();
        assert_eq!(form.golfers.len(), 2);
        assert_eq!(form.selected, 1);
        assert_eq!(form.golfers[1].record.name, "Golfer 2");
        assert!(form.golfers[1].record.identifiers.is_empty());
        assert!(form.dirty);
        form.remove_selected();
        assert_eq!(form.selected, 0);
        assert_eq!(form.golfers.len(), 1);
        form.remove_selected();
        assert!(form.golfers.is_empty());
        assert_eq!(form.selected, 0);
    }

    /// Add after a Remove never repeats a name another
    /// golfer has.
    #[test]
    fn add_after_a_remove_gives_a_name_no_golfer_has() {
        let mut form = GolfersForm::from_records(&[], UnitSystem::Metric);
        form.add();
        form.add();
        form.selected = 0;
        form.remove_selected();
        form.add();
        let names: Vec<&str> = form
            .golfers
            .iter()
            .map(|g| g.record.name.as_str())
            .collect();
        assert_eq!(names, ["Golfer 2", "Golfer 3"]);
    }

    /// Only game profiles are offered, each once and in id order; one
    /// linked to any golfer on the card is not.
    #[test]
    fn only_unlinked_game_profiles_are_linkable() {
        let mut states = std::collections::BTreeMap::new();
        for (from, id) in [
            ("pp", "pureplay.local-4"),
            ("fh", "flighthook.e"),
            ("pp", "pureplay.local-3"),
            ("bn", "backnine.p7"),
        ] {
            let profile = flighthook::GolferProfile::new(id.parse().unwrap(), "P");
            let msg = flighthook::BusMessage::new(from, profile);
            states.insert(msg.state_key().unwrap(), msg);
        }
        let profiles = game_profiles(&states);
        let ids: Vec<&str> = profiles.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["backnine.p7", "pureplay.local-3", "pureplay.local-4"]);
        let mut sam = GolferRecord::new("flighthook.s", "Sam");
        sam.identifiers.push("pureplay.local-3".into());
        let mut form = GolfersForm::from_records(
            &[GolferRecord::new("flighthook.e", "Eric"), sam],
            UnitSystem::Metric,
        );
        let linkable: Vec<&str> = form
            .linkable(&profiles)
            .iter()
            .map(|p| p.id.as_str())
            .collect();
        assert_eq!(linkable, ["backnine.p7", "pureplay.local-4"]);
        form.link("backnine.p7");
        assert!(form.dirty);
        assert_eq!(
            form.golfers[0].record.identifiers,
            ["flighthook.e", "backnine.p7"]
        );
        assert_eq!(form.linkable(&profiles).len(), 1);
        form.unlink("backnine.p7");
        assert_eq!(form.golfers[0].record.identifiers, ["flighthook.e"]);
        assert_eq!(profiles[2].label(), "Pureplay: P");
    }

    /// A game golfer the Golfer card lists is offered even with no
    /// profile on the bus (a game between rounds); a flighthook golfer and
    /// "No golfer" are not, and a live profile is not offered twice.
    #[test]
    fn game_golfers_the_card_lists_are_offered_too() {
        use crate::topbar::{GolferList, KnownGolfer, PastGolfer};
        let list = GolferList {
            known: vec![KnownGolfer {
                id: "flighthook.e".into(),
                name: "Eric".into(),
                game: false,
            }],
            past: vec![
                PastGolfer {
                    key: Some("pureplay.local-3".into()),
                    name: "EJ".into(),
                },
                PastGolfer {
                    key: Some("backnine.p7".into()),
                    name: "Joe".into(),
                },
                PastGolfer {
                    key: None,
                    name: "No golfer".into(),
                },
            ],
        };
        let live = vec![GameProfile {
            id: "backnine.p7".into(),
            name: "Joe".into(),
        }];
        let ids: Vec<String> = with_seen(live, &list).into_iter().map(|p| p.id).collect();
        assert_eq!(ids, ["backnine.p7", "pureplay.local-3"]);
    }

    #[test]
    fn a_game_id_reads_by_its_game() {
        assert_eq!(
            game_id_label("pureplay.local-3", "thill"),
            "Pureplay: thill"
        );
        assert_eq!(
            game_id_label("pureplay.local-3", "pureplay.local-3"),
            "Pureplay: local-3"
        );
        assert_eq!(game_id_label("backnine.p7", ""), "backnine: p7");
        assert_eq!(game_name("pureplay.local-3").as_deref(), Some("Pureplay"));
        assert_eq!(game_name("backnine.p7").as_deref(), Some("backnine"));
        assert_eq!(game_name("plain"), None);
    }
}
