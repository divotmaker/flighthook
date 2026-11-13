//! The Settings page panel (`impl FlighthookApp`): the selected section's cards, device card
//! actions (save, cancel, remove, reconnect, forget pairing, on/off), adding a card, the camera list.

use super::*;

/// The pages Settings draws.
#[derive(Clone, Copy)]
pub(super) enum Pages<'a> {
    /// The selected item's page.
    One(&'a SettingsItem),
    /// Tests only: every page, in the card grid.
    #[cfg(test)]
    Grid(&'a [SettingsItem]),
}

/// Tests only: the height of a page drawn in the card grid.
#[cfg(test)]
const TEST_PAGE_HEIGHT: f32 = 420.0;

impl FlighthookApp {
    /// Settings: the list on the left, the selected item's page right of
    /// it; on the phone the list, or the page a row opened.
    pub(crate) fn render_devices_panel(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        settings::set_narrow(ctx, self.portrait);
        // Settings load with the tab, and again after a save that reloads them.
        if self.settings.fetch_due(ctx.input(|i| i.time)) {
            net::fetch_settings(ctx, &self.pending);
        }

        let conflicts = remote::conflicts(&self.bus_states);
        let remote = remote::remote_cards(
            &self.bus_states,
            &self.card_keys,
            &self.flighthook_id,
            &conflicts,
        );
        let groups = device_card_order(&self.actors, &self.settings.actors);
        let list = self.settings_list(&groups, &remote);
        if self.camera_list_due(self.selected_item(&list).as_ref()) {
            self.refresh_cameras(ctx);
        }

        // The phone: the list is the page; a row opens its page, its
        // header led by a back icon.
        settings::set_page_back(ctx, None);
        if self.portrait {
            // The open item is gone (Cancel on a new card, Remove): back to
            // the list, unless edits wait for Save or Cancel on a page.
            let gone = self
                .settings_item
                .as_ref()
                .is_some_and(|i| !list.iter().any(|(_, items)| items.contains(i)));
            if gone && !self.settings.locked() {
                self.settings_open = false;
            }
            if self.settings_open && self.selected_item(&list).is_some() {
                settings::set_page_back(ctx, Some(self.settings.locked()));
                self.render_settings_page(ctx, ui, &list, &groups, &remote, &conflicts);
                if settings::take_back(ctx) {
                    self.settings_open = false;
                }
            } else {
                self.render_settings_nav(ui, &list, &remote);
            }
            return;
        }
        let width = (ui.available_width() * 0.18).clamp(
            *shots::LIST_DEFAULT_WIDTH.start(),
            *shots::LIST_DEFAULT_WIDTH.end(),
        );
        egui::SidePanel::left(egui::Id::new("settings_nav"))
            .resizable(false)
            .show_separator_line(false)
            .exact_width(width)
            .frame(egui::Frame::new().inner_margin(egui::Margin {
                right: 8,
                ..Default::default()
            }))
            .show_inside(ui, |ui| self.render_settings_nav(ui, &list, &remote));

        egui::CentralPanel::default()
            .frame(egui::Frame::new())
            .show_inside(ui, |ui| {
                self.render_settings_page(ctx, ui, &list, &groups, &remote, &conflicts);
            });
    }

    /// The Settings list: General leads with Global and ends with
    /// Simulation around the web server; Cameras ends with Replay and Body
    /// tracking; Golfers holds one row per golfer. The settings' own items
    /// wait for the settings to load.
    pub(super) fn settings_list(
        &self,
        groups: &[(CardGroup, Vec<DeviceCard>)],
        remote: &[remote::RemoteCard],
    ) -> settings_nav::SettingsList {
        let loaded = self.settings.loaded;
        SettingsSection::ALL
            .into_iter()
            .map(|section| {
                let group = section.group();
                let (lead, trail): (&[SettingsItem], &[SettingsItem]) = if loaded {
                    match section {
                        SettingsSection::General => {
                            (&[SettingsItem::Global], &[SettingsItem::Simulation])
                        }
                        SettingsSection::Cameras => {
                            (&[], &[SettingsItem::Replay, SettingsItem::BodyTracking])
                        }
                        _ => (&[], &[]),
                    }
                } else {
                    (&[], &[])
                };
                let mut items = lead.to_vec();
                items.extend(
                    groups
                        .iter()
                        .filter(|(g, _)| Some(*g) == group)
                        .flat_map(|(_, m)| m.iter())
                        .map(|c| SettingsItem::Card(c.id.clone())),
                );
                items.extend(
                    remote
                        .iter()
                        .filter(|c| Some(c.group) == group)
                        .map(|c| SettingsItem::Card(c.key())),
                );
                items.extend_from_slice(trail);
                if section == SettingsSection::Golfers && loaded {
                    items
                        .extend((0..self.settings.golfers.golfers.len()).map(SettingsItem::Golfer));
                }
                (section, items)
            })
            .collect()
    }

    /// The selected item's page, as tall as Settings.
    pub(super) fn render_settings_page(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        list: &settings_nav::SettingsList,
        groups: &[(CardGroup, Vec<DeviceCard>)],
        remote: &[remote::RemoteCard],
        conflicts: &HashMap<String, String>,
    ) {
        let selected = self.selected_item(list);
        #[cfg(test)]
        if self.all_settings_sections {
            // Every page at once, in the card grid, the golfers' page once.
            let mut all: Vec<SettingsItem> =
                list.iter().flat_map(|(_, items)| items.clone()).collect();
            all.retain(|i| !matches!(i, SettingsItem::Golfer(n) if *n > 0));
            egui::ScrollArea::vertical()
                .id_salt("settings_pages")
                .auto_shrink(false)
                .show(ui, |ui| {
                    let height = TEST_PAGE_HEIGHT;
                    let pages = Pages::Grid(&all);
                    self.render_pages(ctx, ui, pages, height, groups, remote, conflicts);
                });
            return;
        }
        if let Some(item) = selected {
            ui.add_space(4.0);
            let height = ui.available_height();
            let pages = Pages::One(&item);
            self.render_pages(ctx, ui, pages, height, groups, remote, conflicts);
        }
    }

    /// The pages of `pages`, each `height` tall.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_pages(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        pages: Pages<'_>,
        height: f32,
        groups: &[(CardGroup, Vec<DeviceCard>)],
        remote: &[remote::RemoteCard],
        conflicts: &HashMap<String, String>,
    ) {
        let locked = self.settings.locked();
        let saving = self.settings.saving();

        let mut monitors =
            routing_monitors(&self.settings.actors, self.settings.saved_config_ref());
        monitors.extend(remote_monitors(&self.bus_states, remote));

        let general_editable = (!locked || self.settings.global_dirty) && !saving;
        let golfers_editable = (!locked || self.settings.golfers.dirty) && !saving;
        let mut clicked: Option<(String, Option<usize>, CardAction)> = None;
        let mut page = |ui: &mut egui::Ui, item: &SettingsItem| match item {
            SettingsItem::Global => self.render_global_card(ctx, ui, general_editable, height),
            SettingsItem::Simulation => {
                self.render_simulation_card(ctx, ui, general_editable, height)
            }
            SettingsItem::Replay => self.render_replay_card(ctx, ui, general_editable, height),
            SettingsItem::BodyTracking => {
                self.render_body_tracking_card(ctx, ui, general_editable, height);
            }
            SettingsItem::Golfer(n) => {
                let n = *n;
                self.settings.golfers.selected = n;
                self.render_golfers_card(ctx, ui, golfers_editable, height);
                let shown = self.settings.golfers.selected;
                if shown != n && !self.settings.golfers.golfers.is_empty() {
                    self.select_item(SettingsSection::Golfers, SettingsItem::Golfer(shown));
                }
            }
            SettingsItem::Card(id) => {
                if let Some(other) = remote.iter().find(|c| c.key() == *id) {
                    self.render_remote_card(ui, other, height);
                } else if let Some(card) = groups
                    .iter()
                    .flat_map(|(_, m)| m.iter())
                    .find(|c| c.id == *id)
                {
                    let conflict = self.conflict_of(&card.id, conflicts);
                    if let Some(action) =
                        self.render_device_card(ui, card, height, &monitors, conflict.as_deref())
                    {
                        clicked = Some((card.id.clone(), card.form, action));
                    }
                }
            }
        };
        match pages {
            Pages::One(item) => page(ui, item),
            #[cfg(test)]
            Pages::Grid(items) => cards::card_grid(ui, items.len(), |ui, i| page(ui, &items[i])),
        }

        if let Some((id, form, action)) = clicked {
            self.card_action(ctx, &id, form, action);
        }

        ui.add_space(12.0);
        if self.settings.saving() {
            ui.horizontal(|ui| {
                ui.spinner();
            });
        }
    }

    /// Device card `card`'s page, `height` tall. Returns what was clicked.
    pub(super) fn render_device_card(
        &mut self,
        ui: &mut egui::Ui,
        card: &DeviceCard,
        height: f32,
        monitors: &[(String, String)],
        conflict: Option<&str>,
    ) -> Option<CardAction> {
        let locked = self.settings.locked();
        let saving = self.settings.saving();
        let error = self
            .settings
            .save_error
            .as_ref()
            .filter(|e| e.target.scope() == Some(card.id.as_str()))
            .map(|e| e.message.clone());
        let saved_rotation = self.camera_rotation(&card.id);
        // Off in the saved settings, or in the save turning it off: shown
        // Disabled at once, before the server has stopped it.
        let off = self.settings.turned_off(&card.id);
        let disabled = off
            .then(|| self.actors.get(&card.id))
            .filter(|a| a.is_none_or(|a| !a.is_disabled()))
            .map(|a| {
                let name = a.map(|a| a.name.clone()).unwrap_or_default();
                ActorStatusResponse::disabled(name)
            });
        // A card that is off cannot be turned on while an enabled card of its
        // type has its address.
        let clash = off
            .then(|| self.settings.enable_clash(&card.id))
            .flatten()
            .map(|c| c.message());
        let form = card.form.and_then(|idx| self.settings.actors.get_mut(idx));
        let dirty = form.as_ref().is_some_and(|f| f.dirty());
        let still = (entry_type(&card.id) == "camera")
            .then(|| self.stills.entry(card.id.clone()).or_default());
        // A failed reconnect request shows as the card's `error`, like an
        // error the actor reports. An ownership conflict is added to
        // the card's own error in the same row.
        let reconnect_error = self.reconnect_errors.get(&card.id);
        let failed = (reconnect_error.is_some() || conflict.is_some()).then(|| {
            let mut shown = self
                .actors
                .get(&card.id)
                .cloned()
                .unwrap_or_else(|| crate::app::new_actor(String::new()));
            let own = reconnect_error.cloned().or_else(|| {
                shown
                    .telemetry
                    .get("error")
                    .filter(|e| !e.is_empty())
                    .cloned()
            });
            shown
                .telemetry
                .insert("error".into(), error_row(own.as_deref(), conflict));
            shown
        });
        let rotation = match form.as_deref() {
            Some(ActorFormEntry::Device(d)) if d.is_camera() => {
                flighthook::rotation_in_effect_deg(d.cam.rotation)
            }
            _ => saved_rotation,
        };
        let switch = form
            .as_ref()
            .filter(|_| settings::switchable(entry_type(&card.id)))
            .map(|_| settings::HeaderSwitch {
                on: !off,
                enabled: !locked && !saving && clash.is_none(),
                clash,
            });
        let view = CardView {
            rotation,
            switch,
            telemetry: failed
                .as_ref()
                .or(disabled.as_ref())
                .or_else(|| self.actors.get(&card.id)),
            now_ms: self.now_ms,
            height,
            editable: (!locked || dirty) && !saving,
            can_save: form.as_ref().is_some_and(|f| settings::actor_valid(f)),
            saving,
            error: error.as_deref(),
            reconnecting: self.reconnecting.contains(&card.id),
            monitors,
            cameras: &self.camera_list,
            remote: false,
        };
        device_card(ui, &card.id, &view, form, still)
    }

    /// The ownership conflict row of local card `card`: the Web
    /// Server card speaks for flighthook's own actor (session, practice,
    /// the `flighthook` namespace), any other card for its own actor.
    pub(super) fn conflict_of(
        &self,
        card: &str,
        conflicts: &HashMap<String, String>,
    ) -> Option<String> {
        let actor = if entry_type(card) == "webserver" {
            Some(self.flighthook_id.as_str())
        } else {
            self.card_keys
                .iter()
                .find(|(_, key)| key.as_str() == card)
                .map(|(id, _)| id.as_str())
        };
        actor.and_then(|id| conflicts.get(id)).cloned()
    }

    /// The read-only page of a remote actor: its telemetry only, no
    /// settings and no Reconnect.
    pub(super) fn render_remote_card(
        &self,
        ui: &mut egui::Ui,
        card: &remote::RemoteCard,
        height: f32,
    ) {
        let view = CardView {
            rotation: 0,
            switch: None,
            telemetry: Some(&card.status),
            now_ms: self.now_ms,
            height,
            editable: false,
            can_save: false,
            saving: false,
            error: None,
            reconnecting: false,
            monitors: &[],
            cameras: &self.camera_list,
            remote: true,
        };
        let _ = device_card(ui, &card.key(), &view, None, None);
    }

    /// Carry out a click on card `id`, whose form entry is at `form`.
    pub(super) fn card_action(
        &mut self,
        ctx: &egui::Context,
        id: &str,
        form: Option<usize>,
        action: CardAction,
    ) {
        match (action, form) {
            (CardAction::FindMat, _) => net::post_find_mat(ctx, id),
            (CardAction::Reconnect, _) => self.reconnect(ctx, id),
            (CardAction::ForgetPairing, _) => self.forget_pairing(ctx, id),
            (CardAction::Enable(on), _) => self.set_enabled(ctx, id, on),
            (CardAction::Still, _) => {
                #[cfg(not(test))]
                net::fetch_still(ctx, &self.pending, id);
            }
            (CardAction::RefreshCameras, _) => self.refresh_cameras(ctx),
            (CardAction::Save, Some(_)) => self.post_save(ctx, SaveTarget::Actor(id.to_string())),
            (CardAction::Cancel, Some(idx)) => self.cancel_device(idx),
            (CardAction::Remove, Some(idx)) if cards::removable(entry_type(id)) => {
                let name = self.settings.actors[idx].name().to_string();
                self.confirm_remove = Some(PendingRemoval {
                    id: id.to_string(),
                    name,
                });
            }
            (CardAction::Remove, Some(_)) | (_, None) => {}
        }
    }

    /// Reconnect actor `id`: one request at a time per card, the card's
    /// Reconnect disabled until the new instance has started or the request
    /// has failed.
    pub(crate) fn reconnect(&mut self, ctx: &egui::Context, id: &str) {
        if self.reconnecting.contains(id) {
            return;
        }
        self.reconnect_done(id);
        self.reconnecting.insert(id.to_owned());
        self.reconnect_errors.remove(id);
        #[cfg(not(test))]
        net::post_reconnect(ctx, &self.pending, id);
        #[cfg(test)]
        let _ = ctx;
    }

    /// Forget Pureplay Accessory card `id`'s pairing, then reconnect it, as
    /// [`reconnect`](Self::reconnect) does: one request at a time per card.
    pub(crate) fn forget_pairing(&mut self, ctx: &egui::Context, id: &str) {
        if self.reconnecting.contains(id) {
            return;
        }
        self.reconnect_done(id);
        self.reconnecting.insert(id.to_owned());
        self.reconnect_errors.remove(id);
        #[cfg(not(test))]
        net::post_forget_pairing(ctx, &self.pending, id);
        #[cfg(test)]
        let _ = ctx;
    }

    /// Turn card `id` on or off: a save of only that, scoped to it, posted
    /// at once.
    pub(crate) fn set_enabled(&mut self, ctx: &egui::Context, id: &str, on: bool) {
        if self.settings.locked() {
            return;
        }
        if let Some(posted) = self.settings.begin_enable(id, on) {
            #[cfg(not(test))]
            net::post_settings(ctx, &self.pending, &posted, Some(id));
            #[cfg(test)]
            let _ = (ctx, posted);
        }
    }

    /// Ask the server for its camera list again.
    pub(crate) fn refresh_cameras(&mut self, ctx: &egui::Context) {
        if self.camera_list.loading {
            return;
        }
        self.camera_list.loading = true;
        #[cfg(not(test))]
        net::fetch_cameras(ctx, &self.pending);
        #[cfg(test)]
        let _ = ctx;
    }

    /// Fetch the camera list the first time a camera's page shows, or a
    /// camera card has unsaved edits.
    pub(super) fn camera_list_due(&self, shown: Option<&SettingsItem>) -> bool {
        // Tests drawing every page show every camera's.
        #[cfg(test)]
        let every = self.all_settings_sections;
        #[cfg(not(test))]
        let every = false;
        self.camera_list.list.is_none()
            && self.camera_list.error.is_none()
            && !self.camera_list.loading
            && self.settings.actors.iter().any(|a| {
                matches!(a, ActorFormEntry::Device(d) if d.is_camera())
                    && (a.dirty() || shown == Some(&SettingsItem::Card(a.global_id())) || every)
            })
    }

    /// Add a new, unsaved card from (+) menu `preset`, showing its settings.
    pub(crate) fn add_device(&mut self, preset: &cards::AddPreset) {
        if let Some(id) = self.settings.add_preset(preset) {
            self.select_item(SettingsSection::of_actor(&id), SettingsItem::Card(id));
        }
    }

    /// Cancel the edits on the card whose form entry is at `idx`: back to
    /// the saved settings, or gone if it was never saved.
    pub(crate) fn cancel_device(&mut self, idx: usize) {
        let _ = self.settings.cancel_actor(idx);
    }
}
