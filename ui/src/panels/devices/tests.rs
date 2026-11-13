//! Tests of the Settings page: cards, sides, saves, locks, sections and layout.

use super::*;
use crate::panels::settings::{GeneralCard, SettingsForm};
use crate::panels::{Tab, next_alert_change, settings_alert};
use crate::types::{
    ActorStatus, ConnectionState, FlighthookConfig, RECONNECT_ALERT_MS, UnitSystem,
};
use egui::{Color32, Event, Key, Modifiers, PointerButton, Pos2, Rect, pos2, vec2};

/// The Software (+) menu entry labeled `label`.
fn output_preset(label: &str) -> &'static cards::AddPreset {
    cards::add_options(CardGroup::Software)
        .iter()
        .find(|p| p.label == label)
        .unwrap_or_else(|| panic!("no {label} preset"))
}

/// A saved config with a Mevo and two Open Connect clients: GSPro
/// (`openconnect_client.0`) and Muni Golf (`openconnect_client.1`).
fn config() -> FlighthookConfig {
    let mut form = SettingsForm::default();
    form.load_from(&FlighthookConfig::default());
    let _ = form.add_actor("mevo");
    let _ = form.add_preset(output_preset("GSPro (Open Connect)"));
    let _ = form.add_preset(output_preset("Muni Golf (Open Connect)"));
    form.to_request()
}

/// An app on the Devices tab with `config()` loaded and its actors
/// running.
fn app() -> FlighthookApp {
    let mut app = FlighthookApp::with_pending(net::new_pending());
    app.settings.load_from(&config());
    for a in &app.settings.actors {
        let mut status = ActorStatusResponse::new(a.name(), ActorStatus::Connected);
        status.telemetry = HashMap::from([("ready".to_string(), "true".to_string())]);
        app.actors.insert(a.global_id(), status);
    }
    app
}

fn form_index(app: &FlighthookApp, id: &str) -> usize {
    app.settings
        .actors
        .iter()
        .position(|a| a.global_id() == id)
        .unwrap()
}

/// Edit the name of actor `id`, as its name field does.
fn edit_name(app: &mut FlighthookApp, id: &str) {
    let idx = form_index(app, id);
    match &mut app.settings.actors[idx] {
        ActorFormEntry::Device(d) => d.name.push_str(" edited"),
        ActorFormEntry::Integration(i) => i.name.push_str(" edited"),
    }
    app.settings.actors[idx].set_dirty(true);
}

/// Draws the top bar and Settings, 1600×1600 unless sized: tall
/// enough for every card of the tests' configs to be on screen. Every
/// section's cards are drawn, one after another, unless the screen is
/// [`Screen::one_section`].
struct Screen(egui::Context, egui::Vec2, bool);

impl Screen {
    fn new() -> Self {
        Self::sized(1600.0, 2400.0)
    }

    fn sized(width: f32, height: f32) -> Self {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        Self(ctx, vec2(width, height), true)
    }

    /// Settings as the app draws it: only the selected section.
    fn one_section(width: f32, height: f32) -> Self {
        let mut screen = Self::sized(width, height);
        screen.2 = false;
        screen
    }

    /// One pass with `events`. Returns the marked controls.
    fn frame(&self, app: &mut FlighthookApp, events: Vec<Event>) -> Marks {
        self.painted(app, events).0
    }

    /// One pass with `events`. Returns the marked controls and every
    /// shape painted.
    fn painted(&self, app: &mut FlighthookApp, events: Vec<Event>) -> (Marks, Vec<egui::Shape>) {
        self.0.data_mut(|d| d.remove_temp::<Marks>(egui::Id::NULL));
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.1)),
            events,
            ..Default::default()
        };
        app.all_settings_sections = self.2;
        let output = self.0.run(input, |ctx| {
            app.render_header_in_test(ctx);
            egui::CentralPanel::default().show(ctx, |ui| {
                let ctx = ui.ctx().clone();
                app.render_devices_panel(&ctx, ui);
            });
            if app.confirm_remove.is_some() {
                app.render_remove_confirm(ctx);
            }
        });
        let marks = self
            .0
            .data(|d| d.get_temp::<Marks>(egui::Id::NULL))
            .unwrap_or_default();
        let mut shapes = Vec::new();
        for clipped in output.shapes {
            flatten(clipped.shape, &mut shapes);
        }
        (marks, shapes)
    }

    /// One pass. Returns every text painted.
    fn texts(&self, app: &mut FlighthookApp) -> Vec<String> {
        self.painted(app, Vec::new())
            .1
            .into_iter()
            .filter_map(|shape| match shape {
                egui::Shape::Text(t) => Some(t.galley.text().to_owned()),
                _ => None,
            })
            .collect()
    }

    /// Click at `at`, then draw once more. Returns the marked controls.
    fn click(&self, app: &mut FlighthookApp, at: Pos2) -> Marks {
        let button = |pressed| Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        self.frame(app, vec![Event::PointerMoved(at)]);
        self.frame(app, vec![button(true)]);
        self.frame(app, vec![button(false)]);
        self.frame(app, Vec::new())
    }

    /// The card bodies of the last pass.
    fn bodies(&self) -> Bodies {
        self.0
            .data(|d| d.get_temp::<Bodies>(bodies_id()))
            .unwrap_or_default()
    }

    /// Click the middle of control `name`.
    fn click_mark(&self, app: &mut FlighthookApp, marks: &Marks, name: &str) -> Marks {
        let (rect, _) = marks[name];
        self.click(app, rect.center())
    }
}

fn enabled(marks: &Marks, name: &str) -> bool {
    marks
        .get(name)
        .unwrap_or_else(|| panic!("{name} not drawn"))
        .1
}

/// `shape` and the shapes inside it, into `out`.
fn flatten(shape: egui::Shape, out: &mut Vec<egui::Shape>) {
    match shape {
        egui::Shape::Vec(shapes) => {
            for s in shapes {
                flatten(s, out);
            }
        }
        other => out.push(other),
    }
}

/// Actor `id`, Connected since it started, then `status` at `at_ms`.
fn lose(app: &mut FlighthookApp, id: &str, status: ActorStatus, at_ms: u64) {
    let actor = app.actors.get_mut(id).unwrap();
    actor.observe(ActorStatus::Connected, at_ms);
    actor.observe(status, at_ms);
}

#[test]
fn the_alert_dot_shows_only_while_a_card_is_red() {
    let mut app = app();
    app.actors.insert(
        "openconnect_server.0".into(),
        ActorStatusResponse::new("Range Bridge", ActorStatus::Connected),
    );
    assert!(!settings_alert(&app.actors, 0));

    // Not Connected since it started: Connecting, no dot.
    for id in ["mevo.0", "openconnect_client.0", "openconnect_server.0"] {
        for status in [
            ActorStatus::Disconnected,
            ActorStatus::Reconnecting,
            ActorStatus::Starting,
        ] {
            let actor = app.actors.get_mut(id).unwrap();
            actor.observe(ActorStatus::Starting, 0);
            actor.observe(status, 1);
            assert_eq!(actor.state(), ConnectionState::Connecting);
            assert!(!settings_alert(&app.actors, 1), "{id} {status:?}");
        }
        app.actors
            .get_mut(id)
            .unwrap()
            .observe(ActorStatus::Connected, 2);
    }
    // An `error` alone does not light it.
    app.actors
        .get_mut("openconnect_client.0")
        .unwrap()
        .telemetry
        .insert("error".into(), "Open Connect Server: refused ...".into());
    assert!(!settings_alert(&app.actors, 2));

    // A lost connection lights it for ten minutes, whatever the actor:
    // a launch monitor, an output, a server whose peer left.
    for (id, status) in [
        ("mevo.0", ActorStatus::Reconnecting),
        ("openconnect_client.1", ActorStatus::Reconnecting),
        ("openconnect_server.0", ActorStatus::Disconnected),
    ] {
        let at = 1_000;
        lose(&mut app, id, status, at);
        assert!(settings_alert(&app.actors, at), "{id}");
        assert!(
            settings_alert(&app.actors, at + RECONNECT_ALERT_MS - 1),
            "{id}"
        );
        assert!(
            !settings_alert(&app.actors, at + RECONNECT_ALERT_MS),
            "{id}"
        );
        assert_eq!(
            app.actors[id].state(),
            ConnectionState::Reconnecting,
            "{id}"
        );
        app.actors
            .get_mut(id)
            .unwrap()
            .observe(ActorStatus::Connected, at + 1);
        assert!(!settings_alert(&app.actors, at + 1), "{id}");
    }
}

#[test]
fn the_next_alert_change_is_when_the_first_red_card_turns_yellow() {
    let mut app = app();
    assert_eq!(next_alert_change(&app.actors, 0), None);
    lose(&mut app, "mevo.0", ActorStatus::Reconnecting, 0);
    lose(
        &mut app,
        "openconnect_client.0",
        ActorStatus::Reconnecting,
        60_000,
    );
    assert_eq!(
        next_alert_change(&app.actors, 1_000),
        Some(RECONNECT_ALERT_MS - 1_000)
    );
    assert_eq!(
        next_alert_change(&app.actors, RECONNECT_ALERT_MS),
        Some(60_000)
    );
    assert_eq!(
        next_alert_change(&app.actors, RECONNECT_ALERT_MS + 60_000),
        None
    );
}

/// The text and color of card `id`'s badge, and the color of the card
/// stripe left of it.
fn badge_look(marks: &Marks, shapes: &[egui::Shape], id: &str) -> (String, Color32, Color32) {
    let (badge, _) = marks[&format!("badge:{id}")];
    let (text, color) = shapes
        .iter()
        .find_map(|s| match s {
            egui::Shape::Text(t) if badge.contains_rect(t.visual_bounding_rect()) => {
                Some((t.galley.text().to_string(), t.fallback_color))
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("no badge text for {id}"));
    let stripe = shapes
        .iter()
        .filter_map(|s| match s {
            egui::Shape::Rect(r)
                if (r.rect.width() - 3.0).abs() < 0.1
                    && r.rect.y_range().contains(badge.center().y)
                    && r.rect.right() < badge.left() =>
            {
                Some((r.rect.left(), r.fill))
            }
            _ => None,
        })
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .unwrap_or_else(|| panic!("no stripe for {id}"))
        .1;
    (text, color, stripe)
}

#[test]
fn a_cards_badge_and_stripe_show_its_state() {
    let mut app = app();
    let lost_at = 2 * 60 * 1000;
    app.actors
        .get_mut("openconnect_client.0")
        .unwrap()
        .observe(ActorStatus::Starting, 0);
    lose(
        &mut app,
        "openconnect_client.1",
        ActorStatus::Reconnecting,
        lost_at,
    );
    let screen = Screen::new();
    let look = |app: &mut FlighthookApp, now: u64, id: &str| {
        app.now_ms = now;
        let (marks, shapes) = screen.painted(app, Vec::new());
        badge_look(&marks, &shapes, id)
    };
    let now = lost_at + 1_000;
    for (id, label, color) in [
        ("mevo.0", "CONNECTED", theme::STATUS_OK),
        ("openconnect_client.0", "CONNECTING", theme::STATUS_PENDING),
        ("openconnect_client.1", "RECONNECTING", theme::STATUS_DOWN),
    ] {
        assert_eq!(
            look(&mut app, now, id),
            (label.into(), color, color),
            "{id}"
        );
    }
    // Yellow once the alert is over.
    let later = lost_at + RECONNECT_ALERT_MS;
    assert_eq!(
        look(&mut app, later, "openconnect_client.1"),
        (
            "RECONNECTING".into(),
            theme::STATUS_PENDING,
            theme::STATUS_PENDING
        )
    );
}

/// [`app`] with `openconnect_client.1` (Muni Golf) turned off in the
/// saved settings, its actor `status` as the UI holds it.
fn app_with_muni_off(status: ActorStatusResponse) -> FlighthookApp {
    let mut app = app();
    let mut config = app.settings.to_request();
    config.openconnect_client.get_mut("1").unwrap().enabled = false;
    settings_reply(&mut app, &config);
    app.actors.insert("openconnect_client.1".into(), status);
    app
}

/// A card that is off reads DISABLED in gray on its badge and stripe,
/// says "Disabled" in its body instead of the spinner, has Reconnect
/// disabled, and is never an alert: no red, no tab dot, no error.
#[test]
fn a_card_turned_off_is_disabled_in_gray_and_never_an_alert() {
    let id = "openconnect_client.1";
    let screen = Screen::new();
    // The server's Disabled entry, or (before it arrives) the actor as
    // last seen: red after losing its connection. Both read Disabled.
    let mut lost = ActorStatusResponse::new("Muni Golf", ActorStatus::Connected);
    lost.observe(ActorStatus::Reconnecting, 1_000);
    for status in [ActorStatusResponse::disabled("Muni Golf"), lost] {
        let mut app = app_with_muni_off(status);
        app.now_ms = 2_000;
        let (marks, shapes) = screen.painted(&mut app, Vec::new());
        assert_eq!(
            badge_look(&marks, &shapes, id),
            ("DISABLED".into(), theme::STATUS_OFF, theme::STATUS_OFF)
        );
        assert_ne!(theme::STATUS_OFF, theme::STATUS_DOWN);
        assert_ne!(theme::STATUS_OFF, theme::STATUS_PENDING);
        assert!(marks.contains_key(&format!("disabled:{id}")));
        assert!(!marks.contains_key(&format!("spinner:{id}")));
        assert!(!enabled(&marks, &format!("reconnect:{id}")));
        assert!(enabled(&marks, "reconnect:openconnect_client.0"));
        assert!(enabled(&marks, &format!("enabled:{id}")));
        assert!(enabled(&marks, "enabled:openconnect_client.0"));
        assert!(enabled(&marks, "enabled:mevo.0"));
        assert!(!marks.contains_key("enabled:webserver.0"), "no switch");
        assert!(!marks.contains_key(&format!("error:{id}")));
    }
    let app = app_with_muni_off(ActorStatusResponse::disabled("Muni Golf"));
    assert!(!settings_alert(&app.actors, 2_000));
    assert!(!app.actors[id].telemetry.contains_key("error"));
    // Its settings stay editable.
    let mut app = app;
    let marks = screen.frame(&mut app, Vec::new());
    assert!(enabled(&marks, &format!("enabled:{id}")));
    assert!(enabled(&marks, &format!("remove:{id}")));
}

/// The switch saves at once, scoped to its card and carrying only the
/// new `enabled`; the card reads Disabled while the save is out, and
/// every switch waits for it.
#[test]
fn the_switch_saves_its_card_alone_at_once() {
    let id = "openconnect_client.1";
    let screen = Screen::new();
    let mut app = app();
    let before = app.settings.to_request();
    let marks = screen.frame(&mut app, Vec::new());
    let marks = screen.click_mark(&mut app, &marks, &format!("enabled:{id}"));
    let (target, posted) = app.settings.posted().expect("a save is out");
    assert_eq!(*target, SaveTarget::Actor(id.into()));
    let mut want = before.clone();
    want.openconnect_client.get_mut("1").unwrap().enabled = false;
    assert_eq!(*posted, want, "only its enabled changes");
    assert!(!app.settings.dirty(), "no Save or Cancel");
    assert!(!marks.contains_key(&format!("save:{id}")));
    let (marks, shapes) = screen.painted(&mut app, Vec::new());
    assert_eq!(badge_look(&marks, &shapes, id).0, "DISABLED");
    for other in [id, "openconnect_client.0", "mevo.0"] {
        assert!(!enabled(&marks, &format!("enabled:{other}")), "{other}");
    }

    reply(&mut app, Ok(()));
    assert!(!app.settings.saving());
    assert!(app.settings.turned_off(id));
    let saved = app.settings.to_request();
    assert_eq!(saved, want);
    // The server stops it: the UI shows it Disabled, not gone.
    app.handle_ui_message(crate::socket::feed::local_msg(
        "system",
        flighthook::UiLocalEvent::ConfigOutcome {
            request_id: None,
            restarted: Vec::new(),
            stopped: vec![id.into()],
            started: Vec::new(),
            config_generation: 0,
        },
    ));
    assert!(app.actors[id].is_disabled());
    assert_eq!(app.actors[id].name, "Muni Golf");

    // On again: the same, the other way.
    let marks = screen.frame(&mut app, Vec::new());
    screen.click_mark(&mut app, &marks, &format!("enabled:{id}"));
    let (_, posted) = app.settings.posted().expect("a save is out");
    assert_eq!(*posted, before);
    reply(&mut app, Ok(()));
    assert!(!app.settings.turned_off(id));
    // The new instance's first status ends Disabled.
    app.handle_ui_message(crate::socket::feed::local_msg(
        id,
        flighthook::UiLocalEvent::CardStatus {
            status: ActorStatus::Starting,
            telemetry: HashMap::new(),
        },
    ));
    assert!(!app.actors[id].is_disabled());
}

/// The switch is locked while another card has unsaved edits, and
/// while its own has, like the card's other header controls.
#[test]
fn the_switch_is_locked_while_a_card_is_dirty() {
    let screen = Screen::new();
    let mut app = app();
    edit_name(&mut app, "mevo.0");
    let marks = screen.frame(&mut app, Vec::new());
    for id in ["mevo.0", "openconnect_client.0", "openconnect_client.1"] {
        assert!(!enabled(&marks, &format!("enabled:{id}")), "{id}");
    }
    let _ = screen.click_mark(&mut app, &marks, "enabled:openconnect_client.1");
    assert!(app.settings.posted().is_none(), "nothing saved");
    assert!(!app.settings.turned_off("openconnect_client.1"));
}

#[test]
fn device_cards_cover_configured_and_running_actors_in_card_order() {
    let mut app = app();
    // Running, not in the form (a config edited by hand since the load).
    app.actors.insert(
        "mock_monitor.0".into(),
        ActorStatusResponse::new("Mock", ActorStatus::Connected),
    );
    // In the form, not running yet.
    app.actors.remove("openconnect_client.1");
    let order = device_card_order(&app.actors, &app.settings.actors);
    let ids = |group: CardGroup| -> Vec<(String, bool)> {
        order
            .iter()
            .find(|(g, _)| *g == group)
            .unwrap()
            .1
            .iter()
            .map(|c| (c.id.clone(), c.form.is_some()))
            .collect()
    };
    assert_eq!(ids(CardGroup::General), [("webserver.0".into(), true)]);
    assert_eq!(
        ids(CardGroup::LaunchMonitors),
        [("mevo.0".into(), true), ("mock_monitor.0".into(), false)]
    );
    assert_eq!(
        ids(CardGroup::Software),
        [
            ("openconnect_client.0".into(), true),
            ("openconnect_client.1".into(), true),
        ]
    );
    for (_, cards) in &order {
        for card in cards {
            if let Some(idx) = card.form {
                assert_eq!(app.settings.actors[idx].global_id(), card.id);
            }
        }
    }
}

#[test]
fn each_page_shows_its_telemetry_beside_its_settings() {
    let screen = Screen::new();
    let mut app = app();
    let marks = screen.frame(&mut app, Vec::new());
    let bodies = screen.bodies();
    for id in ["mevo.0", "openconnect_client.0", "openconnect_client.1"] {
        let (telemetry, _, _) = bodies[&format!("{id}.telemetry")];
        let (settings, _, _) = bodies[id];
        assert!(telemetry.right() < settings.left(), "{id} side by side");
        assert!((telemetry.top() - settings.top()).abs() < 0.5, "{id}");
        assert!(
            (telemetry.height() - TELEMETRY_HEIGHT).abs() < 0.5,
            "{id} fixed height"
        );
        assert!(enabled(&marks, &format!("remove:{id}")), "{id}");
        assert!(enabled(&marks, &format!("badge:{id}")), "{id}");
    }
    assert!(!marks.contains_key("remove:webserver.0"));
    assert!(!app.settings.locked());
    // A longer telemetry scrolls inside its block: nothing moves.
    let before = marks["card:mevo.0"].0;
    let telemetry = &mut app.actors.get_mut("mevo.0").unwrap().telemetry;
    for i in 0..40 {
        telemetry.insert(format!("key_{i:02}"), "value".into());
    }
    let marks = screen.frame(&mut app, Vec::new());
    assert_eq!(marks["card:mevo.0"].0, before);
    let (telemetry, content, _) = screen.bodies()["mevo.0.telemetry"];
    assert!((telemetry.height() - TELEMETRY_HEIGHT).abs() < 0.5);
    assert!(content > TELEMETRY_HEIGHT, "it overflows and scrolls");
}

#[test]
fn unsaved_edits_swap_the_icons_for_save_and_cancel() {
    let screen = Screen::new();
    let mut app = app();
    let marks = screen.frame(&mut app, Vec::new());
    assert!(marks.contains_key("remove:openconnect_client.0"));
    assert!(marks.contains_key("reconnect:openconnect_client.0"));
    assert!(!marks.contains_key("save:openconnect_client.0"));

    edit_name(&mut app, "openconnect_client.0");
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!marks.contains_key("remove:openconnect_client.0"));
    assert!(!marks.contains_key("reconnect:openconnect_client.0"));
    assert!(enabled(&marks, "save:openconnect_client.0"));
    assert!(enabled(&marks, "cancel:openconnect_client.0"));
    assert!(marks.contains_key("badge:openconnect_client.0"));

    // An invalid form can't be saved, only canceled.
    let idx = form_index(&app, "openconnect_client.0");
    if let ActorFormEntry::Integration(i) = &mut app.settings.actors[idx] {
        i.address = "127.0.0.1:".into();
    }
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!enabled(&marks, "save:openconnect_client.0"));
    assert!(enabled(&marks, "cancel:openconnect_client.0"));

    // While a save is out, neither.
    if let ActorFormEntry::Integration(i) = &mut app.settings.actors[idx] {
        i.address = "127.0.0.1:921".into();
    }
    let _ = app
        .settings
        .begin_save(SaveTarget::Actor("openconnect_client.0".into()));
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!enabled(&marks, "save:openconnect_client.0"));
    assert!(!enabled(&marks, "cancel:openconnect_client.0"));
}

#[test]
fn cancel_puts_the_card_back_to_its_saved_settings() {
    let screen = Screen::new();
    let mut app = app();
    let saved = app.settings.actors[form_index(&app, "openconnect_client.1")]
        .name()
        .to_string();
    edit_name(&mut app, "openconnect_client.1");
    let marks = screen.frame(&mut app, Vec::new());
    assert!(app.settings.dirty());

    let marks = screen.click_mark(&mut app, &marks, "cancel:openconnect_client.1");
    let entry = &app.settings.actors[form_index(&app, "openconnect_client.1")];
    assert_eq!(entry.name(), saved);
    assert!(!entry.dirty());
    assert!(!app.settings.dirty());
    assert!(!app.settings.locked());
    // Back to the icons.
    assert!(enabled(&marks, "remove:openconnect_client.1"));
    assert!(enabled(&marks, "nav:shots"));
}

#[test]
fn unsaved_edits_lock_the_rest_of_the_tab() {
    let screen = Screen::new();
    let mut app = app();

    // Unlocked, a click on the Shots icon switches tab.
    let marks = screen.frame(&mut app, Vec::new());
    let tabs = marks["nav:shots"].0.center();
    screen.click(&mut app, tabs);
    assert_eq!(app.active_tab, Tab::Shots);
    app.active_tab = Tab::Devices;

    edit_name(&mut app, "openconnect_client.0");
    let marks = screen.frame(&mut app, Vec::new());
    assert!(app.settings.locked());
    assert!(!enabled(&marks, "nav:shots"));
    for id in ["mevo.0", "openconnect_client.1"] {
        assert!(!enabled(&marks, &format!("enabled:{id}")), "{id}");
        assert!(!enabled(&marks, &format!("reconnect:{id}")), "{id}");
    }
    assert!(!enabled(&marks, "remove:mevo.0"));
    assert!(!enabled(&marks, "add:Launch monitors"));
    assert!(!enabled(&marks, "add:Software"));
    assert!(enabled(&marks, "save:openconnect_client.0"));
    assert!(enabled(&marks, "cancel:openconnect_client.0"));

    // Clicks on the nav, another card's switch or a (+) do nothing.
    screen.click(&mut app, tabs);
    assert_eq!(app.active_tab, Tab::Devices);
    screen.click_mark(&mut app, &marks, "enabled:openconnect_client.1");
    assert!(!app.settings.saving());
    let marks = screen.click_mark(&mut app, &marks, "add:Software");
    assert!(
        !marks.contains_key("add:Software:GSPro (Open Connect)"),
        "no menu"
    );

    // Escape doesn't discard.
    let escape = Event::Key {
        key: Key::Escape,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    };
    screen.frame(&mut app, vec![escape]);
    assert!(app.settings.actors[form_index(&app, "openconnect_client.0")].dirty());
    assert!(app.settings.locked());
}

