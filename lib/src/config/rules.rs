//! The rules over a whole config (`impl FlighthookConfig`): actor ids and sections, golfer
//! lookup (`golfer_key`), shot type, validation, routing and the other cross-section queries.

use super::*;

impl FlighthookConfig {
    /// The golfer `id` means (the lookup rule): the golfer whose
    /// identifiers contain it. `None` when no golfer lists it; the id then
    /// stands on its own.
    #[must_use]
    pub fn golfer_of(&self, id: &str) -> Option<&GolferRecord> {
        self.golfers.iter().find(|g| g.is(id))
    }

    /// The key golfer id `id` is counted under: the own id of the golfer
    /// the lookup rule finds ([`golfer_of`](Self::golfer_of)), else `id`
    /// itself. `None` (no golfer) is its own key.
    #[must_use]
    pub fn golfer_key(&self, id: Option<&str>) -> Option<String> {
        let id = id?;
        Some(
            self.golfer_of(id)
                .and_then(GolferRecord::own_id)
                .unwrap_or(id)
                .to_owned(),
        )
    }

    /// The golfer up at start: the [`selected_golfer`](Self::selected_golfer)
    /// when a golfer has it, else the first golfer; its own id. `None`
    /// with no golfers.
    #[must_use]
    pub fn start_golfer(&self) -> Option<&GolferRecord> {
        self.selected_golfer
            .as_deref()
            .and_then(|id| self.golfer_of(id))
            .or_else(|| self.golfers.first())
            .filter(|g| g.own_id().is_some())
    }

    /// The first golfer id two golfers hold, in file order.
    #[must_use]
    pub fn duplicate_golfer_id(&self) -> Option<DuplicateGolferId> {
        let mut seen: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
        for golfer in &self.golfers {
            let mut own: std::collections::HashSet<&str> = std::collections::HashSet::new();
            for id in &golfer.identifiers {
                if !own.insert(id.as_str()) {
                    continue;
                }
                if let Some(first) = seen.insert(id.as_str(), golfer.name.as_str()) {
                    return Some(DuplicateGolferId {
                        id: id.clone(),
                        first: first.to_owned(),
                        second: golfer.name.clone(),
                    });
                }
            }
        }
        None
    }

    /// Remove every golfer id a golfer earlier in file order already holds
    /// (and repeats within one golfer); what was removed.
    pub fn dedupe_golfer_ids(&mut self) -> Vec<DuplicateGolferId> {
        let mut seen: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        let mut removed = Vec::new();
        for golfer in &mut self.golfers {
            let name = golfer.name.clone();
            let mut own: std::collections::HashSet<String> = std::collections::HashSet::new();
            golfer.identifiers.retain(|id| {
                if !own.insert(id.clone()) {
                    return false;
                }
                if let Some(first) = seen.get(id) {
                    removed.push(DuplicateGolferId {
                        id: id.clone(),
                        first: first.clone(),
                        second: name.clone(),
                    });
                    return false;
                }
                seen.insert(id.clone(), name.clone());
                true
            });
        }
        removed
    }

    /// Give every golfer without a `flighthook.*` identifier one, made of
    /// `generate()` (`flighthook.<generate()>`), first in its list. True
    /// when one was added.
    pub fn fill_golfer_ids(&mut self, mut generate: impl FnMut() -> String) -> bool {
        let mut changed = false;
        for golfer in &mut self.golfers {
            if golfer.own_id().is_none() {
                golfer
                    .identifiers
                    .insert(0, format!("{GOLFER_NAMESPACE}.{}", generate()));
                changed = true;
            }
        }
        changed
    }

    /// The first golfer error ([`GolferRecord::error`]), naming the golfer.
    #[must_use]
    pub fn golfer_error(&self) -> Option<String> {
        self.golfers
            .iter()
            .find_map(|g| g.error().map(|e| format!("golfer {}: {e}", g.name)))
    }

    /// Why the golfers can't be saved as they are: a golfer with a blank
    /// name, or an identifier that is not a golfer id (a lowercase
    /// namespace, a dot, then the id). `None` when every golfer is fine.
    #[must_use]
    pub fn golfer_identity_error(&self) -> Option<String> {
        self.golfers.iter().enumerate().find_map(|(i, g)| {
            if g.name.trim().is_empty() {
                return Some(format!("golfer {} has no name", i + 1));
            }
            g.identifiers
                .iter()
                .find(|id| id.parse::<flightrelay::GolferId>().is_err())
                .map(|id| format!("golfer {}: '{id}' is not a golfer id", g.name))
        })
    }

