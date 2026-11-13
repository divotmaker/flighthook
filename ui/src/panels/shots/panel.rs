//! The Shots tab panel: shot caches, the viewed golfer and follow, the toolbar, the shot
//! list, the Cards and Table views and the Clear history confirm (`impl FlighthookApp`).

use super::*;

/// The Shots tab's filter over the device filter `device` and the viewed
/// golfer `view` (a golfer key, `None` = shots with no golfer), resolving
/// golfer ids with `groups`' keys.
pub(crate) fn shot_filter<'a>(
    device: &'a Option<String>,
    view: &'a Option<String>,
    groups: &'a ShotGroups,
) -> ShotFilter<'a> {
    ShotFilter::device(device.as_deref()).golfer(Some(view.as_deref()), groups.golfers())
}

impl FlighthookApp {
    /// Bring the caches derived from the shot list up to date: the golfer
    /// keys of the saved golfers and the golfer up, the viewed golfer, the
    /// rows in display units and the device ids. A device filter whose
    /// device has left the history falls back to every device.
    pub(crate) fn refresh_shot_caches(&mut self) {
        let keys = crate::groups::GolferKeys::of(self.settings.saved_config_ref());
        self.groups.set_golfers(keys, self.turn_golfer.as_deref());
        self.refresh_golfer_list();
        self.follow_view();
        let (rev, units) = (self.shots_rev, self.units());
        let shots = &self.shots;
        self.display_rows.get((rev, units), || {
            shots.iter().map(|s| s.to_unit_system(units)).collect()
        });
        let ids = self.device_ids.get(rev, || chart::device_ids(shots));
        if self
            .device_filter
            .as_ref()
            .is_some_and(|d| !ids.contains(d))
        {
            self.device_filter = None;
        }
    }

    /// Whether the view follows the turn: Follow turns is on, or fewer than
    /// two golfers are known (the toggle is hidden and counts as on).
    pub(crate) fn follows_turns(&self) -> bool {
        self.follow_turns || self.known_golfers().len() < 2
    }

    /// Bring the viewed golfer up to date. While the view follows the turn
    /// it is the golfer up, or a view-only row picked since the golfer up's
    /// last event ([`FlighthookApp::view_hold`]). Otherwise it stays put while
    /// the Golfer card lists it (a known golfer, or a golfer or "No golfer"
    /// with shots in the history), and falls back to the golfer up once it
    /// does not.
    pub(crate) fn follow_view(&mut self) {
        let up = self.groups.up_key().map(str::to_owned);
        let list = &self.golfer_list;
        let view = if self.follows_turns() {
            match self.view_hold.take() {
                Some((key, at)) if at == up && list.viewable(key.as_deref()) => {
                    self.view_hold = Some((key.clone(), at));
                    key
                }
                _ => up,
            }
        } else {
            self.view_hold = None;
            match &self.viewed_golfer {
                key if list.viewable(key.as_deref()) => key.clone(),
                _ => up,
            }
        };
        self.set_view(view);
    }

    /// A view-only row of the Golfer card picked: view `key` (`None` = the
    /// shots with no golfer). Nothing is sent, Follow on or off; with Follow
    /// on the view goes back to the golfer up at their next event (a turn
    /// event, a new shot, a click on a known golfer's row).
    pub(crate) fn pick_view_only(&mut self, key: Option<String>) {
        if self.follows_turns() {
            self.view_hold = Some((key.clone(), self.groups.up_key().map(str::to_owned)));
        }
        self.set_view(key);
    }

    /// View the golfer with key `view` (`None` = no golfer). A change moves
    /// the selection to that golfer's latest shot, and while the Cameras
    /// pane shows, to Live when that golfer has no shot.
    pub(crate) fn set_view(&mut self, view: Option<String>) {
        let changed = view != self.viewed_golfer;
        if changed {
            self.viewed_golfer = view;
            self.shot_focus.follow();
        }
        let other = self.viewed_golfer.as_deref() != self.groups.up_key();
        self.groups
            .set_view(other.then(|| self.viewed_golfer.clone()));
        // The Cameras pane's opening rule: Live when the new view has no
        // shot to show, else its latest shot.
        if changed && self.cameras_pane_shown() {
            self.live_selected = self.shown_index().is_none();
        }
    }

    /// The golfer New set names: `None` (the golfer up) while the view is
    /// the golfer up, else the viewed golfer.
    pub(crate) fn new_set_golfer(&self) -> Option<flighthook::GolferId> {
        if !self.viewing_other() {
            return None;
        }
        self.viewed_golfer.as_deref()?.parse().ok()
    }

    /// Whether Clear history covers more than one golfer: two or more are
    /// known, or the shots belong to more than one golfer (or to a golfer
    /// and "No golfer").
    pub(crate) fn clear_covers_golfers(&self) -> bool {
        let keys = self.groups.golfers();
        let mut golfers = self.shots.iter().map(|r| keys.key(r.golfer.as_deref()));
        let first = golfers.next();
        self.known_golfers().len() >= 2 || golfers.any(|k| Some(k) != first)
    }

    /// Whether the view is on someone other than the golfer up.
    pub(crate) fn viewing_other(&self) -> bool {
        !self.groups.viewing_up()
    }

    /// Whether the view is on a view-only row of the Golfer card (a golfer
    /// who is no longer known, or "No golfer") other than the golfer up:
    /// New set has no golfer to act on then.
    pub(crate) fn viewing_view_only(&self) -> bool {
        self.viewing_other()
            && !self
                .viewed_golfer
                .as_deref()
                .is_some_and(|key| self.known_golfers().iter().any(|g| g.id == key))
    }