#[test]
fn general_cards_show_save_and_cancel_where_edited_and_cancel_together() {
    let screen = Screen::new();
    let mut app = app();
    let marks = screen.frame(&mut app, Vec::new());
    for card in ["global", "simulation"] {
        assert!(!marks.contains_key(&format!("save:{card}")));
    }

    // A Global edit: the pair on Global only, the rest locked.
    app.set_default_units(UnitSystem::Metric);
    assert_eq!(app.units(), UnitSystem::Metric);
    let marks = screen.frame(&mut app, Vec::new());
    assert!(enabled(&marks, "save:global"));
    assert!(enabled(&marks, "cancel:global"));
    assert!(!marks.contains_key("save:simulation"));
    assert!(!enabled(&marks, "nav:shots"));
    assert!(!enabled(&marks, "enabled:mevo.0"));
    assert!(!enabled(&marks, "add:Software"));

    // A Simulation edit too: the pair on both.
    app.settings.simulation.altitude_val = "100".into();
    app.settings.mark_general(GeneralCard::Simulation);
    let marks = screen.frame(&mut app, Vec::new());
    assert!(enabled(&marks, "save:global"));
    assert!(enabled(&marks, "save:simulation"));

    // Cancel on either card reverts both, and the Shots units with them.
    screen.click_mark(&mut app, &marks, "cancel:simulation");
    assert!(!app.settings.global_dirty);
    assert!(!app.settings.locked());
    assert_eq!(app.settings.default_units, UnitSystem::Imperial);
    assert!(app.settings.simulation.fahrenheit);
    assert_eq!(app.settings.simulation.altitude_val, "0");
    assert_eq!(app.units(), UnitSystem::Imperial);
}

#[test]
fn the_units_control_on_the_global_card_sets_the_units_and_the_saved_config() {
    let screen = Screen::new();
    let mut app = app();
    let marks = screen.frame(&mut app, Vec::new());
    assert!(marks["units"].0.top() > marks["card:global"].0.top());
    // The control picks Metric: the Shots tab's units and the request
    // that Save posts follow.
    let marks = screen.click_mark(&mut app, &marks, "units");
    screen.click_mark(&mut app, &marks, "units:metric");
    assert_eq!(app.units(), UnitSystem::Metric);
    assert_eq!(
        app.settings.build_global_request().default_units,
        UnitSystem::Metric
    );
    // Saved and read back, it stays; a stored Imperial reads back too.
    app.settings.global_dirty = false;
    let mut saved = app.settings.to_request();
    app.settings.load_from(&saved);
    assert_eq!(app.units(), UnitSystem::Metric);
    saved.default_units = UnitSystem::Imperial;
    app.settings.load_from(&saved);
    assert_eq!(app.units(), UnitSystem::Imperial);
}

#[test]
fn the_units_setting_is_the_shots_tabs_units_and_cancel_puts_the_saved_ones_back() {
    let screen = Screen::new();
    let mut app = app();
    assert_eq!(app.units(), UnitSystem::Imperial);
    // Changed on the Global card, the Shots tab follows at once.
    app.set_default_units(UnitSystem::Metric);
    assert_eq!(app.units(), UnitSystem::Metric);
    let marks = screen.frame(&mut app, Vec::new());
    screen.click_mark(&mut app, &marks, "cancel:global");
    assert_eq!(app.settings.default_units, UnitSystem::Imperial);
    assert_eq!(app.units(), UnitSystem::Imperial);
}

#[test]
fn a_new_card_opens_dirty_in_settings_and_cancel_removes_it() {
    let screen = Screen::new();
    let mut app = app();
    // The saved Muni Golf runs on another PC, so the menu offers
    // Muni Golf on this one.
    integration(&mut app, "openconnect_client.1").address = "192.168.1.30:9210".into();
    let before = app.settings.actors.len();
    let marks = screen.frame(&mut app, Vec::new());
    assert!(enabled(&marks, "add:Software"));

    let marks = screen.click_mark(&mut app, &marks, "add:Software");
    let mut offered: Vec<&str> = marks
        .keys()
        .filter_map(|k| k.strip_prefix("add:Software:"))
        .collect();
    offered.sort_unstable();
    assert_eq!(
        offered,
        [
            "GSPro (Open Connect)",
            "Muni Golf (Open Connect)",
            "Pureplay (Open Connect)",
            "Pureplay Accessory",
        ],
        "the menu offers exactly these"
    );
    let marks = screen.click_mark(&mut app, &marks, "add:Software:Muni Golf (Open Connect)");
    assert_eq!(app.settings.actors.len(), before + 1);
    let new = &app.settings.actors[before];
    assert_eq!(new.global_id(), "openconnect_client.2");
    assert_eq!(new.name(), "Muni Golf");
    assert_eq!(new.type_label(), "Open Connect Client");
    let ActorFormEntry::Integration(entry) = new else {
        panic!("an Open Connect client is an integration entry");
    };
    assert_eq!(entry.address, "127.0.0.1:9210");
    assert!(new.dirty());
    assert!(
        !marks.contains_key("add:Software:Muni Golf (Open Connect)"),
        "the menu closes"
    );
    assert!(enabled(&marks, "save:openconnect_client.2"));
    assert!(enabled(&marks, "cancel:openconnect_client.2"));
    assert!(!enabled(&marks, "nav:shots"));
    assert!(!enabled(&marks, "add:Launch monitors"));

    let marks = screen.click_mark(&mut app, &marks, "cancel:openconnect_client.2");
    assert_eq!(app.settings.actors.len(), before);
    assert!(
        app.settings
            .actors
            .iter()
            .all(|a| a.global_id() != "openconnect_client.2")
    );
    assert!(!marks.keys().any(|k| k.ends_with("openconnect_client.2")));
    assert!(!app.settings.locked());
    assert!(enabled(&marks, "nav:shots"));
}

/// [`app`] without its two Open Connect clients: a Mevo alone.
fn app_without_outputs() -> FlighthookApp {
    let mut app = app();
    app.settings
        .actors
        .retain(|a| a.type_key() != "openconnect_client");
    app.actors
        .retain(|id, _| !id.starts_with("openconnect_client."));
    app
}

/// The GSPro (Open Connect) and Pureplay (Open Connect) entries both add
/// the same card: an Open Connect client named Open Connect at
/// `127.0.0.1:921`.
#[test]
fn both_921_entries_add_one_open_connect_card() {
    let screen = Screen::new();
    for label in ["GSPro (Open Connect)", "Pureplay (Open Connect)"] {
        let mut app = app_without_outputs();
        let marks = screen.frame(&mut app, Vec::new());
        let marks = screen.click_mark(&mut app, &marks, "add:Software");
        assert!(enabled(&marks, &format!("add:Software:{label}")), "{label}");
        let _ = screen.click_mark(&mut app, &marks, &format!("add:Software:{label}"));
        let new = &app.settings.actors[form_index(&app, "openconnect_client.0")];
        assert_eq!(new.name(), "Open Connect", "{label}");
        assert_eq!(new.type_label(), "Open Connect Client", "{label}");
        assert!(new.dirty(), "{label}");
        assert_eq!(
            integration(&mut app, "openconnect_client.0").address,
            "127.0.0.1:921",
            "{label}"
        );
        assert!(
            app.settings
                .actors
                .iter()
                .all(|a| a.type_key() != "pureplay"),
            "no Pureplay Connect output from {label}"
        );
        let config = app.settings.to_request();
        assert_eq!(config.openconnect_client["0"].name, "Open Connect");
        assert_eq!(
            config.openconnect_client["0"].address.as_deref(),
            Some("127.0.0.1:921")
        );
        assert!(config.pureplay.is_empty());
    }
}

/// [`app_without_outputs`] with a saved Open Connect card
/// (`openconnect_client.0`, on, `127.0.0.1:921`), connected.
fn app_with_open_connect() -> FlighthookApp {
    let mut app = app_without_outputs();
    let mut config = app.settings.to_request();
    config.openconnect_client.insert(
        "0".into(),
        flighthook::OpenConnectClientSection {
            name: "Open Connect".into(),
            enabled: true,
            actor_id: None,
            address: Some("127.0.0.1:921".into()),
            full_monitor: None,
            chipping_monitor: None,
            putting_monitor: None,
            chipping_clubs: flighthook::default_chipping_clubs(),
            putting_clubs: flighthook::default_putting_clubs(),
        },
    );
    settings_reply(&mut app, &config);
    app.actors.insert(
        "openconnect_client.0".into(),
        ActorStatusResponse::new("Open Connect", ActorStatus::Connected),
    );
    app
}

/// Add the Software preset `label` through the (+) menu and save the new
/// card; the server answers with the posted settings. Returns the
/// posted config.
fn add_and_save(
    screen: &Screen,
    app: &mut FlighthookApp,
    label: &str,
    id: &str,
) -> FlighthookConfig {
    let marks = screen.frame(app, Vec::new());
    let marks = screen.click_mark(app, &marks, "add:Software");
    let marks = screen.click_mark(app, &marks, &format!("add:Software:{label}"));
    let _ = screen.click_mark(app, &marks, &format!("save:{id}"));
    let posted = app.settings.posted().expect("a save is out").1.clone();
    reply(app, Ok(()));
    settings_reply(app, &posted);
    posted
}

/// No (+) menu entry is ever disabled for a duplicate address: with an
/// Open Connect card at `127.0.0.1:921`, all three Open Connect entries take
/// clicks, and GSPro (Open Connect) adds a second Open Connect card.
#[test]
fn no_menu_entry_is_disabled_for_a_duplicate_address() {
    let screen = Screen::new();
    let mut app = app_with_open_connect();
    let marks = screen.frame(&mut app, Vec::new());
    let marks = screen.click_mark(&mut app, &marks, "add:Software");
    for label in [
        "GSPro (Open Connect)",
        "Pureplay (Open Connect)",
        "Muni Golf (Open Connect)",
    ] {
        assert!(enabled(&marks, &format!("add:Software:{label}")), "{label}");
    }
    let _ = screen.click_mark(&mut app, &marks, "add:Software:GSPro (Open Connect)");
    let new = &app.settings.actors[form_index(&app, "openconnect_client.1")];
    assert_eq!(new.name(), "Open Connect");
    assert_eq!(
        integration(&mut app, "openconnect_client.1").address,
        "127.0.0.1:921"
    );
}

/// A duplicate is created off: no error, no alert, a Disabled card whose
/// switch is locked with the reason on hover. The card on the address
/// keeps running and its switch works.
#[test]
fn a_duplicate_is_created_off_with_its_switch_locked() {
    let (id, holder) = ("openconnect_client.1", "openconnect_client.0");
    let screen = Screen::new();
    let mut app = app_with_open_connect();
    for label in ["GSPro (Open Connect)", "Pureplay (Open Connect)"] {
        let mut app = app_with_open_connect();
        let marks = screen.frame(&mut app, Vec::new());
        let marks = screen.click_mark(&mut app, &marks, "add:Software");
        let _ = screen.click_mark(&mut app, &marks, &format!("add:Software:{label}"));
        assert!(
            !app.settings.to_request().openconnect_client[id.rsplit('.').next().unwrap()].enabled,
            "{label}: created off"
        );
    }

    let posted = add_and_save(&screen, &mut app, "GSPro (Open Connect)", id);
    assert!(!posted.openconnect_client["1"].enabled, "saved off");
    assert!(posted.openconnect_client["0"].enabled);
    assert_eq!(app.settings.to_request(), posted);
    assert!(!app.settings.saving() && !app.settings.dirty());

    app.now_ms = 2_000;
    let (marks, shapes) = screen.painted(&mut app, Vec::new());
    assert_eq!(
        badge_look(&marks, &shapes, id),
        ("DISABLED".into(), theme::STATUS_OFF, theme::STATUS_OFF)
    );
    assert!(!settings_alert(&app.actors, 2_000), "no alert");
    assert!(!marks.contains_key(&format!("error:{id}")));
    assert!(!enabled(&marks, &format!("enabled:{id}")), "locked");
    assert!(enabled(&marks, &format!("enabled:{holder}")));
    let _ = screen.click_mark(&mut app, &marks, &format!("enabled:{id}"));
    assert!(
        app.settings.posted().is_none(),
        "a click on it saves nothing"
    );
    assert!(app.settings.turned_off(id));
    assert_eq!(
        app.settings.enable_clash(id).map(|c| c.message()),
        Some("An Open Connect card already uses 127.0.0.1:921".into())
    );
    assert_eq!(app.settings.enable_clash(holder), None);
}

/// The locked switch says why on hover.
#[test]
fn the_locked_switch_names_the_card_that_holds_the_address() {
    let screen = Screen::new();
    let mut app = app_with_open_connect();
    let _ = add_and_save(
        &screen,
        &mut app,
        "GSPro (Open Connect)",
        "openconnect_client.1",
    );
    screen.0.style_mut(|s| {
        s.interaction.tooltip_delay = 0.0;
        s.interaction.show_tooltips_only_when_still = false;
    });
    let marks = screen.frame(&mut app, Vec::new());
    let at = marks["enabled:openconnect_client.1"].0.center();
    screen.frame(&mut app, vec![Event::PointerMoved(at)]);
    for _ in 0..3 {
        screen.frame(&mut app, Vec::new());
    }
    let texts = screen.texts(&mut app);
    assert!(
        texts
            .iter()
            .any(|t| t == "An Open Connect card already uses 127.0.0.1:921"),
        "{texts:?}"
    );
    // The card on the address shows the plain hover.
    let at = marks["enabled:openconnect_client.0"].0.center();
    screen.frame(&mut app, vec![Event::PointerMoved(at)]);
    for _ in 0..3 {
        screen.frame(&mut app, Vec::new());
    }
    let texts = screen.texts(&mut app);
    assert!(texts.iter().any(|t| t == "Turn off"), "{texts:?}");
    assert!(!texts.iter().any(|t| t.contains("already uses")));
}

/// After the duplicate's address is edited to a free one and saved, its
/// switch takes input and turns it on; the save carries only `enabled`.
#[test]
fn a_duplicate_can_be_turned_on_after_moving_to_a_free_address() {
    let id = "openconnect_client.1";
    let screen = Screen::new();
    let mut app = app_with_open_connect();
    let _ = add_and_save(&screen, &mut app, "GSPro (Open Connect)", id);

    integration(&mut app, id).address = "127.0.0.1:9210".into();
    let idx = form_index(&app, id);
    app.settings.actors[idx].set_dirty(true);
    let marks = screen.frame(&mut app, Vec::new());
    let _ = screen.click_mark(&mut app, &marks, &format!("save:{id}"));
    let posted = app.settings.posted().expect("a save is out").1.clone();
    assert_eq!(
        posted.openconnect_client["1"].address.as_deref(),
        Some("127.0.0.1:9210")
    );
    assert!(!posted.openconnect_client["1"].enabled, "still off");
    reply(&mut app, Ok(()));
    settings_reply(&mut app, &posted);

    let marks = screen.frame(&mut app, Vec::new());
    assert!(enabled(&marks, &format!("enabled:{id}")), "unlocked");
    assert_eq!(app.settings.enable_clash(id), None);
    let _ = screen.click_mark(&mut app, &marks, &format!("enabled:{id}"));
    let (target, turned_on) = app.settings.posted().expect("a save is out");
    assert_eq!(*target, SaveTarget::Actor(id.into()));
    let mut want = posted.clone();
    want.openconnect_client.get_mut("1").unwrap().enabled = true;
    assert_eq!(*turned_on, want);
}

/// Saving an enabled card with its address edited onto a held one writes
/// it off, keeping the edit and leaving the holder on; a new card made
/// on a free address and edited onto a held one before its first save
/// does the same.
#[test]
fn editing_an_address_onto_a_held_one_saves_the_card_off() {
    let screen = Screen::new();
    let mut app = app();
    let id = "openconnect_client.1";
    integration(&mut app, id).address = "127.0.0.1:921".into();
    let idx = form_index(&app, id);
    app.settings.actors[idx].set_dirty(true);
    let marks = screen.frame(&mut app, Vec::new());
    let _ = screen.click_mark(&mut app, &marks, &format!("save:{id}"));
    let (target, posted) = app.settings.posted().expect("a save is out");
    assert_eq!(*target, SaveTarget::Actor(id.into()));
    assert!(!posted.openconnect_client["1"].enabled, "saved off");
    assert_eq!(
        posted.openconnect_client["1"].address.as_deref(),
        Some("127.0.0.1:921")
    );
    assert!(posted.openconnect_client["0"].enabled, "the holder is on");
    let posted = posted.clone();
    reply(&mut app, Ok(()));
    settings_reply(&mut app, &posted);
    assert!(app.settings.turned_off(id));
    assert!(app.settings.enable_clash(id).is_some());

    // A new card on a free address is on; edited onto a held one before
    // it is saved, it is saved off.
    let mut app = app_with_open_connect();
    let marks = screen.frame(&mut app, Vec::new());
    let marks = screen.click_mark(&mut app, &marks, "add:Software");
    let _ = screen.click_mark(&mut app, &marks, "add:Software:Muni Golf (Open Connect)");
    let id = "openconnect_client.1";
    assert!(app.settings.to_request().openconnect_client["1"].enabled);
    integration(&mut app, id).address = "127.0.0.1:921".into();
    let marks = screen.frame(&mut app, Vec::new());
    let _ = screen.click_mark(&mut app, &marks, &format!("save:{id}"));
    let (_, posted) = app.settings.posted().expect("a save is out");
    assert!(!posted.openconnect_client["1"].enabled);
}

/// The (+) menu offers no Pureplay Connect output; one in the config
/// file shows as a `pureplay` card with Pureplay's address and the
/// routing dropdowns, and saves as a `pureplay` section.
#[test]
fn a_configured_pureplay_connect_output_saves_its_address_and_routing() {
    let screen = Screen::new();
    let mut app = app();
    let marks = screen.frame(&mut app, Vec::new());
    let menu = screen.click_mark(&mut app, &marks, "add:Software");
    assert!(
        !menu
            .keys()
            .any(|k| k.starts_with("add:Software:") && k.contains("Pureplay Connect")),
        "no Pureplay Connect entry"
    );
    let mut config = app.settings.to_request();
    config.pureplay.insert(
        "0".into(),
        crate::types::PureplaySection {
            name: "Pureplay Connect".into(),
            address: Some("127.0.0.1:9760".into()),
            ..Default::default()
        },
    );
    app.settings.load_from(&config);
    let marks = screen.frame(&mut app, Vec::new());
    let new = &app.settings.actors[form_index(&app, "pureplay.0")];
    assert_eq!(new.name(), "Pureplay Connect");
    assert_eq!(new.type_label(), "Pureplay Connect");
    assert_eq!(
        integration(&mut app, "pureplay.0").address,
        "127.0.0.1:9760"
    );
    assert_header_fits(&marks, "pureplay.0", "default");

    let entry = integration(&mut app, "pureplay.0");
    entry.address = "192.168.1.20:9760".into();
    entry.putting_monitor = "mevo.0".into();
    let config = app.settings.to_request();
    assert_eq!(
        config.pureplay["0"],
        crate::types::PureplaySection {
            name: "Pureplay Connect".into(),
            enabled: true,
            actor_id: None,
            address: Some("192.168.1.20:9760".into()),
            full_monitor: None,
            chipping_monitor: None,
            putting_monitor: Some("mevo.0".into()),
        }
    );
}

/// The Software (+) menu's Pureplay Accessory entry adds a Pureplay
/// Accessory card under Software with Pureplay's address. Its Settings side has the Address
/// field and, beside Remove, Forget pairing (a request like Reconnect);
/// it saves as a `pureplay_accessory` section, a blank address left
/// out; a bad address cannot be saved.
#[test]
fn the_pureplay_card_saves_its_address_and_forgets_its_pairing() {
    let screen = Screen::new();
    let mut app = app();
    let marks = screen.frame(&mut app, Vec::new());
    let menu = screen.click_mark(&mut app, &marks, "add:Launch monitors");
    assert!(
        !menu.contains_key("add:Launch monitors:Pureplay Accessory"),
        "not a Launch monitors entry"
    );
    let menu = screen.click_mark(&mut app, &marks, "add:Software");
    let _ = screen.click_mark(&mut app, &menu, "add:Software:Pureplay Accessory");
    let id = "pureplay_accessory.0";
    let idx = form_index(&app, id);
    assert_eq!(app.settings.actors[idx].name(), "Pureplay Accessory");
    assert_eq!(app.settings.actors[idx].type_label(), "Pureplay Accessory");
    assert_eq!(
        cards::group_of("pureplay_accessory"),
        cards::CardGroup::Software
    );
    let config = app.settings.to_request();
    assert_eq!(
        config.pureplay_accessory["0"],
        crate::types::PureplayAccessorySection {
            name: "Pureplay Accessory".into(),
            enabled: true,
            actor_id: None,
            address: Some("127.0.0.1:9770".into()),
            shots: flighthook::PureplayShots::Auto,
            impact_lead_s: None,
        }
    );
    let marks = screen.frame(&mut app, Vec::new());
    assert!(
        marks.contains_key(&format!("address:{id}")),
        "Address field"
    );
    assert!(marks.contains_key(&format!("shots:{id}")), "Shots picker");
    assert!(
        marks.contains_key(&format!("camera_timing:{id}")),
        "Camera timing field"
    );
    // The Shots choice and the camera timing save, and load back. A
    // form test: both are set on the form entry, the picker and the
    // field are only checked to be drawn above, not clicked.
    let ActorFormEntry::Device(dev) = &mut app.settings.actors[idx] else {
        panic!("a device card");
    };
    assert_eq!(
        crate::panels::settings::pureplay_shots_label(dev.pureplay_shots),
        "Auto"
    );
    dev.pureplay_shots = flighthook::PureplayShots::FromFlighthook;
    dev.impact_lead = "3".into();
    let saved = app.settings.to_request().pureplay_accessory["0"].clone();
    assert_eq!(saved.shots, flighthook::PureplayShots::FromFlighthook);
    assert_eq!(saved.impact_lead_s, Some(3.0));
    let mut reloaded = app.settings.to_request();
    reloaded
        .pureplay_accessory
        .insert("0".into(), saved.clone());
    let mut again = FlighthookApp::with_pending(net::new_pending());
    again.settings.load_from(&reloaded);
    assert_eq!(again.settings.to_request().pureplay_accessory["0"], saved);
    let ActorFormEntry::Device(dev) = &mut app.settings.actors[idx] else {
        panic!("a device card");
    };
    dev.impact_lead = "9".into();
    assert!(!crate::panels::settings::actor_valid(
        &app.settings.actors[idx]
    ));
    let ActorFormEntry::Device(dev) = &mut app.settings.actors[idx] else {
        panic!("a device card");
    };
    dev.impact_lead.clear();
    dev.pureplay_shots = flighthook::PureplayShots::Auto;
    let ActorFormEntry::Device(dev) = &mut app.settings.actors[idx] else {
        panic!("a device card");
    };
    dev.address = "not an address".into();
    assert!(!crate::panels::settings::actor_valid(
        &app.settings.actors[idx]
    ));
    let ActorFormEntry::Device(dev) = &mut app.settings.actors[idx] else {
        panic!("a device card");
    };
    dev.address.clear();
    assert!(crate::panels::settings::actor_valid(
        &app.settings.actors[idx]
    ));
    assert_eq!(
        app.settings.to_request().pureplay_accessory["0"].address,
        None
    );

    // Saved and running: Forget pairing beside Remove, on the Settings
    // side only; Reconnect on the Telemetry side.
    let mut config = app.settings.to_request();
    config.pureplay_accessory.insert(
        "0".into(),
        crate::types::PureplayAccessorySection::default(),
    );
    let mut app = FlighthookApp::with_pending(net::new_pending());
    app.settings.load_from(&config);
    for a in &app.settings.actors {
        app.actors.insert(
            a.global_id(),
            ActorStatusResponse::new(a.name(), ActorStatus::Connected),
        );
    }
    let marks = screen.frame(&mut app, Vec::new());
    assert!(marks.contains_key(&format!("reconnect:{id}")));
    let (forget, enabled) = marks[&format!("forget_pairing:{id}")];
    let (remove, _) = marks[&format!("remove:{id}")];
    assert!(enabled);
    assert!(forget.right() < remove.left(), "left of Remove");
    assert!(
        (forget.center().y - remove.center().y).abs() < 1.0,
        "one row"
    );
    let card = marks[&format!("card:{id}")].0;
    assert!(card.contains_rect(forget));
    let marks = screen.click_mark(&mut app, &marks, &format!("forget_pairing:{id}"));
    assert!(app.reconnecting.contains(id), "the request is out");
    assert!(!marks[&format!("forget_pairing:{id}")].1, "one at a time");
    // Other cards have no Forget pairing.
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!marks.contains_key("forget_pairing:mevo.0"));
}

