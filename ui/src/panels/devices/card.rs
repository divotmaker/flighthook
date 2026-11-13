//! One device page: its telemetry beside its settings, the card order, the fixed card height and
//! regions, painting the page, and what a click on it asks for.

use super::*;

/// A device card: an actor ID, and its entry in the settings form (an index
/// into its actors) when the actor is configured.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DeviceCard {
    pub(crate) id: String,
    pub(crate) form: Option<usize>,
}

/// The device cards by group, in card order ([`cards::grouped`]): every
/// configured actor, and every running one the form does not list.
pub(crate) fn device_card_order(
    actors: &HashMap<String, ActorStatusResponse>,
    form: &[ActorFormEntry],
) -> Vec<(CardGroup, Vec<DeviceCard>)> {
    let mut ids: Vec<(String, Option<usize>)> = form
        .iter()
        .enumerate()
        .map(|(i, a)| (a.global_id(), Some(i)))
        .collect();
    for id in actors.keys() {
        if !ids.iter().any(|(known, _)| known == id) {
            ids.push((id.clone(), None));
        }
    }
    cards::grouped(ids.iter().map(|(id, form)| {
        let (type_key, index) = cards::split_actor_id(id);
        (
            type_key,
            index,
            DeviceCard {
                id: id.clone(),
                form: *form,
            },
        )
    }))
}

/// A Settings card being laid out, top to bottom: its header and the
/// lines under it ([`top`](Self::top)), then its [`body`](Self::body).
pub(crate) struct CardRegions<'a> {
    pub(super) ui: &'a mut egui::Ui,
    /// The card's ID, which keys its scroll position.
    pub(super) key: &'a str,
}

impl CardRegions<'_> {
    /// The top of the card: its header and any line under it.
    pub(crate) fn top(&mut self) -> &mut egui::Ui {
        self.ui
    }

    /// The card's body: everything under the top. It scrolls inside the
    /// card when it does not fit, keeping its position for the session.
    pub(crate) fn body<R>(&mut self, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
        let height = self.ui.available_height();
        scrolled_region(self.ui, self.key, height, add)
    }
}

/// A Settings page, `height` tall, with an `accent` stripe; `add` lays
/// out its regions. `key` names it.
pub(crate) fn fixed_card<R>(
    ui: &mut egui::Ui,
    key: &str,
    accent: egui::Color32,
    height: f32,
    add: impl FnOnce(&mut CardRegions<'_>) -> R,
) -> egui::InnerResponse<R> {
    theme::accent_card(ui, accent, height, |ui| add(&mut CardRegions { ui, key }))
}

/// What a click on a device card asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CardAction {
    Save,
    Cancel,
    Remove,
    FindMat,
    /// Fetch the camera's still frame.
    Still,
    /// Look for cameras again.
    RefreshCameras,
    /// Stop the actor and start it again.
    Reconnect,
    /// Delete a Pureplay Accessory card's saved pairing and reconnect it.
    ForgetPairing,
    /// Turn the card on (`true`) or off, saved at once.
    Enable(bool),
}

/// How a device card is drawn.
pub(super) struct CardView<'a> {
    pub(super) telemetry: Option<&'a ActorStatusResponse>,
    /// The frame's time, for the red of a lost connection.
    pub(super) now_ms: u64,
    /// The page's height.
    pub(super) height: f32,
    /// Its edit controls take input: nothing else is dirty and no save is
    /// out.
    pub(super) editable: bool,
    pub(super) can_save: bool,
    pub(super) saving: bool,
    /// Why its last save failed.
    pub(super) error: Option<&'a str>,
    /// A reconnect of it is in flight.
    pub(super) reconnecting: bool,
    /// Launch monitors for the routing dropdowns: (actor ID, name).
    pub(super) monitors: &'a [(String, String)],
    /// The camera list, for a camera card's picker.
    pub(super) cameras: &'a crate::app::CameraList,
    /// A camera card's rotation, clockwise degrees, for its still frame:
    /// the card's own setting, saved or not.
    pub(super) rotation: u16,
    /// The header's on/off switch; none on a web server's card or a card
    /// the settings don't hold.
    pub(super) switch: Option<settings::HeaderSwitch>,
    /// A remote actor's page: its telemetry only, no actions.
    pub(super) remote: bool,
}

