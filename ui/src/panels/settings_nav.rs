//! The Settings list: one heading per section (General, Devices, Software,
//! Cameras, Golfers), with its (+) where it has one, and one row per item
//! under it with the item's status dot. The selected row's page fills the
//! rest of Settings.

use crate::app::FlighthookApp;
use crate::panels::cards::{self, CardGroup};
use crate::panels::devices;
use crate::panels::monitoring;
use crate::panels::remote;
use crate::panels::shots::{EntryState, GLOW_WIDTH, LIST_HEADER_HEIGHT, entry_frame};
use crate::theme;

/// Height of a list row; on the phone, a touch target's.
pub(crate) const ROW_HEIGHT: f32 = 34.0;

/// Space between two list rows.
const ROW_GAP: f32 = 4.0;

/// A section of Settings, in list order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum SettingsSection {
    /// Global, Web Server and Simulation.
    General,
    /// Launch monitors, local and remote.
    #[default]
    Devices,
    /// Cards that talk to another program.
    Software,
    /// Cameras, Replay and Body tracking.
    Cameras,
    /// The saved golfers.
    Golfers,
}

impl SettingsSection {
    pub(crate) const ALL: [SettingsSection; 5] = [
        SettingsSection::General,
        SettingsSection::Devices,
        SettingsSection::Software,
        SettingsSection::Cameras,
        SettingsSection::Golfers,
    ];

    /// The card group whose cards the section shows; Golfers has none.
    pub(crate) fn group(self) -> Option<CardGroup> {
        match self {
            SettingsSection::General => Some(CardGroup::General),
            SettingsSection::Devices => Some(CardGroup::LaunchMonitors),
            SettingsSection::Software => Some(CardGroup::Software),
            SettingsSection::Cameras => Some(CardGroup::Cameras),
            SettingsSection::Golfers => None,
        }
    }

    /// The section that shows the cards of `group`.
    pub(crate) fn of_group(group: CardGroup) -> SettingsSection {
        match group {
            CardGroup::General => SettingsSection::General,
            CardGroup::LaunchMonitors => SettingsSection::Devices,
            CardGroup::Software => SettingsSection::Software,
            CardGroup::Cameras => SettingsSection::Cameras,
        }
    }

    /// The section that shows the card of actor `id`.
    pub(crate) fn of_actor(id: &str) -> SettingsSection {
        Self::of_group(cards::group_of(cards::split_actor_id(id).0))
    }
}

/// One row of the Settings list, and the page it opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SettingsItem {
    Global,
    Simulation,
    Replay,
    BodyTracking,
    /// A device card: a local actor's ID, or a remote card's key.
    Card(String),
    /// A saved golfer, by its place in the Golfers form.
    Golfer(usize),
}

impl SettingsItem {
    /// The name used in test marks: `settings_item:<key>`.
    pub(crate) fn key(&self) -> String {
        match self {
            SettingsItem::Global => "global".to_owned(),
            SettingsItem::Simulation => "simulation".to_owned(),
            SettingsItem::Replay => "replay".to_owned(),
            SettingsItem::BodyTracking => "body_tracking".to_owned(),
            SettingsItem::Card(id) => id.clone(),
            SettingsItem::Golfer(i) => format!("golfer.{i}"),
        }
    }
}

/// The Settings list: each section and its items, in order.
pub(crate) type SettingsList = Vec<(SettingsSection, Vec<SettingsItem>)>;

/// What a row shows: its title, a tag after it, its status dot, and
/// whether the dot is the red of an alert.
struct RowLook {
    title: String,
    tag: Option<&'static str>,
    dot: Option<egui::Color32>,
    red: bool,
}

impl FlighthookApp {
    /// The item whose page shows: the picked one while it is in the list,
    /// else the first item of the picked section.
    /// While the golfers' edits leave none, the golfers' page stays for
    /// their Save and Cancel.
    pub(crate) fn selected_item(&self, list: &SettingsList) -> Option<SettingsItem> {
        let golfers = &self.settings.golfers;
        if self.settings_section == SettingsSection::Golfers
            && golfers.dirty
            && golfers.golfers.is_empty()
        {
            return Some(SettingsItem::Golfer(0));
        }
        let items = list
            .iter()
            .find(|(s, _)| *s == self.settings_section)
            .map(|(_, items)| items.as_slice())?;
        self.settings_item
            .as_ref()
            .filter(|i| items.contains(i))
            .or_else(|| items.first())
            .cloned()
    }