/// A Pureplay Accessory card that is turned off has Forget pairing, disabled.
#[test]
fn forget_pairing_is_disabled_on_a_card_that_is_off() {
    let screen = Screen::new();
    let mut config = app().settings.to_request();
    config.pureplay_accessory.insert(
        "0".into(),
        crate::types::PureplayAccessorySection {
            enabled: false,
            ..crate::types::PureplayAccessorySection::default()
        },
    );
    let mut app = FlighthookApp::with_pending(net::new_pending());
    app.settings.load_from(&config);
    let id = "pureplay_accessory.0";
    app.actors
        .insert(id.into(), ActorStatusResponse::disabled("Pureplay"));
    let marks = screen.frame(&mut app, Vec::new());
    assert!(
        !marks[&format!("forget_pairing:{id}")].1,
        "disabled when off"
    );
    assert!(marks[&format!("remove:{id}")].1, "Remove still works");
}

/// A Pureplay output and the Player card on one Devices tab: both
/// cards show, and saving either keeps the other's section.
#[test]
fn the_golfers_card_and_a_pureplay_output_save_apart() {
    let screen = Screen::new();
    let mut app = app();
    let mut config = app.settings.to_request();
    config.pureplay.insert(
        "0".into(),
        crate::types::PureplaySection {
            name: "Pureplay".into(),
            address: Some("192.168.1.20:9760".into()),
            ..Default::default()
        },
    );
    config.golfers = vec![flighthook::GolferRecord {
        sex: Some(flighthook::Sex::Female),
        height: Some(170.0),
        ..flighthook::GolferRecord::new("flighthook.a", "Eric")
    }];
    app.settings.load_from(&config);
    app.actors.insert(
        "pureplay.0".into(),
        ActorStatusResponse::new("Pureplay", ActorStatus::Connected),
    );
    let marks = screen.frame(&mut app, Vec::new());
    let (golfers, _) = marks["card:golfers"];
    let (pureplay, _) = marks["card:pureplay.0"];
    assert!(pureplay.top() < golfers.top(), "Software above Golfers");
    assert!(marks.contains_key("add:Software"));
    assert_eq!(
        app.settings.build_global_request().pureplay,
        config.pureplay
    );
    assert_eq!(
        app.settings.build_actor_request("pureplay.0").golfers,
        config.golfers
    );
    assert_eq!(app.settings.to_request(), config);
}

fn device<'a>(app: &'a mut FlighthookApp, id: &str) -> &'a mut settings::DeviceFormEntry {
    let idx = form_index(app, id);
    match &mut app.settings.actors[idx] {
        ActorFormEntry::Device(d) => d,
        ActorFormEntry::Integration(_) => panic!("{id} is an integration"),
    }
}

#[test]
fn the_generic_openconnect_server_edits_its_bind_address() {
    let screen = Screen::new();
    let mut app = app();
    let marks = screen.frame(&mut app, Vec::new());
    let marks = screen.click_mark(&mut app, &marks, "add:Launch monitors");
    assert!(
        marks.contains_key("add:Launch monitors:Open Connect Server"),
        "the menu offers the Open Connect Server"
    );
    let open_connect = marks
        .keys()
        .filter(|k| k.starts_with("add:Launch monitors:") && k.contains("Open Connect"))
        .count();
    assert_eq!(open_connect, 1, "one Open Connect Server entry");
    let marks = screen.click_mark(&mut app, &marks, "add:Launch monitors:Open Connect Server");
    let id = "openconnect_server.0";
    let entry = &app.settings.actors[form_index(&app, id)];
    assert_eq!(entry.name(), "Open Connect Server");
    assert_eq!(entry.type_label(), "Open Connect Server");
    assert_eq!(device(&mut app, id).address, "0.0.0.0:921");
    assert!(enabled(&marks, &format!("bind:{id}")));
    assert!(enabled(&marks, &format!("save:{id}")));
    let bind = |app: &FlighthookApp| {
        app.settings.build_actor_request(id).openconnect_server["0"]
            .bind
            .clone()
    };

    device(&mut app, id).address = "0.0.0.0:".into();
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!enabled(&marks, &format!("save:{id}")), "invalid bind");

    // Blank listens on the default.
    device(&mut app, id).address = String::new();
    let marks = screen.frame(&mut app, Vec::new());
    assert!(enabled(&marks, &format!("save:{id}")));
    assert_eq!(bind(&app), None);

    device(&mut app, id).address = "127.0.0.1:9921".into();
    let marks = screen.frame(&mut app, Vec::new());
    assert!(enabled(&marks, &format!("save:{id}")));
    assert_eq!(bind(&app).as_deref(), Some("127.0.0.1:9921"));
}

/// The Launch monitors menu offers the Trackball only when the server
/// can read one; the added card shows the driver's fields, refuses what
/// the driver refuses, and has no Camera timing.
#[test]
fn the_trackball_is_added_where_the_server_can_read_one() {
    let screen = Screen::new();
    let mut app = app();
    let marks = screen.frame(&mut app, Vec::new());
    let marks = screen.click_mark(&mut app, &marks, "add:Launch monitors");
    assert!(!marks.contains_key("add:Launch monitors:Trackball"));
    assert!(marks.contains_key("add:Launch monitors:ExPutt"));

    let screen = Screen::new();
    let mut app = self::app();
    app.trackball_supported = true;
    let marks = screen.frame(&mut app, Vec::new());
    let marks = screen.click_mark(&mut app, &marks, "add:Launch monitors");
    let marks = screen.click_mark(&mut app, &marks, "add:Launch monitors:Trackball");
    let id = "trackball.0";
    let entry = &app.settings.actors[form_index(&app, id)];
    assert_eq!(entry.name(), "Trackball");
    assert_eq!(entry.type_label(), "Trackball");
    assert!(entry.type_tooltip().starts_with("Trackball (alpha)"));
    assert!(enabled(&marks, &format!("trackball:device:{id}")));
    assert!(enabled(
        &marks,
        &format!("trackball:tuning.full_power_speed:{id}")
    ));
    assert!(!marks.contains_key(&format!("camera_timing:{id}")));
    assert!(enabled(&marks, &format!("save:{id}")));

    device(&mut app, id)
        .trackball_form()
        .numbers
        .insert("tuning.full_power_speed", "0".into());
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!enabled(&marks, &format!("save:{id}")), "refused");
    assert!(marks.contains_key(&format!("driver_error:{id}")));
    device(&mut app, id)
        .trackball_form()
        .numbers
        .insert("tuning.full_power_speed", "40".into());
    let marks = screen.frame(&mut app, Vec::new());
    assert!(enabled(&marks, &format!("save:{id}")));
    let section = app.settings.build_actor_request(id).trackball["0"].clone();
    assert!((section.driver.tuning.full_power_speed - 40.0).abs() < f64::EPSILON);
    assert!(!section.driver.config.grab);
}

#[test]
fn an_added_exputt_takes_an_optional_ip_address() {
    let screen = Screen::new();
    let mut app = app();
    let marks = screen.frame(&mut app, Vec::new());
    let marks = screen.click_mark(&mut app, &marks, "add:Launch monitors");
    let marks = screen.click_mark(&mut app, &marks, "add:Launch monitors:ExPutt");
    let id = "exputt.0";
    let entry = &app.settings.actors[form_index(&app, id)];
    assert_eq!(entry.name(), "ExPutt");
    assert_eq!(entry.type_label(), "ExPutt");
    assert!(enabled(&marks, &format!("ip:{id}")));
    let field_texts = |app: &mut FlighthookApp| {
        let (marks, shapes) = screen.painted(app, Vec::new());
        let (field, _) = marks[&format!("ip:{id}")];
        shapes
            .iter()
            .filter_map(|s| match s {
                egui::Shape::Text(t) if field.contains(t.visual_bounding_rect().center()) => {
                    Some(t.galley.text().to_string())
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    // An empty field shows the placeholder, a filled one only its text.
    assert_eq!(field_texts(&mut app), [settings::EXPUTT_IP_PLACEHOLDER]);
    assert_eq!(settings::EXPUTT_IP_PLACEHOLDER, "empty = auto-detect");
    device(&mut app, id).address = "10.0.0.7".into();
    assert_eq!(field_texts(&mut app), ["10.0.0.7"]);
    device(&mut app, id).address = String::new();
    // Blank finds the device on the network.
    assert!(enabled(&marks, &format!("save:{id}")));
    let address = |app: &FlighthookApp| {
        app.settings.build_actor_request(id).exputt["0"]
            .driver
            .address
            .clone()
    };
    assert_eq!(address(&app), None);

    device(&mut app, id).address = "192.168.1.50:5000".into();
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!enabled(&marks, &format!("save:{id}")), "an IP, no port");

    device(&mut app, id).address = " 192.168.1.50 ".into();
    let marks = screen.frame(&mut app, Vec::new());
    assert!(enabled(&marks, &format!("save:{id}")));
    assert_eq!(address(&app).as_deref(), Some("192.168.1.50"));
}

/// `app()` plus a saved ExPutt, `exputt.0`, with status `status` (not
/// running when `None`).
fn app_with_exputt(status: Option<ActorStatus>) -> FlighthookApp {
    let mut app = app();
    let mut config = app.settings.to_request();
    config.exputt.insert(
        "0".into(),
        flighthook::ExPuttSection {
            name: "ExPutt".into(),
            ..flighthook::ExPuttSection::default()
        },
    );
    app.settings.load_from(&config);
    if let Some(status) = status {
        let mut actor = ActorStatusResponse::new("ExPutt", status);
        actor.telemetry = HashMap::from([("ready".to_string(), "true".to_string())]);
        app.actors.insert("exputt.0".into(), actor);
    }
    app
}

#[test]
fn find_mat_is_an_icon_in_the_exputt_header_and_needs_a_connection() {
    let screen = Screen::new();
    let mut app = app_with_exputt(Some(ActorStatus::Connected));
    let marks = screen.frame(&mut app, Vec::new());
    assert!(enabled(&marks, "find_mat:exputt.0"));
    let finds: Vec<&String> = marks
        .keys()
        .filter(|k| k.starts_with("find_mat:"))
        .collect();
    assert_eq!(finds, ["find_mat:exputt.0"], "ExPutt cards only");
    // Inside its page, in the header beside the badge.
    let (card, _) = marks["card:exputt.0"];
    let (button, _) = marks["find_mat:exputt.0"];
    assert!(card.contains_rect(button), "{button:?} in {card:?}");
    let (badge, _) = marks["badge:exputt.0"];
    assert!((button.center().y - badge.center().y).abs() < 4.0);
    assert!(button.left() > badge.right());
    // An icon the size of Reconnect's, its text only as the tip.
    let (reconnect, _) = marks["reconnect:exputt.0"];
    assert_eq!(button.size(), reconnect.size());
    assert!(!screen.texts(&mut app).iter().any(|t| t == "Find mat"));

    for status in [
        ActorStatus::Starting,
        ActorStatus::Reconnecting,
        ActorStatus::Disconnected,
    ] {
        let mut app = app_with_exputt(Some(status));
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!enabled(&marks, "find_mat:exputt.0"), "{status:?}");
    }
    let mut app = app_with_exputt(None);
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!enabled(&marks, "find_mat:exputt.0"), "not running");
}

#[test]
fn a_saved_openconnect_server_shows_its_bind_on_the_settings_side() {
    let screen = Screen::new();
    let mut app = app();
    let mut config = app.settings.to_request();
    config.openconnect_server.insert(
        "0".into(),
        flighthook::OpenConnectServerSection {
            name: "Range Bridge".into(),
            enabled: true,
            actor_id: None,
            bind: Some("0.0.0.0:9921".into()),
            impact_lead_s: None,
        },
    );
    app.settings.load_from(&config);
    let marks = screen.frame(&mut app, Vec::new());
    assert!(enabled(&marks, "bind:openconnect_server.0"));
    assert_eq!(
        device(&mut app, "openconnect_server.0").address,
        "0.0.0.0:9921"
    );
    // Only the server has one.
    assert!(
        !marks
            .keys()
            .any(|k| k.starts_with("bind:") && k != "bind:openconnect_server.0")
    );
}

/// The Square Golf Omni telemetry seen live, whose `device_info` is
/// longer than a card's value column.
const OMNI_DEVICE_INFO: &str = "Square Golf SGO300A SquareGolf(CD01) (02:00:00:AB:CD:01)";

/// `app()` plus a Square Golf Omni between the Mevo and an Open Connect
/// server, reporting [`OMNI_DEVICE_INFO`].
fn app_with_omni() -> FlighthookApp {
    let mut app = app();
    let mut config = app.settings.to_request();
    config.square.insert(
        "0".into(),
        flighthook::SquareSection {
            name: "Square Golf Omni".into(),
            ..Default::default()
        },
    );
    config
        .openconnect_server
        .insert("0".into(), flighthook::OpenConnectServerSection::default());
    app.settings.load_from(&config);
    for (id, name, telemetry) in [
        (
            "square.0",
            "Square Golf Omni",
            vec![
                ("device_info", OMNI_DEVICE_INFO),
                ("ble_address", "02:00:00:AB:CD:01"),
                ("ready", "true"),
                ("battery_pct", "85"),
            ],
        ),
        (
            "openconnect_server.0",
            "Range Bridge",
            vec![("bind", "0.0.0.0:921")],
        ),
    ] {
        let mut actor = ActorStatusResponse::new(name, ActorStatus::Connected);
        actor.telemetry = telemetry
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        app.actors.insert(id.into(), actor);
    }
    app
}

/// Cards `ids`, in card order and one grid row, keep to equal column
/// widths and stay out of each other's way.
fn assert_row_fits(marks: &Marks, ids: &[&str], at: &str) {
    let rects: Vec<Rect> = ids
        .iter()
        .map(|id| {
            marks
                .get(&format!("card:{id}"))
                .unwrap_or_else(|| panic!("{id} not drawn at {at}"))
                .0
        })
        .collect();
    let width = rects[0].width();
    for (id, r) in ids.iter().zip(&rects) {
        assert!(
            (r.width() - width).abs() < 0.5,
            "{id} is {} wide, its row {width} at {at}",
            r.width()
        );
        assert!(
            r.width() <= cards::CARD_MAX_WIDTH + 0.5,
            "{id} wider than a column at {at}"
        );
    }
    // Side by side, or one under the other where a column is all
    // that fits.
    for pair in rects.windows(2) {
        assert!(
            !pair[0].shrink(0.5).intersects(pair[1].shrink(0.5)),
            "cards overlap at {at}: {:?} / {:?}",
            pair[0],
            pair[1]
        );
    }
}

#[test]
fn a_long_telemetry_value_stays_inside_its_card() {
    for (w, h, rows) in [
        (
            1600.0,
            900.0,
            vec![vec!["mevo.0", "square.0", "openconnect_server.0"]],
        ),
        (
            1024.0,
            768.0,
            vec![vec!["mevo.0", "square.0"], vec!["openconnect_server.0"]],
        ),
    ] {
        let at = format!("{w}x{h}");
        let screen = Screen::sized(w, h);
        let mut app = app_with_omni();
        screen.frame(&mut app, Vec::new());
        let marks = screen.frame(&mut app, Vec::new());
        for row in &rows {
            assert_row_fits(&marks, row, &at);
        }
        // Every card of the group the same width.
        let (mevo, _) = marks["card:mevo.0"];
        let (server, _) = marks["card:openconnect_server.0"];
        assert!((mevo.width() - server.width()).abs() < 0.5, "{at}");

        // The long value is cut short inside the card.
        let (card, _) = marks["card:square.0"];
        let (value, _) = marks["telemetry:square.0:device_info"];
        assert!(
            value.right() <= card.right(),
            "{value:?} past {card:?} at {at}"
        );
        let (ble, _) = marks["telemetry:square.0:ble_address"];
        assert!(ble.right() <= card.right(), "{at}");
    }
}

#[test]
fn a_long_name_stays_inside_its_card_on_both_sides() {
    let long = "Square Golf SGO300A SquareGolf(CD01) in the back bay by the window";
    for (w, h) in [(1600.0, 900.0), (1024.0, 768.0)] {
        let at = format!("{w}x{h}");
        let screen = Screen::sized(w, h);
        let mut app = app_with_omni();
        app.actors.get_mut("square.0").unwrap().name = long.into();
        screen.frame(&mut app, Vec::new());
        let marks = screen.frame(&mut app, Vec::new());
        assert_row_fits(&marks, &["mevo.0", "square.0"], &at);

        // The name and BLE address fields holding long text.
        device(&mut app, "square.0").name = long.into();
        device(&mut app, "square.0").address = OMNI_DEVICE_INFO.into();
        device(&mut app, "square.0").dirty = true;
        screen.frame(&mut app, Vec::new());
        let after = screen.frame(&mut app, Vec::new());
        assert_row_fits(&after, &["mevo.0", "square.0"], &at);
        assert!(
            (after["card:square.0"].0.width() - marks["card:square.0"].0.width()).abs() < 0.5,
            "edits keep the page's width at {at}"
        );
        // Save and Cancel stay in the card.
        let (card, _) = after["card:square.0"];
        for control in ["save:square.0", "cancel:square.0"] {
            let (r, _) = after[control];
            assert!(
                card.contains_rect(r),
                "{control} {r:?} outside {card:?} at {at}"
            );
        }

        // A client routed to it shows the long name in its dropdown.
        integration(&mut app, "openconnect_client.0").full_monitor = "square.0".into();
        screen.frame(&mut app, Vec::new());
        let outputs = screen.frame(&mut app, Vec::new());
        assert_row_fits(
            &outputs,
            &["openconnect_client.0", "openconnect_client.1"],
            &at,
        );
    }
}

/// The header of page `id` in `marks`: its title, type tag, badge,
/// Reconnect, Remove, Cancel and Save (those drawn) stay inside the page,
/// left to right, apart.
fn assert_header_fits(marks: &Marks, id: &str, at: &str) {
    let (card, _) = marks[&format!("card:{id}")];
    let parts: Vec<(String, Rect)> = [
        "title",
        "tag",
        "badge",
        "reconnect",
        "remove",
        "cancel",
        "save",
    ]
    .iter()
    .filter_map(|p| {
        let name = format!("{p}:{id}");
        marks.get(&name).map(|(r, _)| (name, *r))
    })
    .collect();
    for (name, r) in &parts {
        assert!(
            card.contains_rect(*r),
            "{name} {r:?} outside {card:?} at {at}"
        );
    }
    for pair in parts.windows(2) {
        let ((a, ra), (b, rb)) = (&pair[0], &pair[1]);
        assert!(
            ra.right() <= rb.left(),
            "{a} {ra:?} runs into {b} {rb:?} at {at}"
        );
    }
}

/// Page `id`'s type tag in `marks` sits right of its title and right
/// against `next` (the badge), on the title's row.
fn assert_tag_by_buttons(marks: &Marks, id: &str, next: &str, at: &str) {
    let (title, _) = marks[&format!("title:{id}")];
    let (tag, _) = marks[&format!("tag:{id}")];
    let (next_rect, _) = marks[&format!("{next}:{id}")];
    assert!(
        title.right() <= tag.left(),
        "{id} tag left of its title at {at}"
    );
    assert!(
        tag.right() <= next_rect.left() && next_rect.left() - tag.right() < 12.0,
        "{id} tag {tag:?} not right against {next} {next_rect:?} at {at}"
    );
    assert!(
        tag.center().y > title.top() && tag.center().y < title.bottom(),
        "{id} tag off the title's row at {at}"
    );
}

#[test]
fn the_type_tag_sits_by_the_badge_and_a_long_name_gives_way_to_it() {
    let long = "Square Golf SGO300A SquareGolf(CD01) in the back bay by the window";
    let ids = ["mevo.0", "square.0", "openconnect_client.0", "webserver.0"];
    for (w, h) in [(1600.0, 900.0), (1024.0, 768.0)] {
        let at = format!("{w}x{h}");
        let screen = Screen::sized(w, h);
        let mut app = app_with_omni();
        app.actors.get_mut("square.0").unwrap().name = "Bay Omni".into();
        app.actors.get_mut("webserver.0").unwrap().name = "Dashboard".into();
        device(&mut app, "square.0").name = "Bay Omni".into();
        screen.frame(&mut app, Vec::new());
        let short = screen.frame(&mut app, Vec::new());
        let tag_width = |m: &Marks| m["tag:square.0"].0.width();
        for id in ids {
            assert!(short.contains_key(&format!("tag:{id}")), "{id} at {at}");
            assert!(short.contains_key(&format!("badge:{id}")), "{id} at {at}");
            assert_header_fits(&short, id, &at);
            assert_tag_by_buttons(&short, id, "badge", &at);
        }
        // Right-aligned, apart from a short name.
        let gap = short["tag:square.0"].0.left() - short["title:square.0"].0.right();
        assert!(gap > 40.0, "the tag beside the name at {at}: {gap}");

        // Dirty: the same tag, Save and Cancel instead of the icons.
        device(&mut app, "square.0").dirty = true;
        screen.frame(&mut app, Vec::new());
        let dirty = screen.frame(&mut app, Vec::new());
        assert!(!dirty.contains_key("reconnect:square.0"), "{at}");
        assert_header_fits(&dirty, "square.0", &at);
        assert_tag_by_buttons(&dirty, "square.0", "badge", &at);
        assert!(
            (tag_width(&dirty) - tag_width(&short)).abs() < 0.5,
            "the tag shrank while dirty at {at}"
        );
        device(&mut app, "square.0").dirty = false;

        // A long name: the name is cut short, the tag keeps its full
        // width.
        app.actors.get_mut("square.0").unwrap().name = long.into();
        screen.frame(&mut app, Vec::new());
        let marks = screen.frame(&mut app, Vec::new());
        assert_header_fits(&marks, "square.0", &at);
        assert_tag_by_buttons(&marks, "square.0", "badge", &at);
        assert!(
            (tag_width(&marks) - tag_width(&short)).abs() < 0.5,
            "the tag shrank at {at}"
        );
        assert!(
            marks["title:square.0"].0.width() < 300.0,
            "the name is cut short at {at}"
        );
    }
}

#[test]
fn the_type_tag_is_left_out_when_the_name_says_it() {
    let screen = Screen::new();
    let mut app = app_with_omni();
    // Named as their type: the Omni and the web server at their default
    // names.
    screen.frame(&mut app, Vec::new());
    let marks = screen.frame(&mut app, Vec::new());
    for id in ["square.0", "webserver.0"] {
        assert!(marks.contains_key(&format!("title:{id}")), "{id}");
        assert!(!marks.contains_key(&format!("tag:{id}")), "{id}");
    }
    assert!(marks.contains_key("tag:openconnect_client.0"));

    // Renamed, the web server's tag names its type.
    app.actors.get_mut("webserver.0").unwrap().name = "Dashboard".into();
    screen.frame(&mut app, Vec::new());
    let marks = screen.frame(&mut app, Vec::new());
    assert!(marks.contains_key("tag:webserver.0"));
    let entry = &app.settings.actors[form_index(&app, "webserver.0")];
    assert_eq!(entry.type_label(), "Web Server");

    // The generic preset at its default name, in any case.
    let marks = screen.frame(&mut app, Vec::new());
    let marks = screen.click_mark(&mut app, &marks, "add:Launch monitors");
    assert!(enabled(&marks, "add:Launch monitors:Open Connect Server"));
    let escape = Event::Key {
        key: Key::Escape,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    };
    screen.frame(&mut app, vec![escape]);
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!marks.contains_key("add:Launch monitors:Open Connect Server"));
    device(&mut app, "openconnect_server.0").address = "0.0.0.0:922".into();
    let (menu, preset, id) = (
        "add:Launch monitors",
        "Open Connect Server",
        "openconnect_server.1",
    );
    let marks = screen.frame(&mut app, Vec::new());
    let marks = screen.click_mark(&mut app, &marks, menu);
    screen.click_mark(&mut app, &marks, &format!("{menu}:{preset}"));
    let marks = screen.frame(&mut app, Vec::new());
    assert!(marks.contains_key(&format!("title:{id}")));
    assert!(!marks.contains_key(&format!("tag:{id}")));
    device(&mut app, id).name = preset.to_uppercase();
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!marks.contains_key(&format!("tag:{id}")));
    let marks = screen.click_mark(&mut app, &marks, &format!("cancel:{id}"));
    assert!(!marks.keys().any(|k| k.ends_with(id)));
}

#[test]
fn on_a_narrow_window_the_tag_shrinks_before_the_badge_and_icons() {
    let long = "Square Golf SGO300A SquareGolf(CD01) in the back bay by the window";
    // The pages' widths of a 480, 400 and 360 px content area, beside
    // the list.
    let nav = *shots::LIST_DEFAULT_WIDTH.start() + 8.0;
    for w in [480.0 + nav, 400.0 + nav, 360.0 + nav] {
        let at = format!("{w}x800");
        let screen = Screen::sized(w, 800.0);
        let mut app = app_with_omni();
        app.actors.get_mut("square.0").unwrap().name = long.into();
        device(&mut app, "square.0").name = long.into();
        // Not named as its type, so the web server's tag shows.
        app.actors.get_mut("webserver.0").unwrap().name = "Dashboard".into();
        screen.frame(&mut app, Vec::new());
        let marks = screen.frame(&mut app, Vec::new());
        for id in ["square.0", "openconnect_client.0", "webserver.0"] {
            assert!(marks.contains_key(&format!("badge:{id}")), "{id} at {at}");
            assert_header_fits(&marks, id, &at);
            if marks.contains_key(&format!("tag:{id}")) {
                assert_tag_by_buttons(&marks, id, "badge", &at);
            }
        }
    }
}