/// A card's `error` row: its own error (`own`) and an ownership conflict,
/// joined when it has both.
pub(super) fn error_row(own: Option<&str>, conflict: Option<&str>) -> String {
    own.into_iter()
        .chain(conflict)
        .collect::<Vec<_>>()
        .join("; ")
}

/// Height of a page's telemetry block; a longer telemetry scrolls inside
/// it, so nothing under or beside it moves.
pub(crate) const TELEMETRY_HEIGHT: f32 = 240.0;

/// Most of a phone's page the telemetry block takes, over the settings.
const STACKED_TELEMETRY_SHARE: f32 = 0.35;

/// Width of a page's telemetry column, as a share of the page.
const TELEMETRY_SHARE: f32 = 0.38;

/// Size of a page header's icon buttons; in the portrait layout they are
/// [`crate::topbar::TOUCH_TARGET`] (the glyph keeps its size).
const HEADER_ICON: f32 = 24.0;

/// A page header's icon button, `hover` naming it (also as a touch tip),
/// marked `mark_name`.
pub(crate) fn header_icon(
    ui: &mut egui::Ui,
    icon: theme::Icon,
    hover: &str,
    mark_name: &str,
    enabled: bool,
) -> bool {
    let size = if settings::narrow(ui) {
        crate::topbar::TOUCH_TARGET
    } else {
        HEADER_ICON
    };
    let response = ui
        .add_enabled_ui(enabled, |ui| {
            theme::icon(ui, icon, size, theme::ACCENT, enabled)
        })
        .inner
        .on_hover_text(hover)
        .on_disabled_hover_text(hover);
    let response = crate::topbar::touch_tip(response, hover.into());
    mark(ui, || mark_name.to_owned(), &response);
    enabled && response.clicked()
}