    /// Show `item`'s page, in `section` (on the phone, opened over the
    /// list).
    pub(crate) fn select_item(&mut self, section: SettingsSection, item: SettingsItem) {
        self.settings_section = section;
        self.settings_item = Some(item);
        self.settings_open = true;
    }

    /// How the row of `item` looks.
    fn row_look(&self, item: &SettingsItem, remote: &[remote::RemoteCard]) -> RowLook {
        let plain = |title: &str, tag| RowLook {
            title: title.to_owned(),
            tag,
            dot: None,
            red: false,
        };
        match item {
            SettingsItem::Global => plain("Global", None),
            SettingsItem::Simulation => plain("Simulation", None),
            SettingsItem::Replay => plain("Replay", None),
            SettingsItem::BodyTracking => plain("Body tracking", Some("alpha")),
            SettingsItem::Golfer(i) => {
                let name = self.settings.golfers.golfers.get(*i).map_or_else(
                    || format!("Golfer {}", i + 1),
                    |g| crate::panels::settings::golfers::shown_name(&g.record, *i),
                );
                plain(&name, None)
            }
            SettingsItem::Card(id) => {
                if let Some(card) = remote.iter().find(|c| c.key() == *id) {
                    return RowLook {
                        title: card.status.name.clone(),
                        tag: None,
                        dot: Some(monitoring::status_color(&card.status, self.now_ms)),
                        red: false,
                    };
                }
                let actor = self.actors.get(id);
                let form_name = self
                    .settings
                    .actors
                    .iter()
                    .find(|a| a.global_id() == *id)
                    .map(|a| a.name().to_owned());
                let title = actor
                    .map(|a| a.name.clone())
                    .filter(|n| !n.is_empty())
                    .or(form_name)
                    .unwrap_or_else(|| id.clone());
                // Off in the saved settings: shown off at once, before the
                // server has stopped it.
                let dot = if self.settings.turned_off(id) {
                    theme::STATUS_OFF
                } else {
                    actor.map_or(theme::TEXT_DIM, |a| {
                        monitoring::status_color(a, self.now_ms)
                    })
                };
                RowLook {
                    title,
                    tag: None,
                    dot: Some(dot),
                    red: actor.is_some_and(|a| a.red(self.now_ms)),
                }
            }
        }
    }