#[test]
fn the_general_section_holds_global_the_web_server_and_simulation() {
    let screen = Screen::one_section(1600.0, 900.0);
    let mut app = app();
    let marks = settled(&screen, &mut app);
    let (global, _) = marks["settings_item:global"];
    let (web, _) = marks["settings_item:webserver.0"];
    let (simulation, _) = marks["settings_item:simulation"];
    assert!(global.bottom() < web.top());
    assert!(web.bottom() < simulation.top());
    let (mevo, _) = marks["settings_item:mevo.0"];
    assert!(simulation.bottom() < mevo.top(), "General leads");
    // General has no (+).
    assert!(!marks.contains_key("add:General"));
}

#[test]
fn the_web_server_settings_side_has_no_remove_and_its_bind_stays_editable() {
    let screen = Screen::new();
    let mut app = app();
    let marks = screen.frame(&mut app, Vec::new());
    assert!(marks.contains_key("card:webserver.0"));
    assert!(!marks.contains_key("remove:webserver.0"));

    // Edited, Save and Cancel, still no Remove.
    integration(&mut app, "webserver.0").address = "0.0.0.0:5881".into();
    integration(&mut app, "webserver.0").dirty = true;
    let marks = screen.frame(&mut app, Vec::new());
    assert!(enabled(&marks, "save:webserver.0"));
    assert!(enabled(&marks, "cancel:webserver.0"));
    assert!(!marks.contains_key("remove:webserver.0"));
    assert_eq!(
        app.settings.build_actor_request("webserver.0").webserver["0"].bind,
        "0.0.0.0:5881"
    );

    // Not even a stray Remove action takes it out.
    let idx = form_index(&app, "webserver.0");
    app.card_action(
        &egui::Context::default(),
        "webserver.0",
        Some(idx),
        CardAction::Remove,
    );
    assert!(app.confirm_remove.is_none());
}

/// Deliver a save reply, as the network callback does, and drain it.
fn reply(app: &mut FlighthookApp, result: Result<(), &str>) {
    let reply = result
        .map(|()| crate::types::PostSettingsResponse::default())
        .map_err(str::to_owned);
    app.pending.lock().unwrap().settings_save = Some(reply);
    app.apply_pending();
}

/// Deliver a `GET /api/settings` reply and drain it.
fn settings_reply(app: &mut FlighthookApp, config: &FlighthookConfig) {
    app.pending.lock().unwrap().settings = Some(config.clone());
    app.apply_pending();
}

fn integration<'a>(app: &'a mut FlighthookApp, id: &str) -> &'a mut settings::IntegrationFormEntry {
    let idx = form_index(app, id);
    match &mut app.settings.actors[idx] {
        ActorFormEntry::Integration(i) => i,
        ActorFormEntry::Device(_) => panic!("{id} is a device"),
    }
}

#[test]
fn a_failed_save_unlocks_its_card_keeps_the_edits_and_says_why() {
    let screen = Screen::new();
    let mut app = app();
    edit_name(&mut app, "openconnect_client.0");
    let marks = screen.frame(&mut app, Vec::new());
    let marks = screen.click_mark(&mut app, &marks, "save:openconnect_client.0");
    assert!(app.settings.saving());
    assert!(!enabled(&marks, "save:openconnect_client.0"));
    assert!(!enabled(&marks, "cancel:openconnect_client.0"));

    // A non-2xx reply, a body that isn't a save response, a network
    // error: all the same failure.
    reply(&mut app, Err("504 the settings were not applied in time"));
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!app.settings.saving());
    assert!(app.settings.actors[form_index(&app, "openconnect_client.0")].dirty());
    assert!(enabled(&marks, "save:openconnect_client.0"));
    assert!(enabled(&marks, "cancel:openconnect_client.0"));
    assert!(marks.contains_key("error:openconnect_client.0"));
    assert!(!marks.contains_key("error:openconnect_client.1"));
    assert!(!enabled(&marks, "nav:shots"), "still locked to the edits");

    // The saved config did not take the edit: Cancel goes back to it.
    let marks = screen.click_mark(&mut app, &marks, "cancel:openconnect_client.0");
    assert_eq!(
        app.settings.actors[form_index(&app, "openconnect_client.0")].name(),
        "Open Connect"
    );
    assert!(!marks.contains_key("error:openconnect_client.0"));
    assert!(enabled(&marks, "nav:shots"));
}

#[test]
fn a_failed_get_started_unlocks_the_wizard() {
    let mut app = FlighthookApp::with_pending(net::new_pending());
    app.settings.load_from(&FlighthookConfig::default());
    app.wizard_picked.insert("GSPro (Open Connect)");
    app.apply_wizard(&egui::Context::default());
    assert!(app.wizard_saving);

    reply(&mut app, Err("Failed to fetch"));
    assert!(!app.wizard_saving);
    assert!(!app.wizard_dismissed);
    assert_eq!(app.wizard_error.as_deref(), Some("Failed to fetch"));
    assert!(!app.settings.saving());

    // Get Started again: saved, and the form holds what it saved.
    app.apply_wizard(&egui::Context::default());
    assert_eq!(app.wizard_error, None);
    reply(&mut app, Ok(()));
    assert!(app.wizard_dismissed);
    assert!(app.settings.loaded, "no reload window");
    let gspro = &app.settings.actors[form_index(&app, "openconnect_client.0")];
    assert!(!gspro.dirty());
    assert!(!app.settings.locked());
}

#[test]
fn a_lost_reply_is_settled_by_reading_the_settings_again() {
    let screen = Screen::new();
    let mut app = app();
    integration(&mut app, "webserver.0").address = "0.0.0.0:5881".into();
    integration(&mut app, "webserver.0").dirty = true;
    let marks = screen.frame(&mut app, Vec::new());
    screen.click_mark(&mut app, &marks, "save:webserver.0");
    let saved_before = app.settings.to_request();

    // The bind change restarts the server answering: the reply is lost.
    reply(&mut app, Err("connection reset"));
    assert!(app.settings.fetch_due(100.0), "the settings are read again");
    assert!(!app.settings.fetch_due(100.5));

    // The server did not apply it: still dirty, still saying why.
    let mut unchanged = saved_before.clone();
    unchanged.webserver.get_mut("0").unwrap().bind = "0.0.0.0:5880".into();
    settings_reply(&mut app, &unchanged);
    assert!(app.settings.actors[form_index(&app, "webserver.0")].dirty());
    assert_eq!(integration(&mut app, "webserver.0").address, "0.0.0.0:5881");
    assert!(app.settings.save_error.is_some());

    // It did: clean, the error gone, the tab unlocked.
    reply_after_retry(&mut app, &screen);
    settings_reply(&mut app, &saved_before);
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!app.settings.locked());
    assert_eq!(app.settings.save_error, None);
    assert!(!marks.contains_key("error:webserver.0"));
    assert!(marks.contains_key("card:webserver.0"));
    assert!(enabled(&marks, "nav:shots"));
    // The saved config, which Cancel goes back to, has the new bind.
    assert_eq!(
        app.settings.build_actor_request("webserver.0"),
        saved_before
    );
}

/// Save the dirty Web Server card again and lose the reply again.
fn reply_after_retry(app: &mut FlighthookApp, screen: &Screen) {
    let marks = screen.frame(app, Vec::new());
    screen.click_mark(app, &marks, "save:webserver.0");
    reply(app, Err("connection reset"));
}

/// The text painted in card `id`'s title.
fn title_text(marks: &Marks, shapes: &[egui::Shape], id: &str) -> String {
    let (title, _) = marks[&format!("title:{id}")];
    shapes
        .iter()
        .find_map(|s| match s {
            egui::Shape::Text(t) if title.contains(t.visual_bounding_rect().center()) => {
                Some(t.galley.text().to_string())
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("no title text for {id}"))
}

#[test]
fn a_rename_shows_on_the_telemetry_side_and_in_another_window_without_a_reload() {
    let id = "openconnect_client.0";
    let screen = Screen::new();
    // Another window, open on the Telemetry side all along.
    let other_screen = Screen::new();
    let mut other = app();
    let mut app = app();
    let old = app.actors[id].name.clone();
    other_screen.frame(&mut other, Vec::new());
    let (marks, shapes) = other_screen.painted(&mut other, Vec::new());
    assert_eq!(title_text(&marks, &shapes, id), old);

    integration(&mut app, id).name = "Bay Sim".into();
    let idx = form_index(&app, id);
    app.settings.actors[idx].set_dirty(true);
    let marks = screen.frame(&mut app, Vec::new());
    screen.click_mark(&mut app, &marks, &format!("save:{id}"));
    reply(&mut app, Ok(()));

    // The rename restarts nothing; its outcome asks every window for the
    // status, and the status carries the new name.
    for app in [&mut app, &mut other] {
        app.needs_status_refresh = false;
        app.handle_ui_message(crate::socket::feed::local_msg(
            "system",
            flighthook::UiLocalEvent::ConfigOutcome {
                request_id: None,
                restarted: Vec::new(),
                stopped: Vec::new(),
                started: Vec::new(),
                config_generation: 0,
            },
        ));
        assert!(app.needs_status_refresh);
        let mut actors = app.actors.clone();
        actors.get_mut(id).unwrap().name = "Bay Sim".into();
        app.pending.lock().unwrap().status = Some(crate::types::StatusResponse {
            cards: actors,
            actors: HashMap::new(),
            turn: None,
            cameras_supported: false,
            trackball_supported: false,
            clip_dir: None,
            config_error: None,
            round: Default::default(),
        });
        app.apply_pending();
    }

    let marks = screen.frame(&mut app, Vec::new());
    assert!(marks.contains_key(&format!("badge:{id}")));
    let (marks, shapes) = screen.painted(&mut app, Vec::new());
    assert_eq!(title_text(&marks, &shapes, id), "Bay Sim");
    let (marks, shapes) = other_screen.painted(&mut other, Vec::new());
    assert_eq!(title_text(&marks, &shapes, id), "Bay Sim");
}

#[test]
fn a_save_locks_every_card_and_the_reply_cleans_only_what_it_posted() {
    let screen = Screen::new();
    let mut app = app();
    edit_name(&mut app, "openconnect_client.0");
    let marks = screen.frame(&mut app, Vec::new());
    let marks = screen.click_mark(&mut app, &marks, "save:openconnect_client.0");
    // Out: nothing takes input, the saving card included.
    assert!(!enabled(&marks, "nav:shots"));
    assert!(!enabled(&marks, "add:Launch monitors"));
    assert!(!enabled(&marks, "add:Software"));
    assert!(!enabled(&marks, "cancel:openconnect_client.0"));
    for id in ["mevo.0", "openconnect_client.1"] {
        assert!(!enabled(&marks, &format!("enabled:{id}")), "{id}");
        assert!(!enabled(&marks, &format!("reconnect:{id}")), "{id}");
    }
    let posted = app.settings.actors[form_index(&app, "openconnect_client.0")]
        .name()
        .to_string();

    // An edit that lands while the save is out stays unsaved.
    edit_name(&mut app, "openconnect_client.0");
    reply(&mut app, Ok(()));
    let entry = &app.settings.actors[form_index(&app, "openconnect_client.0")];
    assert!(entry.dirty());
    assert_eq!(entry.name(), format!("{posted} edited"));
    app.cancel_device(form_index(&app, "openconnect_client.0"));
    assert_eq!(
        app.settings.actors[form_index(&app, "openconnect_client.0")].name(),
        posted
    );
    assert!(!app.settings.locked());
}

#[test]
fn a_removal_waits_for_its_reply_and_a_card_added_meanwhile_stays_unsaved() {
    let screen = Screen::new();
    let mut app = app();
    let marks = screen.frame(&mut app, Vec::new());
    let marks = screen.click_mark(&mut app, &marks, "remove:openconnect_client.0");
    assert!(enabled(&marks, "confirm:remove"));
    let marks = screen.click_mark(&mut app, &marks, "confirm:remove");
    assert!(app.confirm_remove.is_none());
    assert!(app.settings.saving());
    assert!(!enabled(&marks, "add:Software"));
    assert!(!enabled(&marks, "nav:shots"));
    // The card stays until the server has removed it.
    assert!(app.settings.position("openconnect_client.0").is_some());

    // A card that slips in while the removal is out.
    app.add_device(output_preset("GSPro (Open Connect)"));
    reply(&mut app, Ok(()));
    assert_eq!(app.settings.position("openconnect_client.0"), None);
    let new = &app.settings.actors[form_index(&app, "openconnect_client.2")];
    assert!(new.dirty(), "never saved");
    assert!(app.settings.locked());
}

#[test]
fn a_failed_removal_keeps_the_card_and_says_why() {
    let screen = Screen::new();
    let mut app = app();
    let _ = app
        .settings
        .begin_save(SaveTarget::Remove("openconnect_client.1".into()));
    reply(&mut app, Err("Failed to fetch"));
    let marks = screen.frame(&mut app, Vec::new());
    assert!(app.settings.position("openconnect_client.1").is_some());
    assert!(marks.contains_key("error:openconnect_client.1"));
    assert!(enabled(&marks, "remove:openconnect_client.1"));
    assert!(enabled(&marks, "nav:shots"));
}

#[test]
fn the_removal_dialog_removes_by_id_and_waits_for_a_save_out() {
    let screen = Screen::new();
    let mut app = app();
    app.confirm_remove = Some(PendingRemoval {
        id: "openconnect_client.1".into(),
        name: "GSPro".into(),
    });
    // Entries move under the open dialog.
    let moved = app.settings.actors.remove(form_index(&app, "mevo.0"));
    app.settings.actors.push(moved);

    let _ = app
        .settings
        .begin_save(SaveTarget::Actor("openconnect_client.0".into()));
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!enabled(&marks, "confirm:remove"));
    reply(&mut app, Ok(()));

    let marks = screen.frame(&mut app, Vec::new());
    screen.click_mark(&mut app, &marks, "confirm:remove");
    reply(&mut app, Ok(()));
    assert_eq!(app.settings.position("openconnect_client.1"), None);
    for id in ["mevo.0", "openconnect_client.0", "webserver.0"] {
        assert!(app.settings.position(id).is_some(), "{id}");
    }
}

#[test]
fn an_invalid_card_blocks_only_its_own_save() {
    let screen = Screen::new();
    let mut app = app();
    // A hand-edited config: openconnect_client.1 without an address, never opened.
    integration(&mut app, "openconnect_client.1").address = String::new();
    edit_name(&mut app, "openconnect_client.0");
    let marks = screen.frame(&mut app, Vec::new());
    assert!(enabled(&marks, "save:openconnect_client.0"));

    app.settings.simulation.altitude_val = "99999".into();
    app.settings.mark_general(GeneralCard::Simulation);
    let marks = screen.frame(&mut app, Vec::new());
    assert!(enabled(&marks, "save:openconnect_client.0"));
    assert!(!enabled(&marks, "save:simulation"));
}

#[test]
fn add_is_offered_only_once_the_settings_have_loaded() {
    let screen = Screen::new();
    let mut app = FlighthookApp::with_pending(net::new_pending());
    app.settings.fetched_at = Some(f64::MAX);
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!enabled(&marks, "add:Launch monitors"));
    assert!(!enabled(&marks, "add:Software"));
    assert!(!marks.contains_key("save:global"));
}

/// The camera list a server with one USB camera, in use by `camera.1`,
/// and the test pattern returns.
fn camera_list() -> crate::types::CameraListResponse {
    crate::types::CameraListResponse {
        supported: true,
        cameras: vec![
            crate::types::CameraListEntry {
                id: "/dev/v4l/by-id/usb-c920".into(),
                name: "HD Pro Webcam C920".into(),
                usb: Some("046d:085e".into()),
                modes: vec!["1920x1080@30".into(), "1280x720@30".into()],
                in_use_by: Some("camera.1".into()),
                error: None,
            },
            crate::types::CameraListEntry {
                id: "test-pattern".into(),
                name: "Test pattern".into(),
                modes: vec!["640x360@30".into()],
                ..Default::default()
            },
        ],
    }
}

#[test]
fn cameras_get_a_group_whose_add_needs_server_support() {
    let screen = Screen::new();
    let mut app = app();
    let marks = screen.frame(&mut app, Vec::new());
    assert!(
        !marks.contains_key("add:Cameras"),
        "no (+) while the server can't record"
    );
    app.cameras_supported = true;
    let marks = screen.frame(&mut app, Vec::new());
    let (cameras, _) = marks["add:Cameras"];
    let (monitors, _) = marks["add:Launch monitors"];
    let (outputs, _) = marks["add:Software"];
    // In sub-nav order: Devices, Software, Cameras.
    assert!(monitors.top() < outputs.top() && outputs.top() < cameras.top());

    // A camera already configured keeps its card without support.
    let id = app.settings.add_actor("camera").unwrap();
    assert_eq!(id, "camera.0");
    device(&mut app, &id).cam.id = "test-pattern".into();
    let config = app.settings.to_request();
    app.settings = SettingsForm::default();
    app.settings.load_from(&config);
    app.cameras_supported = false;
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!marks.contains_key("add:Cameras"));
    assert!(marks.contains_key(&format!("card:{id}")));
}

/// The launch monitor kinds and the default camera timing each shows in
/// its empty field.
const MONITOR_TIMINGS: [(&str, &str); 6] = [
    ("mevo", "0"),
    ("r10", "2"),
    ("square", "1"),
    ("exputt", "0"),
    ("mock_monitor", "0"),
    ("openconnect_server", "2.5"),
];

/// An app with one saved `kind.0` launch monitor.
fn app_with_monitor(kind: &str) -> FlighthookApp {
    let mut config = FlighthookConfig::default();
    match kind {
        "mevo" => drop(config.mevo.insert("0".into(), Default::default())),
        "r10" => drop(config.r10.insert("0".into(), Default::default())),
        "square" => drop(config.square.insert("0".into(), Default::default())),
        "exputt" => drop(config.exputt.insert("0".into(), Default::default())),
        "mock_monitor" => drop(config.mock_monitor.insert(
            "0".into(),
            flighthook::MockMonitorSection {
                name: "Mock".into(),
                enabled: true,
                actor_id: None,
                impact_lead_s: None,
                driver: Default::default(),
            },
        )),
        _ => drop(
            config
                .openconnect_server
                .insert("0".into(), Default::default()),
        ),
    }
    let mut app = FlighthookApp::with_pending(net::new_pending());
    app.settings.load_from(&config);
    app
}

#[test]
fn camera_timing_is_on_every_launch_monitors_settings_side_only() {
    let screen = Screen::new();
    for (kind, default) in MONITOR_TIMINGS {
        let id = format!("{kind}.0");
        let mark = format!("camera_timing:{id}");
        let mut app = app_with_monitor(kind);
        let (marks, shapes) = screen.painted(&mut app, Vec::new());
        let (field, _) = marks[&mark];
        let (card, _) = marks[&format!("card:{id}")];
        // Mevo's long settings scroll inside the card; the others fit.
        assert!(
            card.x_range().contains(field.left()) && card.x_range().contains(field.right()),
            "{kind}: {field:?} in {card:?}"
        );
        if kind != "mevo" {
            assert!(card.contains_rect(field), "{kind}: {field:?} in {card:?}");
        }
        // Empty: the device's default is shown in the field itself.
        let texts: Vec<String> = shapes
            .iter()
            .filter_map(|s| match s {
                egui::Shape::Text(t) if field.contains(t.visual_bounding_rect().center()) => {
                    Some(t.galley.text().to_string())
                }
                _ => None,
            })
            .collect();
        // (Mevo's field is scrolled out of the card, so not painted.)
        if kind != "mevo" {
            assert_eq!(texts, [default], "{kind}");
        }
    }
    // Not on a camera or an output.
    let mut app = app();
    let marks = screen.frame(&mut app, Vec::new());
    assert!(marks.contains_key("camera_timing:mevo.0"));
    assert!(
        !marks
            .keys()
            .any(|k| k.starts_with("camera_timing:") && !k.contains("mevo")),
        "{:?}",
        marks.keys().collect::<Vec<_>>()
    );
    let mut app = app_with_monitor("mevo");
    app.cameras_supported = true;
    app.camera_list.list = Some(camera_list());
    let marks = screen.frame(&mut app, Vec::new());
    let marks = screen.click_mark(&mut app, &marks, "add:Cameras");
    let _ = screen.click_mark(&mut app, &marks, "add:Cameras:Camera");
    let marks = screen.frame(&mut app, Vec::new());
    assert!(marks.contains_key("camera_pick:camera.0"));
    assert!(!marks.contains_key("camera_timing:camera.0"));
}

#[test]
fn camera_timing_saves_and_is_validated() {
    let screen = Screen::new();
    for (kind, _) in MONITOR_TIMINGS {
        let id = format!("{kind}.0");
        let mut app = app_with_monitor(kind);
        let lead = |app: &FlighthookApp| {
            app.settings
                .build_actor_request(&id)
                .configured_impact_lead_s(&id)
        };
        // Blank: the device's default, nothing saved.
        assert_eq!(lead(&app), None, "{kind}");

        for (typed, saved) in [("0", 0.0), (" 1.25 ", 1.25), ("5", 5.0)] {
            device(&mut app, &id).impact_lead = typed.into();
            device(&mut app, &id).dirty = true;
            let marks = screen.frame(&mut app, Vec::new());
            assert!(enabled(&marks, &format!("save:{id}")), "{kind} {typed}");
            assert_eq!(lead(&app), Some(saved), "{kind} {typed}");
        }
        for bad in ["-0.1", "5.1", "NaN", "inf", "soon"] {
            device(&mut app, &id).impact_lead = bad.into();
            let marks = screen.frame(&mut app, Vec::new());
            assert!(!enabled(&marks, &format!("save:{id}")), "{kind} {bad}");
        }
        device(&mut app, &id).impact_lead = String::new();
        let marks = screen.frame(&mut app, Vec::new());
        assert!(enabled(&marks, &format!("save:{id}")), "{kind}: blank");
        assert_eq!(lead(&app), None);
    }
    // A saved value loads back into the field.
    let mut config = FlighthookConfig::default();
    config.square.insert(
        "0".into(),
        flighthook::SquareSection {
            impact_lead_s: Some(0.5),
            ..Default::default()
        },
    );
    let mut app = FlighthookApp::with_pending(net::new_pending());
    app.settings.load_from(&config);
    assert_eq!(device(&mut app, "square.0").impact_lead, "0.5");
}

#[test]
fn the_r10_aim_offset_is_on_its_settings_side_only() {
    let screen = Screen::new();
    let mut app = app_with_monitor("r10");
    let (marks, shapes) = screen.painted(&mut app, Vec::new());
    let (field, _) = marks["aim_offset:0"];
    let (card, _) = marks["card:r10.0"];
    assert!(card.contains_rect(field), "{field:?} in {card:?}");
    // Empty: the hint `0` is shown in the field, and the unit beside it.
    let texts: Vec<String> = shapes
        .iter()
        .filter_map(|s| match s {
            egui::Shape::Text(t) if field.contains(t.visual_bounding_rect().center()) => {
                Some(t.galley.text().to_string())
            }
            _ => None,
        })
        .collect();
    assert_eq!(texts, ["0"]);
    assert!(
        shapes.iter().any(|s| matches!(s, egui::Shape::Text(t)
                if t.galley.text() == "°" && t.visual_bounding_rect().left() >= field.right())),
        "the degree sign sits right of the field"
    );

    // Not on other launch monitors.
    for kind in ["square", "exputt", "mock_monitor"] {
        let mut app = app_with_monitor(kind);
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!marks.contains_key("aim_offset:0"), "{kind}");
    }
}