/// One device page. Its header holds the name, the on/off switch
/// (`view.switch`) and, on the right, the status chip, then the action
/// icons: Find mat on an ExPutt's (enabled while it is connected), Still
/// frame on a camera's (`still`), Forget pairing on a Pureplay Accessory's,
/// Reconnect and Remove; Save and Cancel instead of the icons while its
/// settings have unsaved edits. Under it, the telemetry (and a camera's still frame) on the left
/// in a block of fixed height that scrolls inside, the settings on the
/// right, scrolling inside the page. A page the settings don't hold shows
/// its telemetry only. Reconnect is disabled while one is in flight, while
/// Settings is locked and while the card is off. The left stripe is the
/// actor's status color, dim while it is not running.
pub(super) fn device_card(
    ui: &mut egui::Ui,
    id: &str,
    view: &CardView<'_>,
    form: Option<&mut ActorFormEntry>,
    mut still: Option<&mut crate::panels::replay::Still>,
) -> Option<CardAction> {
    let accent = view.telemetry.map_or(theme::TEXT_DIM, |a| {
        monitoring::status_color(a, view.now_ms)
    });
    let dirty = form.as_ref().is_some_and(|f| f.dirty());
    let form_name = form
        .as_ref()
        .map(|f| f.name().to_string())
        .filter(|n| !n.trim().is_empty());
    // The running actor's name; the form's while it has edits.
    let running = view
        .telemetry
        .map(|t| t.name.clone())
        .filter(|n| !n.is_empty());
    let title = if dirty {
        form_name.or(running)
    } else {
        running.or(form_name)
    }
    .unwrap_or_else(|| id.to_string());
    // The type tag, unless the title already says it ("Web Server [Web
    // Server]"); the title's hover names the type.
    let tag = form
        .as_ref()
        .map(|f| f.type_label().to_string())
        .filter(|t| t.to_lowercase() != title.trim().to_lowercase());
    let hover = form.as_ref().map(|f| f.type_tooltip().to_string());
    let has_form = form.is_some();
    let removable = has_form && !view.remote && cards::removable(entry_type(id));
    let forget = has_form && entry_type(id) == "pureplay_accessory";
    let reconnect = !view.remote && cards::reconnectable(entry_type(id));
    let find_mat = !view.remote && entry_type(id) == "exputt";
    let off = view.telemetry.is_some_and(ActorStatusResponse::is_disabled);
    let connected = view
        .telemetry
        .is_some_and(|a| a.status == ActorStatus::Connected);

    let card = theme::accent_card(ui, accent, view.height, |ui| {
        let mut action = None;
        let tag = tag.as_deref().map(|t| (t, settings::TagPlace::Buttons));
        let (_, flipped) = settings::card_header_with(
            ui,
            id,
            &title,
            tag,
            hover.as_deref(),
            view.switch.clone(),
            |ui| {
                if dirty {
                    action = match settings::save_cancel(ui, id, view.can_save, view.saving) {
                        Some(Pending::Save) => Some(CardAction::Save),
                        Some(Pending::Cancel) => Some(CardAction::Cancel),
                        None => None,
                    };
                }
                if removable
                    && !dirty
                    && header_icon(
                        ui,
                        theme::Icon::Trash,
                        "Remove",
                        &format!("remove:{id}"),
                        view.editable,
                    )
                {
                    action = Some(CardAction::Remove);
                }
                if reconnect
                    && !dirty
                    && header_icon(
                        ui,
                        theme::Icon::Refresh,
                        "Reconnect",
                        &format!("reconnect:{id}"),
                        view.editable && !view.reconnecting && !off,
                    )
                {
                    action = Some(CardAction::Reconnect);
                }
                if forget
                    && !dirty
                    && header_icon(
                        ui,
                        theme::Icon::Disconnect,
                        "Forget pairing",
                        &format!("forget_pairing:{id}"),
                        view.editable && !view.reconnecting && !off,
                    )
                {
                    action = Some(CardAction::ForgetPairing);
                }
                if find_mat
                    && !dirty
                    && header_icon(
                        ui,
                        theme::Icon::Search,
                        "Find mat",
                        &format!("find_mat:{id}"),
                        connected,
                    )
                {
                    action = Some(CardAction::FindMat);
                }
                if let Some(still) = still.as_deref_mut().filter(|_| !dirty)
                    && crate::panels::replay::still_button(ui, id, still, connected)
                {
                    action = Some(CardAction::Still);
                }
                if let Some(actor) = view.telemetry {
                    let badge = theme::badge(ui, actor.state().label(), accent);
                    mark(ui, || format!("badge:{id}"), &badge);
                }
            },
        );
        if flipped && let Some(switch) = view.switch.as_ref() {
            action = Some(CardAction::Enable(!switch.on));
        }
        if dirty {
            settings::lock_hint(ui);
        }
        if let Some(error) = view.error {
            settings::save_error_line(ui, id, error);
        }
        ui.add_space(8.0);
        let area = ui.available_rect_before_wrap();
        let gap = 24.0;
        // On the phone the telemetry goes over the settings.
        let stacked = settings::narrow(ui) && has_form;
        let telemetry_width = if has_form && !stacked {
            ((area.width() - gap) * TELEMETRY_SHARE).floor().max(0.0)
        } else {
            area.width()
        };
        let left = egui::Rect::from_min_size(area.min, egui::vec2(telemetry_width, area.height()));
        let mut column = ui.new_child(
            egui::UiBuilder::new()
                .id_salt(("page_telemetry", id))
                .max_rect(left)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        if has_form {
            column.label(theme::caps("Telemetry", 11.0, theme::TEXT_MUTED));
            column.add_space(4.0);
        }
        let mut height = TELEMETRY_HEIGHT.min(column.available_height());
        if stacked {
            height = height.min(area.height() * STACKED_TELEMETRY_SHARE);
        }
        let telemetry_key = format!("{id}.telemetry");
        scrolled_region(&mut column, &telemetry_key, height, |ui| {
            // With nothing to show, the actor's state instead.
            if let Some(actor) = view.telemetry {
                if monitoring::has_telemetry(actor) || still.as_deref().is_some_and(|s| s.shown()) {
                    monitoring::telemetry_body(ui, id, actor);
                } else {
                    monitoring::no_telemetry(ui, id, actor, accent);
                }
            }
        });
        if let Some(still) = still {
            column.add_space(8.0);
            crate::panels::replay::still_picture(&mut column, id, still, view.rotation);
        }
        let settings_top = if stacked {
            column.min_rect().bottom() + 12.0
        } else {
            area.top()
        };
        if let Some(entry) = form {
            let right_left = if stacked {
                area.left()
            } else {
                area.left() + telemetry_width + gap
            };
            let right = egui::Rect::from_min_max(egui::pos2(right_left, settings_top), area.max);
            let mut column = ui.new_child(
                egui::UiBuilder::new()
                    .id_salt(("page_settings", id))
                    .max_rect(right)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            column.label(theme::caps("Settings", 11.0, theme::TEXT_MUTED));
            column.add_space(4.0);
            let height = column.available_height();
            let refresh = scrolled_region(&mut column, id, height, |ui| {
                ui.add_enabled_ui(view.editable, |ui| {
                    settings::actor_fields(ui, entry, view.monitors, view.cameras)
                })
                .inner
            });
            if refresh {
                action = Some(CardAction::RefreshCameras);
            }
        }
        ui.allocate_rect(area, egui::Sense::hover());
        action
    });
    mark(ui, || format!("card:{id}"), &card.response);
    card.inner
}

/// A region `height` tall at `ui`'s cursor, as wide as `ui`, whose content
/// scrolls inside it, keeping its position for the session (keyed by
/// `key`). Its scroll state is marked as body `key` for the tests.
fn scrolled_region<R>(
    ui: &mut egui::Ui,
    key: &str,
    height: f32,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let height = height.max(0.0);
    // Whole points, so the scroll area's pixel rounding never reaches past
    // the region's width, with or without its bar.
    let width = ui.available_width().floor();
    let rect = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(width, height));
    let mut region = ui.new_child(
        egui::UiBuilder::new()
            .id(("devices_card_body", key))
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    region.set_clip_rect(rect.intersect(ui.clip_rect()));
    region.spacing_mut().scroll = egui::style::ScrollStyle::thin();
    let shown = egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .max_height(height)
        .show(&mut region, add);
    mark_body(&region, key, &shown);
    ui.allocate_rect(rect, egui::Sense::hover());
    shown.inner
}

/// The actor type of card `id`.
pub(super) fn entry_type(id: &str) -> &str {
    cards::split_actor_id(id).0
}

/// The cards the outputs' routing pickers offer, as (actor ID, name): the
/// launch monitors, without the cameras and the Pureplay Accessory card (a simulator,
/// never a shot source for an output).
pub(super) fn routing_monitors(
    actors: &[ActorFormEntry],
    saved: Option<&flighthook::FlighthookConfig>,
) -> Vec<(String, String)> {
    actors
        .iter()
        .filter_map(|a| match a {
            ActorFormEntry::Device(d)
                if d.is_launch_monitor() && d.monitor_type != "pureplay_accessory" =>
            {
                // Routing names a launch monitor by its actor id, which a
                // card gets when it is first saved.
                let id = saved?.actor_of_card(&a.global_id())?;
                Some((id, d.name.clone()))
            }
            ActorFormEntry::Device(_) | ActorFormEntry::Integration(_) => None,
        })
        .collect()
}

/// The remote launch monitors the routing pickers offer, as (actor ID,
/// name): every remote card in the Launch monitors group, except one that
/// also holds `game` (a game's shots never feed a simulator output).
pub(super) fn remote_monitors(
    states: &std::collections::BTreeMap<flighthook::StateKey, flighthook::BusMessage>,
    remote: &[remote::RemoteCard],
) -> Vec<(String, String)> {
    remote
        .iter()
        .filter(|c| c.group == CardGroup::LaunchMonitors)
        .filter(|c| {
            !states.values().any(|m| {
                m.from == c.id
                    && matches!(&m.body, flighthook::FrpBody::BusActor(a)
                        if a.has_role(flighthook::roles::GAME))
            })
        })
        .map(|c| (c.id.clone(), c.status.name.clone()))
        .collect()
}