    /// `shots[i]` in display units: from the cache `refresh_shot_caches`
    /// keeps, converted on the spot when the cache is out of date.
    pub(super) fn display_row(&self, i: usize) -> Option<Cow<'_, ShotRow>> {
        let cached = self
            .display_rows
            .peek(&(self.shots_rev, self.units()))
            .and_then(|rows| rows.get(i));
        match cached {
            Some(row) => Some(Cow::Borrowed(row)),
            None => self
                .shots
                .get(i)
                .map(|s| Cow::Owned(s.to_unit_system(self.units()))),
        }
    }

    /// The distinct actor ids in the shot list, exact and sorted.
    pub(in crate::panels) fn devices(&self) -> Cow<'_, [String]> {
        match self.device_ids.peek(&self.shots_rev) {
            Some(ids) => Cow::Borrowed(ids.as_slice()),
            None => Cow::Owned(chart::device_ids(&self.shots)),
        }
    }

    /// Key of the view's members around the shown shot `shown`.
    pub(in crate::panels) fn members_key(&self, shown: usize) -> MembersKey {
        MembersKey {
            shots_rev: self.shots_rev,
            groups_rev: self.groups.rev(),
            view: self.groups.mode,
            device: self.device_filter.clone(),
            golfer: self.viewed_golfer.clone(),
            shown: Some(shown),
            club: self.selected_club,
        }
    }

    /// Bring the cached view's members around the shown shot `shown` up to
    /// date ([`chart::members_for`], after the device filter) and return
    /// their key. The range bars, the stats and face impact cards, Flight
    /// and Dispersion all take their members from here.
    pub(in crate::panels) fn refresh_members(&mut self, shown: usize) -> MembersKey {
        let key = self.members_key(shown);
        let set = bar_group(&self.groups, self.shots.get(shown), self.selected_club);
        let filter = shot_filter(&self.device_filter, &self.viewed_golfer, &self.groups);
        let (shots, groups, view) = (&self.shots, &self.groups, self.groups.mode);
        self.view_members.get(key.clone(), || {
            chart::members_for(shots, groups, view, set, Some(shown), filter)
        });
        key
    }

    /// The Shots tab's filter: the device filter and the viewed golfer.
    pub(crate) fn filter(&self) -> ShotFilter<'_> {
        shot_filter(&self.device_filter, &self.viewed_golfer, &self.groups)
    }

    /// Index of the shot on show, among the shots the filter passes.
    pub(crate) fn shown_index(&self) -> Option<usize> {
        self.shot_focus.index(&self.shots, self.filter())
    }

    /// Whether the shot on show is the latest the filter passes.
    pub(crate) fn following(&self) -> bool {
        self.shot_focus.following(&self.shots, self.filter())
    }

    /// Note the latest shot the filter passes ([`ShotFocus::observe`]).
    pub(crate) fn observe_shots(&mut self, now: f64) {
        let filter = shot_filter(&self.device_filter, &self.viewed_golfer, &self.groups);
        self.shot_focus.observe(&self.shots, filter, now);
    }

    /// Show `shots[i]`, leaving Live.
    pub(crate) fn select_shot(&mut self, i: usize) {
        let filter = shot_filter(&self.device_filter, &self.viewed_golfer, &self.groups);
        self.shot_focus.select(&self.shots, filter, i);
        self.live_selected = false;
    }

    /// Indices into `shots`, ascending, of the rows the filter passes (the
    /// viewed golfer's, narrowed by the device filter): the Table view's
    /// rows.
    pub(super) fn filtered_rows(&mut self) -> &[usize] {
        let filter = shot_filter(&self.device_filter, &self.viewed_golfer, &self.groups);
        let key = (self.shots_rev, self.groups.rev(), filter.key());
        let shots = &self.shots;
        self.filtered_rows.get(key, || {
            (0..shots.len())
                .filter(|&i| filter.passes(&shots[i]))
                .collect()
        })
    }

    /// The charts' places for `view` ([`chart_slots`]); none in the
    /// portrait layout, where the charts are part of the Cards.
    pub(super) fn chart_slots(&self, view: ShotsView, ctx: &egui::Context) -> ChartSlots {
        if self.portrait {
            ChartSlots {
                right: None,
                bottom: None,
            }
        } else {
            chart_slots(view, ctx.content_rect().size())
        }
    }

    pub(crate) fn render_shots_panel(&mut self, ui: &mut egui::Ui) {
        self.refresh_shot_caches();
        self.sync_cameras_pane();

        // The shots navigation runs the full height on the left, the same
        // panel (width, scroll position) in the Shots and Cameras tabs; the
        // tab's toolbar and body sit right of it. In the portrait layout it
        // sits on top, resizable, and the body under it.
        if self.portrait {
            let height = ui.available_height();
            egui::TopBottomPanel::top(egui::Id::new("shots_list_top"))
                .resizable(true)
                .show_separator_line(true)
                .default_height((height * LIST_PORTRAIT_SHARE).round())
                .height_range(
                    LIST_PORTRAIT_MIN..=(height - LIST_PORTRAIT_MIN).max(LIST_PORTRAIT_MIN),
                )
                .frame(egui::Frame::new().inner_margin(egui::Margin {
                    bottom: 8,
                    ..Default::default()
                }))
                .show_inside(ui, |ui| self.render_shot_list(ui));
        } else {
            let view = self.shots_layout().view();
            let slots = self.chart_slots(view, ui.ctx());
            let beside = slots.right.unwrap_or(0.0);
            let default = ((ui.available_width() - beside) * 0.18)
                .clamp(*LIST_DEFAULT_WIDTH.start(), *LIST_DEFAULT_WIDTH.end());
            egui::SidePanel::left(egui::Id::new("shots_list"))
                .resizable(true)
                .show_separator_line(false)
                .default_width(default)
                .width_range(LIST_MIN_WIDTH..=LIST_MAX_WIDTH)
                .frame(egui::Frame::new().inner_margin(egui::Margin {
                    right: 8,
                    ..Default::default()
                }))
                .show_inside(ui, |ui| self.render_shot_list(ui));
        }
        if self.shots.is_empty() {
            self.confirm_clear = false;
        }
        if self.confirm_clear {
            self.render_clear_confirm(ui.ctx());
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::new())
            .show_inside(ui, |ui| self.render_shots_body(ui));
        self.render_replay(ui.ctx());
    }

    /// The tab's toolbar and, under it, its body: Cards or Table with the
    /// charts, or the Cameras pane.
    pub(super) fn render_shots_body(&mut self, ui: &mut egui::Ui) {
        self.render_shots_toolbar(ui);
        // Whatever the toolbar changed is settled before the panes read it.
        self.sync_cameras_pane();
        ui.add_space(8.0);

        // The right-hand column is laid out first so it runs the full
        // height; the bottom row then spans the width left of it. Chart
        // sizes are fixed for a window size.
        let layout = self.shots_layout();
        let view = layout.view();
        let slots = self.chart_slots(view, ui.ctx());
        let gap = CHART_GAP as i8;
        // The Cameras pane takes everything right of the shot list.
        let cameras = layout == ShotsLayout::Cameras;
        if let Some(width) = slots.right.filter(|_| !cameras) {
            egui::SidePanel::right(egui::Id::new("shots_right"))
                .resizable(false)
                .exact_width(width)
                .frame(egui::Frame::new().inner_margin(egui::Margin {
                    left: gap,
                    ..Default::default()
                }))
                .show_inside(ui, |ui| self.render_chart(ui, Chart::Dispersion));
        }
        if let Some((height, row)) = slots.bottom.filter(|_| !cameras) {
            let height = bottom_height(height, row, ui.available_width());
            egui::TopBottomPanel::bottom(egui::Id::new(("shots_bottom", view)))
                .resizable(false)
                .exact_height(height)
                .frame(egui::Frame::new().inner_margin(egui::Margin {
                    top: gap,
                    ..Default::default()
                }))
                .show_inside(ui, |ui| match row {
                    BottomRow::Flight => self.render_chart(ui, Chart::Flight),
                    BottomRow::Dispersion => self.render_chart(ui, Chart::Dispersion),
                    BottomRow::FlightAndDispersion => {
                        let rect = ui.available_rect_before_wrap();
                        let (flight, _) = row_split(rect.width());
                        let left = rect.with_max_x(rect.left() + flight);
                        let right = rect.with_min_x(left.right() + CHART_GAP);
                        for (rect, chart) in [(left, Chart::Flight), (right, Chart::Dispersion)] {
                            ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                                self.render_chart(ui, chart);
                            });
                        }
                    }
                });
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new())
            .show_inside(ui, |ui| match layout {
                ShotsLayout::Table => self.render_shots_table(ui),
                ShotsLayout::Cameras => self.render_cameras_pane(ui),
                ShotsLayout::Cards => self.render_shot_cards(ui),
            });
    }

    /// Draw `chart` in its panel `ui`.
    pub(super) fn render_chart(&mut self, ui: &mut egui::Ui, chart: Chart) {
        match chart {
            Chart::Flight => self.render_flight_panel(ui),
            Chart::Dispersion => {
                let table = self.shots_view == ShotsView::Table;
                self.render_dispersion_panel(ui, table);
            }
        }
    }

    /// The tab's toolbar: Live and the reference picker in the Cameras tab,
    /// else `Cards | Table` and, in Table, the "italic = simulated" legend.
    pub(super) fn render_shots_toolbar(&mut self, ui: &mut egui::Ui) {
        // A fixed row height, taller than any item, so every item centers on
        // the same line; in the portrait layout the items wrap.
        let row = egui::vec2(ui.available_width(), TOOLBAR_HEIGHT);
        let layout = egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(self.portrait);
        ui.allocate_ui_with_layout(row, layout, |ui| {
            // The Cards | Table choice is kept while the Cameras tab shows.
            if self.render_live_toggle(ui) {
                ui.add_space(12.0);
                self.render_reference_picker(ui);
                return;
            }
            let current = ShotsView::ALL
                .iter()
                .position(|v| *v == self.shots_view)
                .unwrap_or(0);
            let labels: Vec<&str> = ShotsView::ALL.iter().map(|v| v.label()).collect();
            let view = theme::segmented(ui, &labels, current, None);
            devices::mark(ui, || "view_toggle".to_owned(), &view.response);
            if let Some(i) = view.inner {
                // A pick made in Cards is scrolled into view in the Table.
                if ShotsView::ALL[i] == ShotsView::Table && !self.following() {
                    self.shot_focus.reveal();
                }
                self.shots_view = ShotsView::ALL[i];
            }
            if self.shots_layout().view().shows_simulated_legend() {
                ui.add_space(24.0);
                ui.label(
                    egui::RichText::new("italic = simulated")
                        .italics()
                        .color(theme::TEXT_MUTED)
                        .size(12.0),
                )
                .on_hover_text(
                    "Values simulated from the measured launch, or spin estimated by \
                     flighthook, not measured by the device. \
                     Offline is + right of the target line.",
                );
            }
        });
    }

    /// The navigation's device filter, `DEVICE [All ▾]`, while the shots
    /// come from two or more devices ([`device_options`]).
    pub(super) fn render_device_filter(&mut self, ui: &mut egui::Ui, inset: f32) {
        let ids = self.devices().into_owned();
        let Some(labels) = device_options_with(&ids, &self.actors, &self.remote_names) else {
            return;
        };
        let at = self
            .device_filter
            .as_ref()
            .and_then(|d| ids.iter().position(|id| id == d))
            .map_or(0, |p| p + 1);
        let mut picked = at;
        ui.horizontal(|ui| {
            ui.add_space(inset);
            ui.label(theme::caps("Device", 11.0, theme::TEXT_MUTED));
            ui.add_space(6.0);
            let width = (ui.available_width() - inset).max(60.0);
            let combo = egui::ComboBox::from_id_salt("device_filter")
                .width(width)
                .selected_text(labels[at].as_str())
                .show_ui(ui, |ui| {
                    for (i, label) in labels.iter().enumerate() {
                        let item = ui.selectable_value(&mut picked, i, label.as_str());
                        devices::mark(ui, || format!("device-filter:{i}"), &item);
                    }
                });
            devices::mark(ui, || "device-filter".to_owned(), &combo.response);
        });
        ui.add_space(6.0);
        if picked != at {
            self.device_filter = picked.checked_sub(1).and_then(|p| ids.get(p).cloned());
        }
    }

    /// The Clear history confirmation, over everything. Delete sends the
    /// command; Cancel, Escape or a click outside closes it.
    pub(super) fn render_clear_confirm(&mut self, ctx: &egui::Context) {
        let modal = egui::Modal::new(egui::Id::new("clear_history_confirm"))
            .frame(theme::card().inner_margin(egui::Margin::same(20)))
            .show(ctx, |ui| {
                ui.set_width(if self.portrait {
                    CLEAR_CONFIRM_WIDTH.min(crate::topbar::menu_max_width(ui.ctx()) - 20.0)
                } else {
                    CLEAR_CONFIRM_WIDTH
                });
                ui.label(
                    egui::RichText::new(clear_confirm_title(
                        self.shots.len(),
                        self.clear_covers_golfers(),
                    ))
                    .size(17.0)
                    .strong()
                    .color(theme::TEXT),
                );
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(
                        "This can't be undone. Export first if you want to keep them.",
                    )
                    .color(theme::TEXT_MUTED),
                );
                ui.add_space(18.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.portrait {
                        ui.spacing_mut().interact_size.y = crate::topbar::TOUCH_TARGET;
                    }
                    let delete = ui.add(theme::danger_button("Delete", 15.0));
                    devices::mark(ui, || "clear_confirm:delete".to_owned(), &delete);
                    let cancel = ui.button("Cancel");
                    devices::mark(ui, || "clear_confirm:cancel".to_owned(), &cancel);
                    let (delete, cancel) = (delete.clicked(), cancel.clicked());
                    (delete, cancel)
                })
                .inner
            });
        let (delete, cancel) = modal.inner;
        if delete {
            self.send_group_command(ctx, GroupCommand::ClearHistory);
            self.confirm_clear = false;
        } else if cancel || modal.should_close() {
            self.confirm_clear = false;
        }
    }

    /// Label of the device behind `shot` ([`chart::device_label`]), when
    /// `show` (the session has shots from more than one device).
    pub(in crate::panels) fn device_name<'a>(
        &'a self,
        shot: &'a ShotRow,
        show: bool,
    ) -> Option<&'a str> {
        show.then(|| chart::device_label(&shot.actor, self.actor_name(&shot.actor)))
    }

    /// The name of the device `id` (a shot's actor): its card's name, else a
    /// remote actor's `bus.actor` name.
    pub(crate) fn actor_name(&self, id: &str) -> Option<&str> {
        actor_name(&self.actors, &self.remote_names, id)
    }

    /// The shot header line: number, device name, either the LATEST chip
    /// or, on an older shot, a Back to latest chip, and a Replay chip when
    /// the shot has clips. Returns what was clicked.
    pub(in crate::panels) fn render_shot_header(
        &self,
        ui: &mut egui::Ui,
        i: usize,
        following: bool,
    ) -> HeaderClick {
        let shot = &self.shots[i];
        let show_device = multiple_devices(&self.shots);
        let mut back = false;
        let mut replay = false;
        ui.horizontal_centered(|ui| {
            ui.label(
                egui::RichText::new(format!("Shot #{}", shot.shot_number))
                    .size(22.0)
                    .strong()
                    .color(theme::TEXT),
            );
            if let Some(name) = self.device_name(shot, show_device) {
                ui.label(
                    egui::RichText::new(name)
                        .size(15.0)
                        .color(theme::TEXT_MUTED),
                );
            }
            ui.add_space(4.0);
            match header_chip(&self.shots, i, following, self.filter()) {
                HeaderChip::BackToLatest => {
                    back = theme::jump_chip(ui, "Back to latest").clicked();
                }
                HeaderChip::Latest => {
                    theme::badge(ui, "LATEST", theme::GOLD);
                }
                HeaderChip::None => {}
            }
            if !shot.video.is_empty() {
                ui.add_space(4.0);
                let chip = theme::icon_chip(ui, theme::Icon::Video, "Replay");
                devices::mark(ui, || "replay".to_owned(), &chip);
                replay = chip.clicked();
            }
        });
        HeaderClick { back, replay }
    }

    /// The shots navigation of the Shots and Cameras tabs: the count line
    /// with the export and trash icons, the view selector and the device
    /// filter on top; one entry per shot, newest first,
    /// under the view's headers, scrolling in the middle; and, outside the
    /// view `none`, the hint line and New set / New session pinned at the
    /// foot. Clicking an entry shows that shot.
    pub(super) fn render_shot_list(&mut self, ui: &mut egui::Ui) {
        let inset = f32::from(GLOW_WIDTH);
        // Touch-sized icons on the phone.
        let toolbar = if self.portrait {
            crate::topbar::TOUCH_TARGET
        } else {
            TOOLBAR_HEIGHT
        };
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), toolbar),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.add_space(inset);
                ui.label(theme::caps("Shots", 11.0, theme::TEXT_MUTED));
                ui.label(
                    egui::RichText::new(shot_total(&self.shots, self.filter()).to_string())
                        .size(13.0)
                        .color(theme::TEXT),
                );
                let units = self.units();
                let portrait = self.portrait;
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(inset);
                    let clear = clear_history_button(ui, !self.shots.is_empty(), portrait);
                    devices::mark(ui, || "clear_history".to_owned(), &clear);
                    if clear.clicked() {
                        self.confirm_clear = true;
                    }
                    ui.add_space(4.0);
                    export_menu(ui, units, portrait);
                });
            },
        );
        self.render_view_selector(ui, inset);
        self.render_device_filter(ui, inset);

        let filter = shot_filter(&self.device_filter, &self.viewed_golfer, &self.groups);
        let strip = shown_strip(&self.groups, self.selected_club, self.viewing_view_only());
        let foot = list_foot_height(&strip);
        let list_height = if foot > 0.0 {
            (ui.available_height() - foot - ui.spacing().item_spacing.y).max(0.0)
        } else {
            ui.available_height()
        };
        let imperial = self.units() == UnitSystem::Imperial;
        let show_device = multiple_devices(&self.shots);
        let focus = self.shown_index();
        let latest = latest_index(&self.shots, filter);
        // In Table the Table takes the reveal, after the list used it.
        let reveal = if self.shots_layout() == ShotsLayout::Table {
            self.shot_focus.reveal
        } else {
            self.shot_focus.take_reveal()
        };
        let pulse = self.shot_focus.strength(ui.ctx().input(|i| i.time));
        let mut clicked = None;
        egui::ScrollArea::vertical()
            .id_salt("shot_list")
            .auto_shrink(false)
            .max_height(list_height)
            .show(ui, |ui| {
                // Room for the glow around the entries.
                egui::Frame::new()
                    .inner_margin(egui::Margin::same(GLOW_WIDTH as i8))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = ENTRY_GAP;
                        if self.shots.is_empty() {
                            ui.label(
                                egui::RichText::new("No shots yet")
                                    .size(13.0)
                                    .color(theme::TEXT_DIM),
                            );
                        }
                        let mut first = true;
                        // Top of the headers right above the next entry.
                        let mut headers_top = None;
                        let items =
                            list_items(&self.shots, &self.groups, self.selected_club, filter);
                        for item in items {
                            if !matches!(item, ListItem::Shot(_)) {
                                if !first {
                                    ui.add_space(HEADER_LEAD);
                                }
                                headers_top.get_or_insert(ui.cursor().top());
                            }
                            first = false;
                            let i = match item {
                                ListItem::Shot(i) => i,
                                ListItem::GroupHeader { id, next } => {
                                    self.paint_group_header(ui, &id, next);
                                    continue;
                                }
                                ListItem::ClubHeader {
                                    golfer,
                                    session,
                                    club,
                                    next,
                                } => {
                                    let section = (golfer, session, club);
                                    self.paint_club_header(ui, &section, next);
                                    continue;
                                }
                                ListItem::Ungrouped => {
                                    let size =
                                        egui::vec2(ui.available_width(), GROUP_HEADER_HEIGHT);
                                    let (rect, _) =
                                        ui.allocate_exact_size(size, egui::Sense::hover());
                                    list_header(
                                        ui.painter(),
                                        rect,
                                        "Not in a set",
                                        None,
                                        theme::TEXT_DIM,
                                    );
                                    continue;
                                }
                            };
                            let size = egui::vec2(ui.available_width(), ENTRY_HEIGHT);
                            let (rect, response) =
                                ui.allocate_exact_size(size, egui::Sense::click());
                            let selected = Some(i) == focus && !self.live_selected;
                            let above = headers_top.take();
                            if reveal && selected {
                                // The portrait list can be shorter than a header and the
                                // entry under it: the reveal keeps the header in view.
                                let target = match above {
                                    Some(top) if self.portrait => rect.with_min_y(top),
                                    _ => rect,
                                };
                                ui.scroll_to_rect(target.expand(inset), None);
                            }
                            if ui.is_rect_visible(rect.expand(inset))
                                && let Some(shown) = self.display_row(i)
                            {
                                let shot = &self.shots[i];
                                let entry = ListEntry::new(
                                    &shown,
                                    self.device_name(shot, show_device),
                                    imperial,
                                );
                                let is_latest = Some(i) == latest;
                                let state = EntryState {
                                    selected,
                                    hovered: response.hovered(),
                                    latest: is_latest,
                                    pulse: if is_latest { pulse } else { 0.0 },
                                };
                                list_entry(ui, rect, &entry, state);
                            }
                            devices::mark(ui, || format!("shot_entry:{i}"), &response);
                            if response
                                .on_hover_cursor(egui::CursorIcon::PointingHand)
                                .clicked()
                            {
                                clicked = Some(i);
                            }
                        }
                    });
            });
        if let Some(i) = clicked {
            // The Table, when it shows, scrolls the pick into view.
            self.select_shot(i);
            self.shot_focus.reveal();
        }
        if strip.actions {
            self.render_list_foot(ui, inset, &strip);
        }
    }

    /// The view selector under the list's count line.
    pub(super) fn render_view_selector(&mut self, ui: &mut egui::Ui, inset: f32) {
        let width = ui.available_width() - 2.0 * inset;
        let mode = self.groups.mode;
        let mut picked = None;
        ui.horizontal(|ui| {
            ui.add_space(inset);
            ui.vertical(|ui| {
                ui.set_width(width);
                let current = GROUPING_MODES
                    .iter()
                    .position(|(m, _)| *m == mode)
                    .unwrap_or(0);
                let labels = GROUPING_MODES.map(|(_, label)| label);
                let response = ui
                    .scope(|ui| {
                        picked = theme::segmented_fill(ui, &labels, current, 12.0);
                    })
                    .response;
                devices::mark(ui, || "view_selector".to_owned(), &response);
                response.on_hover_text(GROUPING_HINT);
            });
        });
        ui.add_space(6.0);
        if let Some(i) = picked {
            let ctx = ui.ctx().clone();
            self.set_grouping(&ctx, GROUPING_MODES[i].0);
        }
    }

    /// The list's foot, pinned under the scrolling entries: a hairline, the
    /// hint line when `strip` has one, then New set and New session
    /// sharing the width.
    pub(super) fn render_list_foot(&mut self, ui: &mut egui::Ui, inset: f32, strip: &Strip) {
        let width = ui.available_width() - 2.0 * inset;
        let view = self.groups.mode;
        let off_hint = new_group_off_hint(self.viewing_view_only());
        let mut new_group = false;
        let mut new_session = false;
        ui.horizontal(|ui| {
            ui.add_space(inset);
            ui.vertical(|ui| {
                ui.set_width(width);
                ui.spacing_mut().item_spacing.y = FOOT_SPACING;
                let (line, _) =
                    ui.allocate_exact_size(egui::vec2(width, FOOT_RULE), egui::Sense::hover());
                ui.painter().hline(
                    line.x_range(),
                    line.top(),
                    egui::Stroke::new(1.0, theme::BORDER),
                );
                if let Some(hint) = &strip.hint {
                    let size = egui::vec2(width, STRIP_HINT_HEIGHT);
                    ui.allocate_ui_with_layout(
                        size,
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.label(
                                egui::RichText::new(hint)
                                    .size(11.0)
                                    .color(theme::TEXT_MUTED),
                            );
                        },
                    );
                }
                let gap = 8.0;
                let chip = egui::vec2((width - gap) / 2.0, STRIP_BUTTONS_HEIGHT);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    let group = theme::chip_button(ui, "New set", strip.new_group, chip);
                    new_group = if strip.new_group {
                        group.on_hover_text(new_group_hint(view)).clicked()
                    } else {
                        group.on_hover_text(off_hint);
                        false
                    };
                    let session = theme::chip_button(ui, "New session", strip.new_session, chip);
                    new_session = if strip.new_session {
                        session.on_hover_text(NEW_SESSION_HINT).clicked()
                    } else {
                        session.on_hover_text("No set is open.");
                        false
                    };
                });
            });
        });

        let ctx = ui.ctx().clone();
        if new_group {
            self.send_group_command(&ctx, GroupCommand::NewGroup);
        }
        if new_session {
            self.send_group_command(&ctx, GroupCommand::NewSession);
        }
    }

    /// Switch the view: applied here at once and saved as the default view,
    /// over the WebSocket when it is up, else over REST.
    pub(super) fn set_grouping(&mut self, ctx: &egui::Context, mode: ShotGrouping) {
        self.groups.mode = mode;
        self.settings.sync_shot_grouping(mode);
        if let Some(tx) = &mut self.ws_sender {
            net::send_command(
                tx,
                &flighthook::UiCommand::Local(flighthook::UiLocalCommand::Grouping { mode }),
            );
        } else {
            net::post_grouping(ctx, mode);
        }
    }

    /// Send `command` over the WebSocket when it is up, else over REST.
    /// Without the socket this UI hears none of the bus events that follow,
    /// so it applies the command itself (see `apply_posted`).
    /// New set rolls the viewed golfer's set ([`Self::new_set_golfer`]).
    pub(crate) fn send_group_command(&mut self, ctx: &egui::Context, command: GroupCommand) {
        let viewed = self.new_set_golfer();
        if command == GroupCommand::NewGroup && self.viewing_view_only() {
            return;
        }
        if let Some(tx) = &mut self.ws_sender {
            let command = match command {
                GroupCommand::NewGroup => {
                    self.groups.asked_new_set();
                    net::new_block_command(viewed, false)
                }
                GroupCommand::NewSession => net::new_block_command(None, true),
                GroupCommand::ClearHistory => {
                    flighthook::UiCommand::Local(flighthook::UiLocalCommand::ClearHistory)
                }
            };
            net::send_command(tx, &command);
            return;
        }
        match command {
            GroupCommand::NewGroup => {
                self.groups.asked_new_set();
                net::post_new_block(ctx, &self.pending, viewed, false);
            }
            GroupCommand::NewSession => net::post_new_block(ctx, &self.pending, None, true),
            GroupCommand::ClearHistory => {
                net::post_clear_history(ctx);
                // Over REST no socket tells this UI: empty the Shots tab now.
                self.clear_shot_history();
            }
        }
    }

    /// Paint the By set header of set `id`: accent while it is open, muted
    /// once closed, gold over the empty set the next shot joins (`next`).
    pub(super) fn paint_group_header(&self, ui: &mut egui::Ui, id: &str, next: bool) {
        let size = egui::vec2(ui.available_width(), GROUP_HEADER_HEIGHT);
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
        let Some(group) = self.groups.get(id) else {
            return;
        };
        // Counted without its Auto-chip shots, which still list under it,
        // and under a device filter, that device's shots alone.
        let counted = SetInfo {
            shots: chart::set_shot_count(group, &self.shots, self.filter()),
            ..group.clone()
        };
        let color = match (next, group.open) {
            (true, _) => theme::GOLD,
            (false, true) => theme::ACCENT,
            (false, false) => theme::TEXT_MUTED,
        };
        let time = clock_time(group.started_at);
        if ui.is_rect_visible(rect) {
            list_header(
                ui.painter(),
                rect,
                &set_title(&counted, next),
                time.as_deref(),
                color,
            );
        }
        let state = if group.open { "open" } else { "closed" };
        let mut hover = if next {
            format!("Set {}: the next shot joins it", group.number)
        } else {
            format!("Set {}, {state}", group.number)
        };
        if let Some(time) = &time {
            hover.push_str(&format!(". Started {time}."));
        }
        response.on_hover_text(hover);
    }

    /// Paint the By club header of `club`'s section in `session`: accent in
    /// the current session, muted for an earlier one, gold over the club
    /// whose first shot of the session is next (`next`). The time is when
    /// the section's first set started. The section is the golfer key,
    /// session and club of `section`.
    pub(super) fn paint_club_header(&self, ui: &mut egui::Ui, section: &SectionKey, next: bool) {
        let (golfer, session, club) = (section.0.as_deref(), section.1, section.2);
        let size = egui::vec2(ui.available_width(), GROUP_HEADER_HEIGHT);
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
        let sets: Vec<&SetInfo> = self
            .groups
            .iter_sets()
            .filter(|g| {
                g.session == session
                    && g.club == club
                    && self.groups.golfers().key(g.golfer.as_deref()) == golfer
            })
            .collect();
        // Counted without Auto-chip shots, which still list in the section,
        // and under a device filter, that device's shots alone.
        let per_set: Vec<u32> = sets
            .iter()
            .map(|g| chart::set_shot_count(g, &self.shots, self.filter()))
            .collect();
        let counted = per_set.iter().filter(|&&n| n > 0).count();
        let shots: u32 = per_set.iter().sum();
        let earlier = session < self.groups.session();
        let tally = (!next).then_some((counted, shots as usize));
        let color = match (next, earlier) {
            (true, _) => theme::GOLD,
            (false, true) => theme::TEXT_MUTED,
            (false, false) => theme::ACCENT,
        };
        let time = sets.first().and_then(|g| clock_time(g.started_at));
        if ui.is_rect_visible(rect) {
            list_header(
                ui.painter(),
                rect,
                &club_header_title(club, earlier, tally),
                time.as_deref(),
                color,
            );
        }
        let name = club.map_or("No club", club_name);
        let sets = match counted {
            1 => "1 set".to_owned(),
            n => format!("{n} sets"),
        };
        let hover = match (next, earlier) {
            (true, _) => format!("{name}: the next shot starts its section"),
            (false, true) => format!("{name}, an earlier session: {sets}"),
            (false, false) => format!("{name} since the last New session: {sets}"),
        };
        response.on_hover_text(hover);
    }

    pub(super) fn render_shot_cards(&mut self, ui: &mut egui::Ui) {
        let Some(i) = self.shown_index() else {
            self.card_clips.clear();
            self.render_empty_state(
                ui,
                "The latest shot's numbers appear here as soon as it lands.",
            );
            return;
        };

        let imperial = self.units() == UnitSystem::Imperial;
        let following = self.following();
        let Some(shot) = self.display_row(i) else {
            return;
        };
        let now = ui.ctx().input(|inp| inp.time);
        // The glow is for the latest shot, which is on show while it lasts
        // unless an older shot is picked in the meantime.
        let pulse = if following {
            self.shot_focus.strength(now)
        } else {
            0.0
        };
        if pulse > 0.0 {
            ui.ctx().request_repaint();
        }
        let handed = shot.handed.or(self.handed).unwrap_or(Handedness::Right);
        let (headline, groups) = card_metrics(&shot, imperial, handed);
        let counts: Vec<usize> = groups.iter().map(|(_, m)| m.len()).collect();
        let height = ui.available_height();
        // The range bars over the view's members around the shown shot,
        // after the device filter. The view none tracks only the last shot:
        // no bars and no device filter.
        let view = self.groups.mode;
        let show_bars = shows_bars(view);
        let key = self.refresh_members(i);
        let set = bar_group(&self.groups, Some(&self.shots[i]), self.selected_club);
        // A handful of ids, copied so the borrow of `self` ends here.
        let devices = self.devices().into_owned();
        // The sources of the cards beside the bars, in cycle order: no
        // cameras while the settings hide the camera cards.
        let face = show_bars && self.shots.iter().any(impact::has_impact);
        let cameras = if self.settings.shot_cards_enabled() {
            self.connected_cameras()
        } else {
            Vec::new()
        };
        let sources = card_row::sources(cameras, face);
        let (face_panel, bars, title) = {
            let members = self.view_members.peek(&key).map_or(&[][..], Vec::as_slice);
            // With every device shown, two or more split the bands by device.
            let split: &[String] = if self.device_filter.is_none() {
                &devices
            } else {
                &[]
            };
            // Face impact is a source while any shot in the history has it,
            // over the bars' members.
            let face_panel = face.then(|| {
                let several = devices.len() >= 2;
                let filter = self.device_filter.as_deref();
                impact::FacePanel::new(
                    &self.shots,
                    members,
                    i,
                    set.and_then(|g| g.club).or(self.selected_club),
                    self.handed,
                    |s| chart::shot_color(&devices, filter, &s.actor),
                    |s| {
                        several.then(|| {
                            chart::device_label(&s.actor, self.actor_name(&s.actor)).to_owned()
                        })
                    },
                )
            });
            (
                face_panel,
                bars_for(&self.shots, members, Some(i), split, imperial),
                bars_title(
                    view,
                    set,
                    self.groups.session(),
                    &self.shots,
                    members,
                    self.filter().narrows(),
                ),
            )
        };
        // Legend chips for the per-device sub-bands.
        let chips: Vec<(String, egui::Color32)> = match (
            device_options_with(&devices, &self.actors, &self.remote_names),
            &self.device_filter,
        ) {
            (Some(labels), None) => labels
                .into_iter()
                .skip(1)
                .zip(&devices)
                .map(|(label, id)| (label, chart::device_color(&devices, id)))
                .collect(),
            _ => Vec::new(),
        };
        let bars_note = if self.groups.is_empty() {
            "Sets start with your first shot"
        } else {
            "The next shot starts a new set"
        };

        // Room for the glow on the outermost cards.
        let inset = f32::from(GLOW_WIDTH);
        let lay_out = |width: f32| {
            let place = |width: f32| {
                legend_place(
                    ui.painter(),
                    width,
                    title.as_ref().map(|(t, _)| t.as_str()),
                    &chips,
                )
            };
            let size = egui::vec2(width, height - 2.0 * inset - CARDS_FOOT);
            let at = |legend: Legend| {
                cards_layout(
                    size,
                    headline.len(),
                    &counts,
                    show_bars.then_some(legend),
                    sources.len(),
                )
            };
            settle_legend(place, at)
        };
        let mut layout = lay_out(ui.available_width() - 2.0 * inset);
        // Too short for everything: a solid scroll bar, always shown, with
        // the cards laid out beside it.
        let scroll = egui::style::ScrollStyle::solid();
        if layout.scrolls {
            let bar = scroll.bar_width + scroll.bar_inner_margin + scroll.bar_outer_margin;
            layout = lay_out(ui.available_width() - 2.0 * inset - bar);
        }
        let visibility = if layout.scrolls {
            egui::scroll_area::ScrollBarVisibility::AlwaysVisible
        } else {
            egui::scroll_area::ScrollBarVisibility::AlwaysHidden
        };
        // What the cards show; the row exists only with the bars.
        let shown = if show_bars {
            self.card_row.update(&sources, layout.cards.len());
            self.card_row.shown.clone()
        } else {
            Vec::new()
        };
        // Under a replay of this shot the row is hidden: no clips.
        let replaying = self
            .replay
            .as_ref()
            .is_some_and(|r| r.shot_id() == self.shots[i].shot_id);
        let card_cameras: Vec<&str> = shown
            .iter()
            .filter(|_| !replaying)
            .filter_map(|s| match s {
                CardSource::Camera(id) => Some(id.as_str()),
                _ => None,
            })
            .collect();
        self.sync_card_clips(i, &card_cameras);
        let title_size = layout
            .headline
            .first()
            .map_or(12.0, |r| CardLook::headline(r.size()).label);

        let mut header = HeaderClick::default();
        let mut picked_shot = None;
        let mut cycled = None;
        ui.scope(|ui| {
            ui.spacing_mut().scroll = scroll;
            egui::ScrollArea::vertical()
                .id_salt("shot_cards")
                .auto_shrink(false)
                .scroll_bar_visibility(visibility)
                .show(ui, |ui| {
                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(
                            ui.available_width(),
                            layout.height + 2.0 * inset + CARDS_FOOT,
                        ),
                        egui::Sense::hover(),
                    );
                    let origin = (rect.min + egui::vec2(inset, inset)).to_vec2();

                    header = ui
                        .scope_builder(
                            egui::UiBuilder::new().max_rect(layout.header.translate(origin)),
                            |ui| self.render_shot_header(ui, i, following),
                        )
                        .inner;

                    if let Some(first) = layout.headline.first() {
                        let look = CardLook::headline(first.size()).fit(
                            ui.painter(),
                            &headline,
                            first.width(),
                        );
                        for (m, r) in headline.iter().zip(&layout.headline) {
                            card(ui, r.translate(origin), m, look, pulse);
                        }
                    }
                    if let Some(at) = layout.bars.map(|b| b.translate(origin)) {
                        range_bars(ui, &at, title.as_ref(), &bars, &chips, bars_note);
                        for (slot, (rect, source)) in layout.cards.iter().zip(&shown).enumerate() {
                            let rect = rect.translate(origin);
                            if rect.width() <= 0.0 {
                                continue;
                            }
                            let clicked = match source {
                                CardSource::Stats => {
                                    stats_card(ui.painter(), rect, &bars, &at.rows, title_size);
                                    row_card_clicked(ui, rect, slot)
                                }
                                CardSource::Camera(id) => {
                                    self.camera_card(ui, rect, id);
                                    row_card_clicked(ui, rect, slot)
                                }
                                CardSource::Face => match face_panel
                                    .as_ref()
                                    .and_then(|panel| impact::paint_face_panel(ui, rect, panel))
                                {
                                    Some(impact::FaceClick::Shot(shot)) => {
                                        picked_shot = Some(shot);
                                        false
                                    }
                                    Some(impact::FaceClick::Panel) => true,
                                    None => false,
                                },
                            };
                            if clicked {
                                cycled = Some(slot);
                            }
                        }
                        #[cfg(test)]
                        ui.ctx().data_mut(|d| {
                            let rects: Vec<egui::Rect> =
                                layout.cards.iter().map(|r| r.translate(origin)).collect();
                            d.insert_temp(egui::Id::new("row_cards"), rects);
                        });
                    }
                    let detail_cards = layout.groups.iter().flat_map(|(_, cards)| cards);
                    if let Some(first) = detail_cards.clone().next() {
                        let all = groups.iter().flat_map(|(_, m)| m);
                        let look =
                            CardLook::detail(first.size()).fit(ui.painter(), all, first.width());
                        for ((title, metrics), (label, cards)) in groups.iter().zip(&layout.groups)
                        {
                            group_label(
                                ui.painter(),
                                label.translate(origin),
                                title,
                                theme::ACCENT,
                            );
                            for (m, r) in metrics.iter().zip(cards) {
                                card(ui, r.translate(origin), m, look, pulse);
                            }
                        }
                    }
                    // In the portrait layout the charts follow the cards in
                    // their scroll.
                    if self.portrait {
                        let mut top = rect.bottom();
                        for chart in [Chart::Flight, Chart::Dispersion] {
                            top += CHART_GAP;
                            let at = egui::Rect::from_min_size(
                                egui::pos2(rect.left(), top),
                                egui::vec2(rect.width(), PORTRAIT_CHART_HEIGHT),
                            );
                            top = at.bottom();
                            ui.allocate_rect(at, egui::Sense::hover());
                            ui.scope_builder(egui::UiBuilder::new().max_rect(at), |ui| {
                                self.render_chart(ui, chart);
                            });
                        }
                    }
                });
        });
        self.decode_card_frame(ui.ctx());
        if header.back {
            self.shot_focus.follow();
        }
        if header.replay {
            self.open_replay(ui.ctx(), i);
        }
        if let Some(shot) = picked_shot {
            self.select_shot(shot);
            self.shot_focus.reveal();
        }
        if let Some(slot) = cycled {
            self.card_row.click(&sources, slot);
        }
    }

    /// The camera actors that are connected, in Devices card order.
    pub(super) fn connected_cameras(&self) -> Vec<String> {
        devices::device_card_order(&self.actors, &self.settings.actors)
            .into_iter()
            .filter(|(group, _)| *group == cards::CardGroup::Cameras)
            .flat_map(|(_, cards)| cards)
            .map(|card| card.id)
            .filter(|id| {
                self.actors
                    .get(id)
                    .is_some_and(|a| a.status == ActorStatus::Connected)
            })
            .collect()
    }

    pub(super) fn render_shots_table(&mut self, ui: &mut egui::Ui) {
        use egui_extras::{Column, TableBuilder};

        let imperial = self.units() == UnitSystem::Imperial;
        let show_device = multiple_devices(&self.shots);
        // The rows the filter passes, copied so the borrow of `self`
        // ends here.
        let rows = self.filtered_rows().to_vec();

        let Some(&last) = rows.last() else {
            self.render_empty_state(ui, "Each shot appears here as a row as soon as it lands.");
            return;
        };
        // The shot selected in the shot list is marked, and scrolled into
        // view once per jump (the list leaves the reveal to the Table).
        let focus = self.shown_index().filter(|_| !self.live_selected);
        let reveal = self.shot_focus.take_reveal();
        let reveal_row = focus
            .filter(|_| reveal)
            .and_then(|f| rows.iter().position(|&i| i == f));

        let pulse = self.shot_focus.strength(ui.ctx().input(|i| i.time));
        if pulse > 0.0 {
            ui.ctx().request_repaint();
        }
        let look = RowLook {
            latest: theme::HEADING.gamma_multiply(0.07 + 0.05 * pulse),
            stripe: ui.visuals().faint_bg_color,
            pulse,
        };
        // The Replay column shows while any row has clips.
        let replay_at = rows
            .iter()
            .any(|&i| !self.shots[i].video.is_empty())
            .then_some(REPLAY_AFTER + usize::from(show_device));
        let mut columns = table_columns(show_device, imperial);
        if let Some(at) = replay_at {
            columns.insert(at, replay_column());
        }
        let header = TableHeader::layout(ui.painter(), &columns);
        let mut replay = None;
        let row_height = ui.text_style_height(&egui::TextStyle::Body) + TABLE_ROW_PAD;

        // One scroll area for both directions, so both scroll bars sit on
        // the view's edges. The table lays out only the rows in view, and
        // the header is painted over the top of the view once they are in.
        // A reveal centers its row and leaves the bottom for its frame, so
        // the scroll holds.
        let mut area = egui::ScrollArea::both()
            .id_salt("shots_table")
            .auto_shrink(false)
            .stick_to_bottom(reveal_row.is_none());
        if let Some(at) = reveal_row {
            #[allow(clippy::cast_precision_loss)] // a row count
            let top = header.height + TABLE_ROW_GAP + at as f32 * (row_height + TABLE_ROW_GAP);
            area = area.vertical_scroll_offset(
                (top + (row_height - ui.available_height()) / 2.0).max(0.0),
            );
        }
        area.show(ui, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(TABLE_COLUMN_GAP, TABLE_ROW_GAP);
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
            let origin = ui.cursor().min;
            ui.add_space(header.height + TABLE_ROW_GAP);
            let mut widths = Vec::new();
            let mut table = TableBuilder::new(ui)
                // A new column set or unit system sizes the columns afresh.
                .id_salt(("shots_table", show_device, imperial))
                .vscroll(false)
                .striped(false)
                .resizable(false)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center));

            for width in &header.widths {
                table = table.column(Column::auto().at_least(width.max(TABLE_MIN_COLUMN)));
            }
            table.body(|body| {
                widths = body.widths().to_vec();
                body.rows(row_height, rows.len(), |mut row| {
                    let at = row.index();
                    let idx = rows[at];
                    let Some(converted) = self.display_row(idx) else {
                        return;
                    };
                    let shot = &self.shots[idx];
                    let device = self.device_name(shot, show_device);
                    let set = shot
                        .group
                        .as_deref()
                        .and_then(|g| self.groups.get(g))
                        .map(|g| g.number);
                    let mut cells = table_cells(&converted, device, set, idx == last);
                    if let Some(at) = replay_at {
                        cells.insert(at, TableCell::Replay(!shot.video.is_empty()));
                    }
                    if table_row(
                        &mut row,
                        &columns,
                        cells,
                        look.for_row(at, rows.len() - 1, Some(idx) == focus),
                    ) {
                        replay = Some(idx);
                    }
                });
            });
            header.paint(ui, origin, &columns, &widths);
        });
        if let Some(idx) = replay {
            self.open_replay(ui.ctx(), idx);
        }
    }
}