#[test]
fn the_r10_aim_offset_saves_and_shows_the_drivers_refusal() {
    let screen = Screen::new();
    let mut app = app_with_monitor("r10");
    let aim = |app: &FlighthookApp| {
        app.settings.build_actor_request("r10.0").r10["0"]
            .driver
            .aim_offset_deg
    };
    assert_eq!(aim(&app), None, "blank: unset");
    for (typed, saved) in [("0", 0.0), (" -1.25 ", -1.25), ("10", 10.0)] {
        device(&mut app, "r10.0").aim_offset = typed.into();
        device(&mut app, "r10.0").dirty = true;
        let marks = screen.frame(&mut app, Vec::new());
        assert!(enabled(&marks, "save:r10.0"), "{typed}");
        assert_eq!(aim(&app), Some(saved), "{typed}");
    }
    for bad in ["10.1", "-11", "NaN", "inf", "left"] {
        device(&mut app, "r10.0").aim_offset = bad.into();
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!enabled(&marks, "save:r10.0"), "{bad}");
        assert!(
            marks.contains_key("driver_error:r10.0"),
            "{bad}: the refusal shows"
        );
    }
    device(&mut app, "r10.0").aim_offset = String::new();
    let marks = screen.frame(&mut app, Vec::new());
    assert!(enabled(&marks, "save:r10.0"), "blank");
    assert_eq!(aim(&app), None);
}

/// An app with one saved test-pattern `camera.0`.
fn app_with_camera() -> FlighthookApp {
    let mut config = FlighthookConfig::default();
    config.camera.insert(
        "0".into(),
        flighthook::CameraSection {
            camera: flighthook::CameraSection::TEST_PATTERN.into(),
            ..Default::default()
        },
    );
    let mut app = FlighthookApp::with_pending(net::new_pending());
    app.settings.load_from(&config);
    app
}

#[test]
fn latency_is_on_a_camera_cards_settings_side_only() {
    let screen = Screen::new();
    let mark = "camera_latency:camera.0";
    let mut cam = app_with_camera();
    let (marks, shapes) = screen.painted(&mut cam, Vec::new());
    let (field, _) = marks[mark];
    let (card, _) = marks["card:camera.0"];
    assert!(card.contains_rect(field), "{field:?} in {card:?}");
    // Empty: the hint `0` is shown in the field itself.
    let texts: Vec<String> = shapes
        .iter()
        .filter_map(|s| match s {
            egui::Shape::Text(t) if field.contains(t.visual_bounding_rect().center()) => {
                Some(t.galley.text().to_string())
            }
            _ => None,
        })
        .collect();
    assert_eq!(texts, ["0"]);

    // Not on a launch monitor or an output.
    let mut others = app();
    let marks = screen.frame(&mut others, Vec::new());
    assert!(
        !marks.keys().any(|k| k.starts_with("camera_latency:")),
        "{:?}",
        marks.keys().collect::<Vec<_>>()
    );
    for (kind, _) in MONITOR_TIMINGS {
        let mut app = app_with_monitor(kind);
        let marks = screen.frame(&mut app, Vec::new());
        assert!(
            !marks.keys().any(|k| k.starts_with("camera_latency:")),
            "{kind}"
        );
    }
}

#[test]
fn camera_latency_saves_and_is_validated() {
    let screen = Screen::new();
    let id = "camera.0";
    let mut app = app_with_camera();
    let latency = |app: &FlighthookApp| app.settings.build_actor_request(id).camera["0"].latency_s;
    // Blank: no latency, nothing saved.
    assert_eq!(latency(&app), None);

    for (typed, saved) in [("0", 0.0), (" 1.25 ", 1.25), ("10", 10.0)] {
        device(&mut app, id).cam.latency = typed.into();
        device(&mut app, id).dirty = true;
        let marks = screen.frame(&mut app, Vec::new());
        assert!(enabled(&marks, &format!("save:{id}")), "{typed}");
        assert_eq!(latency(&app), Some(saved), "{typed}");
    }
    for bad in ["-0.1", "10.1", "NaN", "inf", "soon"] {
        device(&mut app, id).cam.latency = bad.into();
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!enabled(&marks, &format!("save:{id}")), "{bad}");
    }
    device(&mut app, id).cam.latency = String::new();
    let marks = screen.frame(&mut app, Vec::new());
    assert!(enabled(&marks, &format!("save:{id}")), "blank");
    assert_eq!(latency(&app), None);

    // A saved value loads back into the field.
    let mut config = FlighthookConfig::default();
    config.camera.insert(
        "0".into(),
        flighthook::CameraSection {
            camera: flighthook::CameraSection::TEST_PATTERN.into(),
            latency_s: Some(2.5),
            ..Default::default()
        },
    );
    let mut app = FlighthookApp::with_pending(net::new_pending());
    app.settings.load_from(&config);
    assert_eq!(device(&mut app, id).cam.latency, "2.5");
}

#[test]
fn rotation_yaw_and_pitch_are_on_a_camera_card_save_and_are_validated() {
    let screen = Screen::new();
    let id = "camera.0";
    let mut app = app_with_camera();
    let (marks, shapes) = screen.painted(&mut app, Vec::new());
    let (card, _) = marks["card:camera.0"];
    for mark in ["camera_rotation", "camera_yaw", "camera_pitch"] {
        assert!(marks.contains_key(&format!("{mark}:{id}")), "{mark}");
    }
    for r in [0, 90, 180, 270] {
        let (pill, _) = marks[&format!("camera_rotation:{id}:{r}")];
        assert!(card.left() <= pill.left() && pill.right() <= card.right());
    }
    // Rotation right under the camera, in sight without scrolling at
    // the fixed card height; Yaw and Pitch under Latency.
    let (camera, _) = marks[&format!("camera_pick:{id}")];
    let (latency, _) = marks[&format!("camera_latency:{id}")];
    let (rotation, _) = marks[&format!("camera_rotation:{id}")];
    let (yaw, _) = marks[&format!("camera_yaw:{id}")];
    let (pitch, _) = marks[&format!("camera_pitch:{id}")];
    assert!(camera.bottom() <= rotation.top());
    assert!(rotation.bottom() <= latency.top());
    assert!(latency.bottom() <= yaw.top());
    assert!(yaw.bottom() <= pitch.top());
    let (body, _, offset) = screen.bodies()[id];
    assert_eq!(offset, 0.0, "not scrolled");
    assert!(
        body.top() <= rotation.top() && rotation.bottom() <= body.bottom(),
        "{rotation:?} in {body:?}"
    );
    drop(shapes);
    // The card's body scrolls to the rows under Latency.
    let wheel = |dy| Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: vec2(0.0, dy),
        modifiers: Modifiers::NONE,
    };
    screen.frame(
        &mut app,
        vec![Event::PointerMoved(card.center()), wheel(-1000.0)],
    );
    // The yaw's sign, in words, under it.
    let hint = "Positive: the camera points to the right of the target line for a right-hander";
    assert!(screen.texts(&mut app).iter().any(|t| t == hint));
    let section = |app: &FlighthookApp| app.settings.build_actor_request(id).camera["0"].clone();
    assert_eq!(section(&app).rotation_deg, 0);
    assert_eq!(
        (section(&app).yaw_deg, section(&app).pitch_deg),
        (None, None)
    );

    // A rotation pill (back at the top of the body): the card turns
    // dirty and saves it.
    screen.frame(
        &mut app,
        vec![Event::PointerMoved(card.center()), wheel(1000.0)],
    );
    let marks = screen.frame(&mut app, Vec::new());
    let marks = screen.click_mark(&mut app, &marks, &format!("camera_rotation:{id}:90"));
    assert_eq!(device(&mut app, id).cam.rotation, 90);
    assert!(enabled(&marks, &format!("save:{id}")));
    assert_eq!(section(&app).rotation_deg, 90);
    let json = serde_json::to_value(section(&app)).unwrap();
    assert_eq!(json["rotation_deg"], 90);
    assert!(json.get("yaw_deg").is_none());

    for (typed, saved) in [("-30", -30.0), (" 7.5 ", 7.5), ("30", 30.0)] {
        device(&mut app, id).cam.yaw = typed.into();
        device(&mut app, id).cam.pitch = typed.into();
        let marks = screen.frame(&mut app, Vec::new());
        assert!(enabled(&marks, &format!("save:{id}")), "{typed}");
        assert_eq!(section(&app).yaw_deg, Some(saved), "{typed}");
        assert_eq!(section(&app).pitch_deg, Some(saved), "{typed}");
    }
    let range = "Must be a number, -30 to 30";
    for bad in ["-30.5", "31", "NaN", "level"] {
        for field in ["yaw", "pitch"] {
            device(&mut app, id).cam.yaw = String::new();
            device(&mut app, id).cam.pitch = String::new();
            match field {
                "yaw" => device(&mut app, id).cam.yaw = bad.into(),
                _ => device(&mut app, id).cam.pitch = bad.into(),
            }
            screen.frame(
                &mut app,
                vec![Event::PointerMoved(card.center()), wheel(-1000.0)],
            );
            let texts = screen.texts(&mut app);
            let marks = screen.frame(&mut app, Vec::new());
            assert!(!enabled(&marks, &format!("save:{id}")), "{field} {bad}");
            assert_eq!(
                texts.iter().filter(|t| *t == range).count(),
                1,
                "{field} {bad}"
            );
        }
    }
    device(&mut app, id).cam.yaw = String::new();
    device(&mut app, id).cam.pitch = String::new();
    // A rotation from a hand-edited file that is not a quarter turn: the
    // error under Rotation, at the top of the body.
    device(&mut app, id).cam.rotation = 45;
    screen.frame(
        &mut app,
        vec![Event::PointerMoved(card.center()), wheel(1000.0)],
    );
    let texts = screen.texts(&mut app);
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!enabled(&marks, &format!("save:{id}")));
    assert!(texts.iter().any(|t| t == "0°, 90°, 180° or 270°"));

    // Saved values load back into the fields.
    let mut config = FlighthookConfig::default();
    config.camera.insert(
        "0".into(),
        flighthook::CameraSection {
            camera: flighthook::CameraSection::TEST_PATTERN.into(),
            rotation_deg: 270,
            yaw_deg: Some(-6.0),
            pitch_deg: Some(12.5),
            ..Default::default()
        },
    );
    let mut app = FlighthookApp::with_pending(net::new_pending());
    app.settings.load_from(&config);
    let cam = &device(&mut app, id).cam;
    assert_eq!(
        (cam.rotation, cam.yaw.as_str(), cam.pitch.as_str()),
        (270, "-6", "12.5")
    );
    assert_eq!(app.settings.saved_rotation(id), Some(270));
    assert_eq!(app.settings.saved_rotation("camera.9"), None);
}

#[test]
fn an_added_camera_needs_a_camera_from_the_list() {
    let screen = Screen::new();
    let mut app = app();
    app.cameras_supported = true;
    app.camera_list.list = Some(camera_list());
    let marks = screen.frame(&mut app, Vec::new());
    let marks = screen.click_mark(&mut app, &marks, "add:Cameras");
    let marks = screen.click_mark(&mut app, &marks, "add:Cameras:Camera");
    let id = "camera.0";
    let idx = form_index(&app, id);
    assert_eq!(app.settings.actors[idx].name(), "Camera");
    assert_eq!(app.settings.actors[idx].type_label(), "Camera");
    assert!(marks.contains_key(&format!("camera_pick:{id}")));
    assert!(marks.contains_key(&format!("camera_mode:{id}")));
    assert!(!enabled(&marks, &format!("save:{id}")), "no camera yet");

    // Picking from the list, as the picker does.
    let entry = camera_list().cameras[0].clone();
    let dev = device(&mut app, id);
    dev.cam.id = entry.id.clone();
    dev.cam.name = Some(entry.name.clone());
    dev.cam.position = crate::types::CameraPosition::TargetLine;
    dev.cam.mode = "1280x720@30".into();
    let marks = screen.frame(&mut app, Vec::new());
    assert!(enabled(&marks, &format!("save:{id}")));
    let section = &app.settings.build_actor_request(id).camera["0"];
    assert_eq!(section.camera, "/dev/v4l/by-id/usb-c920");
    assert_eq!(section.camera_name.as_deref(), Some("HD Pro Webcam C920"));
    assert_eq!(section.position, crate::types::CameraPosition::TargetLine);
    assert_eq!(section.mode, "1280x720@30");

    // A mode that doesn't parse can't be saved.
    device(&mut app, id).cam.mode = "fast".into();
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!enabled(&marks, &format!("save:{id}")));
}

#[test]
fn the_camera_list_loads_with_a_camera_cards_settings_and_on_refresh() {
    let screen = Screen::new();
    let mut app = app();
    let _ = app.settings.add_actor("camera");
    screen.frame(&mut app, Vec::new());
    assert!(app.camera_list.loading, "asked for once a card needs it");
    // The reply lands.
    app.pending.lock().unwrap().cameras = Some(Ok(camera_list()));
    app.apply_pending();
    assert!(!app.camera_list.loading);
    assert_eq!(app.camera_list.list, Some(camera_list()));
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!app.camera_list.loading, "loaded: not asked again");
    let marks = screen.click_mark(&mut app, &marks, "camera_refresh:camera.0");
    let _ = marks;
    assert!(app.camera_list.loading, "refresh asks again");
    app.pending.lock().unwrap().cameras = Some(Err("504 Gateway Timeout".into()));
    app.apply_pending();
    assert_eq!(
        app.camera_list.error.as_deref(),
        Some("504 Gateway Timeout")
    );
    assert_eq!(
        app.camera_list.list,
        Some(camera_list()),
        "the last list stays"
    );
}

/// The outputs' routing pickers offer the launch monitors: no camera
/// and no Pureplay Accessory card.
#[test]
fn the_routing_pickers_offer_no_camera_and_no_pureplay_card() {
    let mut app = app();
    let _ = app.settings.add_actor("camera");
    let _ = app.settings.add_actor("pureplay_accessory");
    let _ = app.settings.add_actor("square");
    // Routing names a card by its actor id, which the saved config
    // holds.
    let mut saved = app.settings.to_request();
    let mut n = 0;
    let _ = saved.fill_actor_ids(|| {
        n += 1;
        format!("actor-{n}")
    });
    let ids: Vec<String> = routing_monitors(&app.settings.actors, Some(&saved))
        .into_iter()
        .map(|(id, _)| saved.card_of_actor(&id).expect("a card"))
        .collect();
    assert_eq!(ids, ["mevo.0", "square.0"]);
    assert!(
        app.settings
            .actors
            .iter()
            .any(|a| a.global_id() == "pureplay_accessory.0")
    );
}

#[test]
fn cameras_are_not_routing_monitors() {
    let screen = Screen::new();
    let mut app = app();
    let _ = app.settings.add_actor("camera");
    let mut config = app.settings.to_request();
    config.camera.get_mut("0").unwrap().camera = "test-pattern".into();
    app.settings = SettingsForm::default();
    app.settings.load_from(&config);
    // The routing dropdowns list the Mevo only.
    let monitors: Vec<String> = app
        .settings
        .actors
        .iter()
        .filter_map(|a| match a {
            ActorFormEntry::Device(d) if !d.is_camera() => Some(a.global_id()),
            _ => None,
        })
        .collect();
    assert_eq!(monitors, ["mevo.0"]);
    let marks = screen.frame(&mut app, Vec::new());
    assert!(marks.contains_key("card:camera.0"));
}

#[test]
fn a_running_cameras_card_fetches_and_shows_a_still_frame() {
    use stance::{Source, TestPattern, TestPatternConfig};

    let screen = Screen::one_section(1600.0, 1000.0);
    let mut app = app();
    app.select_item(
        SettingsSection::Cameras,
        SettingsItem::Card("camera.0".into()),
    );
    let _ = app.settings.add_actor("camera");
    let mut config = app.settings.to_request();
    config.camera.get_mut("0").unwrap().camera = "test-pattern".into();
    app.settings = SettingsForm::default();
    app.settings.load_from(&config);
    let id = "camera.0";
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!enabled(&marks, &format!("still:{id}")), "not running");

    app.actors.insert(
        id.into(),
        ActorStatusResponse::new("Camera", ActorStatus::Connected),
    );
    let marks = screen.frame(&mut app, Vec::new());
    let marks = screen.click_mark(&mut app, &marks, &format!("still:{id}"));
    assert!(
        !enabled(&marks, &format!("still:{id}")),
        "waiting for the frame"
    );
    assert!(app.stills[id].loading);

    let mut pattern = TestPattern::new(TestPatternConfig {
        width: 160,
        height: 90,
        paced: false,
        ..TestPatternConfig::default()
    })
    .unwrap();
    let frame = pattern
        .next_frame(std::time::Duration::ZERO)
        .unwrap()
        .unwrap();
    app.pending
        .lock()
        .unwrap()
        .stills
        .push((id.into(), Ok(frame.jpeg.to_vec())));
    app.apply_pending();
    let marks = screen.frame(&mut app, Vec::new());
    assert!(enabled(&marks, &format!("still:{id}")));
    let (image, _) = marks[&format!("still_image:{id}")];
    let (card, _) = marks[&format!("card:{id}")];
    assert!(card.contains_rect(image), "in its page");
    assert!((image.height() / image.width() - 90.0 / 160.0).abs() < 0.02);
    // Under the telemetry block, at its width; the block keeps its
    // height.
    let (telemetry, _, _) = screen.bodies()[&format!("{id}.telemetry")];
    assert!(image.top() >= telemetry.bottom());
    assert!(
        image.width() > telemetry.width() * 0.8,
        "at the block's width"
    );
    assert!((telemetry.height() - TELEMETRY_HEIGHT).abs() < 0.5);
}

#[test]
fn the_body_tracking_card_picks_the_mode_and_the_model_saved_with_the_global_settings() {
    use flighthook::{BodyTrackingMode, PoseModel};
    let screen = Screen::new();
    let mut app = app();
    assert_eq!(
        app.settings.body_tracking.mode,
        BodyTrackingMode::Off,
        "off by default"
    );
    // Saved on: changing the mode is an edit, saved with Save (turning
    // it on from Off is the download prompt's, see body_models).
    let mut saved = app.settings.to_request();
    saved.body_tracking.min_score = 0.45;
    saved.body_tracking.mode = BodyTrackingMode::Historical;
    app.settings.load_from(&saved);
    let marks = screen.frame(&mut app, Vec::new());
    assert!(marks.contains_key("card:body_tracking"));
    assert!(marks.contains_key("body_tracking_model"));
    assert!(!marks.contains_key("body_tracking_enabled"));
    assert!(!marks.contains_key("save:body_tracking"));
    // Off | Historical | Live, left to right in one row, Historical as
    // saved.
    let pills: Vec<egui::Rect> = BodyTrackingMode::all()
        .map(|m| marks[&format!("body_tracking_mode:{}", m.key())].0)
        .to_vec();
    assert!(pills[0].right() <= pills[1].left() && pills[1].right() <= pills[2].left());
    assert!(
        pills
            .iter()
            .all(|p| (p.center().y - pills[0].center().y).abs() < 0.5)
    );
    let (control, _) = marks["body_tracking_mode"];
    assert!(pills.iter().all(|p| control.contains_rect(*p)));
    assert_eq!(
        app.settings.body_tracking.mode,
        BodyTrackingMode::Historical
    );
    // Each mode, and the lighter model: Save on this card alone.
    for mode in [
        BodyTrackingMode::Live,
        BodyTrackingMode::Off,
        BodyTrackingMode::Historical,
        BodyTrackingMode::Off,
    ] {
        let marks = screen.frame(&mut app, Vec::new());
        screen.click_mark(
            &mut app,
            &marks,
            &format!("body_tracking_mode:{}", mode.key()),
        );
        assert_eq!(app.settings.body_tracking.mode, mode);
    }
    app.settings.body_tracking.model = PoseModel::RtmposeS;
    let marks = screen.frame(&mut app, Vec::new());
    assert!(enabled(&marks, "save:body_tracking"));
    assert!(!marks.contains_key("save:replay"));
    assert!(!marks.contains_key("save:global"));
    let request = app.settings.build_global_request();
    assert_eq!(request.body_tracking.mode, BodyTrackingMode::Off);
    assert_eq!(request.body_tracking.model, PoseModel::RtmposeS);
    assert_eq!(
        request.body_tracking.min_score, 0.45,
        "the other fields as loaded"
    );
    // Off is the default: left out, and no `enabled`.
    let json = serde_json::to_string(&request.body_tracking).unwrap();
    assert_eq!(json, r#"{"model":"rtmpose-s","min_score":0.45}"#);
    // Cancel puts it back.
    let marks = screen.click_mark(&mut app, &marks, "cancel:body_tracking");
    assert!(!marks.contains_key("cancel:body_tracking"));
    assert_eq!(
        app.settings.body_tracking.mode,
        BodyTrackingMode::Historical
    );
    assert_eq!(app.settings.body_tracking.model, PoseModel::RtmposeM);
}

/// The Body tracking card's fields, top to bottom.
const BODY_TRACKING_FIELDS: [&str; 6] = [
    "body_tracking_mode",
    "body_tracking_model",
    "body_tracking_skeleton",
    "body_tracking_min_score",
    "body_tracking_smoothing",
    "body_tracking_detect_every",
];

/// Click text field `name`, select its text and type `text` over it.
fn type_over(screen: &Screen, app: &mut FlighthookApp, name: &str, text: &str) {
    let marks = screen.frame(app, Vec::new());
    screen.click_mark(app, &marks, name);
    let select_all = Event::Key {
        key: Key::A,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::COMMAND,
    };
    screen.frame(app, vec![select_all, Event::Text(text.into())]);
}

#[test]
fn the_body_tracking_card_shows_every_setting_in_order_in_every_mode() {
    use flighthook::BodyTrackingMode;
    let screen = Screen::new();
    let mut app = app();
    for mode in BodyTrackingMode::all() {
        let mut saved = app.settings.to_request();
        saved.body_tracking.mode = mode;
        app.settings.load_from(&saved);
        let marks = screen.frame(&mut app, Vec::new());
        let (card, _) = marks["card:body_tracking"];
        let rows: Vec<Rect> = BODY_TRACKING_FIELDS.iter().map(|f| marks[*f].0).collect();
        for (pair, names) in rows.windows(2).zip(BODY_TRACKING_FIELDS.windows(2)) {
            assert!(
                pair[0].bottom() <= pair[1].top() + 0.5,
                "{mode:?}: {} above {}",
                names[0],
                names[1]
            );
        }
        assert!(
            rows.iter().all(|r| card.contains_rect(*r)),
            "{mode:?}: every field fits the card"
        );
        for field in BODY_TRACKING_FIELDS {
            assert!(enabled(&marks, field), "{mode:?}: {field}");
        }
        let (body, content, _) = screen.bodies()["body_tracking"];
        assert!(
            content <= body.height(),
            "{mode:?}: the body fits without scrolling ({content} in {})",
            body.height()
        );
    }
    // At the zooms the UI picks, too.
    for ppp in [0.8, 1.25, 1.5, 1.875, 2.0, 2.5, 3.0] {
        screen.0.set_pixels_per_point(ppp);
        screen.frame(&mut app, Vec::new());
        screen.frame(&mut app, Vec::new());
        let (body, content, _) = screen.bodies()["body_tracking"];
        assert!(
            content <= body.height(),
            "ppp {ppp}: {content} in {}",
            body.height()
        );
    }
    screen.0.set_pixels_per_point(1.0);
    // As loaded: the defaults, the skeleton shown.
    let form = &app.settings.body_tracking;
    assert!(form.show_skeleton);
    assert_eq!(
        (
            form.min_score.as_str(),
            form.smoothing.as_str(),
            form.detect_every.as_str()
        ),
        ("0.3", "1", "10")
    );
}

#[test]
fn the_body_tracking_card_saves_the_skeleton_and_the_tuning_values() {
    use flighthook::{BodyTrackingMode, BodyTrackingSection};
    let screen = Screen::one_section(1600.0, 1600.0);
    let mut app = app();
    app.select_item(SettingsSection::Cameras, SettingsItem::BodyTracking);
    let mut saved = app.settings.to_request();
    saved.body_tracking.mode = BodyTrackingMode::Historical;
    app.settings.load_from(&saved);
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!marks.contains_key("save:body_tracking"));
    // Hide the skeleton and type the three values.
    let marks = screen.click_mark(&mut app, &marks, "body_tracking_skeleton");
    assert!(!app.settings.body_tracking.show_skeleton);
    assert!(enabled(&marks, "save:body_tracking"));
    type_over(&screen, &mut app, "body_tracking_min_score", "0.45");
    type_over(&screen, &mut app, "body_tracking_smoothing", "0.5");
    type_over(&screen, &mut app, "body_tracking_detect_every", "30");
    let form = &app.settings.body_tracking;
    assert_eq!(
        (
            form.min_score.as_str(),
            form.smoothing.as_str(),
            form.detect_every.as_str()
        ),
        ("0.45", "0.5", "30")
    );
    let marks = screen.frame(&mut app, Vec::new());
    assert!(enabled(&marks, "save:body_tracking"));
    assert!(!marks.contains_key("save:global"));
    let wanted = BodyTrackingSection {
        mode: BodyTrackingMode::Historical,
        min_score: 0.45,
        smoothing: 0.5,
        detect_every: 30,
        show_skeleton: false,
        ..BodyTrackingSection::default()
    };
    assert_eq!(app.settings.build_global_request().body_tracking, wanted);
    // Out of range, at the ranges the server holds: the range under the
    // field and no Save.
    let texts = |shapes: &[egui::Shape]| -> Vec<String> {
        shapes
            .iter()
            .filter_map(|s| match s {
                egui::Shape::Text(t) => Some(t.galley.text().to_owned()),
                _ => None,
            })
            .collect()
    };
    for (field, bad, range) in [
        ("min_score", "1.5", "0 to 1"),
        ("min_score", "x", "0 to 1"),
        ("smoothing", "0.05", "0, or 0.1 to 10 Hz"),
        ("smoothing", "11", "0, or 0.1 to 10 Hz"),
        ("detect_every", "0", "1 to 60"),
        ("detect_every", "61", "1 to 60"),
        ("detect_every", "2.5", "1 to 60"),
    ] {
        let form = &mut app.settings.body_tracking;
        let text = match field {
            "min_score" => &mut form.min_score,
            "smoothing" => &mut form.smoothing,
            _ => &mut form.detect_every,
        };
        let good = std::mem::replace(text, bad.to_owned());
        // The range row lengthens the body: scrolled to its end.
        let marks = screen.frame(&mut app, Vec::new());
        let (card, _) = marks["card:body_tracking"];
        let wheel = |dy| Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, dy),
            modifiers: Modifiers::NONE,
        };
        screen.frame(
            &mut app,
            vec![Event::PointerMoved(card.center()), wheel(-500.0)],
        );
        let (marks, shapes) = screen.painted(&mut app, Vec::new());
        screen.frame(&mut app, vec![wheel(500.0)]);
        assert!(!enabled(&marks, "save:body_tracking"), "{field} {bad}");
        assert!(texts(&shapes).contains(&range.to_owned()), "{field} {bad}");
        let form = &mut app.settings.body_tracking;
        match field {
            "min_score" => form.min_score = good,
            "smoothing" => form.smoothing = good,
            _ => form.detect_every = good,
        }
    }
    // Smoothing 0 is off, and valid.
    app.settings.body_tracking.smoothing = "0".into();
    assert_eq!(app.settings.body_tracking.smoothing_value(), Some(0.0));
    app.settings.body_tracking.smoothing = "0.5".into();
    let (_, shapes) = screen.painted(&mut app, Vec::new());
    assert!(!texts(&shapes).contains(&"0, or 0.1 to 10 Hz".to_owned()));
    // Saved: the card reads the saved values back, clean.
    let posted = app.settings.begin_save(SaveTarget::Global);
    assert_eq!(posted.body_tracking, wanted);
    assert_eq!(
        serde_json::to_string(&posted.body_tracking).unwrap(),
        r#"{"mode":"historical","min_score":0.45,"smoothing":0.5,"detect_every":30,"show_skeleton":false}"#
    );
    assert_eq!(app.settings.finish_save(Ok(())), None);
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!marks.contains_key("save:body_tracking"));
    assert_eq!(app.settings.saved_body_tracking(), wanted);
    assert_eq!(app.body_tracking(), wanted);
    let form = &app.settings.body_tracking;
    assert!(!form.show_skeleton);
    assert_eq!(
        (
            form.min_score.as_str(),
            form.smoothing.as_str(),
            form.detect_every.as_str()
        ),
        ("0.45", "0.5", "30")
    );
    assert_eq!(form.to_section(), Some(wanted));
    // Turned back on: shown again, and the field left out at its default.
    let marks = screen.click_mark(&mut app, &marks, "body_tracking_skeleton");
    assert!(app.settings.body_tracking.show_skeleton);
    assert!(enabled(&marks, "save:body_tracking"));
    let json = serde_json::to_string(&app.settings.build_global_request().body_tracking);
    assert!(!json.unwrap().contains("show_skeleton"));
}