    /// The list column: the `SETTINGS` caption, then each section's heading
    /// (with its (+)) and rows. Rows and (+) are disabled while Settings has
    /// unsaved edits.
    pub(crate) fn render_settings_nav(
        &mut self,
        ui: &mut egui::Ui,
        list: &SettingsList,
        remote: &[remote::RemoteCard],
    ) {
        let inset = f32::from(GLOW_WIDTH);
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), LIST_HEADER_HEIGHT),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.add_space(inset);
                ui.label(theme::caps("Settings", 11.0, theme::TEXT_MUTED));
            },
        );
        let locked = self.settings.locked();
        let loaded = self.settings.loaded;
        let selected = self.selected_item(list);
        let mut picked = None;
        let mut added = None;
        let mut add_golfer = false;
        egui::ScrollArea::vertical()
            .id_salt("settings_list")
            .auto_shrink(false)
            .show(ui, |ui| {
                for (section, items) in list {
                    ui.horizontal(|ui| {
                        ui.add_space(inset);
                        ui.vertical(|ui| match section.group() {
                            Some(g @ CardGroup::General) => cards::group_heading(ui, g),
                            Some(g @ CardGroup::Cameras) if !self.cameras_supported => {
                                cards::group_heading(ui, g);
                            }
                            Some(g) => {
                                added = added.or(cards::add_heading(
                                    ui,
                                    g,
                                    loaded && !locked,
                                    self.trackball_supported,
                                ));
                            }
                            None => {
                                add_golfer |= cards::add_golfer_heading(ui, loaded && !locked);
                            }
                        });
                    });
                    for item in items {
                        let look = self.row_look(item, remote);
                        let is_selected = selected.as_ref() == Some(item);
                        if self.list_row(ui, item, &look, is_selected, locked) {
                            picked = Some((*section, item.clone()));
                        }
                    }
                    ui.add_space(6.0);
                }
            });
        if let Some((section, item)) = picked {
            self.select_item(section, item);
        }
        if let Some(preset) = added {
            self.add_device(preset);
        }
        if add_golfer {
            self.settings.golfers.add();
            let last = self.settings.golfers.golfers.len() - 1;
            self.select_item(SettingsSection::Golfers, SettingsItem::Golfer(last));
        }
    }

    /// One list row: its frame, status dot, title and tag. Returns whether
    /// it was clicked.
    fn list_row(
        &self,
        ui: &mut egui::Ui,
        item: &SettingsItem,
        look: &RowLook,
        selected: bool,
        locked: bool,
    ) -> bool {
        let inset = f32::from(GLOW_WIDTH);
        // Touch-sized on the phone.
        let height = if self.portrait {
            crate::topbar::TOUCH_TARGET
        } else {
            ROW_HEIGHT
        };
        let top = ui.cursor().min.y;
        let full = ui.max_rect();
        ui.allocate_rect(
            egui::Rect::from_min_max(
                egui::pos2(full.left(), top),
                egui::pos2(full.right(), top + height + ROW_GAP),
            ),
            egui::Sense::hover(),
        );
        let rect = egui::Rect::from_min_size(
            egui::pos2(full.left() + inset, top),
            egui::vec2(full.width() - 2.0 * inset, height),
        );
        let key = item.key();
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        if locked {
            child.disable();
        }
        let response = child.interact(
            rect,
            ui.id().with(("settings_item", &key)),
            egui::Sense::click(),
        );
        entry_frame(
            ui.painter(),
            rect,
            EntryState {
                selected,
                hovered: response.hovered() && !locked,
                ..EntryState::default()
            },
        );
        let color = if locked && !selected {
            theme::TEXT_DIM
        } else {
            theme::TEXT
        };
        let mid = rect.center().y;
        if let Some(dot) = look.dot {
            let center = egui::pos2(rect.left() + 14.0, mid);
            ui.painter().circle_filled(center, 3.5, dot);
            if look.red {
                let marker = ui.interact(
                    egui::Rect::from_center_size(center, egui::vec2(7.0, 7.0)),
                    ui.id().with(("settings_nav_alert", &key)),
                    egui::Sense::hover(),
                );
                devices::mark(ui, || format!("settings_nav_alert:{key}"), &marker);
            }
        }
        let left = rect.left() + 26.0;
        let mut job = egui::text::LayoutJob::single_section(
            look.title.clone(),
            egui::TextFormat {
                font_id: egui::FontId::proportional(14.0),
                color,
                ..Default::default()
            },
        );
        job.wrap = egui::text::TextWrapping::truncate_at_width(rect.right() - 10.0 - left);
        let title = ui.painter().layout_job(job);
        let title_width = title.size().x;
        ui.painter()
            .galley(egui::pos2(left, mid - title.size().y / 2.0), title, color);
        if let Some(tag) = look.tag {
            let galley = ui.painter().layout_no_wrap(
                tag.to_owned(),
                egui::FontId::proportional(11.0),
                theme::HEADING,
            );
            let at = egui::pos2(left + title_width + 8.0, mid - galley.size().y / 2.0);
            let frame = egui::Rect::from_min_size(at, galley.size()).expand2(egui::vec2(5.0, 2.0));
            if frame.right() < rect.right() - 4.0 {
                ui.painter().rect_filled(frame, 4.0, theme::SURFACE_HOVER);
                ui.painter().galley(at, galley, theme::HEADING);
            }
        }
        devices::mark(ui, || format!("settings_item:{key}"), &response);
        let response = if locked {
            let reason = crate::topbar::LOCKED_REASON;
            crate::topbar::touch_tip(response.on_disabled_hover_text(reason), reason.into())
        } else {
            response.on_hover_cursor(egui::CursorIcon::PointingHand)
        };
        response.clicked()
    }
}