    /// Fold a `[player]` section into the golfers: the first golfer takes
    /// its sex and height where it has none; with no golfer and a `player`
    /// that holds anything, one golfer named "Golfer" is made with them
    /// (its own id is given by [`fill_golfer_ids`](Self::fill_golfer_ids)).
    pub fn take_player(&mut self, player: &PlayerSection) {
        if player.is_default() {
            return;
        }
        let sex = (!player.sex.is_default()).then_some(player.sex);
        match self.golfers.first_mut() {
            Some(first) => {
                first.sex = first.sex.or(sex);
                first.height = first.height.or(player.height);
            }
            None => self.golfers.push(GolferRecord {
                name: "Golfer".into(),
                sex,
                height: player.height,
                ..GolferRecord::default()
            }),
        }
    }

    /// Every launch monitor section as (type, index, `impact_lead_s`), types
    /// in the order of the sections' fields and indices sorted.
    #[must_use]
    pub fn launch_monitor_leads(&self) -> Vec<(&'static str, &str, Option<f64>)> {
        fn rows<'a, T>(
            out: &mut Vec<(&'static str, &'a str, Option<f64>)>,
            kind: &'static str,
            map: &'a std::collections::HashMap<String, T>,
            lead: impl Fn(&T) -> Option<f64>,
        ) {
            let mut rows: Vec<_> = map
                .iter()
                .map(|(i, s)| (kind, i.as_str(), lead(s)))
                .collect();
            rows.sort_by(|a, b| a.1.cmp(b.1));
            out.extend(rows);
        }
        let mut out = Vec::new();
        rows(&mut out, "mevo", &self.mevo, |s| s.impact_lead_s);
        rows(&mut out, "r10", &self.r10, |s| s.impact_lead_s);
        rows(&mut out, "square", &self.square, |s| s.impact_lead_s);
        rows(&mut out, "exputt", &self.exputt, |s| s.impact_lead_s);
        rows(&mut out, "mock_monitor", &self.mock_monitor, |s| {
            s.impact_lead_s
        });
        rows(
            &mut out,
            "openconnect_server",
            &self.openconnect_server,
            |s| s.impact_lead_s,
        );
        rows(
            &mut out,
            "pureplay_accessory",
            &self.pureplay_accessory,
            |s| s.impact_lead_s,
        );
        out
    }

    /// The actor IDs (e.g. `mevo.0`) of every section that is off
    /// (`enabled = false`), sorted. Such a section keeps its card and its
    /// settings and runs no actor.
    #[must_use]
    pub fn disabled_actors(&self) -> Vec<String> {
        fn off<T>(
            out: &mut Vec<String>,
            kind: &str,
            map: &std::collections::HashMap<String, T>,
            enabled: impl Fn(&T) -> bool,
        ) {
            out.extend(
                map.iter()
                    .filter(|(_, s)| !enabled(s))
                    .map(|(i, _)| format!("{kind}.{i}")),
            );
        }
        let mut out = Vec::new();
        off(&mut out, "mevo", &self.mevo, |s| s.enabled);
        off(&mut out, "r10", &self.r10, |s| s.enabled);
        off(&mut out, "square", &self.square, |s| s.enabled);
        off(&mut out, "exputt", &self.exputt, |s| s.enabled);
        off(&mut out, "trackball", &self.trackball, |s| s.enabled);
        off(&mut out, "mock_monitor", &self.mock_monitor, |s| s.enabled);
        off(
            &mut out,
            "openconnect_server",
            &self.openconnect_server,
            |s| s.enabled,
        );
        off(
            &mut out,
            "openconnect_client",
            &self.openconnect_client,
            |s| s.enabled,
        );
        off(&mut out, "pureplay", &self.pureplay, |s| s.enabled);
        off(
            &mut out,
            "pureplay_accessory",
            &self.pureplay_accessory,
            |s| s.enabled,
        );
        off(&mut out, "random_club", &self.random_club, |s| s.enabled);
        off(&mut out, "camera", &self.camera, |s| s.enabled);
        out.sort();
        out
    }

    /// Whether actor `actor_id` (e.g. `mevo.0`) may run: false only for a
    /// configured section that is off.
    #[must_use]
    pub fn actor_enabled(&self, actor_id: &str) -> bool {
        !self.disabled_actors().iter().any(|id| id == actor_id)
    }

    /// Whether the Shot Type picker has anything to steer: an enabled launch
    /// monitor card of a type that takes the shot type as a hint (Mevo,
    /// Trackball, Mock), or an enabled output card that routes chips or
    /// putts to a specific launch monitor (`chipping_monitor` or
    /// `putting_monitor` set; "any" and `full_monitor` alone do not count).
    #[must_use]
    pub fn shot_type_available(&self) -> bool {
        let routes = |chipping: &Option<String>, putting: &Option<String>| {
            chipping.is_some() || putting.is_some()
        };
        self.mevo.values().any(|s| s.enabled)
            || self.trackball.values().any(|s| s.enabled)
            || self.mock_monitor.values().any(|s| s.enabled)
            || self
                .openconnect_client
                .values()
                .any(|s| s.enabled && routes(&s.chipping_monitor, &s.putting_monitor))
            || self
                .pureplay
                .values()
                .any(|s| s.enabled && routes(&s.chipping_monitor, &s.putting_monitor))
    }

    /// Whether the card `actor_id` (e.g. `openconnect_client.0`) is an
    /// enabled card that sets the shot type itself, so a manual pick would
    /// be set back while it is connected: an Open Connect client with a
    /// club list, a Pureplay Connect output or a Pureplay Accessory.
    #[must_use]
    pub fn card_sets_shot_type(&self, actor_id: &str) -> bool {
        let Some((kind, index)) = actor_id.split_once('.') else {
            return false;
        };
        match kind {
            "openconnect_client" => self.openconnect_client.get(index).is_some_and(|s| {
                s.enabled && !(s.chipping_clubs.is_empty() && s.putting_clubs.is_empty())
            }),
            "pureplay" => self.pureplay.get(index).is_some_and(|s| s.enabled),
            "pureplay_accessory" => self
                .pureplay_accessory
                .get(index)
                .is_some_and(|s| s.enabled),
            _ => false,
        }
    }

    /// The `impact_lead_s` configured on launch monitor `actor_id` (e.g.
    /// `mevo.0`), or `None` when it is absent or there is no such section.
    #[must_use]
    pub fn configured_impact_lead_s(&self, actor_id: &str) -> Option<f64> {
        let (kind, index) = actor_id.split_once('.')?;
        self.launch_monitor_leads()
            .into_iter()
            .find(|(k, i, _)| *k == kind && *i == index)
            .and_then(|(_, _, lead)| lead)
    }

    /// The first camera section, by index, with a setting out of range
    /// ([`CameraSection::error`]), as a message.
    #[must_use]
    pub fn camera_error(&self) -> Option<String> {
        let mut cameras: Vec<_> = self.camera.iter().collect();
        cameras.sort_by(|a, b| a.0.cmp(b.0));
        cameras
            .into_iter()
            .find_map(|(index, s)| s.error(&format!("camera.{index}")))
    }

    /// The first camera section whose `latency_s` is out of range, as a
    /// message.
    #[must_use]
    pub fn latency_error(&self) -> Option<String> {
        let mut cameras: Vec<_> = self.camera.iter().collect();
        cameras.sort_by(|a, b| a.0.cmp(b.0));
        cameras
            .into_iter()
            .find_map(|(index, s)| latency_error(&format!("camera.{index}"), s.latency_s))
    }

    /// Every card that is an FRP actor, by card key (`mevo.0`), with its
    /// actor id (`None` until one is generated): the launch monitors, the
    /// outputs, the Pureplay Accessory cards and random club. Cameras and
    /// the web server are not actors.
    #[must_use]
    pub fn card_actors(&self) -> Vec<(String, Option<&str>)> {
        let mut out = Vec::new();
        macro_rules! cards {
            ($($field:ident),*) => {$(
                for (index, section) in &self.$field {
                    out.push((format!("{}.{index}", stringify!($field)), section.actor_id.as_deref()));
                }
            )*};
        }
        cards!(
            mevo,
            r10,
            square,
            exputt,
            trackball,
            mock_monitor,
            openconnect_server,
            openconnect_client,
            pureplay,
            pureplay_accessory,
            random_club
        );
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// The actor id of the card `key` (`mevo.0`), if it has one.
    #[must_use]
    pub fn actor_of_card(&self, key: &str) -> Option<String> {
        self.card_actors()
            .into_iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, id)| id.map(str::to_owned))
    }

    /// The key of the card whose actor id is `actor_id`, if one is.
    #[must_use]
    pub fn card_of_actor(&self, actor_id: &str) -> Option<String> {
        self.card_actors()
            .into_iter()
            .find(|(_, id)| *id == Some(actor_id))
            .map(|(k, _)| k)
    }

    /// Gives a new id from `generate` to every actor whose id an earlier
    /// actor already holds. The order is the flighthook actor, Auto-chip,
    /// the flight model, then the cards by type (as
    /// [`fill_actor_ids`](Self::fill_actor_ids) visits them) and index.
    /// Returns the keys (`mevo.1`, `frp`, `simulation`) of the actors that
    /// got a new id.
    pub fn renew_duplicate_actor_ids(
        &mut self,
        mut generate: impl FnMut() -> String,
    ) -> Vec<String> {
        let mut slots: Vec<(usize, String, String, &mut Option<String>)> = vec![
            (0, String::new(), "frp".into(), &mut self.frp.actor_id),
            (
                1,
                String::new(),
                "frp.auto_chip".into(),
                &mut self.frp.auto_chip_actor_id,
            ),
            (
                2,
                String::new(),
                "simulation".into(),
                &mut self.simulation.actor_id,
            ),
        ];
        let mut kind = 3;
        macro_rules! cards {
            ($($field:ident),*) => {$(
                for (index, section) in &mut self.$field {
                    slots.push((
                        kind,
                        index.clone(),
                        format!("{}.{index}", stringify!($field)),
                        &mut section.actor_id,
                    ));
                }
                kind += 1;
            )*};
        }
        cards!(
            mevo,
            r10,
            square,
            exputt,
            trackball,
            mock_monitor,
            openconnect_server,
            openconnect_client,
            pureplay,
            pureplay_accessory,
            random_club
        );
        let _ = kind;
        slots.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| crate::index_order(&a.1, &b.1)));
        let mut seen = std::collections::HashSet::new();
        let mut renewed = Vec::new();
        for (_, _, key, slot) in slots {
            if let Some(id) = slot.as_ref()
                && !seen.insert(id.clone())
            {
                let fresh = generate();
                seen.insert(fresh.clone());
                *slot = Some(fresh);
                renewed.push(key);
            }
        }
        renewed
    }

    /// Gives every actor that has no id one from `generate`: the
    /// flighthook actor, Auto-chip, the flight model (the Simulation card)
    /// and every card [`card_actors`](Self::card_actors) lists. Returns
    /// whether it generated any.
    pub fn fill_actor_ids(&mut self, mut generate: impl FnMut() -> String) -> bool {
        let mut changed = false;
        let mut fill = |slot: &mut Option<String>| {
            if slot.is_none() {
                *slot = Some(generate());
                changed = true;
            }
        };
        fill(&mut self.frp.actor_id);
        fill(&mut self.frp.auto_chip_actor_id);
        fill(&mut self.simulation.actor_id);
        macro_rules! cards {
            ($($field:ident),*) => {$(
                let mut indexes: Vec<String> = self.$field.keys().cloned().collect();
                indexes.sort_by(|a, b| crate::index_order(a, b));
                for index in indexes {
                    if let Some(section) = self.$field.get_mut(&index) {
                        fill(&mut section.actor_id);
                    }
                }
            )*};
        }
        cards!(
            mevo,
            r10,
            square,
            exputt,
            trackball,
            mock_monitor,
            openconnect_server,
            openconnect_client,
            pureplay,
            pureplay_accessory,
            random_club
        );
        changed
    }

    /// Every actor id `self` lacks that `before` holds for the same actor
    /// (the same card key; flighthook's own actors): a save from a form
    /// that does not carry the ids keeps them.
    pub fn keep_actor_ids_from(&mut self, before: &FlighthookConfig) {
        let keep = |slot: &mut Option<String>, old: &Option<String>| {
            if slot.is_none() {
                slot.clone_from(old);
            }
        };
        keep(&mut self.frp.actor_id, &before.frp.actor_id);
        keep(
            &mut self.frp.auto_chip_actor_id,
            &before.frp.auto_chip_actor_id,
        );
        keep(&mut self.simulation.actor_id, &before.simulation.actor_id);
        macro_rules! cards {
            ($($field:ident),*) => {$(
                for (index, section) in &mut self.$field {
                    if let Some(old) = before.$field.get(index) {
                        keep(&mut section.actor_id, &old.actor_id);
                    }
                }
            )*};
        }
        cards!(
            mevo,
            r10,
            square,
            exputt,
            trackball,
            mock_monitor,
            openconnect_server,
            openconnect_client,
            pureplay,
            pureplay_accessory,
            random_club
        );
    }

    /// Output routing that names a card by its key (`mevo.0`) names it by
    /// its actor id instead. A key that names no card with an id is kept as
    /// it is (it routes nothing). Returns whether it changed any.
    pub fn route_by_actor_id(&mut self) -> bool {
        let ids: std::collections::HashMap<String, String> = self
            .card_actors()
            .into_iter()
            .filter_map(|(k, id)| id.map(|id| (k, id.to_owned())))
            .collect();
        let mut changed = false;
        let mut convert = |slot: &mut Option<String>| {
            if let Some(id) = slot.as_deref().and_then(|key| ids.get(key)) {
                *slot = Some(id.clone());
                changed = true;
            }
        };
        for s in self.openconnect_client.values_mut() {
            convert(&mut s.full_monitor);
            convert(&mut s.chipping_monitor);
            convert(&mut s.putting_monitor);
        }
        for s in self.pureplay.values_mut() {
            convert(&mut s.full_monitor);
            convert(&mut s.chipping_monitor);
            convert(&mut s.putting_monitor);
        }
        changed
    }

    /// The outputs routed to a Pureplay Accessory card, as actor IDs, sorted: such a
    /// route passes nothing ([`SIMULATOR_ROUTE_ERROR`]).
    #[must_use]
    pub fn simulator_routes(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .openconnect_client
            .iter()
            .filter(|(_, s)| s.routes_to_simulator(self))
            .map(|(i, _)| format!("openconnect_client.{i}"))
            .chain(
                self.pureplay
                    .iter()
                    .filter(|(_, s)| s.routes_to_simulator(self))
                    .map(|(i, _)| format!("pureplay.{i}")),
            )
            .collect();
        out.sort();
        out
    }

    /// The first launch monitor section whose `impact_lead_s` is out of
    /// range, as a message.
    #[must_use]
    pub fn impact_lead_error(&self) -> Option<String> {
        self.launch_monitor_leads()
            .into_iter()
            .find_map(|(kind, index, lead)| impact_lead_error(&format!("{kind}.{index}"), lead))
    }

    /// The first card whose device driver refuses its settings
    /// ([`DeviceSection::driver_error`]), as `"<id>: <field>: <reason>"`,
    /// e.g. `mock_monitor.0: shot_interval_s: must be 0 to 86400`.
    #[must_use]
    pub fn device_error(&self) -> Option<String> {
        first_driver_error::<MevoSection>(self, None)
            .or_else(|| first_driver_error::<MockMonitorSection>(self, None))
            .or_else(|| first_driver_error::<R10Section>(self, None))
            .or_else(|| first_driver_error::<SquareSection>(self, None))
            .or_else(|| first_driver_error::<ExPuttSection>(self, None))
            .or_else(|| first_driver_error::<TrackballSection>(self, None))
    }

    /// [`device_error`](Self::device_error) for saving this config over
    /// `saved`: only a card whose driver settings the save changes into
    /// refused ones counts
    /// ([`DeviceSection::changed_driver_error`]).
    #[must_use]
    pub fn changed_device_error(&self, saved: &FlighthookConfig) -> Option<String> {
        first_driver_error::<MevoSection>(self, Some(saved))
            .or_else(|| first_driver_error::<MockMonitorSection>(self, Some(saved)))
            .or_else(|| first_driver_error::<R10Section>(self, Some(saved)))
            .or_else(|| first_driver_error::<SquareSection>(self, Some(saved)))
            .or_else(|| first_driver_error::<ExPuttSection>(self, Some(saved)))
            .or_else(|| first_driver_error::<TrackballSection>(self, Some(saved)))
    }

    /// Returns true if any user-configured actors (devices or integrations)
    /// exist. Webservers are infrastructure and don't count.
    pub fn has_user_actors(&self) -> bool {
        !self.mevo.is_empty()
            || !self.r10.is_empty()
            || !self.square.is_empty()
            || !self.exputt.is_empty()
            || !self.trackball.is_empty()
            || !self.mock_monitor.is_empty()
            || !self.openconnect_server.is_empty()
            || !self.openconnect_client.is_empty()
            || !self.pureplay.is_empty()
            || !self.pureplay_accessory.is_empty()
            || !self.random_club.is_empty()
            || !self.camera.is_empty()
    }
}