/// The Golfers card: Add, Patch (edit the shown golfer), Remove and
/// its own Save (`scope=golfers`), apart from the General cards' save.
#[test]
fn the_golfers_card_adds_edits_and_removes_golfers_with_its_own_save() {
    let screen = Screen::one_section(1600.0, 1600.0);
    let mut app = app();
    app.settings_section = SettingsSection::Golfers;
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!marks.contains_key("card:golfers"), "no golfers");
    assert!(!marks.contains_key("card:player"));
    assert!(!marks.contains_key("settings_item:golfer.0"));
    // (+): a new golfer, its row and page shown, unsaved.
    let marks = screen.click_mark(&mut app, &marks, "add:Golfers");
    assert!(marks.contains_key("settings_item:golfer.0"));
    assert!(marks.contains_key("card:golfers"));
    assert_eq!(app.settings.golfers.golfers.len(), 1);
    assert!(enabled(&marks, "save:golfers"));
    assert!(!marks.contains_key("save:global"));
    for key in [
        "golfer_name",
        "golfer_sex:female",
        "golfer_height",
        "golfer_bag:DR",
    ] {
        assert!(marks.contains_key(key), "{key}");
    }
    // A new golfer shows Right-handed.
    assert!(marks.contains_key("golfer_hand:rh"));
    assert!(marks.contains_key("golfer_hand:lh"));
    let marks = screen.click_mark(&mut app, &marks, "golfer_sex:female");
    let marks = screen.click_mark(&mut app, &marks, "golfer_bag:MD");
    let marks = screen.click_mark(&mut app, &marks, "golfer_bag:DR");
    app.settings.golfers.golfers[0].record.name = "Eric".into();
    app.settings.golfers.golfers[0].height.text = "71".into();
    screen.frame(&mut app, Vec::new());
    assert!(enabled(&marks, "save:golfers"));
    let posted = app.settings.begin_save(SaveTarget::Golfers);
    assert_eq!(SaveTarget::Golfers.scope(), Some("golfers"));
    let g = &posted.golfers[0];
    assert_eq!(g.name, "Eric");
    assert_eq!(g.handed, None, "no hand picked");
    assert_eq!(g.sex, Some(flighthook::Sex::Female));
    assert_eq!(
        g.bag,
        [flighthook::Club::Driver, flighthook::Club::MiniDriver],
        "bag order"
    );
    assert_eq!(g.height, Some(180.3));
    assert!(g.identifiers.is_empty(), "the server gives the id");
    app.settings.finish_save(Ok(()));
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!marks.contains_key("save:golfers"));
    // The server's copy (with the id) loads over the clean card.
    let mut saved = app.settings.saved_config_ref().cloned().expect("saved");
    saved.golfers[0].identifiers = vec!["flighthook.e".into(), "pureplay.local-3".into()];
    app.settings.load_from(&saved);
    let marks = screen.frame(&mut app, Vec::new());
    assert!(
        screen
            .texts(&mut app)
            .iter()
            .any(|t| t == "Pureplay: local-3")
    );
    assert!(marks.contains_key("golfer_link:pureplay.local-3"));
    // Remove, then Cancel brings it back.
    let marks = screen.click_mark(&mut app, &marks, "golfer_remove");
    assert!(app.settings.golfers.golfers.is_empty());
    assert!(enabled(&marks, "save:golfers"));
    let marks = screen.click_mark(&mut app, &marks, "cancel:golfers");
    assert!(!marks.contains_key("cancel:golfers"));
    assert_eq!(
        app.settings.golfers.golfers[0].record.identifiers[0],
        "flighthook.e"
    );
}

/// Publish `golfer.profile` `id` named `name` on the app's bus.
fn publish_profile(app: &mut FlighthookApp, id: &str, name: &str) {
    let profile = flighthook::GolferProfile::new(id.parse().unwrap(), name);
    let msg = flighthook::BusMessage::new("pp", profile);
    app.bus_states.insert(msg.state_key().unwrap(), msg);
}

/// The Golfers card links a game profile on the bus to the golfer
/// shown through its picker and removes a linked one, both saved with
/// the card's Save; only unlinked game profiles are offered, and never
/// a flighthook golfer's own id.
#[test]
fn the_golfers_card_links_and_removes_game_profiles() {
    let screen = Screen::one_section(1600.0, 1600.0);
    let mut app = app();
    app.settings_section = SettingsSection::Golfers;
    let mut saved = app.settings.saved_config_ref().cloned().expect("saved");
    let mut eric = flighthook::GolferRecord::new("flighthook.e", "Eric");
    eric.identifiers.push("pureplay.local-3".into());
    saved.golfers = vec![eric, flighthook::GolferRecord::new("flighthook.s", "Sam")];
    app.settings.load_from(&saved);
    // No game publishing: the picker is disabled.
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!enabled(&marks, "golfer_link_pick"));
    publish_profile(&mut app, "flighthook.e", "Eric");
    publish_profile(&mut app, "pureplay.local-3", "Eric P");
    publish_profile(&mut app, "pureplay.local-4", "Sam P");
    let marks = screen.frame(&mut app, Vec::new());
    assert!(enabled(&marks, "golfer_link_pick"));
    assert!(marks.contains_key("golfer_unlink:pureplay.local-3"));
    assert!(!marks.keys().any(|k| k.contains("flighthook.")), "own id");
    // A linked id reads by the name its game gives the player.
    assert!(
        screen
            .texts(&mut app)
            .iter()
            .any(|t| t == "Pureplay: Eric P")
    );
    // Only the unlinked profile is offered.
    screen.click_mark(&mut app, &marks, "golfer_link_pick");
    let marks = screen.frame(&mut app, Vec::new());
    let offered: Vec<&String> = marks
        .keys()
        .filter(|k| k.starts_with("golfer_link_option:"))
        .collect();
    assert_eq!(offered, ["golfer_link_option:pureplay.local-4"]);
    assert!(
        screen
            .texts(&mut app)
            .iter()
            .any(|t| t == "Pureplay: Sam P")
    );
    // Pick it for Eric and Save.
    let marks = screen.click_mark(&mut app, &marks, "golfer_link_option:pureplay.local-4");
    assert!(enabled(&marks, "save:golfers"));
    let posted = app.settings.begin_save(SaveTarget::Golfers);
    assert_eq!(
        posted.golfers[0].identifiers,
        ["flighthook.e", "pureplay.local-3", "pureplay.local-4"]
    );
    app.settings.finish_save(Ok(()));
    let mut saved = app.settings.saved_config_ref().cloned().expect("saved");
    saved.golfers = posted.golfers;
    app.settings.load_from(&saved);
    // Every profile linked: disabled again.
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!enabled(&marks, "golfer_link_pick"));
    // Remove one and Save.
    let marks = screen.click_mark(&mut app, &marks, "golfer_unlink:pureplay.local-3");
    assert!(enabled(&marks, "save:golfers"));
    assert!(!marks.contains_key("golfer_link:pureplay.local-3"));
    let posted = app.settings.begin_save(SaveTarget::Golfers);
    assert_eq!(
        posted.golfers[0].identifiers,
        ["flighthook.e", "pureplay.local-4"]
    );
}

/// A new golfer gets its id once: the save's answer carries it, the
/// card takes it at once, and a second save before the settings are
/// read again sends it back instead of asking for another.
#[test]
fn a_new_golfer_keeps_the_id_its_first_save_gave_it() {
    let screen = Screen::new();
    let mut app = app();
    let marks = screen.frame(&mut app, Vec::new());
    screen.click_mark(&mut app, &marks, "add:Golfers");
    let posted = app.settings.begin_save(SaveTarget::Golfers);
    assert!(posted.golfers[0].identifiers.is_empty());
    let mut issued = posted.golfers.clone();
    issued[0].identifiers = vec!["flighthook.new".into()];
    app.pending.lock().unwrap().settings_save = Some(Ok(crate::types::PostSettingsResponse {
        golfers: issued,
        ..crate::types::PostSettingsResponse::default()
    }));
    app.apply_pending();
    assert!(!app.settings.golfers.dirty, "clean after the save");
    assert_eq!(
        app.settings.golfers.golfers[0].record.own_id(),
        Some("flighthook.new")
    );
    // A quick second edit and save, before any re-read.
    app.settings.golfers.golfers[0].record.name = "Sam".into();
    app.settings.golfers.dirty = true;
    let again = app.settings.begin_save(SaveTarget::Golfers);
    assert_eq!(again.golfers[0].identifiers, ["flighthook.new"]);
}

/// Add leaves the card scrolled to its top: the golfer picker, Add and
/// the new golfer's name stay in sight.
#[test]
fn add_keeps_the_golfers_card_at_its_top() {
    let screen = Screen::new();
    let mut app = app();
    let mut config = app.settings.to_request();
    config.golfers = vec![flighthook::GolferRecord::new("flighthook.a", "Eric")];
    app.settings.load_from(&config);
    let marks = screen.frame(&mut app, Vec::new());
    screen.click_mark(&mut app, &marks, "add:Golfers");
    screen.frame(&mut app, Vec::new());
    let marks = screen.frame(&mut app, Vec::new());
    let (_, _, offset) = screen.bodies()["golfers"];
    assert_eq!(offset, 0.0);
    assert!(marks.contains_key("golfer_name"));
}

#[test]
fn a_golfers_height_is_typed_in_the_unit_setting_and_saved_in_centimeters() {
    let screen = Screen::one_section(1600.0, 1600.0);
    let mut app = app();
    app.settings_section = SettingsSection::Golfers;
    let marks = screen.frame(&mut app, Vec::new());
    let marks = screen.click_mark(&mut app, &marks, "add:Golfers");
    // Imperial by default: inches; empty is no height.
    let golfer = &app.settings.golfers.golfers[0];
    assert!(golfer.height.inches);
    assert_eq!(golfer.height.unit(), "in");
    assert_eq!(
        app.settings.golfers.to_records().expect("valid")[0].height,
        None
    );
    for bad in ["30", "tall"] {
        app.settings.golfers.golfers[0].height.text = bad.into();
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!enabled(&marks, "save:golfers"), "{bad}");
        let texts = screen.texts(&mut app);
        assert!(texts.iter().any(|t| t == "40 to 98 in"), "{bad}: {texts:?}");
    }
    assert!(enabled(&marks, "save:golfers"));
    app.settings.golfers.golfers[0].height.text = "71".into();
    app.set_default_units(UnitSystem::Metric);
    assert_eq!(app.settings.golfers.golfers[0].height.text, "180.3");
    assert_eq!(app.settings.golfers.golfers[0].height.unit(), "cm");
    app.settings.golfers.golfers[0].height.text = "99".into();
    assert!(screen.texts(&mut app).iter().any(|t| t == "100 to 250 cm"));
}

#[test]
fn the_replay_card_saves_with_the_global_settings() {
    let screen = Screen::new();
    let mut app = app();
    app.clip_dir = Some("/home/golfer/.local/share/flighthook/clips".into());
    let marks = screen.frame(&mut app, Vec::new());
    assert!(marks.contains_key("replay_folder"));
    assert!(!marks.contains_key("save:replay"));

    app.settings.replay.keep_shots = "50".into();
    app.settings.mark_general(GeneralCard::Replay);
    let marks = screen.frame(&mut app, Vec::new());
    assert!(enabled(&marks, "save:replay"));
    assert!(!marks.contains_key("save:simulation"));
    assert!(!marks.contains_key("save:global"));
    let request = app.settings.build_global_request();
    assert_eq!(request.replay.keep_shots, 50);
    assert_eq!(request.replay.pre_roll_s, 4.0);
    assert_eq!(request.replay.start_before_impact_s, 2.0);
    assert!(marks.contains_key("replay_keep"));

    // The start field saves with the rest and rejects out-of-range text.
    app.settings.replay.start_before_impact_s = "3.5".into();
    let request = app.settings.build_global_request();
    assert_eq!(request.replay.start_before_impact_s, 3.5);
    app.settings.replay.start_before_impact_s = "11".into();
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!enabled(&marks, "save:replay"));
    app.settings.replay.start_before_impact_s = "2".into();

    // Out of range: no save.
    app.settings.replay.pre_roll_s = "12".into();
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!enabled(&marks, "save:replay"));
    // Cancel puts every General card back.
    let marks = screen.click_mark(&mut app, &marks, "cancel:replay");
    assert!(!marks.contains_key("cancel:replay"));
    assert_eq!(app.settings.replay.keep_shots, "20");
    assert_eq!(app.settings.replay.pre_roll_s, "4");
    assert_eq!(app.settings.replay.start_before_impact_s, "2");
    assert!(!app.settings.global_dirty);
}

#[test]
fn the_replay_card_saves_the_shot_cards_checkbox() {
    let screen = Screen::new();
    let mut app = app();
    let saved = app.settings.to_request();
    app.settings.load_from(&saved);
    // As loaded: on.
    let marks = screen.frame(&mut app, Vec::new());
    assert!(app.settings.replay.shot_cards);
    assert!(!marks.contains_key("save:replay"));
    // Unticked: saved with the global settings; the shown cards follow
    // the saved setting.
    let marks = screen.click_mark(&mut app, &marks, "replay_shot_cards");
    assert!(!app.settings.replay.shot_cards);
    assert!(enabled(&marks, "save:replay"));
    assert!(!app.settings.build_global_request().replay.shot_cards);
    assert!(app.settings.shot_cards_enabled(), "not saved yet");
    let mut off = app.settings.build_global_request();
    app.settings.global_dirty = false;
    app.settings.load_from(&off);
    assert!(!app.settings.replay.shot_cards);
    assert!(!app.settings.shot_cards_enabled());
    // Ticked again and saved.
    let marks = screen.frame(&mut app, Vec::new());
    let _ = screen.click_mark(&mut app, &marks, "replay_shot_cards");
    assert!(app.settings.build_global_request().replay.shot_cards);
    off.replay.shot_cards = true;
    app.settings.global_dirty = false;
    app.settings.load_from(&off);
    assert!(app.settings.shot_cards_enabled());
}

// ----- Fixed-height cards and Reconnect -----

/// Every card on the Devices tab, in card order.
const EVERY_CARD: [&str; 13] = [
    "global",
    "simulation",
    "replay",
    "webserver.0",
    "mevo.0",
    "r10.0",
    "square.0",
    "exputt.0",
    "mock_monitor.0",
    "openconnect_server.0",
    "camera.0",
    "openconnect_client.0",
    "random_club.0",
];

/// The cards with a Reconnect: every launch monitor and output.
const RECONNECTABLE: [&str; 8] = [
    "mevo.0",
    "r10.0",
    "square.0",
    "exputt.0",
    "mock_monitor.0",
    "openconnect_server.0",
    "openconnect_client.0",
    "random_club.0",
];

/// An app with a card of every type, each actor connected with the
/// telemetry its device reports.
fn app_with_every_card() -> FlighthookApp {
    let mut app = FlighthookApp::with_pending(net::new_pending());
    let mut config = FlighthookConfig::default();
    config.mevo.insert(
        "0".into(),
        serde_json::from_str(r#"{"name":"Mevo+"}"#).expect("mevo"),
    );
    config.r10.insert(
        "0".into(),
        serde_json::from_str(r#"{"name":"R10"}"#).expect("r10"),
    );
    config.square.insert(
        "0".into(),
        flighthook::SquareSection {
            name: "Square Golf Omni".into(),
            ..Default::default()
        },
    );
    config
        .exputt
        .insert("0".into(), flighthook::ExPuttSection::default());
    config.mock_monitor.insert(
        "0".into(),
        flighthook::MockMonitorSection {
            name: "Mock".into(),
            enabled: true,
            actor_id: None,
            impact_lead_s: None,
            driver: Default::default(),
        },
    );
    config
        .openconnect_server
        .insert("0".into(), flighthook::OpenConnectServerSection::default());
    config
        .camera
        .insert("0".into(), flighthook::CameraSection::default());
    config.openconnect_client.insert(
        "0".into(),
        flighthook::OpenConnectClientSection {
            name: "GSPro".into(),
            address: Some("127.0.0.1:921".into()),
            ..Default::default()
        },
    );
    config.random_club.insert(
        "0".into(),
        flighthook::RandomClubSection {
            name: "Random club".into(),
            enabled: true,
            actor_id: None,
        },
    );
    app.settings.load_from(&config);
    let telemetry: [(&str, &[(&str, &str)]); 9] = [
        (
            "mevo.0",
            &[
                ("ready", "true"),
                ("battery_pct", "85"),
                ("tilt", "0.4"),
                ("roll", "-1.2"),
                ("temp_c", "41.5"),
                ("external_power", "false"),
            ],
        ),
        (
            "square.0",
            &[
                ("device_info", OMNI_DEVICE_INFO),
                ("ble_address", "02:00:00:AB:CD:01"),
                ("shot_count", "12"),
                ("ready", "true"),
                ("device_state", "ready"),
                ("battery_pct", "85"),
                ("zero_spin_discards", "2"),
            ],
        ),
        (
            "exputt.0",
            &[
                ("device_info", "ExPutt RG (192.168.1.40)"),
                ("phase", "measuring"),
                ("ready", "true"),
            ],
        ),
        ("mock_monitor.0", &[("shot_count", "3"), ("ready", "true")]),
        ("openconnect_server.0", &[("bind", "0.0.0.0:921")]),
        ("camera.0", &[("fps", "119.8"), ("dropped", "0")]),
        (
            "openconnect_client.0",
            &[
                ("address", "127.0.0.1:921"),
                ("club", "7I"),
                ("handed", "rh"),
                ("last_shot", "#12 at 16:41:07"),
                ("monitor_ready", "true"),
                ("response", "200 Shot received successfully"),
                ("shots_sent", "3"),
            ],
        ),
        ("random_club.0", &[]),
        ("webserver.0", &[("ws_clients", "1")]),
    ];
    for (id, pairs) in telemetry {
        let mut actor = ActorStatusResponse::new(id, ActorStatus::Connected);
        actor.telemetry = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        app.actors.insert(id.into(), actor);
    }
    app
}

/// Every card's rect, by ID.
fn card_rects(marks: &Marks) -> HashMap<String, Rect> {
    EVERY_CARD
        .iter()
        .map(|id| {
            let (rect, _) = marks
                .get(&format!("card:{id}"))
                .unwrap_or_else(|| panic!("{id} not drawn"));
            ((*id).to_owned(), *rect)
        })
        .collect()
}

/// Draw twice (layouts settle in one pass) and return the marks.
fn settled(screen: &Screen, app: &mut FlighthookApp) -> Marks {
    screen.frame(app, Vec::new());
    screen.frame(app, Vec::new())
}

#[test]
fn every_device_page_fills_the_height_of_settings() {
    for (w, h) in [(1600.0, 900.0), (1024.0, 768.0)] {
        let screen = Screen::one_section(w, h);
        let mut app = app_with_every_card();
        let mut heights = Vec::new();
        for id in EVERY_CARD.iter().filter(|id| id.contains('.')) {
            app.select_item(
                SettingsSection::of_actor(id),
                SettingsItem::Card((*id).to_owned()),
            );
            let marks = settled(&screen, &mut app);
            let (page, _) = marks[&format!("card:{id}")];
            assert!(h - page.bottom() < 12.0, "{id} reaches the bottom at {w}");
            heights.push(page.height());
        }
        assert!(
            heights.iter().all(|x| (x - heights[0]).abs() < 0.5),
            "{heights:?} at {w}"
        );
    }
}

#[test]
fn the_common_cards_fit_without_scrolling() {
    let screen = Screen::sized(1600.0, 2400.0);
    let mut app = app_with_every_card();
    settled(&screen, &mut app);
    let fits = |screen: &Screen, id: &str| {
        let (body, content, _) = screen.bodies()[id];
        content <= body.height()
    };
    for id in ["mevo.0", "square.0", "openconnect_client.0"] {
        assert!(
            fits(&screen, &format!("{id}.telemetry")),
            "{id}'s telemetry"
        );
    }
    settled(&screen, &mut app);
    assert!(fits(&screen, "global"), "the Global settings");
}

#[test]
fn telemetry_a_flip_a_validation_message_or_an_error_moves_no_card() {
    let screen = Screen::sized(1600.0, 2400.0);
    let mut app = app_with_every_card();
    let before = card_rects(&settled(&screen, &mut app));
    let unchanged = |app: &mut FlighthookApp, what: &str| {
        let after = card_rects(&settled(&screen, app));
        for id in EVERY_CARD {
            assert_eq!(after[id], before[id], "{id} after {what}");
        }
    };

    // Telemetry discovered: many more rows than the body holds.
    let mevo = app.actors.get_mut("mevo.0").unwrap();
    for n in 0..25 {
        mevo.telemetry
            .insert(format!("extra_{n:02}"), n.to_string());
    }
    unchanged(&mut app, "new telemetry");
    let (body, content, _) = screen.bodies()["mevo.0"];
    assert!(content > body.height(), "the body scrolls instead");

    // A side flip.
    unchanged(&mut app, "a flip to Settings");

    // An edit with a validation message, and the lock hint.
    let idx = form_index(&app, "mevo.0");
    if let ActorFormEntry::Device(d) = &mut app.settings.actors[idx] {
        d.address = "not an address".into();
    }
    app.settings.actors[idx].set_dirty(true);
    unchanged(&mut app, "an invalid address");

    // A failed save's error line.
    app.settings.save_error = Some(settings::SaveError {
        target: SaveTarget::Actor("mevo.0".into()),
        message: "504 the settings were not applied in time".into(),
    });
    unchanged(&mut app, "a save error");

    // A General card's edits.
    app.settings.save_error = None;
    app.settings.actors[idx].set_dirty(false);
    app.settings.default_units = UnitSystem::Metric;
    app.settings.mark_general(GeneralCard::Global);
    unchanged(&mut app, "a Global edit");
}

/// `n` wheel notches over `at`, then one more pass.
fn wheel(screen: &Screen, app: &mut FlighthookApp, at: Pos2, down: f32, n: usize) {
    screen.frame(app, vec![Event::PointerMoved(at)]);
    for _ in 0..n {
        let events = vec![
            Event::PointerMoved(at),
            Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: vec2(0.0, -down),
                modifiers: Modifiers::NONE,
            },
        ];
        screen.frame(app, events);
    }
    screen.frame(app, Vec::new());
}

#[test]
fn a_long_body_scrolls_inside_its_card_before_the_page() {
    // The Mevo card on screen, and the tab long enough to scroll too.
    let screen = Screen::sized(1600.0, 1100.0);
    let mut app = app_with_every_card();
    let marks = settled(&screen, &mut app);
    let card = marks["card:mevo.0"].0;
    let title = marks["title:mevo.0"].0;
    let remove = marks["remove:mevo.0"].0;
    let (body, content, offset) = screen.bodies()["mevo.0"];
    assert!(body.bottom() < 1100.0, "on screen");
    assert!(content > body.height(), "the Mevo settings overflow");
    assert_eq!(offset, 0.0);

    // The wheel over the body scrolls the card, not the page.
    wheel(&screen, &mut app, body.center(), 7.0, 6);
    let marks = screen.frame(&mut app, Vec::new());
    let (_, _, scrolled) = screen.bodies()["mevo.0"];
    assert!(scrolled > 0.0, "the card scrolled");
    assert_eq!(marks["card:mevo.0"].0, card, "the page did not move");
    // Header and action row stay put; another card keeps its own place.
    assert_eq!(marks["title:mevo.0"].0, title);
    assert_eq!(marks["remove:mevo.0"].0, remove);
    assert_eq!(screen.bodies()["square.0"].2, 0.0);

    // Once the card is at its end, the page takes the wheel.
    wheel(&screen, &mut app, body.center(), 7.0, 120);
    let marks = screen.frame(&mut app, Vec::new());
    let (body_after, content, end) = screen.bodies()["mevo.0"];
    assert!(
        (end - (content - body_after.height())).abs() < 1.0,
        "the card reached its end: {end}"
    );
    assert!(
        marks["card:mevo.0"].0.top() < card.top(),
        "the page scrolled"
    );

    // The card keeps its position through later passes.
    for _ in 0..3 {
        screen.frame(&mut app, Vec::new());
    }
    assert_eq!(screen.bodies()["mevo.0"].2, end);
}

#[test]
fn reconnect_is_on_every_launch_monitor_and_output_telemetry_side_and_no_other() {
    let screen = Screen::sized(1600.0, 2400.0);
    let mut app = app_with_every_card();
    let marks = settled(&screen, &mut app);
    for id in EVERY_CARD {
        let shown = marks.contains_key(&format!("reconnect:{id}"));
        assert_eq!(shown, RECONNECTABLE.contains(&id), "{id}");
    }
    // In the page's header, at its right.
    for id in RECONNECTABLE {
        let (button, enabled) = marks[&format!("reconnect:{id}")];
        let card = marks[&format!("card:{id}")].0;
        assert!(enabled, "{id}");
        assert!(card.contains_rect(button), "{id}");
        assert!(button.top() - card.top() < 40.0, "{id} at the top");
        assert!(card.right() - button.right() < 60.0, "{id} at the right");
    }
    // The ExPutt's Find mat sits beside it.
    let (find, _) = marks["find_mat:exputt.0"];
    let (reconnect, _) = marks["reconnect:exputt.0"];
    assert!(find.right() <= reconnect.left());
    assert!((find.center().y - reconnect.center().y).abs() < 0.5);
}

#[test]
fn reconnect_posts_once_and_waits_for_its_answer() {
    let screen = Screen::sized(1600.0, 2400.0);
    let mut app = app_with_every_card();
    let marks = settled(&screen, &mut app);
    let marks = screen.click_mark(&mut app, &marks, "reconnect:mevo.0");
    assert!(app.reconnecting.contains("mevo.0"), "the request is out");
    assert!(!enabled(&marks, "reconnect:mevo.0"), "in flight");
    assert!(enabled(&marks, "reconnect:square.0"), "others are free");

    // A second click while it is out sends nothing more.
    app.reconnect(&screen.0, "mevo.0");
    assert_eq!(app.reconnecting.len(), 1);

    app.pending
        .lock()
        .unwrap()
        .reconnects
        .push(("mevo.0".into(), Ok(net::Reconnected::Started)));
    app.apply_pending();
    let marks = screen.frame(&mut app, Vec::new());
    assert!(app.reconnecting.is_empty());
    assert!(enabled(&marks, "reconnect:mevo.0"));

    // A refusal frees it too; the card says why through its telemetry.
    let marks = screen.click_mark(&mut app, &marks, "reconnect:square.0");
    assert!(!enabled(&marks, "reconnect:square.0"));
    app.pending
        .lock()
        .unwrap()
        .reconnects
        .push(("square.0".into(), Err("409 still closing".into())));
    app.apply_pending();
    let marks = screen.frame(&mut app, Vec::new());
    assert!(enabled(&marks, "reconnect:square.0"));
}

/// A reconnect request that fails (an error status, a network error, a
/// 504) shows on its card as the card's `error`, the same row an error
/// the actor reports takes, until the actor's next status or the next
/// press. A request still running (202) shows nothing.
#[test]
fn a_failed_reconnect_shows_on_its_card_until_the_next_status_or_press() {
    let screen = Screen::sized(1600.0, 2400.0);
    let mut app = app_with_every_card();
    let marks = settled(&screen, &mut app);
    let height = marks["card:mevo.0"].0.height();
    assert!(!marks.contains_key("telemetry:mevo.0:error"));
    let reply = |app: &mut FlighthookApp, reply: Result<net::Reconnected, String>| {
        app.pending
            .lock()
            .unwrap()
            .reconnects
            .push(("mevo.0".into(), reply));
        app.apply_pending();
    };

    screen.click_mark(&mut app, &marks, "reconnect:mevo.0");
    reply(&mut app, Err("the reconnect did not finish".into()));
    let marks = screen.frame(&mut app, Vec::new());
    assert!(marks.contains_key("telemetry:mevo.0:error"), "on the card");
    assert_eq!(
        app.reconnect_errors.get("mevo.0").map(String::as_str),
        Some("the reconnect did not finish")
    );
    assert!(
        (marks["card:mevo.0"].0.height() - height).abs() < 0.5,
        "the card keeps its height"
    );
    assert!(enabled(&marks, "reconnect:mevo.0"));
    assert!(
        !marks.contains_key("telemetry:square.0:error"),
        "its card only"
    );

    // The actor's next status replaces it.
    app.handle_ui_message(crate::socket::feed::local_msg(
        "mevo.0",
        flighthook::UiLocalEvent::CardStatus {
            status: ActorStatus::Connected,
            telemetry: HashMap::new(),
        },
    ));
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!marks.contains_key("telemetry:mevo.0:error"));

    // So does pressing again; a 202 adds nothing.
    screen.click_mark(&mut app, &marks, "reconnect:mevo.0");
    reply(&mut app, Err("connection refused".into()));
    let marks = screen.frame(&mut app, Vec::new());
    assert!(marks.contains_key("telemetry:mevo.0:error"));
    screen.click_mark(&mut app, &marks, "reconnect:mevo.0");
    let marks = screen.frame(&mut app, Vec::new());
    assert!(
        !marks.contains_key("telemetry:mevo.0:error"),
        "cleared by the press"
    );
    reply(&mut app, Ok(net::Reconnected::Started));
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!marks.contains_key("telemetry:mevo.0:error"));
    assert!(app.reconnect_errors.is_empty());
}

fn reconnect_reply(app: &mut FlighthookApp, id: &str, reply: Result<net::Reconnected, String>) {
    app.pending
        .lock()
        .unwrap()
        .reconnects
        .push((id.into(), reply));
    app.apply_pending();
}

fn status_of(app: &mut FlighthookApp, id: &str, status: ActorStatus) {
    app.handle_ui_message(crate::socket::feed::local_msg(
        id,
        flighthook::UiLocalEvent::CardStatus {
            status,
            telemetry: HashMap::new(),
        },
    ));
}

/// A reconnect that starts later (202: its old connection is still
/// closing, or it is still running) keeps Reconnect disabled until the
/// actor starts again: its next Starting status, or an outcome listing
/// it restarted, the one with no request ID included. Other statuses,
/// Connected included, leave it waiting.
#[test]
fn a_reconnect_that_starts_later_waits_for_the_restart() {
    let screen = Screen::sized(1600.0, 2400.0);
    let mut app = app_with_every_card();
    let marks = settled(&screen, &mut app);

    // Freed by the next Starting status.
    screen.click_mark(&mut app, &marks, "reconnect:mevo.0");
    reconnect_reply(&mut app, "mevo.0", Ok(net::Reconnected::Later));
    status_of(&mut app, "mevo.0", ActorStatus::Disconnected);
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!enabled(&marks, "reconnect:mevo.0"), "still closing");
    // The instance being replaced may still report Connected.
    status_of(&mut app, "mevo.0", ActorStatus::Connected);
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!enabled(&marks, "reconnect:mevo.0"), "only Starting frees");
    status_of(&mut app, "mevo.0", ActorStatus::Starting);
    let marks = screen.frame(&mut app, Vec::new());
    assert!(enabled(&marks, "reconnect:mevo.0"), "started again");
    assert!(app.reconnect_errors.is_empty());

    // Freed by the self-completed outcome.
    screen.click_mark(&mut app, &marks, "reconnect:square.0");
    reconnect_reply(&mut app, "square.0", Ok(net::Reconnected::Later));
    app.handle_ui_message(crate::socket::feed::local_msg(
        "system",
        flighthook::UiLocalEvent::ConfigOutcome {
            request_id: None,
            restarted: vec!["mevo.0".into()],
            stopped: Vec::new(),
            started: Vec::new(),
            config_generation: 0,
        },
    ));
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!enabled(&marks, "reconnect:square.0"), "another actor's");
    app.handle_ui_message(crate::socket::feed::local_msg(
        "system",
        flighthook::UiLocalEvent::ConfigOutcome {
            request_id: None,
            restarted: vec!["square.0".into()],
            stopped: Vec::new(),
            started: Vec::new(),
            config_generation: 0,
        },
    ));
    let marks = screen.frame(&mut app, Vec::new());
    assert!(enabled(&marks, "reconnect:square.0"));

    // Started before the answer came: freed by the answer.
    screen.click_mark(&mut app, &marks, "reconnect:mevo.0");
    status_of(&mut app, "mevo.0", ActorStatus::Starting);
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!enabled(&marks, "reconnect:mevo.0"), "not answered yet");
    reconnect_reply(&mut app, "mevo.0", Ok(net::Reconnected::Later));
    let marks = screen.frame(&mut app, Vec::new());
    assert!(enabled(&marks, "reconnect:mevo.0"));
}

/// A reconnect that starts later but never does is given up on after
/// a while: Reconnect takes clicks again and the card says why.
#[test]
fn a_reconnect_that_never_starts_is_given_up_on() {
    let screen = Screen::sized(1600.0, 2400.0);
    let mut app = app_with_every_card();
    let marks = settled(&screen, &mut app);
    app.now_ms = 1_000;
    screen.click_mark(&mut app, &marks, "reconnect:mevo.0");
    reconnect_reply(&mut app, "mevo.0", Ok(net::Reconnected::Later));
    app.now_ms += crate::app::RECONNECT_WAIT_MS - 1;
    app.apply_pending();
    assert!(app.reconnecting.contains("mevo.0"));
    app.now_ms += 1;
    app.apply_pending();
    let marks = screen.frame(&mut app, Vec::new());
    assert!(enabled(&marks, "reconnect:mevo.0"));
    assert_eq!(
        app.reconnect_errors.get("mevo.0").map(String::as_str),
        Some(crate::app::RECONNECT_UNFINISHED)
    );
    assert!(marks.contains_key("telemetry:mevo.0:error"), "on the card");
}

/// The give-up leaves the actor's own `error` row (the old connection is
/// still closing) on the card instead of replacing it.
#[test]
fn giving_up_keeps_the_actors_own_error() {
    let screen = Screen::sized(1600.0, 2400.0);
    let mut app = app_with_every_card();
    let marks = settled(&screen, &mut app);
    app.now_ms = 1_000;
    screen.click_mark(&mut app, &marks, "reconnect:mevo.0");
    reconnect_reply(&mut app, "mevo.0", Ok(net::Reconnected::Later));
    let closing = "The previous connection has not closed yet. It reconnects when it does.";
    app.handle_ui_message(crate::socket::feed::local_msg(
        "mevo.0",
        flighthook::UiLocalEvent::CardStatus {
            status: ActorStatus::Disconnected,
            telemetry: HashMap::from([("error".into(), closing.into())]),
        },
    ));
    app.now_ms += crate::app::RECONNECT_WAIT_MS;
    app.apply_pending();
    let marks = screen.frame(&mut app, Vec::new());
    assert!(enabled(&marks, "reconnect:mevo.0"), "given up on");
    assert!(app.reconnect_errors.is_empty(), "the actor's row stays");
    assert_eq!(
        app.actors["mevo.0"]
            .telemetry
            .get("error")
            .map(String::as_str),
        Some(closing)
    );
    assert!(marks.contains_key("telemetry:mevo.0:error"), "on the card");
}

/// A conflict joins the card's own error row instead of hiding it.
#[test]
fn a_conflict_joins_the_cards_own_error() {
    assert_eq!(
        error_row(Some("bind failed"), Some("Another session")),
        "bind failed; Another session"
    );
    assert_eq!(error_row(None, Some("Another session")), "Another session");
    assert_eq!(error_row(Some("bind failed"), None), "bind failed");
    let mut app = app_with_every_card();
    app.flighthook_id = "me".into();
    app.actors
        .get_mut("webserver.0")
        .expect("web server card")
        .telemetry
        .insert("error".into(), "bind failed".into());
    let conflicts = HashMap::from([("me".to_string(), "Another session".to_string())]);
    assert_eq!(
        app.conflict_of("webserver.0", &conflicts).as_deref(),
        Some("Another session")
    );
}

/// A remote launch monitor and a second session show as read-only
/// cards (no Reconnect, no toggle), and the conflict row goes on the
/// Web Server card and the other session's card.
#[test]
fn remote_actors_show_read_only_cards_and_conflict_rows() {
    use flighthook::{BusActor, roles};
    let screen = Screen::sized(1600.0, 2400.0);
    let mut app = app_with_every_card();
    app.flighthook_id = "me".into();
    let with = |name: &str, role: &str| {
        let mut a = BusActor::new(name, 2.0);
        a.roles = vec![role.to_owned()];
        a
    };
    for (from, actor) in [
        ("me", with("flighthook", roles::SESSION)),
        ("far", with("Far radar", roles::LAUNCH_MONITOR)),
        ("other", with("Range Session", roles::SESSION)),
    ] {
        app.handle_ui_message(flighthook::UiMessage::Frp(Box::new(
            flighthook::BusMessage::new(from, actor),
        )));
    }
    let marks = settled(&screen, &mut app);
    assert!(
        marks.contains_key("badge:remote:far"),
        "the remote card shows"
    );
    assert!(!marks.contains_key("reconnect:remote:far"), "no Reconnect");
    assert!(!marks.contains_key("remove:remote:far"), "no settings");
    assert!(marks.contains_key("telemetry:remote:other:error"));
    assert!(marks.contains_key("telemetry:webserver.0:error"));
    // Gone with its removal.
    app.handle_ui_message(flighthook::UiMessage::Frp(Box::new(
        flighthook::BusMessage::new(
            "far",
            flighthook::Removal::of(flighthook::StateKind::BusActor),
        ),
    )));
    let marks = settled(&screen, &mut app);
    assert!(!marks.contains_key("badge:remote:far"));
}

/// The repaint the last pass asked for: how soon.
fn repaint_after(screen: &Screen, app: &mut FlighthookApp) -> std::time::Duration {
    let input = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, screen.1)),
        ..Default::default()
    };
    app.all_settings_sections = screen.2;
    let output = screen.0.run(input, |ctx| {
        app.render_header_in_test(ctx);
        egui::CentralPanel::default().show(ctx, |ui| {
            let ctx = ui.ctx().clone();
            app.render_devices_panel(&ctx, ui);
        });
    });
    output.viewport_output[&egui::ViewportId::ROOT].repaint_delay
}

/// Set `id`'s statuses to `statuses` in turn, with no telemetry.
fn without_telemetry(app: &mut FlighthookApp, id: &str, statuses: &[ActorStatus]) {
    let actor = app.actors.get_mut(id).expect("running");
    actor.telemetry.clear();
    for status in statuses {
        actor.observe(*status, 0);
    }
}

/// A card whose Telemetry side has no row to show shows a spinner in
/// the middle of its body while its actor is starting, connecting or
/// reconnecting, and a check in a ring once it is connected; either
/// goes as soon as a row arrives. A spinner asks for repaints
/// (`monitoring::tests` checks nothing else does).
#[test]
fn an_empty_telemetry_side_shows_a_spinner_or_the_connected_mark() {
    let screen = Screen::sized(1600.0, 2400.0);
    let mut app = app_with_every_card();
    let marks = settled(&screen, &mut app);
    assert!(!marks.keys().any(|k| k.starts_with("spinner:")));
    assert!(
        marks.contains_key("connected_icon:random_club.0"),
        "no rows"
    );
    assert!(!marks.contains_key("connected_icon:mevo.0"), "rows");

    let cases: [(&str, &[ActorStatus]); 3] = [
        ("mevo.0", &[ActorStatus::Starting]),
        ("square.0", &[ActorStatus::Disconnected]),
        (
            "openconnect_client.0",
            &[ActorStatus::Connected, ActorStatus::Reconnecting],
        ),
    ];
    for (id, statuses) in cases {
        without_telemetry(&mut app, id, statuses);
    }
    let marks = settled(&screen, &mut app);
    for (id, _) in cases {
        let (spinner, _) = marks[&format!("spinner:{id}")];
        let (body, content, _) = screen.bodies()[&format!("{id}.telemetry")];
        assert!(
            (spinner.center().x - body.center().x).abs() < 1.0
                && (spinner.center().y - body.center().y).abs() < 2.0,
            "{id}: centered in the body"
        );
        assert!(content <= body.height(), "{id}: nothing to scroll");
        assert!(!marks.contains_key(&format!("connected_icon:{id}")));
    }
    assert!(
        repaint_after(&screen, &mut app) <= theme::LOADING_FRAME,
        "a spinner turns"
    );

    // A card cut off by the bottom of the window keeps its spinner in
    // the middle of its whole body, not of the part on screen.
    let (body, _, _) = screen.bodies()["mevo.0.telemetry"];
    let cut = Screen::sized(1600.0, (body.top() + body.height() * 0.3).round());
    let marks = settled(&cut, &mut app);
    let (spinner, _) = marks["spinner:mevo.0"];
    let (cut_body, _, _) = cut.bodies()["mevo.0.telemetry"];
    assert!((cut_body.height() - body.height()).abs() < 0.5);
    assert!(
        (spinner.center().y - cut_body.center().y).abs() < 2.0,
        "{spinner:?} in {cut_body:?}"
    );

    // Connected with no rows: the mark, not the spinner.
    without_telemetry(&mut app, "mevo.0", &[ActorStatus::Connected]);
    let marks = settled(&screen, &mut app);
    assert!(marks.contains_key("connected_icon:mevo.0"));
    assert!(!marks.contains_key("spinner:mevo.0"));
    let (body, content, _) = screen.bodies()["mevo.0.telemetry"];
    assert!(content <= body.height(), "nothing to scroll");

    // A row arrives: both go.
    for id in ["mevo.0", "square.0", "openconnect_client.0"] {
        app.actors
            .get_mut(id)
            .expect("running")
            .telemetry
            .insert("ready".into(), "false".into());
    }
    let marks = settled(&screen, &mut app);
    for id in ["mevo.0", "square.0", "openconnect_client.0"] {
        assert!(!marks.contains_key(&format!("spinner:{id}")), "{id}");
        assert!(!marks.contains_key(&format!("connected_icon:{id}")), "{id}");
        assert!(marks.contains_key(&format!("telemetry:{id}:ready")), "{id}");
    }
}

#[test]
fn reconnect_waits_while_the_tab_is_locked_or_a_save_is_out() {
    let screen = Screen::sized(1600.0, 2400.0);
    let mut app = app_with_every_card();
    edit_name(&mut app, "openconnect_client.0");
    let marks = settled(&screen, &mut app);
    for id in RECONNECTABLE {
        if id != "openconnect_client.0" {
            assert!(!enabled(&marks, &format!("reconnect:{id}")), "{id} locked");
        }
    }
    // Clicking a disabled one sends nothing.
    screen.click_mark(&mut app, &marks, "reconnect:mevo.0");
    assert!(app.reconnecting.is_empty());

    let marks = screen.click_mark(&mut app, &marks, "save:openconnect_client.0");
    assert!(app.settings.saving());
    for id in RECONNECTABLE {
        if id != "openconnect_client.0" {
            assert!(!enabled(&marks, &format!("reconnect:{id}")), "{id} saving");
        }
    }
}

/// [`app_with_every_card`] plus a Pureplay Connect and a Pureplay
/// Accessory card, a remote launch monitor and a remote session.
fn app_with_every_section() -> FlighthookApp {
    use flighthook::{BusActor, roles};
    let mut app = app_with_every_card();
    let mut config = app.settings.to_request();
    config.pureplay.insert(
        "0".into(),
        crate::types::PureplaySection {
            name: "Pureplay".into(),
            ..Default::default()
        },
    );
    app.settings.load_from(&config);
    app.settings
        .add_actor("pureplay_accessory")
        .expect("accessory");
    app.flighthook_id = "me".into();
    let with = |name: &str, role: &str| {
        let mut a = BusActor::new(name, 2.0);
        a.roles = vec![role.to_owned()];
        a
    };
    for (from, actor) in [
        ("me", with("flighthook", roles::SESSION)),
        ("far", with("Far radar", roles::LAUNCH_MONITOR)),
        ("other", with("Range Session", roles::SESSION)),
    ] {
        app.handle_ui_message(flighthook::UiMessage::Frp(Box::new(
            flighthook::BusMessage::new(from, actor),
        )));
    }
    app
}

/// The cards of [`app_with_every_section`], by the section that shows
/// them.
const SECTION_CARDS: [(SettingsSection, &[&str]); 5] = [
    (
        SettingsSection::General,
        &["global", "webserver.0", "simulation"],
    ),
    (
        SettingsSection::Devices,
        &[
            "mevo.0",
            "r10.0",
            "square.0",
            "exputt.0",
            "mock_monitor.0",
            "openconnect_server.0",
            "remote:far",
        ],
    ),
    (
        SettingsSection::Software,
        &[
            "openconnect_client.0",
            "pureplay.0",
            "pureplay_accessory.0",
            "random_club.0",
            "remote:other",
        ],
    ),
    (
        SettingsSection::Cameras,
        &["camera.0", "replay", "body_tracking"],
    ),
    (SettingsSection::Golfers, &["golfer.0"]),
];

#[test]
fn every_card_type_has_its_row_in_its_section_in_order() {
    let screen = Screen::one_section(1600.0, 2400.0);
    let mut app = app_with_every_section();
    let mut config = app.settings.to_request();
    config.golfers = vec![flighthook::GolferRecord::new("flighthook.a", "Eric")];
    app.settings.load_from(&config);
    let marks = settled(&screen, &mut app);
    assert!(!app.settings.locked());
    let tops: Vec<f32> = SECTION_CARDS
        .iter()
        .flat_map(|(_, c)| c.iter())
        .map(|c| marks[&format!("settings_item:{c}")].0.top())
        .collect();
    assert!(tops.windows(2).all(|p| p[0] < p[1]), "{tops:?}");
    // Each row opens its own page and only that one, in the same place.
    let mut page_top = None;
    for (section, cards) in SECTION_CARDS {
        for card in cards.iter().copied() {
            let marks = screen.click_mark(&mut app, &marks, &format!("settings_item:{card}"));
            assert_eq!(app.settings_section, section, "{card}");
            let page = if card.starts_with("golfer.") {
                "golfers"
            } else {
                card
            };
            let shown: Vec<&String> = marks.keys().filter(|k| k.starts_with("card:")).collect();
            assert_eq!(shown, [&format!("card:{page}")], "{card}");
            let top = marks[&format!("card:{page}")].0.top();
            assert!((top - *page_top.get_or_insert(top)).abs() < 0.5, "{card}");
        }
    }
}

#[test]
fn each_sections_plus_adds_its_own_card_types() {
    let screen = Screen::one_section(1600.0, 1200.0);
    let mut app = app();
    app.cameras_supported = true;
    app.trackball_supported = true;
    let marks = settled(&screen, &mut app);
    assert!(!marks.contains_key("add:General"));
    assert!(marks.contains_key("add:Golfers"));
    for (section, group) in [
        (SettingsSection::Devices, "Launch monitors"),
        (SettingsSection::Software, "Software"),
        (SettingsSection::Cameras, "Cameras"),
    ] {
        let marks = settled(&screen, &mut app);
        let marks = screen.click_mark(&mut app, &marks, &format!("add:{group}"));
        // Every type the menu offers, and only its group's.
        let offered = cards::offered(section.group().expect("a group"), true);
        assert!(!offered.is_empty(), "{section:?}");
        for preset in &offered {
            assert!(
                marks.contains_key(&format!("add:{group}:{}", preset.label)),
                "{section:?} menu: {}",
                preset.label
            );
        }
        let entries = marks
            .keys()
            .filter(|k| k.starts_with("add:") && k.matches(':').count() == 2)
            .count();
        assert_eq!(entries, offered.len(), "{section:?}");
        // Close the menu.
        screen.click(&mut app, pos2(1500.0, 1100.0));
    }
}

#[test]
fn the_alert_dot_follows_a_red_card_to_its_row() {
    let screen = Screen::one_section(1600.0, 1200.0);
    let mut app = app_with_every_card();
    let dots = |marks: &Marks| -> Vec<String> {
        let mut dots: Vec<String> = marks
            .keys()
            .filter_map(|k| k.strip_prefix("settings_nav_alert:"))
            .map(str::to_owned)
            .collect();
        dots.sort();
        dots
    };
    app.now_ms = 1_000;
    assert!(dots(&settled(&screen, &mut app)).is_empty());
    for id in [
        "mevo.0",
        "openconnect_server.0",
        "openconnect_client.0",
        "camera.0",
    ] {
        let mut app = app_with_every_card();
        app.actors
            .entry(id.to_owned())
            .or_insert_with(|| ActorStatusResponse::new(id, ActorStatus::Connected));
        lose(&mut app, id, ActorStatus::Reconnecting, 1_000);
        app.now_ms = 1_001;
        let marks = settled(&screen, &mut app);
        assert_eq!(dots(&marks), [id], "{id}");
        // On the card's row, at its left.
        let (dot, _) = marks[&format!("settings_nav_alert:{id}")];
        let (row, _) = marks[&format!("settings_item:{id}")];
        assert!(row.contains_rect(dot), "{id}");
        assert!(dot.center().x < row.center().x, "{id}");
        // The Settings icon carries it too, and both go together.
        assert!(marks.contains_key("alert_dot"), "{id}");
        app.now_ms = 1_001 + crate::types::RECONNECT_ALERT_MS;
        let marks = settled(&screen, &mut app);
        assert!(dots(&marks).is_empty(), "{id} turned yellow");
        assert!(!marks.contains_key("alert_dot"), "{id}");
    }
    // Two red cards: two dots.
    lose(&mut app, "mevo.0", ActorStatus::Disconnected, 1_000);
    lose(&mut app, "random_club.0", ActorStatus::Disconnected, 1_000);
    app.now_ms = 1_001;
    assert_eq!(
        dots(&settled(&screen, &mut app)),
        ["mevo.0", "random_club.0"]
    );
}

#[test]
fn unsaved_edits_lock_the_list_and_the_nav_icons() {
    let screen = Screen::one_section(1600.0, 1200.0);
    let mut app = app();
    app.active_tab = Tab::Devices;
    let marks = settled(&screen, &mut app);
    let rows = |marks: &Marks| -> Vec<String> {
        marks
            .keys()
            .filter(|k| k.starts_with("settings_item:"))
            .cloned()
            .collect()
    };
    assert!(rows(&marks).len() > 4);
    for row in rows(&marks) {
        assert!(enabled(&marks, &row), "{row}");
    }
    assert!(enabled(&marks, "nav:shots"));
    // An edit on the Mevo card.
    edit_name(&mut app, "mevo.0");
    let marks = settled(&screen, &mut app);
    for row in rows(&marks) {
        assert!(!enabled(&marks, &row), "{row} locked");
    }
    for nav in ["nav:shots", "nav:settings", "nav:log"] {
        assert!(!enabled(&marks, nav), "{nav} locked");
    }
    // A click on another row changes nothing.
    let marks = screen.click_mark(&mut app, &marks, "settings_item:global");
    assert_eq!(app.settings_section, SettingsSection::Devices);
    assert!(!marks.contains_key("card:global"));
    assert!(
        screen
            .texts(&mut app)
            .iter()
            .any(|t| t == "Save or cancel your changes"),
    );
    // Cancel unlocks.
    let marks = screen.click_mark(&mut app, &marks, "cancel:mevo.0");
    for row in rows(&marks) {
        assert!(enabled(&marks, &row), "{row}");
    }
    assert!(enabled(&marks, "nav:shots"));
    let marks = screen.click_mark(&mut app, &marks, "settings_item:global");
    assert_eq!(app.settings_section, SettingsSection::General);
    assert!(marks.contains_key("card:global"));
}

#[test]
fn the_item_is_kept_for_the_session_and_first_opens_on_devices() {
    let screen = Screen::one_section(1600.0, 1200.0);
    assert_eq!(
        FlighthookApp::with_pending(net::new_pending()).settings_section,
        SettingsSection::Devices
    );
    let mut app = app();
    app.active_tab = Tab::Devices;
    let marks = settled(&screen, &mut app);
    // The first device shown, nothing else.
    assert!(marks.contains_key("card:mevo.0"));
    assert!(!marks.contains_key("card:global"));
    assert!(!marks.contains_key("card:openconnect_client.0"));
    let marks = screen.click_mark(&mut app, &marks, "settings_item:replay");
    assert_eq!(app.settings_section, SettingsSection::Cameras);
    assert!(marks.contains_key("card:replay"));
    let marks = screen.click_mark(&mut app, &marks, "settings_item:openconnect_client.1");
    assert!(marks.contains_key("card:openconnect_client.1"));
    assert!(!marks.contains_key("card:replay"));
    // Away to Shots and back by the Settings icon: still the same page.
    let marks = screen.click_mark(&mut app, &marks, "nav:shots");
    assert_eq!(app.active_tab, Tab::Shots);
    let marks = screen.click_mark(&mut app, &marks, "nav:settings");
    assert_eq!(app.active_tab, Tab::Devices);
    assert!(marks.contains_key("card:openconnect_client.1"));
    // A removed item gives way to its section's first.
    app.settings_item = Some(SettingsItem::Card("openconnect_client.9".into()));
    let marks = settled(&screen, &mut app);
    assert!(marks.contains_key("card:openconnect_client.0"));
}

#[test]
fn the_list_rows_are_styled_like_the_shot_list_with_a_status_dot() {
    let screen = Screen::one_section(1600.0, 1800.0);
    let mut app = app_with_every_section();
    app.settings.golfers.add();
    app.settings.golfers.add();
    let (marks, shapes) = {
        screen.frame(&mut app, Vec::new());
        screen.painted(&mut app, Vec::new())
    };
    let mut rows: Vec<Rect> = marks
        .iter()
        .filter(|(k, _)| k.starts_with("settings_item:"))
        .map(|(_, (r, _))| *r)
        .collect();
    rows.sort_by(|a, b| a.top().total_cmp(&b.top()));
    assert!(rows.len() > 10);
    for pair in rows.windows(2) {
        assert!((pair[0].height() - settings_nav::ROW_HEIGHT).abs() < 0.5);
        assert!(pair[1].top() > pair[0].bottom(), "one under the other");
        assert!((pair[1].left() - pair[0].left()).abs() < 0.5);
    }
    // Global, two golfers.
    assert!(marks.contains_key("settings_item:global"));
    assert!(marks.contains_key("settings_item:golfer.0"));
    assert!(marks.contains_key("settings_item:golfer.1"));
    // The list sits left of the page.
    let (mevo_row, _) = marks["settings_item:mevo.0"];
    let (mevo, _) = marks["card:mevo.0"];
    assert!(mevo_row.right() < mevo.left());
    let text = |t: &str| {
        shapes.iter().find_map(|s| match s {
            egui::Shape::Text(x) if x.galley.text() == t => Some(x.clone()),
            _ => None,
        })
    };
    let caption = text("SETTINGS").expect("caption");
    assert!(caption.pos.y < rows[0].top());
    for heading in [
        "GENERAL",
        "LAUNCH MONITORS",
        "SOFTWARE",
        "CAMERAS",
        "GOLFERS",
    ] {
        assert!(text(heading).is_some(), "{heading}");
    }
    assert!(text("beta").is_none(), "Simulation and Replay have no tag");
    // The selected row: an accent border and the left bar.
    let bar = shapes.iter().any(|s| match s {
        egui::Shape::Rect(r) => {
            r.fill == theme::ACCENT
                && (r.rect.left() - mevo_row.left()).abs() < 0.5
                && (r.rect.width() - 4.0).abs() < 0.5
        }
        _ => false,
    });
    assert!(bar, "the left bar on the selected row");
    // The Mevo's row carries its status color as a dot.
    let dot = shapes.iter().any(|s| match s {
        egui::Shape::Circle(c) => mevo_row.contains(c.center) && c.fill == theme::STATUS_OK,
        _ => false,
    });
    assert!(dot, "the Mevo's green dot");
}

/// The golfer page's Hand: Right-handed while unset; picking Left-handed
/// marks the card dirty and saves `handed = lh`. An edit elsewhere keeps
/// the hand as loaded.
#[test]
fn the_golfers_card_sets_the_golfers_hand() {
    let screen = Screen::one_section(1600.0, 1200.0);
    let mut app = app();
    let mut config = app.settings.to_request();
    config.golfers = vec![
        flighthook::GolferRecord::new("flighthook.a", "Alex"),
        flighthook::GolferRecord {
            handed: Some(flighthook::Handedness::Left),
            ..flighthook::GolferRecord::new("flighthook.s", "Sam")
        },
    ];
    app.settings.load_from(&config);
    app.settings_section = SettingsSection::Golfers;
    let marks = settled(&screen, &mut app);
    assert!(marks.contains_key("card:golfers"));
    let texts = screen.texts(&mut app);
    for shown in ["Hand:", "Right-handed", "Left-handed"] {
        assert!(texts.iter().any(|t| t == shown), "{shown}");
    }
    assert!(!app.settings.golfers.dirty);
    assert!(!marks.contains_key("save:golfers"));
    // Picking the hand shown changes nothing.
    let marks = screen.click_mark(&mut app, &marks, "golfer_hand:rh");
    assert!(!app.settings.golfers.dirty);
    let marks = screen.click_mark(&mut app, &marks, "golfer_hand:lh");
    assert!(app.settings.golfers.dirty);
    assert!(enabled(&marks, "save:golfers"));
    let posted = app.settings.begin_save(SaveTarget::Golfers);
    assert_eq!(posted.golfers[0].handed, Some(flighthook::Handedness::Left));
    assert_eq!(posted.golfers[1].handed, Some(flighthook::Handedness::Left));
    app.settings.finish_save(Ok(()));
    // Sam: an edit elsewhere on the card saves the hand as loaded.
    app.settings.golfers.selected = 1;
    app.settings.golfers.golfers[1].record.name = "Sam Lee".into();
    app.settings.golfers.dirty = true;
    let posted = app.settings.begin_save(SaveTarget::Golfers);
    assert_eq!(posted.golfers[1].handed, Some(flighthook::Handedness::Left));
    assert_eq!(posted.golfers[1].name, "Sam Lee");
}

/// The stock card names show in full at 1600, connected or reconnecting.
#[test]
fn stock_card_names_show_in_full_at_1600() {
    let mut form = SettingsForm::default();
    form.load_from(&FlighthookConfig::default());
    for t in ["openconnect_server", "pureplay_accessory"] {
        let _ = form.add_actor(t);
    }
    let config = form.to_request();
    for status in [ActorStatus::Connected, ActorStatus::Reconnecting] {
        let screen = Screen::one_section(1600.0, 900.0);
        let mut app = FlighthookApp::with_pending(net::new_pending());
        app.settings.load_from(&config);
        for a in &app.settings.actors {
            let mut s = ActorStatusResponse::new(a.name(), ActorStatus::Connected);
            s.observe(ActorStatus::Connected, 1000);
            s.observe(status, 2000);
            app.actors.insert(a.global_id(), s);
        }
        for section in [SettingsSection::Devices, SettingsSection::Software] {
            app.settings_section = section;
            screen.frame(&mut app, Vec::new());
            let marks = screen.frame(&mut app, Vec::new());
            for a in &app.settings.actors {
                let id = a.global_id();
                let Some((title, _)) = marks.get(&format!("title:{id}")) else {
                    continue;
                };
                let natural = screen.0.fonts_mut(|f| {
                    f.layout_no_wrap(
                        a.name().to_owned(),
                        egui::FontId::proportional(16.0),
                        Color32::WHITE,
                    )
                    .size()
                    .x
                });
                assert!(
                    title.width() >= natural - 0.5,
                    "{id} {status:?}: {} of {natural}",
                    title.width()
                );
            }
        }
    }
}

/// A window as narrow as a phone draws the page without panicking.
#[test]
fn a_narrow_window_draws_the_page() {
    let screen = Screen::one_section(390.0, 844.0);
    let mut app = app();
    app.active_tab = Tab::Devices;
    let marks = settled(&screen, &mut app);
    assert!(marks.contains_key("card:mevo.0"));
}

/// Golfers: one row per golfer, each opening its own page; the trash on
/// the page removes that golfer, and Cancel brings it back. Golfer edits
/// lock the list and (+).
#[test]
fn each_golfer_has_a_row_and_a_page_with_a_trash_icon() {
    let screen = Screen::one_section(1600.0, 1000.0);
    let mut app = app();
    let mut config = app.settings.to_request();
    config.golfers = vec![
        flighthook::GolferRecord::new("flighthook.a", "Eric"),
        flighthook::GolferRecord::new("flighthook.b", "Sam"),
    ];
    app.settings.load_from(&config);
    let marks = settled(&screen, &mut app);
    assert!(!marks.contains_key("golfer_pick"), "no picker");
    let marks = screen.click_mark(&mut app, &marks, "settings_item:golfer.1");
    assert_eq!(app.settings_section, SettingsSection::Golfers);
    assert_eq!(app.settings.golfers.selected, 1);
    let title = |screen: &Screen, app: &mut FlighthookApp| {
        let (marks, shapes) = screen.painted(app, Vec::new());
        title_text(&marks, &shapes, "golfers")
    };
    assert_eq!(title(&screen, &mut app), "Sam");
    let marks = screen.click_mark(&mut app, &marks, "settings_item:golfer.0");
    assert_eq!(title(&screen, &mut app), "Eric");
    // The trash removes the golfer shown; Sam's row moves up.
    let marks = screen.click_mark(&mut app, &marks, "golfer_remove");
    assert_eq!(app.settings.golfers.golfers.len(), 1);
    assert!(!marks.contains_key("settings_item:golfer.1"));
    assert_eq!(title(&screen, &mut app), "Sam");
    assert!(enabled(&marks, "save:golfers"));
    // Golfer edits lock the rows and (+) as every other page's edits do.
    for key in [
        "settings_item:golfer.0",
        "settings_item:global",
        "add:Golfers",
    ] {
        assert!(!enabled(&marks, key), "{key} locked");
    }
    let marks = screen.click_mark(&mut app, &marks, "cancel:golfers");
    assert!(enabled(&marks, "add:Golfers"));
    assert!(marks.contains_key("settings_item:golfer.1"));
    assert_eq!(app.settings.golfers.golfers.len(), 2);
}

/// On the phone the list is the Settings page, its rows touch-sized; a
/// row opens its page full width under a back icon, which returns to the
/// list. While the page has unsaved edits, back is locked.
#[test]
fn on_the_phone_a_row_opens_its_page_and_back_returns_to_the_list() {
    let screen = Screen::one_section(390.0, 844.0);
    let mut app = app();
    app.active_tab = Tab::Devices;
    app.portrait = true;
    let marks = settled(&screen, &mut app);
    assert!(!marks.contains_key("settings_back"));
    assert!(
        !marks.keys().any(|k| k.starts_with("card:")),
        "the list only"
    );
    let (row, _) = marks["settings_item:mevo.0"];
    assert!((row.height() - crate::topbar::TOUCH_TARGET).abs() < 0.5);
    assert!(row.right() <= 390.0 && row.width() > 300.0, "{row:?}");
    let marks = screen.click_mark(&mut app, &marks, "settings_item:openconnect_client.0");
    let (card, _) = marks["card:openconnect_client.0"];
    let (back, _) = marks["settings_back"];
    assert!(card.contains_rect(back) && card.width() > 330.0, "{card:?}");
    assert!(card.right() <= 390.0, "{card:?}");
    assert!(!marks.keys().any(|k| k.starts_with("settings_item:")));
    // The telemetry over the settings.
    let bodies = screen.bodies();
    let (telemetry, _, _) = bodies["openconnect_client.0.telemetry"];
    let (settings, _, _) = bodies["openconnect_client.0"];
    assert!(telemetry.bottom() < settings.top());
    // Edits lock back.
    edit_name(&mut app, "openconnect_client.0");
    let marks = settled(&screen, &mut app);
    assert!(!enabled(&marks, "settings_back"));
    let marks = screen.click_mark(&mut app, &marks, "settings_back");
    assert!(marks.contains_key("card:openconnect_client.0"));
    let marks = screen.click_mark(&mut app, &marks, "cancel:openconnect_client.0");
    let marks = screen.click_mark(&mut app, &marks, "settings_back");
    assert!(!marks.contains_key("card:openconnect_client.0"));
    assert!(marks.contains_key("settings_item:openconnect_client.0"));
}

/// On the phone Cancel on a new card leaves the list in view, not
/// another item's page.
#[test]
fn on_the_phone_cancel_on_a_new_card_returns_to_the_list() {
    let screen = Screen::one_section(390.0, 844.0);
    let mut app = app();
    app.active_tab = Tab::Devices;
    app.portrait = true;
    let marks = settled(&screen, &mut app);
    let marks = screen.click_mark(&mut app, &marks, "add:Launch monitors");
    let _ = screen.click_mark(&mut app, &marks, "add:Launch monitors:Open Connect Server");
    let id = "openconnect_server.0";
    let marks = settled(&screen, &mut app);
    assert!(
        marks.contains_key(&format!("card:{id}")),
        "the new card's page"
    );
    let _ = screen.click_mark(&mut app, &marks, &format!("cancel:{id}"));
    let marks = settled(&screen, &mut app);
    assert!(!app.settings_open);
    assert!(
        !marks.keys().any(|k| k.starts_with("card:")),
        "the list, no page"
    );
    assert!(marks.contains_key("settings_item:mevo.0"));
}

/// On the phone a page's title sits on the back icon's row, in full,
/// with the on/off switch after it; the type tag, status chip and icons
/// (Save and Cancel while dirty) go on the row under it. The back icon and
/// the header icons are touch targets.
#[test]
fn on_the_phone_the_page_title_shares_the_back_row() {
    let screen = Screen::one_section(390.0, 844.0);
    let mut app = app();
    app.active_tab = Tab::Devices;
    app.portrait = true;
    let id = "openconnect_client.0";
    app.actors.get_mut(id).unwrap().name = "Launch Monitor".into();
    let marks = settled(&screen, &mut app);
    let _ = screen.click_mark(&mut app, &marks, &format!("settings_item:{id}"));
    let marks = settled(&screen, &mut app);
    let (back, _) = marks["settings_back"];
    let (title, _) = marks[&format!("title:{id}")];
    let (switch, _) = marks[&format!("enabled:{id}")];
    let row = back.y_range();
    assert!(row.contains(title.center().y), "{title:?} {back:?}");
    assert!(row.contains(switch.center().y), "{switch:?} {back:?}");
    assert!(back.right() <= title.left() && title.right() <= switch.left());
    assert!(
        screen.texts(&mut app).iter().any(|t| t == "Launch Monitor"),
        "the title is cut short"
    );
    let size = crate::topbar::TOUCH_TARGET;
    assert!(back.width() >= size && back.height() >= size, "{back:?}");
    for name in ["tag", "badge", "reconnect", "remove"] {
        let (rect, _) = marks[&format!("{name}:{id}")];
        assert!(rect.top() >= back.bottom(), "{name} {rect:?}");
        assert!(rect.right() <= 390.0, "{name} {rect:?}");
    }
    for name in ["reconnect", "remove"] {
        let (rect, _) = marks[&format!("{name}:{id}")];
        assert!(
            rect.width() >= size && rect.height() >= size,
            "{name} {rect:?}"
        );
    }
    // The tag keeps its own height beside the touch-sized icons.
    assert!(marks[&format!("tag:{id}")].0.height() < size);
    // Dirty: Save and Cancel on the row under the title, touch targets
    // at least 8 pt apart.
    edit_name(&mut app, id);
    let marks = settled(&screen, &mut app);
    for name in ["save", "cancel"] {
        let (rect, _) = marks[&format!("{name}:{id}")];
        assert!(rect.top() >= back.bottom(), "{name} {rect:?}");
        assert!(rect.height() >= size, "{name} {rect:?}");
    }
    let (save, cancel) = (
        marks[&format!("save:{id}")].0,
        marks[&format!("cancel:{id}")].0,
    );
    assert!(save.left() - cancel.right() >= 8.0, "{cancel:?} {save:?}");
}

/// On the phone the rows of a (+) menu are touch targets.
#[test]
fn on_the_phone_the_add_menu_rows_are_touch_sized() {
    let screen = Screen::one_section(390.0, 844.0);
    let mut app = app();
    app.active_tab = Tab::Devices;
    app.portrait = true;
    let marks = settled(&screen, &mut app);
    let marks = screen.click_mark(&mut app, &marks, "add:Launch monitors");
    let (row, _) = marks["add:Launch monitors:Open Connect Server"];
    assert!(row.height() >= crate::topbar::TOUCH_TARGET, "{row:?}");
}

/// On the phone a header whose icons fit beside the title keeps them on
/// the back icon's row: a golfer's trash; a plain form draws one row.
#[test]
fn on_the_phone_a_short_page_header_is_one_row() {
    let screen = Screen::one_section(390.0, 844.0);
    let mut app = app();
    app.active_tab = Tab::Devices;
    app.portrait = true;
    let mut config = app.settings.to_request();
    config.golfers = vec![flighthook::GolferRecord::new("flighthook.a", "Eric")];
    app.settings.load_from(&config);
    let marks = settled(&screen, &mut app);
    let _ = screen.click_mark(&mut app, &marks, "settings_item:golfer.0");
    let marks = settled(&screen, &mut app);
    let (back, _) = marks["settings_back"];
    let (trash, _) = marks["golfer_remove"];
    assert!(
        back.y_range().contains(trash.center().y),
        "{trash:?} {back:?}"
    );
    assert!(trash.right() <= 390.0);
    // Back on the list, then Replay: the first setting right under the
    // back row.
    let marks = screen.click_mark(&mut app, &marks, "settings_back");
    let _ = screen.click_mark(&mut app, &marks, "settings_item:replay");
    let marks = settled(&screen, &mut app);
    let (back, _) = marks["settings_back"];
    let below = marks
        .iter()
        .filter(|(k, _)| {
            !k.starts_with("settings_") && !k.starts_with("title:") && !k.starts_with("nav")
        })
        .map(|(_, (r, _))| r.top())
        .filter(|top| *top >= back.bottom())
        .fold(f32::MAX, f32::min);
    assert!(below - back.bottom() < 20.0, "{below} {back:?}");
}

/// Wider than a phone the page header stays one row, its icons 24 pt.
#[test]
fn off_the_phone_the_page_header_is_one_row() {
    let screen = Screen::one_section(1600.0, 900.0);
    let mut app = app();
    app.active_tab = Tab::Devices;
    let id = "openconnect_client.0";
    let marks = settled(&screen, &mut app);
    let marks = screen.click_mark(&mut app, &marks, &format!("settings_item:{id}"));
    assert!(!marks.contains_key("settings_back"));
    let (title, _) = marks[&format!("title:{id}")];
    let (remove, _) = marks[&format!("remove:{id}")];
    assert!(remove.y_range().contains(title.center().y));
    assert!(remove.width() < crate::topbar::TOUCH_TARGET, "{remove:?}");
}
