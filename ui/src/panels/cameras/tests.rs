//! Tests of the Cameras tab: live views, the inline player, swing analysis and grades.

use super::*;
use crate::panels::devices::Marks;
use crate::panels::settings::SettingsForm;
use crate::types::{
    ActorStatus, ActorStatusResponse, CameraPosition, FlighthookConfig, ShotKey, ShotRow, ShotVideo,
};
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect, vec2};
use flighthook::ShotGrouping;

#[test]
fn values_that_round_to_zero_show_no_minus_sign() {
    assert_eq!(fixed(-0.004, 2), "0.00");
    assert_eq!(fixed(-0.3, 0), "0");
    assert_eq!(fixed(-0.6, 0), "-1");
    assert_eq!(degrees(-0.2), "0\u{b0}");
    assert_eq!(fixed(1.25, 1), "1.2");
}

/// A saved config with cameras `indices` (camera.N named "Cam N"):
/// camera.0 faces the golfer, the others look down the target line.
/// A config with cameras `indices`, body tracking in Live mode (the
/// mode most of these tests are about).
fn config(indices: &[u32]) -> FlighthookConfig {
    let mut config = FlighthookConfig::default();
    config.body_tracking.mode = BodyTrackingMode::Live;
    for i in indices {
        config.camera.insert(
            i.to_string(),
            CameraSection {
                name: format!("Cam {i}"),
                camera: CameraSection::TEST_PATTERN.into(),
                position: if *i == 0 {
                    CameraPosition::GolferFront
                } else {
                    CameraPosition::TargetLine
                },
                ..CameraSection::default()
            },
        );
    }
    config
}

/// The app on the Shots tab's Cards view, showing the shot's data, with
/// cameras `cameras` connected.
fn app(cameras: &[u32], supported: bool) -> FlighthookApp {
    let mut app = FlighthookApp::with_pending(crate::net::new_pending());
    let mut form = SettingsForm::default();
    form.load_from(&config(cameras));
    app.settings = form;
    app.cameras_supported = supported;
    app.active_tab = Tab::Shots;
    app.shots_view = ShotsView::Cards;
    app.shots_pane = ShotsPane::Data;
    for i in cameras {
        app.actors.insert(
            format!("camera.{i}"),
            ActorStatusResponse::new(format!("Cam {i}"), ActorStatus::Connected),
        );
    }
    app
}

/// Draws the tab bar and the active tab's panel (the Shots tab, or
/// nothing), as the app does; keeps what the last frame painted as text
/// and the centers of the circles it painted.
struct Screen(
    egui::Context,
    egui::Vec2,
    std::cell::RefCell<Vec<String>>,
    std::cell::RefCell<Vec<Pos2>>,
    std::cell::RefCell<Vec<(String, egui::Color32)>>,
);

impl Screen {
    fn new() -> Self {
        Self::sized(1600.0, 900.0)
    }

    fn sized(w: f32, h: f32) -> Self {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        Self(
            ctx,
            vec2(w, h),
            std::cell::RefCell::default(),
            std::cell::RefCell::default(),
            std::cell::RefCell::default(),
        )
    }

    fn frame(&self, app: &mut FlighthookApp, events: Vec<Event>) -> Marks {
        self.0.data_mut(|d| d.remove_temp::<Marks>(egui::Id::NULL));
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.1)),
            events,
            ..Default::default()
        };
        let out = self.0.run(input, |ctx| {
            app.sync_cameras_pane();
            app.drop_hidden_card_clips();
            app.sync_live_views(ctx);
            app.render_header_in_test(ctx);
            egui::CentralPanel::default().show(ctx, |ui| {
                if app.active_tab == Tab::Shots {
                    app.render_shots_panel(ui);
                }
            });
        });
        *self.2.borrow_mut() = painted_texts(&out.shapes);
        *self.3.borrow_mut() = painted_circles(&out.shapes);
        *self.4.borrow_mut() = painted_colored(&out.shapes);
        self.0
            .data(|d| d.get_temp::<Marks>(egui::Id::NULL))
            .unwrap_or_default()
    }

    /// What the last frame painted as text.
    fn texts(&self) -> Vec<String> {
        self.2.borrow().clone()
    }

    /// The color of the last frame's text `text` (the first painted).
    fn color_of(&self, text: &str) -> Option<egui::Color32> {
        self.4
            .borrow()
            .iter()
            .find(|(t, _)| t == text)
            .map(|(_, c)| *c)
    }

    /// How many circles the last frame painted inside `rect`.
    fn circles_in(&self, rect: Rect) -> usize {
        self.3
            .borrow()
            .iter()
            .filter(|c| rect.contains(**c))
            .count()
    }

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
}

#[test]
fn the_cameras_tab_and_live_show_with_a_configured_camera_where_cameras_work() {
    let screen = Screen::new();
    for (cameras, supported, shown) in [
        (&[0][..], true, true),
        (&[0, 1][..], true, true),
        (&[][..], true, false),
        (&[0][..], false, false),
    ] {
        let mut app = app(cameras, supported);
        assert_eq!(app.cameras_shown(), shown, "{cameras:?} {supported}");
        for view in [ShotsView::Table, ShotsView::Cards] {
            app.shots_view = view;
            let marks = screen.frame(&mut app, Vec::new());
            assert_eq!(
                marks["nav:cameras"].1, shown,
                "the Cameras nav icon: {view:?} {cameras:?} {supported}"
            );
            assert!(!marks.contains_key("pane_toggle"), "no Data | Cameras");
            assert!(marks.contains_key("view_toggle"), "{view:?}");
            assert!(!marks.contains_key("live_toggle"), "not in the Shots tab");
        }
        // With no camera the icon does nothing.
        let marks = screen.frame(&mut app, Vec::new());
        screen.click(&mut app, marks["nav:cameras"].0.center());
        assert_eq!(app.cameras_pane_shown(), shown, "{cameras:?} {supported}");
        app.set_shots_pane(ShotsPane::Cameras);
        let marks = screen.frame(&mut app, Vec::new());
        assert_eq!(
            marks.contains_key("live_toggle"),
            shown,
            "Live in the Cameras tab: {cameras:?} {supported}"
        );
        assert!(!marks.contains_key("live_entry"), "nothing over the list");
        app.set_shots_pane(ShotsPane::Data);
    }
    // Not before the settings have loaded.
    let mut app = FlighthookApp::with_pending(crate::net::new_pending());
    app.cameras_supported = true;
    assert!(!app.cameras_shown());
}

/// The shots navigation is one left column in the Shots tab (Cards and
/// Table) and the Cameras tab: the same place, the same width, running
/// the full height beside the tab's toolbar.
#[test]
fn the_shots_navigation_is_the_same_left_column_in_both_tabs() {
    let screen = Screen::new();
    let mut app = app_with_shots(3);
    let keys = [
        "view_selector",
        "export",
        "clear_history",
        "shot_entry:0",
        "shot_entry:2",
    ];
    let rects = |marks: &Marks| keys.map(|k| marks[k].0);
    let cards = screen.frame(&mut app, Vec::new());
    let toolbar = cards["view_toggle"].0;
    let list = cards["view_selector"].0;
    assert!(toolbar.left() > list.right(), "the toolbar is right of it");
    let header = cards["export"].0;
    // One row height for both; the Cards | Table pill sits 2 px below its
    // row's center.
    assert!(
        (header.center().y - toolbar.center().y).abs() <= 2.5,
        "the header row centers on the toolbar's line: {header:?} {toolbar:?}"
    );
    assert!(
        cards["export"].0.right() <= cards["clear_history"].0.left() + 0.5
            && cards["clear_history"].0.right() <= list.right() + 4.0,
        "export, then trash at the header's right end"
    );
    app.shots_view = ShotsView::Table;
    let table = screen.frame(&mut app, Vec::new());
    assert_eq!(rects(&table), rects(&cards), "Table");
    assert_eq!(table["view_toggle"].0, toolbar);
    let marks = screen.click(&mut app, cards["nav:cameras"].0.center());
    assert!(app.cameras_pane_shown());
    assert_eq!(rects(&marks), rects(&cards), "Cameras");
    assert!(marks["live_toggle"].0.left() > list.right());
}

/// Each tab has its own toolbar right of the navigation: the Shots tab
/// `Cards | Table` (the legend in Table only), the Cameras tab Live and
/// the reference picker. `Data | Cameras` is gone.
#[test]
fn each_tab_has_its_own_toolbar() {
    let screen = Screen::new();
    let mut app = app(&[0], true);
    let marks = screen.frame(&mut app, Vec::new());
    let view = marks["view_toggle"].0;
    assert!(!marks.contains_key("pane_toggle"));
    assert!(
        !marks.contains_key("live_toggle"),
        "no Live in the Shots tab"
    );
    assert!(!marks.contains_key("body_reference"));
    assert!(!screen.texts().iter().any(|t| t == "italic = simulated"));
    app.shots_view = ShotsView::Table;
    screen.frame(&mut app, Vec::new());
    assert!(screen.texts().iter().any(|t| t == "italic = simulated"));
    app.shots_view = ShotsView::Cards;

    // Cameras: Cards | Table is not drawn and the choice is kept; the
    // Live switch takes its slot, as tall as the pills, the reference
    // picker after it.
    screen.click(&mut app, marks["nav:cameras"].0.center());
    let marks = screen.frame(&mut app, Vec::new());
    assert!(app.cameras_pane_shown());
    assert!(!marks.contains_key("view_toggle"));
    assert!(!marks.contains_key("pane_toggle"));
    let live = marks["live_toggle"].0;
    assert!((live.left() - view.left()).abs() < 2.0, "{live:?} {view:?}");
    assert!((live.center().y - view.center().y).abs() < 1.0);
    assert!((live.height() - view.height()).abs() < 1.0);
    let reference = marks["body_reference"].0;
    assert!(reference.left() >= live.right());
    assert!(!screen.texts().iter().any(|t| t == "italic = simulated"));
    assert_eq!(app.shots_view, ShotsView::Cards);

    // Back to Shots: it returns where it was, with the same choice.
    screen.click(&mut app, marks["nav:shots"].0.center());
    let marks = screen.frame(&mut app, Vec::new());
    assert_eq!(marks["view_toggle"].0, view);
    assert!(!marks.contains_key("live_toggle"));
}

/// Cameras picked from the Table view gives the screen it gives from
/// the Cards view: the shots navigation, the Cameras pane and the Live
/// switch in the Cards | Table slot. Shots brings the Table view back.
#[test]
fn cameras_from_the_table_view_is_the_cameras_pane_of_the_cards_view() {
    let from = |view: ShotsView| {
        let screen = Screen::new();
        let mut app = app_with_shots(3);
        app.shots_view = view;
        let marks = screen.frame(&mut app, Vec::new());
        let before = (marks["view_selector"].0, marks["view_toggle"].0);
        assert!(marks.contains_key("view_selector"), "the shots navigation");
        screen.click(&mut app, marks["nav:cameras"].0.center());
        let marks = screen.frame(&mut app, Vec::new());
        let texts = screen.texts();
        (screen, app, marks, texts, before)
    };
    let (_, _, cards, cards_texts, _) = from(ShotsView::Cards);
    let (screen, mut app, table, table_texts, (pane, view)) = from(ShotsView::Table);

    assert!(app.cameras_pane_shown());
    assert_eq!(app.shots_layout(), ShotsLayout::Cameras);
    assert_eq!(app.shots_view, ShotsView::Table, "the choice is kept");
    assert!(!table.contains_key("view_toggle"));
    assert!(table.contains_key("view_selector"), "the shot list");
    assert!(!app.live_selected);
    assert_eq!(app.shown_index(), Some(0), "the selected shot kept");
    for mark in ["live_toggle", "view_selector", "shot_entry:0"] {
        assert_eq!(table[mark].0, cards[mark].0, "{mark}");
    }
    assert_eq!(table.len(), cards.len());
    assert_eq!(table_texts, cards_texts, "the same screen");

    // Data: the Table view again, Cards | Table where it was.
    screen.click(&mut app, table["nav:shots"].0.center());
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!app.cameras_pane_shown());
    assert_eq!(app.shots_layout(), ShotsLayout::Table);
    assert_eq!(app.shots_view, ShotsView::Table);
    assert_eq!(marks["view_selector"].0, pane);
    assert_eq!(marks["view_toggle"].0, view);
    assert!(!marks.contains_key("live_toggle"));
}

/// A kept Cameras choice opens on the Cameras pane in the Table view.
#[test]
fn a_kept_cameras_choice_opens_the_cameras_pane_in_the_table_view() {
    let screen = Screen::new();
    app(&[0], true).set_shots_pane(ShotsPane::Cameras);
    let kept = ShotsPane::stored();
    // Data is stored again before anything is checked, so a failure
    // leaves no Cameras choice for later tests on this thread.
    crate::local::store(PANE_KEY, ShotsPane::Data.key());
    assert_eq!(kept, ShotsPane::Cameras);
    let mut app = app(&[0], true);
    app.shots_pane = kept;
    app.shots_view = ShotsView::Table;
    let marks = screen.frame(&mut app, Vec::new());
    assert!(app.cameras_pane_shown());
    assert!(app.live_selected, "no shots: Live");
    assert!(marks.contains_key("live_toggle"));
    assert!(marks.contains_key("live_title"));
    assert!(!marks.contains_key("view_toggle"));
}

#[test]
fn the_pane_choice_is_kept_and_data_leaves_live() {
    let screen = Screen::new();
    let mut app = app(&[0], true);
    let marks = screen.frame(&mut app, Vec::new());
    let at = marks["nav:cameras"].0.center();
    screen.click(&mut app, at);
    assert_eq!(app.shots_pane, ShotsPane::Cameras);
    assert!(app.cameras_pane_shown());
    assert_eq!(
        ShotsPane::stored(),
        ShotsPane::Cameras,
        "kept on this device"
    );
    // No shots: Live; Data leaves it.
    assert!(app.live_selected);
    let marks = screen.frame(&mut app, Vec::new());
    let at = marks["nav:shots"].0.center();
    screen.click(&mut app, at);
    assert!(!app.live_selected);
    assert_eq!(app.shots_pane, ShotsPane::Data);
    assert!(!app.cameras_pane_shown());
    assert_eq!(ShotsPane::stored(), ShotsPane::Data);
}

/// The app of [`app`] with shots #1..=`n` from the mock monitor, the
/// first one pinned in the shot list.
fn app_with_shots(n: u32) -> FlighthookApp {
    let mut app = app(&[0], true);
    app.shots = (1..=n).map(|i| shot_row(i, &[])).collect();
    app.shots_rev += 1;
    app.observe_shots(0.0);
    app.select_shot(0);
    app
}

#[test]
fn entering_the_cameras_pane_without_shots_turns_live_on() {
    let screen = Screen::new();
    let mut app = app(&[0], true);
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!app.live_selected);
    let marks = screen.click(&mut app, marks["nav:cameras"].0.center());
    assert!(app.live_selected, "no shot history: Live");
    assert!(marks.contains_key("live_title"));
    assert!(marks.contains_key("live_toggle"));
}

#[test]
fn entering_the_cameras_pane_with_shots_shows_the_latest_shot() {
    let screen = Screen::new();
    let mut app = app_with_shots(3);
    let marks = screen.frame(&mut app, Vec::new());
    assert_eq!(app.shown_index(), Some(0), "#1 pinned");
    // From the Shots tab: the selected shot is kept.
    screen.click(&mut app, marks["nav:cameras"].0.center());
    assert!(!app.live_selected);
    assert_eq!(app.shown_index(), Some(0), "the pin kept");
    assert!(!app.following());

    // Leaving for another tab and coming back applies the rule ...
    app.select_live();
    app.select_shot(0);
    assert!(!app.live_selected, "picking a shot leaves Live");
    app.select_live();
    app.active_tab = Tab::Log;
    screen.frame(&mut app, Vec::new());
    assert!(app.live_selected, "kept off screen");
    app.active_tab = Tab::Shots;
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!app.live_selected);
    assert!(!marks.contains_key("live_title"));
    assert_eq!(app.shown_index(), Some(2));

    // ... and so does opening the Cameras tab from another tab.
    app.select_shot(0);
    app.set_shots_pane(ShotsPane::Data);
    app.shots_view = ShotsView::Table;
    app.active_tab = Tab::Log;
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!app.cameras_pane_shown());
    screen.click(&mut app, marks["nav:cameras"].0.center());
    assert_eq!(app.shots_view, ShotsView::Table);
    assert!(app.cameras_pane_shown());
    assert!(!app.live_selected);
    assert_eq!(app.shown_index(), Some(2), "pin dropped");

    // Showing the pane again with no shots: Live.
    app.shots.clear();
    app.shots_rev += 1;
    app.active_tab = Tab::Log;
    screen.frame(&mut app, Vec::new());
    app.active_tab = Tab::Shots;
    screen.frame(&mut app, Vec::new());
    assert!(app.live_selected);
}

#[test]
fn entering_the_cameras_pane_on_a_device_without_shots_turns_live_on() {
    let screen = Screen::new();
    let mut app = app_with_shots(2);
    let mut trigger = shot_row(3, &[]);
    trigger.actor = "mock_monitor.1".into();
    trigger.launch = None;
    app.shots.push(trigger);
    app.shots_rev += 1;
    app.device_filter = Some("mock_monitor.1".into());
    let marks = screen.frame(&mut app, Vec::new());
    assert_eq!(app.shown_index(), None, "no shot with data on it");
    screen.click(&mut app, marks["nav:cameras"].0.center());
    assert!(app.live_selected);
}

#[test]
fn the_live_switch_turns_live_on_and_off() {
    let screen = Screen::new();
    let mut app = app_with_shots(3);
    app.set_shots_pane(ShotsPane::Cameras);
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!app.live_selected);
    let live = marks["live_toggle"].0.center();
    let marks = screen.click(&mut app, live);
    assert!(app.live_selected);
    assert!(marks.contains_key("live_title"));
    // Off: the latest shot.
    let marks = screen.click(&mut app, live);
    assert!(!app.live_selected);
    assert!(!marks.contains_key("live_title"));
    assert_eq!(app.shown_index(), Some(2));
    // Picking a shot leaves Live; off again shows the picked shot.
    screen.click(&mut app, live);
    screen.click(&mut app, marks["shot_entry:0"].0.center());
    assert!(!app.live_selected);
    assert_eq!(app.shown_index(), Some(0));
    screen.click(&mut app, live);
    assert!(app.live_selected);
    let marks = screen.click(&mut app, live);
    assert_eq!(app.shown_index(), Some(0), "the pinned shot");
    // A new shot keeps Live.
    screen.click(&mut app, live);
    app.shots.push(shot_row(4, &[]));
    app.shots_rev += 1;
    app.observe_shots(5.0);
    screen.frame(&mut app, Vec::new());
    assert!(app.live_selected);
    // Data leaves Live.
    screen.click(&mut app, marks["nav:shots"].0.center());
    assert!(!app.live_selected);
    assert!(!app.cameras_pane_shown());
}

/// The selected shot, the list's scroll position, the grouping and the
/// device filter are one state shared by the Shots and Cameras tabs.
#[test]
fn the_tabs_share_selection_scroll_grouping_and_the_device_filter() {
    use egui::{Event, Modifiers};
    let screen = Screen::new();
    let mut app = app_with_shots(40);
    for (k, shot) in app.shots.iter_mut().enumerate() {
        if k % 2 == 1 {
            shot.actor = "mock_monitor.1".into();
        }
    }
    app.shots_rev += 1;
    app.groups.mode = ShotGrouping::None;
    screen.frame(&mut app, Vec::new());
    let marks = screen.frame(&mut app, Vec::new());
    // Two devices: the dropdown; pick the second monitor.
    assert!(marks.contains_key("device-filter"));
    let open = screen.click(&mut app, marks["device-filter"].0.center());
    screen.click(&mut app, open["device-filter:2"].0.center());
    assert_eq!(app.device_filter.as_deref(), Some("mock_monitor.1"));
    // Scroll the list, then pick a shot in view.
    let entries = |marks: &Marks| {
        let mut v: Vec<(String, Rect)> = marks
            .iter()
            .filter(|(k, _)| k.starts_with("shot_entry:"))
            .map(|(k, (r, _))| (k.clone(), *r))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    };
    let marks = screen.frame(&mut app, Vec::new());
    let unscrolled = entries(&marks);
    let over = egui::pos2(marks["view_selector"].0.center().x, 450.0);
    let wheel = Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, 300.0),
        modifiers: Modifiers::NONE,
    };
    screen.frame(&mut app, vec![Event::PointerMoved(over), wheel]);
    for _ in 0..90 {
        screen.frame(&mut app, Vec::new());
    }
    let marks = screen.frame(&mut app, Vec::new());
    let scrolled = entries(&marks);
    assert_ne!(scrolled, unscrolled, "the list scrolled");
    let pick = scrolled
        .iter()
        .filter(|(_, r)| r.top() > 120.0 && r.bottom() < 800.0)
        .map(|(k, _)| k.clone())
        .next_back()
        .expect("an entry in view");
    let i: usize = pick["shot_entry:".len()..].parse().unwrap();
    screen.click(&mut app, marks[pick.as_str()].0.center());
    assert_eq!(app.shown_index(), Some(i));
    let marks = screen.frame(&mut app, Vec::new());
    let before = entries(&marks);
    // The Cameras tab: the same entries in the same places, the same
    // shot, grouping and filter.
    let marks = screen.click(&mut app, marks["nav:cameras"].0.center());
    assert!(app.cameras_pane_shown());
    assert_eq!(entries(&marks), before, "scroll position and rows kept");
    assert_eq!(app.shown_index(), Some(i), "selection kept");
    assert!(!app.live_selected);
    assert_eq!(app.groups.mode, ShotGrouping::None);
    assert_eq!(app.device_filter.as_deref(), Some("mock_monitor.1"));
    assert!(marks.contains_key("device-filter"), "the dropdown here too");
    // A grouping picked here holds in the Shots tab.
    app.groups.mode = ShotGrouping::BySet;
    let marks = screen.frame(&mut app, Vec::new());
    let marks = screen.click(&mut app, marks["nav:shots"].0.center());
    assert!(!app.cameras_pane_shown());
    assert_eq!(app.groups.mode, ShotGrouping::BySet);
    assert_eq!(app.shown_index(), Some(i), "selection kept");
    assert_eq!(app.device_filter.as_deref(), Some("mock_monitor.1"));
    // A device picked in the Cameras tab filters the Shots tab.
    let marks = screen.click(&mut app, marks["nav:cameras"].0.center());
    let open = screen.click(&mut app, marks["device-filter"].0.center());
    screen.click(&mut app, open["device-filter:0"].0.center());
    assert_eq!(app.device_filter, None, "All");
    let marks = screen.frame(&mut app, Vec::new());
    screen.click(&mut app, marks["nav:shots"].0.center());
    assert_eq!(app.device_filter, None);
}

/// Replay, export and trash work the same from the Shots tab and the
/// Cameras tab.
#[test]
fn replay_export_and_trash_work_from_both_tabs() {
    let screen = Screen::new();
    for cameras_tab in [false, true] {
        let at = if cameras_tab { "Cameras" } else { "Shots" };
        let mut app = app(&[0], true);
        app.shots = (1..=3).map(|n| shot_row(n, &["camera.0"])).collect();
        app.shots_rev += 1;
        app.observe_shots(0.0);
        if cameras_tab {
            app.set_shots_pane(ShotsPane::Cameras);
        }
        screen.frame(&mut app, Vec::new());
        let marks = screen.frame(&mut app, Vec::new());
        assert_eq!(app.cameras_pane_shown(), cameras_tab, "{at}");
        // Replay: the shot header's chip opens the replay window.
        assert!(app.replay.is_none());
        screen.click(&mut app, marks["replay"].0.center());
        assert!(app.replay.is_some(), "{at}: the replay opens");
        app.replay = None;
        // Export: the icon opens the format menu.
        let marks = screen.frame(&mut app, Vec::new());
        let menu = screen.click(&mut app, marks["export"].0.center());
        assert!(
            menu.contains_key("export:jsonl") && menu.contains_key("export:csv"),
            "{at}: the export menu"
        );
        screen.frame(
            &mut app,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        // Trash: the confirmation opens.
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!app.confirm_clear);
        screen.click(&mut app, marks["clear_history"].0.center());
        assert!(app.confirm_clear, "{at}: the confirmation");
    }
}

/// The viewed golfer's shots are the list in both tabs.
#[test]
fn the_viewed_golfer_filters_the_list_in_both_tabs() {
    let screen = Screen::new();
    let mut app = app_with_shots(4);
    let mut with_golfers = config(&[0]);
    with_golfers.golfers = vec![
        flighthook::GolferRecord::new("flighthook.e", "Eric"),
        flighthook::GolferRecord::new("flighthook.s", "Sam"),
    ];
    app.settings.load_from(&with_golfers);
    for (k, shot) in app.shots.iter_mut().enumerate() {
        let golfer = if k % 2 == 0 {
            "flighthook.e"
        } else {
            "flighthook.s"
        };
        shot.golfer = Some(golfer.into());
    }
    app.shots_rev += 1;
    app.groups.mode = ShotGrouping::None;
    app.turn_golfer = Some("flighthook.s".into());
    for cameras_tab in [false, true] {
        app.set_shots_pane(if cameras_tab {
            ShotsPane::Cameras
        } else {
            ShotsPane::Data
        });
        screen.frame(&mut app, Vec::new());
        let marks = screen.frame(&mut app, Vec::new());
        assert_eq!(app.cameras_pane_shown(), cameras_tab);
        assert_eq!(app.viewed_golfer.as_deref(), Some("flighthook.s"));
        let mut listed: Vec<&str> = marks
            .keys()
            .filter(|k| k.starts_with("shot_entry:"))
            .map(String::as_str)
            .collect();
        listed.sort_unstable();
        assert_eq!(
            listed,
            ["shot_entry:1", "shot_entry:3"],
            "tab {cameras_tab}"
        );
    }
}

#[test]
fn clear_history_in_the_cameras_pane_turns_live_on() {
    let screen = Screen::new();
    for view in [ShotsView::Cards, ShotsView::Table] {
        let mut app = app_with_shots(2);
        app.shots_view = view;
        screen.frame(&mut app, Vec::new());
        // In the Data pane it changes nothing.
        app.clear_shot_history();
        screen.frame(&mut app, Vec::new());
        assert!(!app.live_selected, "{view:?}");
        app.shots = vec![shot_row(1, &[])];
        app.shots_rev += 1;
        app.set_shots_pane(ShotsPane::Cameras);
        let marks = screen.frame(&mut app, Vec::new());
        assert!(app.cameras_pane_shown(), "{view:?}");
        assert!(!app.live_selected, "{view:?}: the shot");
        assert!(!marks.contains_key("live_title"), "{view:?}");
        app.clear_shot_history();
        let marks = screen.frame(&mut app, Vec::new());
        assert!(app.live_selected, "{view:?}");
        assert!(marks.contains_key("live_title"), "{view:?}");
        assert_eq!(app.shots_view, view, "the Data pane's view is kept");
        app.set_shots_pane(ShotsPane::Data);
    }
}

#[test]
fn clicking_live_shows_a_live_view_per_camera_side_by_side() {
    let screen = Screen::new();
    let mut app = app(&[0, 2], true);
    app.shots = vec![shot_row(1, &[])];
    app.shots_rev += 1;
    app.set_shots_pane(ShotsPane::Cameras);
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!marks.contains_key("camera_live:camera.0"), "the shot");
    assert!(app.live.is_empty(), "no live fetch off Live");
    let at = marks["live_toggle"].0.center();
    let marks = screen.click(&mut app, at);
    assert!(app.live_selected);
    assert!(marks.contains_key("live_title"));
    let left = marks["camera_live:camera.0"].0;
    let right = marks["camera_live:camera.2"].0;
    assert!(left.right() < right.left(), "side by side");
    assert!((left.width() - right.width()).abs() < 0.5);
    assert!((left.height() - left.width() * DEFAULT_ASPECT).abs() < 0.5);
    assert!(
        left.left() > marks["view_selector"].0.right(),
        "right of the list"
    );
    assert_eq!(app.live.len(), 2);
}

#[test]
fn the_views_fetch_one_request_at_a_time_and_stop_off_live() {
    let screen = Screen::new();
    let mut app = app(&[0], true);
    app.select_live();
    screen.frame(&mut app, Vec::new());
    let view = &app.live["camera.0"];
    assert!(view.in_flight, "the first request went out");
    let id = view.id;
    // No second request while it is out.
    screen.frame(&mut app, Vec::new());
    assert!(app.live["camera.0"].in_flight);
    // A frame lands: the next asks after it.
    app.live_replies(
        vec![(
            id,
            LiveReply::Frame {
                seq: 7,
                jpeg: vec![1],
                pose: None,
                state: PoseState::default(),
            },
        )],
        0.0,
    );
    let view = app.live.get_mut("camera.0").unwrap();
    assert!(!view.in_flight);
    assert_eq!(view.due(0.0), Some(7));
    // A failure waits before the next.
    view.reply(LiveReply::Failed("refused".into()), 10.0);
    assert_eq!(view.due(10.5), None);
    assert_eq!(view.due(11.0), Some(7));
    // The same frame again is not taken.
    view.due(11.0);
    view.reply(
        LiveReply::Frame {
            seq: 7,
            jpeg: vec![2],
            pose: None,
            state: PoseState::default(),
        },
        11.0,
    );
    assert_eq!(view.jpeg.as_ref().map(|(j, _)| j.clone()), Some(vec![1]));
    // A lower number is a restarted server: taken, and asked after.
    view.due(11.0);
    view.reply(
        LiveReply::Frame {
            seq: 3,
            jpeg: vec![3],
            pose: None,
            state: PoseState::default(),
        },
        11.0,
    );
    assert_eq!(view.after, 3);
    assert_eq!(view.jpeg.as_ref().map(|(j, _)| j.clone()), Some(vec![3]));
    assert_eq!(view.due(11.0), Some(3));
    // Another tab drops the views; a late reply is ignored.
    app.active_tab = Tab::Log;
    screen.frame(&mut app, Vec::new());
    assert!(app.live.is_empty());
    app.live_replies(vec![(id, LiveReply::Nothing(PoseState::default()))], 12.0);
    assert!(app.live.is_empty());
    // Back on Live: new views, numbered afresh.
    app.active_tab = Tab::Shots;
    screen.frame(&mut app, Vec::new());
    assert!(app.live["camera.0"].id > id);
    // The Data pane drops them too.
    app.set_shots_pane(ShotsPane::Data);
    screen.frame(&mut app, Vec::new());
    assert!(app.live.is_empty());
}

#[test]
fn removing_the_cameras_leaves_live() {
    let screen = Screen::new();
    for view in [ShotsView::Cards, ShotsView::Table] {
        let mut app = app(&[0], true);
        app.shots_view = view;
        app.select_live();
        let marks = screen.frame(&mut app, Vec::new());
        assert!(app.cameras_pane_shown(), "{view:?}");
        assert!(marks.contains_key("live_title"), "{view:?}");
        assert_eq!(app.live.len(), 1, "{view:?}");
        app.settings.load_from(&config(&[]));
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!app.live_selected, "{view:?}");
        assert!(!app.cameras_pane_shown(), "{view:?}");
        assert!(!marks.contains_key("live_title"), "{view:?}");
        assert!(marks.contains_key("view_toggle"), "{view:?}: Cards | Table");
        assert_eq!(app.shots_view, view, "the Data pane's view is kept");
        assert_eq!(app.active_tab, Tab::Shots);
        assert!(app.live.is_empty(), "{view:?}");
        app.set_shots_pane(ShotsPane::Data);
    }
}

/// A 64×48 JPEG, as a camera sends one.
fn frame_jpeg() -> Vec<u8> {
    use stance::{Source, TestPattern, TestPatternConfig};
    let mut pattern = TestPattern::new(TestPatternConfig {
        width: 64,
        height: 48,
        paced: false,
        ..TestPatternConfig::default()
    })
    .unwrap();
    pattern
        .next_frame(std::time::Duration::ZERO)
        .unwrap()
        .unwrap()
        .jpeg
        .to_vec()
}

#[test]
fn a_frame_shows_at_its_own_aspect() {
    let screen = Screen::new();
    let mut app = app(&[0], true);
    app.select_live();
    screen.frame(&mut app, Vec::new());
    let id = app.live["camera.0"].id;
    app.live_replies(
        vec![(
            id,
            LiveReply::Frame {
                seq: 1,
                jpeg: frame_jpeg(),
                pose: None,
                state: PoseState::default(),
            },
        )],
        0.0,
    );
    let marks = screen.frame(&mut app, Vec::new());
    let picture = marks["camera_live:camera.0"].0;
    assert!((picture.height() / picture.width() - 0.75).abs() < 0.01);
    assert!(app.live["camera.0"].jpeg.is_none(), "decoded");
    assert!(!marks.contains_key("camera_status:camera.0"));
}

#[test]
fn a_camera_not_connected_shows_its_status_and_no_picture() {
    let screen = Screen::new();
    let mut app = app(&[0], true);
    app.select_live();
    screen.frame(&mut app, Vec::new());
    let id = app.live["camera.0"].id;
    app.live_replies(
        vec![(
            id,
            LiveReply::Frame {
                seq: 1,
                jpeg: frame_jpeg(),
                pose: None,
                state: PoseState::default(),
            },
        )],
        0.0,
    );
    screen.frame(&mut app, Vec::new());
    let actor = app.actors.get_mut("camera.0").unwrap();
    actor.observe(ActorStatus::Reconnecting, 0);
    actor
        .telemetry
        .insert("error".into(), "camera unplugged".into());
    let marks = screen.frame(&mut app, Vec::new());
    // The last frame gives way to the status, worded as on the card.
    assert!(marks.contains_key("camera_status:camera.0"));
    let picture = marks["camera_live:camera.0"].0;
    assert!(picture.contains_rect(marks["camera_status:camera.0"].0));
    assert_eq!(app.actors["camera.0"].state().label(), "RECONNECTING");
    // No actor yet: like the card of a camera that has not started.
    app.actors.clear();
    let marks = screen.frame(&mut app, Vec::new());
    assert!(marks.contains_key("camera_status:camera.0"));
    assert_eq!(new_actor(String::new()).state().label(), "CONNECTING");
}

#[test]
fn columns_fit_the_panel_keeping_their_aspect() {
    let panel = vec2(1600.0, 900.0);
    // Two 16:9 cameras: limited by the width.
    let w = column_width(2, panel, 9.0 / 16.0);
    assert!((w - (1600.0 - GAP) / 2.0).abs() < 0.01);
    // One: limited by the height.
    let w = column_width(1, panel, 9.0 / 16.0);
    assert!((w * 9.0 / 16.0 + HEADING_H - 900.0).abs() < 0.01);
    // A portrait camera (16:9 on its side): limited by the height.
    let w = column_width(2, panel, 16.0 / 9.0);
    assert!((w * 16.0 / 9.0 + HEADING_H - 900.0).abs() < 0.01);
    assert_eq!(resolution_aspect("1280x720"), Some(0.5625));
    assert_eq!(resolution_aspect("640x0"), None);
    assert_eq!(resolution_aspect(""), None);
}

fn press(key: Key) -> Vec<Event> {
    vec![Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    }]
}

#[test]
fn the_readout_names_every_value_and_each_cameras_rate() {
    let s = BodyTrackingSection::default();
    let one = [CameraReading {
        name: "Face On".into(),
        fps: Some(14.83),
        pose_ms: Some(19.4),
    }];
    assert_eq!(
        readout(&s, &one),
        "RTMPose-m · min score 0.30 · smoothing 1.0 · detect every 10 · 14.8 fps · 19 ms"
    );
    let two = [
        one[0].clone(),
        CameraReading {
            name: "Down the Line".into(),
            fps: Some(14.6),
            pose_ms: None,
        },
    ];
    let tuned = BodyTrackingSection {
        model: flighthook::PoseModel::RtmposeS,
        min_score: 0.45,
        smoothing: 0.0,
        detect_every: 30,
        show_skeleton: false,
        ..s.clone()
    };
    assert_eq!(
        readout(&tuned, &two),
        "RTMPose-s · min score 0.45 · smoothing off · detect every 30 · skeleton off · \
             Face On 14.8 fps 19 ms · Down the Line 14.6 fps"
    );
}

fn pose(x: f32) -> LivePose {
    LivePose {
        skeleton: "halpe26".into(),
        keypoints: vec![[x, 10.0, 0.9]; 26],
        bbox: None,
        ms: 20.0,
        posture: None,
    }
}

#[test]
fn the_pose_drawn_is_the_one_that_came_with_the_picture() {
    let screen = Screen::new();
    let mut app = app(&[0], true);
    app.select_live();
    screen.frame(&mut app, Vec::new());
    let id = app.live["camera.0"].id;
    let frame = |seq, x| LiveReply::Frame {
        seq,
        jpeg: frame_jpeg(),
        pose: Some(pose(x)),
        state: PoseState {
            state: "ready".into(),
            error: None,
        },
    };
    app.live_replies(vec![(id, frame(1, 5.0))], 1.0);
    screen.frame(&mut app, Vec::new());
    assert_eq!(app.live["camera.0"].shown_pose, Some(pose(5.0)));
    // A newer frame waits to be decoded: the shown pose stays with the
    // shown picture until then.
    app.live_replies(vec![(id, frame(2, 7.0))], 1.1);
    assert_eq!(app.live["camera.0"].shown_pose, Some(pose(5.0)));
    screen.frame(&mut app, Vec::new());
    assert_eq!(app.live["camera.0"].shown_pose, Some(pose(7.0)));
    // A frame without a pose (models not ready) shows none.
    app.live_replies(
        vec![(
            id,
            LiveReply::Frame {
                seq: 3,
                jpeg: frame_jpeg(),
                pose: None,
                state: PoseState {
                    state: "loading".into(),
                    error: None,
                },
            },
        )],
        1.2,
    );
    let marks = screen.frame(&mut app, Vec::new());
    assert_eq!(app.live["camera.0"].shown_pose, None);
    // And the models' state shows over the picture.
    let band = marks["pose_status:camera.0"].0;
    assert!(marks["camera_live:camera.0"].0.contains_rect(band));
    assert_eq!(
        pose_status_text(&app.live["camera.0"].pose_state).as_deref(),
        Some("Loading body tracking models")
    );
}

#[test]
fn the_models_state_reads_plainly() {
    let state = |s: &str, e: Option<&str>| PoseState {
        state: s.into(),
        error: e.map(str::to_owned),
    };
    assert_eq!(
        pose_status_text(&state("downloading", None)).as_deref(),
        Some("Downloading body tracking models")
    );
    assert_eq!(
        pose_status_text(&state("missing", None)).as_deref(),
        Some("Body tracking models not downloaded")
    );
    assert_eq!(
        pose_status_text(&state("failed", Some("no network"))).as_deref(),
        Some("Body tracking failed: no network")
    );
    for quiet in ["ready", "off", ""] {
        assert_eq!(pose_status_text(&state(quiet, None)), None, "{quiet}");
    }
}

#[test]
fn a_views_rate_and_pose_time_are_measured() {
    let mut view = LiveView::new(1);
    assert_eq!(view.fps(0.0), None);
    for i in 0..16 {
        view.in_flight = true;
        view.reply(
            LiveReply::Frame {
                seq: i + 1,
                jpeg: Vec::new(),
                pose: Some(pose(1.0)),
                state: PoseState::default(),
            },
            10.0 + i as f64 / 15.0,
        );
    }
    let fps = view.fps(11.0).unwrap();
    assert!((fps - 15.0).abs() < 0.01, "{fps}");
    assert!((view.pose_ms.unwrap() - 20.0).abs() < 1e-4);
    // Long after the last frame: no rate.
    assert_eq!(view.fps(20.0), None);
}

#[test]
fn room_is_left_for_the_readout_line() {
    let screen = Screen::new();
    let mut app = app(&[0], true);
    app.select_live();
    let marks = screen.frame(&mut app, Vec::new());
    let picture = marks["camera_live:camera.0"].0;
    let line = marks["pose_readout"].0;
    assert!(line.top() >= picture.bottom(), "under the live views");
    assert!(line.bottom() <= 900.0, "on screen");
    // The posture tiles sit between the picture and the line.
    let tiles = marks["posture:camera.0"].0;
    assert!(tiles.top() >= picture.bottom() && tiles.bottom() <= line.top());
}

// -----------------------------------------------------------------------
// Phase C: a shot's cameras, swing data, live posture
// -----------------------------------------------------------------------

/// A face-on swing report, as stance writes it, with the golfer's height
/// when `cm`.
fn report(cm: bool) -> SwingReportFile {
    let cm_part = |top: f64, impact: f64| {
        if cm {
            format!(r#","cm":{{"top":{top},"impact":{impact}}}"#)
        } else {
            String::new()
        }
    };
    let json = format!(
        r#"{{"version":1,"view":"face_on","handed":"right",
            "events":{{"address":{{"t_ns":1000000000,"frame":120,"confidence":0.8}},
              "takeaway":{{"t_ns":1100000000,"frame":132,"confidence":0.8}},
              "top":{{"t_ns":1850000000,"frame":222,"confidence":0.9}},
              "impact":{{"t_ns":2150000000,"frame":258,"confidence":0.9}},
              "impact_source":"refined",
              "finish":{{"t_ns":2800000000,"frame":336,"confidence":0.7}}}},
            "tempo":{{"backswing_ns":750000000,"downswing_ns":300000000,"ratio":2.5}},
            "metrics":[
              {{"id":"hip_sway","view":"face_on","unit":"torso","values":{{"top":-0.05,"impact":0.12}}{},"confidence":0.8}},
              {{"id":"lateral_spine_tilt","view":"face_on","unit":"degrees","values":{{"address":8.4,"top":-3.6,"impact":21.2}},"confidence":0.8}},
              {{"id":"stance_width","view":"face_on","unit":"ratio","values":{{"address":1.234}},"confidence":0.8}},
              {{"id":"shoulder_tilt","view":"face_on","unit":"degrees","values":{{"finish":30.0}},"confidence":0.8}}
            ]}}"#,
        cm_part(-2.54, 6.096)
    );
    serde_json::from_str(&json).expect("report")
}

/// A face-on report whose given impact found no swing: the impact alone
/// and the metrics at it, as stance writes it.
fn no_swing_report() -> SwingReportFile {
    serde_json::from_str(
        r#"{"version":1,"view":"face_on","handed":"right",
            "events":{"impact":{"t_ns":2150000000,"frame":258,"confidence":0.5},
              "impact_source":"given"},
            "metrics":[{"id":"lateral_spine_tilt","view":"face_on","unit":"degrees",
              "values":{"impact":21.2},"confidence":0.8}]}"#,
    )
    .expect("report")
}

#[test]
fn the_swing_numbers_read_from_a_report_and_follow_the_units() {
    let r = report(false);
    assert_eq!(
        tempo_tiles(&[&r]),
        [
            ("Tempo", "2.5 : 1".to_owned()),
            ("Backswing", "750 ms".to_owned()),
            ("Downswing", "300 ms".to_owned()),
        ]
    );
    // Body-relative without the golfer's height; a finish-only metric
    // has no row.
    let rows = strings(&rows3(&r, true));
    assert_eq!(
        rows,
        [
            (
                "Hip sway".to_owned(),
                [
                    NONE.to_owned(),
                    "5% torso Away".to_owned(),
                    "12% torso Toward".to_owned()
                ]
            ),
            (
                "Side bend".to_owned(),
                [
                    "8\u{b0}".to_owned(),
                    "-4\u{b0}".to_owned(),
                    "21\u{b0}".to_owned()
                ]
            ),
            (
                "Stance width".to_owned(),
                ["1.23".to_owned(), NONE.to_owned(), NONE.to_owned()]
            ),
        ]
    );
    // With the height: inches in imperial, centimeters in metric.
    let r = report(true);
    assert_eq!(
        strings(&rows3(&r, true))[0].1,
        [NONE, "1.0 in Away", "2.4 in Toward"]
    );
    assert_eq!(
        strings(&rows3(&r, false))[0].1,
        [NONE, "2.5 cm Away", "6.1 cm Toward"]
    );
    assert_eq!(
        strings(&rows3(&r, false))[1].1[0],
        "8\u{b0}",
        "angles are units-free"
    );
    // No tempo, no impact: dashes.
    let mut bare = report(false);
    bare.tempo = None;
    bare.events.impact = None;
    assert!(tempo_tiles(&[&bare]).iter().all(|(_, v)| v == NONE));
    assert!(tempo_tiles(&[]).iter().all(|(_, v)| v == NONE));
}

#[test]
fn every_metric_is_named_for_what_it_measures_and_an_unknown_one_reads_plainly() {
    for (id, name) in [
        ("hip_sway", "Hip sway"),
        ("head_lateral", "Head sway"),
        ("head_vertical", "Head height"),
        ("lateral_spine_tilt", "Side bend"),
        ("shoulder_tilt", "Shoulder tilt"),
        ("lead_arm_angle", "Lead elbow"),
        ("stance_width", "Stance width"),
        ("hip_turn", "Hip turn"),
        ("shoulder_turn", "Shoulder turn"),
        ("spine_forward_bend", "Forward bend"),
        ("early_extension", "Hip depth"),
        ("lead_knee_flex", "Lead knee flex"),
        ("trail_knee_flex", "Trail knee flex"),
        ("trail_elbow_angle", "Trail elbow"),
        ("hand_height", "Hand height"),
    ] {
        assert_eq!(metric_label(id), name, "{id}");
    }
    assert_eq!(metric_label("pelvis_drift"), "Pelvis drift");
    // The hip width ratio and the loss of posture stay in the file but
    // not in the table.
    let r: SwingReportFile = serde_json::from_str(
        r#"{"version":1,"view":"face_on","events":{"impact_source":"refined"},
            "metrics":[{"id":"hip_width_proxy","unit":"ratio","values":{"top":0.8}},
              {"id":"hip_turn","unit":"degrees","values":{"top":36.87,"impact":25.84}},
              {"id":"shoulder_turn","unit":"degrees","values":{"top":70.0,"impact":30.0}}]}"#,
    )
    .unwrap();
    let rows = strings(&rows3(&r, true));
    let names: Vec<&str> = rows.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["Hip turn", "Shoulder turn"]);
    assert_eq!(rows[0].1, [NONE, "37°", "26°"]);
    let r: SwingReportFile = serde_json::from_str(
            r#"{"version":1,"view":"down_the_line","events":{"impact_source":"refined"},
            "metrics":[{"id":"spine_forward_bend","unit":"degrees","values":{"address":31.0,"top":30.0,"impact":26.0}},
              {"id":"loss_of_posture","unit":"degrees","values":{"top":1.0,"impact":5.0}},
              {"id":"early_extension","unit":"torso","values":{"top":0.0,"impact":-0.04}}]}"#,
        )
        .unwrap();
    assert!(r.metrics.iter().any(|m| m.id == "loss_of_posture"), "read");
    let rows = strings(&rows3(&r, true));
    let names: Vec<&str> = rows.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        ["Forward bend", "Hip depth"],
        "no loss of posture row"
    );
    assert_eq!(rows[1].1, [NONE, "0% torso", "4% torso Back"]);
}

/// The table rows at address, top and impact, ungraded.
fn rows3(r: &SwingReportFile, imperial: bool) -> Vec<(String, Vec<Cell>)> {
    use SwingPosition::{Address, Impact, Top};
    metric_rows(r, imperial, &[Address, Top, Impact], None)
}

/// Three-column table rows as text, each value with its direction word
/// after it.
fn strings(rows: &[(String, Vec<Cell>)]) -> Vec<(String, [String; 3])> {
    rows.iter()
        .map(|(name, cells)| {
            let texts: Vec<String> = cells
                .iter()
                .map(|c| joined(&c.shown.text, c.shown.word))
                .collect();
            (name.clone(), texts.try_into().expect("three columns"))
        })
        .collect()
}

/// Each report at the frame it shows, ungraded.
fn plain<'a>(at: &[(&'a SwingReportFile, Option<u64>)]) -> Vec<Swing<'a>> {
    at.iter()
        .map(|&(report, t_ns)| Swing {
            report,
            t_ns,
            grades: None,
        })
        .collect()
}

/// A value and its direction word as one line of text.
fn joined(text: &str, word: Option<&str>) -> String {
    word.map_or_else(|| text.to_owned(), |w| format!("{text} {w}"))
}

#[test]
fn shifts_from_address_read_as_a_distance_and_a_direction() {
    // Hip sway and head sway: the distance and a word, never a sign.
    let shown = |id, v, cm, imperial| {
        let s = metric_shown(id, "torso", v, cm, imperial);
        joined(&s.text, s.word)
    };
    assert_eq!(shown("hip_sway", 0.12, Some(9.652), true), "3.8 in Toward");
    assert_eq!(shown("hip_sway", -0.04, Some(-2.794), true), "1.1 in Away");
    assert_eq!(shown("hip_sway", -0.04, Some(-2.794), false), "2.8 cm Away");
    assert_eq!(shown("head_lateral", -0.16, None, true), "16% torso Away");
    assert_eq!(shown("head_lateral", 0.03, None, false), "3% torso Toward");
    // A distance that shows as 0 has no word.
    assert_eq!(shown("hip_sway", 0.0, Some(0.0), true), "0.0 in");
    assert_eq!(shown("hip_sway", -0.001, Some(-0.05), true), "0.0 in");
    assert_eq!(shown("head_lateral", 0.004, None, true), "0% torso");
    // Hip depth: toward the ball or back from it.
    assert_eq!(
        shown("early_extension", -0.02, Some(-2.032), true),
        "0.8 in Back"
    );
    assert_eq!(
        shown("early_extension", 0.03, Some(3.048), true),
        "1.2 in Toward ball"
    );
    assert_eq!(
        shown("early_extension", 0.03, Some(3.048), false),
        "3.0 cm Toward ball"
    );
    assert_eq!(shown("early_extension", -0.05, None, true), "5% torso Back");
    assert_eq!(shown("early_extension", 0.0, Some(0.0), true), "0.0 in");
    assert_eq!(shown("early_extension", -0.002, Some(-0.1), true), "0.0 in");
    // Head height: up (a rise) or down.
    assert_eq!(shown("head_vertical", 0.01, Some(0.762), true), "0.3 in Up");
    assert_eq!(
        shown("head_vertical", -0.04, Some(-5.334), true),
        "2.1 in Down"
    );
    assert_eq!(shown("head_vertical", 0.001, None, true), "0% torso");
    // Other metrics keep their sign.
    assert_eq!(
        metric_shown("lateral_spine_tilt", "degrees", -3.6, None, true),
        Shown {
            text: "-4\u{b0}".to_owned(),
            word: None,
            color: theme::TEXT,
        }
    );

    // In the table: Top and Impact of the face-on rows.
    let r: SwingReportFile = serde_json::from_str(
        r#"{"version":1,"view":"face_on","events":{"impact_source":"refined"},
            "metrics":[
              {"id":"hip_sway","unit":"torso","values":{"top":-0.0236,"impact":0.0828},
                "cm":{"top":-2.95,"impact":10.35}},
              {"id":"head_lateral","unit":"torso","values":{"top":0.0,"impact":-0.0544},
                "cm":{"top":0.0,"impact":-6.8}}]}"#,
    )
    .unwrap();
    assert_eq!(
        strings(&rows3(&r, true)),
        [
            (
                "Hip sway".to_owned(),
                [
                    NONE.to_owned(),
                    "1.2 in Away".to_owned(),
                    "4.1 in Toward".to_owned()
                ]
            ),
            (
                "Head sway".to_owned(),
                [
                    NONE.to_owned(),
                    "0.0 in".to_owned(),
                    "2.7 in Away".to_owned()
                ]
            ),
        ]
    );
}

#[test]
fn an_earlier_swing_file_with_sway_and_slide_reads_without_them() {
    let r: SwingReportFile = serde_json::from_str(
            r#"{"version":1,"view":"face_on","events":{"impact_source":"refined"},
            "metrics":[
              {"id":"sway","view":"face_on","unit":"torso","values":{"top":0.05},"confidence":0.8},
              {"id":"slide","view":"face_on","unit":"torso","values":{"impact":0.15},"confidence":0.8},
              {"id":"hip_turn","view":"face_on","unit":"degrees","values":{"top":40.0},"confidence":0.8}],
            "series":{"t_ns":[0,16666667],"metrics":[
              {"id":"sway","unit":"torso","values":[0.0,0.01]},
              {"id":"hip_turn","unit":"degrees","values":[0.0,1.0]}]}}"#,
        )
        .unwrap();
    let rows = strings(&rows3(&r, true));
    let names: Vec<&str> = rows.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["Hip turn"], "no Sway or Slide row");
    let texts = frame_texts(&frame_tiles(&plain(&[(&r, Some(16_666_667))]), true));
    assert_eq!(texts[0], ("Hip turn", "1°".to_owned()));
    assert_eq!(texts[2], ("Hip sway", NONE.to_owned()));
}

/// Frames of the test clips ([`give_sidecar`]): 300 at 60 fps.
const FRAME_NS: u64 = 16_666_667;

/// A metric of a test series: its id, unit and value at frame `i`.
type SeriesSpec<'a> = (&'a str, &'a str, fn(u64) -> f64);

/// A swing report of `view` with a series over [`give_sidecar`]'s 300
/// frames: each metric `(id, unit, value at frame i)`, the body scale of
/// a 180 cm golfer (torso 200 px, 0.25 cm/px) when `cm`.
fn series_report(view: &str, metrics: &[SeriesSpec], cm: bool) -> SwingReportFile {
    let t_ns: Vec<u64> = (0..300).map(|i| i * FRAME_NS).collect();
    let series: Vec<serde_json::Value> = metrics
        .iter()
        .map(|(id, unit, f)| {
            let values: Vec<f64> = (0..300).map(f).collect();
            serde_json::json!({ "id": id, "unit": unit, "values": values })
        })
        .collect();
    let mut json = serde_json::json!({
        "version": 1, "view": view, "handed": "right",
        "events": {
            "address": {"t_ns": 60 * FRAME_NS, "frame": 60, "confidence": 0.8},
            "takeaway": {"t_ns": 61 * FRAME_NS, "frame": 61, "confidence": 0.8},
            "top": {"t_ns": 150 * FRAME_NS, "frame": 150, "confidence": 0.9},
            "impact": {"t_ns": 180 * FRAME_NS, "frame": 180, "confidence": 0.9},
            "impact_source": "refined"
        },
        "tempo": {"backswing_ns": 89 * FRAME_NS, "downswing_ns": 30 * FRAME_NS, "ratio": 2.97},
        "metrics": [],
        "series": { "t_ns": t_ns, "metrics": series },
    });
    json["scale"] = if cm {
        serde_json::json!({"torso_px": 200.0, "cm_per_px": 0.25})
    } else {
        serde_json::json!({"torso_px": 200.0})
    };
    serde_json::from_value(json).expect("report")
}

/// The test hip sway at frame `i`: from address (frame 60) away from
/// the target to the top (frame 150, -0.036), then toward it (impact,
/// frame 180: 0.054).
fn hip_sway_at(i: u64) -> f64 {
    let i = i as f64;
    if i <= 150.0 {
        -(i - 60.0) * 0.0004
    } else {
        -0.036 + (i - 150.0) * 0.003
    }
}

fn face_on_series(cm: bool) -> SwingReportFile {
    series_report(
        "face_on",
        &[
            ("hip_sway", "torso", hip_sway_at),
            ("head_lateral", "torso", |i| -(i as f64) * 0.0005),
            ("lateral_spine_tilt", "degrees", |i| 5.2 + i as f64 * 0.01),
            ("hip_turn", "degrees", |i| i as f64 * 0.1),
            ("shoulder_turn", "degrees", |i| i as f64 * 0.2),
            ("some_new_metric", "degrees", |_| 1.0),
        ],
        cm,
    )
}

/// The test hip depth at frame `i`: 0 at address (frame 60), back from
/// the ball before it, toward the ball after it (frame 150: 0.036).
fn hip_depth_at(i: u64) -> f64 {
    (i as f64 - 60.0) * 0.0004
}

fn down_the_line_series() -> SwingReportFile {
    series_report(
        "down_the_line",
        &[
            ("head_vertical", "torso", |i| i as f64 * 0.0001),
            ("spine_forward_bend", "degrees", |i| 30.0 + i as f64 * 0.01),
            ("early_extension", "torso", hip_depth_at),
        ],
        false,
    )
}

/// The tiles' names and values (each with its direction word).
fn frame_texts(tiles: &[FrameValue]) -> Vec<(&'static str, String)> {
    tiles
        .iter()
        .map(|t| (t.label, joined(&t.text, t.word)))
        .collect()
}

#[test]
fn the_frame_tiles_read_each_cameras_series_at_its_frame() {
    let (fo, dtl) = (face_on_series(true), down_the_line_series());
    let at = |i: u64| Some(i * FRAME_NS);
    // Two cameras at frame 150: four tiles face-on, two down the line.
    let tiles = frame_tiles(&plain(&[(&fo, at(150)), (&dtl, at(150))]), false);
    assert_eq!(
        frame_texts(&tiles),
        vec![
            ("Hip turn", "15°".to_owned()),
            ("Shoulder turn", "30°".to_owned()),
            ("Hip sway", "1.8 cm Away".to_owned()),
            ("Head sway", "3.8 cm Away".to_owned()),
            ("Forward bend", "32°".to_owned()),
            ("Hip depth", "4% torso Toward ball".to_owned()),
        ]
    );
    let ids: Vec<&str> = tiles.iter().map(|t| t.id).collect();
    assert_eq!(
        ids,
        [
            "hip_turn",
            "shoulder_turn",
            "hip_sway",
            "head_lateral",
            "spine_forward_bend",
            "early_extension"
        ]
    );
    // Imperial, and each camera at its own frame.
    let tiles = frame_tiles(&plain(&[(&fo, at(100)), (&dtl, at(260))]), true);
    let texts = frame_texts(&tiles);
    assert_eq!(texts[0].1, "10°");
    assert_eq!(texts[2].1, "0.3 in Away", "-0.016 × 50 cm = -0.8 cm");
    assert_eq!(texts[4].1, "33°");
    assert_eq!(texts[5].1, "8% torso Toward ball");
    // Between two frames: the nearest one.
    let tiles = frame_tiles(&plain(&[(&fo, Some(100 * FRAME_NS + 3))]), false);
    assert_eq!(tiles[0].text, "10°");

    // One face-on camera: the spine tile is its side bend, hip depth a
    // dash; one down-the-line camera: the face-on tiles are dashes.
    let texts = frame_texts(&frame_tiles(&plain(&[(&fo, at(150))]), false));
    assert_eq!(texts.len(), 6);
    assert_eq!(texts[4], ("Side bend", "7°".to_owned()));
    assert_eq!(texts[5], ("Hip depth", NONE.to_owned()));
    let texts = frame_texts(&frame_tiles(&plain(&[(&dtl, at(150))]), false));
    assert!(texts[..4].iter().all(|(_, v)| v == NONE), "{texts:?}");
    assert_eq!(texts[4], ("Forward bend", "32°".to_owned()));

    // Outside a camera's clip, or past its series: dashes for its tiles.
    let texts = frame_texts(&frame_tiles(&plain(&[(&fo, None), (&dtl, at(150))]), false));
    assert!(texts[..4].iter().all(|(_, v)| v == NONE));
    assert_eq!(texts[5].1, "4% torso Toward ball");
    let texts = frame_texts(&frame_tiles(&plain(&[(&fo, at(400))]), false));
    assert!(texts.iter().all(|(_, v)| v == NONE));

    // Two face-on cameras: the mean.
    let other = series_report("face_on", &[("hip_turn", "degrees", |_| 25.0)], false);
    let tiles = frame_tiles(&plain(&[(&fo, at(150)), (&other, at(150))]), false);
    assert_eq!(tiles[0].text, "20°");
    // Without the golfer's height, distances are shares of the torso.
    let tiles = frame_tiles(&plain(&[(&face_on_series(false), at(150))]), false);
    assert_eq!(joined(&tiles[2].text, tiles[2].word), "4% torso Away");

    // Each side of address: away before the top, toward the target at
    // impact, no word at address.
    let sway = |i: u64| {
        frame_texts(&frame_tiles(&plain(&[(&fo, at(i))]), false))[2]
            .1
            .clone()
    };
    assert_eq!(sway(150), "1.8 cm Away");
    assert_eq!(sway(180), "2.7 cm Toward");
    assert_eq!(sway(60), "0.0 cm");
    let imperial = frame_texts(&frame_tiles(&plain(&[(&fo, at(180))]), true));
    assert_eq!(imperial[2].1, "1.1 in Toward");

    // Hip depth each side of address: back before it, toward the ball
    // after it, no word at address.
    let depth = |i: u64| {
        frame_texts(&frame_tiles(&plain(&[(&dtl, at(i))]), false))[5]
            .1
            .clone()
    };
    assert_eq!(depth(20), "2% torso Back");
    assert_eq!(depth(60), "0% torso");
    assert_eq!(depth(180), "5% torso Toward ball");

    // A report without a series (an earlier file): dashes.
    let texts = frame_texts(&frame_tiles(&plain(&[(&report(true), at(150))]), false));
    assert!(texts.iter().all(|(_, v)| v == NONE));
    assert!(frame_tiles(&[], false).iter().all(|t| t.text == NONE));
}

/// Every direction word a value can have.
const DIRECTION_WORDS: [&str; 6] = ["Toward", "Away", "Toward ball", "Back", "Up", "Down"];

/// The selected-frame row's painted values, left to right, each with
/// its direction word.
fn row_values(screen: &Screen) -> Vec<String> {
    let texts = screen.texts();
    let start = texts
        .iter()
        .position(|t| t == "HIP TURN")
        .expect("the selected-frame row");
    let mut texts = texts[start..].iter();
    let mut values = Vec::new();
    while values.len() < 6 {
        texts.next().expect("a tile's name");
        let mut value = texts.next().expect("a tile's value").clone();
        if let Some(word) = texts
            .clone()
            .next()
            .filter(|w| DIRECTION_WORDS.contains(&w.as_str()))
        {
            value = format!("{value} {word}");
            texts.next();
        }
        values.push(value);
    }
    values
}

/// Put the inline player's playhead on frame `frame` of the test clips.
fn seek_to_frame(app: &mut FlighthookApp, frame: u64) {
    let p = app.shot_player.as_mut().unwrap().playback.as_mut().unwrap();
    p.playing = false;
    p.playhead = 1_000_000_000 + frame * FRAME_NS;
}

#[test]
fn the_frame_row_sits_under_the_tempo_and_follows_the_playhead() {
    let screen = Screen::new();
    let mut app = app(&[0, 1], true);
    app.settings.default_units = UnitSystem::Metric;
    app.shots = vec![shot_row(1, &["camera.0", "camera.1"])];
    app.shots_rev += 1;
    app.set_shots_pane(ShotsPane::Cameras);
    screen.frame(&mut app, Vec::new());
    give_report(&mut app, "camera.0", face_on_series(true));
    give_report(&mut app, "camera.1", down_the_line_series());
    give_sidecar(&mut app, "camera.0");
    give_sidecar(&mut app, "camera.1");
    let marks = screen.frame(&mut app, Vec::new());
    let (tempo, row) = (marks["swing_tempo"].0, marks["frame_metrics"].0);
    assert!(
        row.top() > tempo.bottom() && row.top() - tempo.bottom() < 12.0,
        "under the tempo"
    );
    assert!((row.left() - tempo.left()).abs() < 0.5 && (row.width() - tempo.width()).abs() < 0.5);
    assert!(
        (row.height() - tempo.height()).abs() < 0.5,
        "the same tiles"
    );
    for camera in ["camera.0", "camera.1"] {
        let table = marks[format!("swing:{camera}").as_str()].0;
        assert!(table.top() > row.bottom(), "the metrics under it");
    }
    let tiles: Vec<Rect> = [
        "hip_turn",
        "shoulder_turn",
        "hip_sway",
        "head_lateral",
        "spine_forward_bend",
        "early_extension",
    ]
    .iter()
    .map(|id| marks[format!("frame_metric:{id}").as_str()].0)
    .collect();
    assert!(
        tiles.windows(2).all(|w| w[0].right() < w[1].left()),
        "one row, in order"
    );
    assert!(tiles.iter().all(|t| row.contains_rect(*t)));
    // Opening: the row reads the frame the playhead opened on.
    let playhead = app
        .shot_player
        .as_ref()
        .unwrap()
        .playback
        .as_ref()
        .unwrap()
        .playhead;
    let frame = (playhead - 1_000_000_000).div_ceil(FRAME_NS);
    let opening = row_values(&screen);
    assert_eq!(opening[0], degrees(frame as f64 * 0.1), "{opening:?}");

    // Moved to frame 150, then 250: the row reads those frames.
    seek_to_frame(&mut app, 150);
    screen.frame(&mut app, Vec::new());
    assert_eq!(
        row_values(&screen),
        [
            "15°",
            "30°",
            "1.8 cm Away",
            "3.8 cm Away",
            "32°",
            "4% torso Toward ball"
        ]
    );
    let texts = screen.texts();
    for name in ["HEAD SWAY", "FORWARD BEND", "HIP DEPTH"] {
        assert!(texts.contains(&name.to_owned()), "{name}: {texts:?}");
    }
    // At impact the hips have moved toward the target and the ball; at
    // address no word; before it, back from the ball.
    seek_to_frame(&mut app, 180);
    screen.frame(&mut app, Vec::new());
    assert_eq!(row_values(&screen)[2], "2.7 cm Toward");
    assert_eq!(row_values(&screen)[5], "5% torso Toward ball");
    seek_to_frame(&mut app, 60);
    screen.frame(&mut app, Vec::new());
    assert_eq!(row_values(&screen)[2], "0.0 cm");
    assert_eq!(row_values(&screen)[5], "0% torso");
    seek_to_frame(&mut app, 20);
    screen.frame(&mut app, Vec::new());
    assert_eq!(row_values(&screen)[5], "2% torso Back");
    seek_to_frame(&mut app, 250);
    let marks = screen.frame(&mut app, Vec::new());
    assert_eq!(row_values(&screen)[..2], ["25°", "50°"]);

    // A frame step moves it one frame.
    let step = marks["player:step:true"].0.center();
    let marks = screen.click(&mut app, step);
    assert_eq!(row_values(&screen)[1], "50°", "frame 251: 50.2°");
    assert_eq!(
        app.shot_player
            .as_ref()
            .unwrap()
            .playback
            .as_ref()
            .unwrap()
            .playhead,
        1_000_000_000 + 251 * FRAME_NS
    );
    // A click on the top's marker seeks there: frame 150.
    let top = marks["player:event:top"].0.center();
    screen.click(&mut app, top);
    assert_eq!(row_values(&screen)[0], "15°");

    // Playing moves it with the clip.
    seek_to_frame(&mut app, 120);
    app.shot_player
        .as_mut()
        .unwrap()
        .playback
        .as_mut()
        .unwrap()
        .playing = true;
    screen.frame(&mut app, Vec::new());
    assert_eq!(row_values(&screen)[0], "12°");
}

/// The width a name takes in a tile, and the sizes a value and its
/// word are drawn at in `room`.
fn tile_fit(
    ctx: &egui::Context,
    name: &str,
    value: &str,
    word: &str,
    room: f32,
) -> (f32, f32, f32, f32) {
    let mut fit = (0.0, 0.0, 0.0, 0.0);
    let _ = ctx.run(egui::RawInput::default(), |ctx| {
        let painter = ctx.layer_painter(egui::LayerId::background());
        let name = painter.layout_no_wrap(
            name.to_uppercase(),
            egui::FontId::proportional(10.0),
            theme::TEXT_MUTED,
        );
        let (v, w) = worded_value(&painter, value, Some(word), 18.0, room);
        let (w, offset) = w.expect("a word");
        let size = |g: &egui::Galley| g.job.sections[0].format.font_id.size;
        fit = (name.size().x, size(&v), size(&w), offset.x + w.size().x);
    });
    fit
}

/// At 1280 wide (the README's Cameras pane), the renamed tiles and
/// their words fit at full size: the frame row's names, a long value
/// with each word, and the live posture tiles' names.
#[test]
fn the_names_and_words_fit_their_tiles_at_1280_wide() {
    let screen = Screen::sized(1280.0, 948.0);
    let mut app = app(&[0, 1], true);
    app.shots = vec![shot_row(1, &["camera.0", "camera.1"])];
    app.shots_rev += 1;
    app.set_shots_pane(ShotsPane::Cameras);
    screen.frame(&mut app, Vec::new());
    give_report(&mut app, "camera.0", face_on_series(true));
    give_report(&mut app, "camera.1", down_the_line_series());
    give_sidecar(&mut app, "camera.0");
    give_sidecar(&mut app, "camera.1");
    seek_to_frame(&mut app, 150);
    let marks = screen.frame(&mut app, Vec::new());
    let tile = marks["frame_metric:early_extension"].0.width();
    let room = tile - 16.0;
    for (name, value, word) in [
        ("Hip sway", "10.5 in", "Toward"),
        ("Head sway", "10.5 in", "Away"),
        ("Forward bend", "32°", "Up"),
        ("Hip depth", "10.5 in", "Toward ball"),
        ("Hip depth", "26.7 cm", "Back"),
        ("Hip depth", "25% torso", "Toward ball"),
    ] {
        let (name_w, value_size, word_size, line_w) = tile_fit(&screen.0, name, value, word, room);
        assert!(name_w <= room, "{name}: {name_w} in {room}");
        assert_eq!(
            value_size, 18.0,
            "{value} {word}: the numeral keeps its size"
        );
        assert_eq!(
            word_size,
            shots::small_size(18.0),
            "{value} {word}: the word keeps its size in {room}"
        );
        assert!(line_w <= room, "{value} {word}: {line_w} in {room}");
    }
    // A distance as long as the torso: the word shrinks, still on the
    // line.
    let (_, value_size, word_size, line_w) =
        tile_fit(&screen.0, "Hip depth", "100% torso", "Toward ball", room);
    assert_eq!(value_size, 18.0);
    assert!(word_size >= shots::WORD_MIN && line_w <= room);
    // Live: the down-the-line camera's four posture tiles.
    app.select_live();
    let marks = screen.frame(&mut app, Vec::new());
    let row = marks["posture:camera.1"].0;
    let room = tile_rects(row, 4)[0].width() - 16.0;
    for name in [
        "Forward bend",
        "Lead knee flex",
        "Trail knee flex",
        "Lead elbow",
        "Side bend",
    ] {
        let (name_w, ..) = tile_fit(&screen.0, name, "0°", "Up", room);
        assert!(name_w <= room, "{name}: {name_w} in {room}");
    }
}

#[test]
fn one_camera_keeps_the_row_with_dashes_for_the_other_view() {
    let screen = Screen::new();
    let mut app = app(&[0], true);
    app.shots = vec![shot_row(1, &["camera.0"])];
    app.shots_rev += 1;
    app.set_shots_pane(ShotsPane::Cameras);
    screen.frame(&mut app, Vec::new());
    give_report(&mut app, "camera.0", face_on_series(false));
    give_sidecar(&mut app, "camera.0");
    seek_to_frame(&mut app, 150);
    let marks = screen.frame(&mut app, Vec::new());
    let (tempo, row) = (marks["swing_tempo"].0, marks["frame_metrics"].0);
    assert!((row.left() - tempo.left()).abs() < 0.5 && (row.width() - tempo.width()).abs() < 0.5);
    assert!(marks.contains_key("frame_metric:lateral_spine_tilt"));
    assert!(marks.contains_key("frame_metric:early_extension"));
    assert_eq!(
        row_values(&screen),
        ["15°", "30°", "4% torso Away", "8% torso Away", "7°", NONE]
    );
    let texts = screen.texts();
    assert!(texts.contains(&"SIDE BEND".to_owned()));
    assert!(!texts.contains(&"FORWARD BEND".to_owned()));

    // An earlier report without a series: the row of dashes.
    let mut app2 = self::app(&[0], true);
    app2.shots = vec![shot_row(1, &["camera.0"])];
    app2.shots_rev += 1;
    app2.set_shots_pane(ShotsPane::Cameras);
    screen.frame(&mut app2, Vec::new());
    give_report(&mut app2, "camera.0", report(false));
    give_sidecar(&mut app2, "camera.0");
    let marks = screen.frame(&mut app2, Vec::new());
    assert!(marks.contains_key("frame_metrics"));
    assert!(row_values(&screen).iter().all(|v| v == NONE));

    // A report that found no swing: no tempo, no frame row.
    let mut app3 = self::app(&[0], true);
    app3.shots = vec![shot_row(1, &["camera.0"])];
    app3.shots_rev += 1;
    app3.set_shots_pane(ShotsPane::Cameras);
    screen.frame(&mut app3, Vec::new());
    give_report(&mut app3, "camera.0", no_swing_report());
    let marks = screen.frame(&mut app3, Vec::new());
    assert!(!marks.contains_key("swing_tempo") && !marks.contains_key("frame_metrics"));
}

#[test]
fn the_live_posture_shows_the_cameras_view() {
    let p = LivePosture {
        view: SwingView::FaceOn,
        spine_angle: Some(12.4),
        shoulder_tilt: Some(-31.0),
        lead_arm_angle: Some(171.6),
        lead_knee_flex: None,
        trail_knee_flex: None,
        trail_elbow_angle: None,
    };
    assert_eq!(
        posture_tiles(CameraPosition::GolferFront, Some(&p)),
        [
            ("Side bend", "12\u{b0}".to_owned()),
            ("Shoulder tilt", "-31\u{b0}".to_owned()),
            ("Lead elbow", "172\u{b0}".to_owned()),
        ]
    );
    let names: Vec<&str> = posture_tiles(CameraPosition::TargetLine, None)
        .into_iter()
        .map(|(n, v)| {
            assert_eq!(v, NONE);
            n
        })
        .collect();
    assert_eq!(
        names,
        [
            "Forward bend",
            "Lead knee flex",
            "Trail knee flex",
            "Trail elbow"
        ]
    );
    assert!(posture_tiles(CameraPosition::GolferBack, Some(&p)).is_empty());
    assert!(posture_tiles(CameraPosition::Overhead, None).is_empty());
}

#[test]
fn the_posture_that_came_with_the_frame_shows_under_its_picture() {
    let screen = Screen::new();
    let mut app = app(&[0, 1], true);
    app.select_live();
    screen.frame(&mut app, Vec::new());
    let id = app.live["camera.0"].id;
    let mut posed = pose(5.0);
    posed.posture = Some(LivePosture {
        view: SwingView::FaceOn,
        spine_angle: Some(10.0),
        shoulder_tilt: Some(20.0),
        lead_arm_angle: Some(170.0),
        lead_knee_flex: None,
        trail_knee_flex: None,
        trail_elbow_angle: None,
    });
    app.live_replies(
        vec![(
            id,
            LiveReply::Frame {
                seq: 1,
                jpeg: frame_jpeg(),
                pose: Some(posed.clone()),
                state: PoseState::default(),
            },
        )],
        0.0,
    );
    let marks = screen.frame(&mut app, Vec::new());
    assert_eq!(app.live["camera.0"].shown_pose, Some(posed));
    let picture = marks["camera_live:camera.0"].0;
    let tiles = marks["posture:camera.0"].0;
    assert!(tiles.top() >= picture.bottom());
    assert!((tiles.width() - picture.width()).abs() < 0.5);
    // The down-the-line camera has its tiles too, dashes until a posture
    // arrives.
    assert!(marks.contains_key("posture:camera.1"));
}

/// `app(cameras, true)` with `edit` applied to its saved config.
fn app_with(cameras: &[u32], edit: impl FnOnce(&mut FlighthookConfig)) -> FlighthookApp {
    let mut app = app(cameras, true);
    let mut saved = config(cameras);
    edit(&mut saved);
    app.settings.load_from(&saved);
    app
}

#[test]
fn a_portrait_live_view_is_drawn_upright_with_its_skeleton_on_it() {
    for (rotation, portrait) in [(0, false), (90, true), (180, false), (270, true)] {
        let screen = Screen::new();
        let mut app = app_with(&[0], |c| {
            c.camera.get_mut("0").unwrap().rotation_deg = rotation
        });
        assert_eq!(app.camera_rotation("camera.0"), rotation);
        app.select_live();
        screen.frame(&mut app, Vec::new());
        let id = app.live["camera.0"].id;
        // A 64×48 frame, the skeleton found on the upright 48×64 one
        // when turned.
        app.live_replies(
            vec![(
                id,
                LiveReply::Frame {
                    seq: 1,
                    jpeg: frame_jpeg(),
                    pose: Some(pose(40.0)),
                    state: PoseState {
                        state: "ready".into(),
                        error: None,
                    },
                },
            )],
            1.0,
        );
        screen.frame(&mut app, Vec::new());
        let marks = screen.frame(&mut app, Vec::new());
        let picture = marks["camera_live:camera.0"].0;
        let aspect = picture.height() / picture.width();
        let want = if portrait { 64.0 / 48.0 } else { 48.0 / 64.0 };
        assert!((aspect - want).abs() < 0.02, "{rotation}: {aspect}");
        assert!(
            (app.camera_aspect("camera.0") - want).abs() < 0.02,
            "{rotation}: the column is laid out for the turned picture"
        );
        assert!(screen.circles_in(picture) > 0, "{rotation}: the skeleton");
    }
}

#[test]
fn a_clip_tracked_at_another_rotation_is_tracked_again_once() {
    let screen = Screen::new();
    let mut app = app_with(&[0, 1], |c| {
        c.camera.get_mut("0").unwrap().rotation_deg = 90
    });
    app.shots = vec![shot_row(1, &["camera.0", "camera.1"])];
    app.shots_rev += 1;
    app.set_shots_pane(ShotsPane::Cameras);
    screen.frame(&mut app, Vec::new());
    // camera.0 was tracked unturned and is turned now; camera.1 matches.
    assert_eq!(
        app.retracked,
        std::collections::HashSet::from([("id1".to_owned(), "camera.0".to_owned())])
    );
    let video = &app.shots[0].video;
    assert!(!video[0].pose && !video[0].swing, "dropped meanwhile");
    assert!(video[1].pose && video[1].swing);
    let player = app.shot_player.as_ref().expect("the inline player");
    assert!(!player.wants_swing_of("camera.0"));
    assert!(player.wants_swing_of("camera.1"));
    // An old pass finishing at the old rotation is not asked about
    // again this session.
    assert!(crate::types::mark_pose(
        &mut app.shots,
        &[ShotKey {
            shot_id: "id1".into(),
            shot_number: 1,
        }],
        "camera.0",
        0,
    ));
    screen.frame(&mut app, Vec::new());
    assert_eq!(app.retracked.len(), 1);
    assert!(app.shots[0].video[0].pose, "kept");
    // Tracked at the camera's rotation: nothing to do.
    let mut fresh = app_with(&[0, 1], |c| {
        c.camera.get_mut("0").unwrap().rotation_deg = 90
    });
    let mut row = shot_row(2, &["camera.0", "camera.1"]);
    row.video[0].rotation = 90;
    fresh.shots = vec![row];
    fresh.shots_rev += 1;
    fresh.set_shots_pane(ShotsPane::Cameras);
    screen.frame(&mut fresh, Vec::new());
    assert!(fresh.retracked.is_empty());
    // Body tracking off: nothing either.
    let mut off = app_with(&[0], |c| {
        c.camera.get_mut("0").unwrap().rotation_deg = 90;
        c.body_tracking.mode = BodyTrackingMode::Off;
    });
    off.shots = vec![shot_row(3, &["camera.0"])];
    off.shots_rev += 1;
    off.set_shots_pane(ShotsPane::Cameras);
    screen.frame(&mut off, Vec::new());
    assert!(off.retracked.is_empty());
}

#[test]
fn only_tracked_clips_of_configured_cameras_at_another_rotation_are_due() {
    let mut row = shot_row(1, &["camera.0", "camera.1", "camera.2"]);
    row.video[1].pose = false;
    let rotation_of = |camera: &str| match camera {
        "camera.0" | "camera.1" => Some(270),
        _ => None,
    };
    let none = std::collections::HashSet::new();
    // camera.1 has no keypoints yet; camera.2 is not configured.
    assert_eq!(retrack_due(&row, rotation_of, &none), ["camera.0"]);
    let asked = std::collections::HashSet::from([("id1".to_owned(), "camera.0".to_owned())]);
    assert!(retrack_due(&row, rotation_of, &asked).is_empty());
    row.video[0].rotation = 270;
    assert!(retrack_due(&row, rotation_of, &none).is_empty());
}

#[test]
fn a_golfer_front_camera_five_degrees_off_square_is_flagged() {
    let line = "Camera 7° off square: one-camera turn and sway are approximate";
    let front = CameraPosition::GolferFront;
    assert_eq!(yaw_warning(front, Some(7.2)).as_deref(), Some(line));
    assert_eq!(yaw_warning(front, Some(-6.6)).as_deref(), Some(line));
    assert!(yaw_warning(front, Some(5.0)).is_some());
    assert!(yaw_warning(front, Some(-5.0)).is_some());
    assert_eq!(yaw_warning(front, Some(4.9)), None);
    assert_eq!(yaw_warning(front, None), None);
    assert_eq!(
        yaw_warning(front, Some(40.0)),
        None,
        "out of range: not in effect"
    );
    for other in [
        CameraPosition::TargetLine,
        CameraPosition::GolferBack,
        CameraPosition::Overhead,
    ] {
        assert_eq!(yaw_warning(other, Some(20.0)), None);
    }
}

#[test]
fn the_yaw_line_sits_over_the_golfer_front_cameras_analysis_only() {
    let screen = Screen::new();
    let mut app = app_with(&[0, 1], |c| {
        c.camera.get_mut("0").unwrap().yaw_deg = Some(-8.0);
        c.camera.get_mut("1").unwrap().yaw_deg = Some(12.0);
    });
    app.shots = vec![shot_row(1, &["camera.0", "camera.1"])];
    app.shots_rev += 1;
    app.set_shots_pane(ShotsPane::Cameras);
    screen.frame(&mut app, Vec::new());
    give_report(&mut app, "camera.0", report(false));
    give_report(&mut app, "camera.1", report(false));
    let marks = screen.frame(&mut app, Vec::new());
    let line = marks["yaw_warning:camera.0"].0;
    let swing = marks["swing:camera.0"].0;
    assert!(line.bottom() <= swing.top() + 0.5, "above its table");
    assert!(
        line.right() <= marks["swing:camera.1"].0.left(),
        "in its column"
    );
    assert!(
        screen
            .texts()
            .iter()
            .any(|t| t == "Camera 8° off square: one-camera turn and sway are approximate")
    );
    assert!(!marks.contains_key("yaw_warning:camera.1"), "target line");
    // Square enough: no line.
    let mut square = app_with(&[0], |c| c.camera.get_mut("0").unwrap().yaw_deg = Some(3.0));
    square.shots = vec![shot_row(2, &["camera.0"])];
    square.shots_rev += 1;
    square.set_shots_pane(ShotsPane::Cameras);
    screen.frame(&mut square, Vec::new());
    give_report(&mut square, "camera.0", report(false));
    let marks = screen.frame(&mut square, Vec::new());
    assert!(marks.contains_key("swing:camera.0"));
    assert!(!marks.contains_key("yaw_warning:camera.0"));
}

fn shot_row(n: u32, cameras: &[&str]) -> ShotRow {
    ShotRow {
        golfer: None,
        actor: "mock_monitor.0".into(),
        shot_id: format!("id{n}"),
        shot_number: n,
        launch: Some(flighthook::Launch::default()),
        flight: None,
        club: None,
        impact: None,
        derived: None,
        club_selected: None,
        handed: None,
        group: None,
        finished: true,
        video: cameras
            .iter()
            .map(|c| ShotVideo {
                camera: (*c).to_owned(),
                position: if *c == "camera.0" {
                    CameraPosition::GolferFront
                } else {
                    CameraPosition::TargetLine
                },
                duration_ms: 5000,
                fps: 60.0,
                trigger_offset_ms: 3000,
                impact_offset_ms: 3000,
                complete: true,
                dropped: 0,
                pose: true,
                swing: true,
                rotation: 0,
                analyzed_with: None,
            })
            .collect(),
    }
}

/// Hand camera `camera`'s swing report to the inline player.
fn give_report(app: &mut FlighthookApp, camera: &str, r: SwingReportFile) {
    let player = app.shot_player.as_mut().expect("a player");
    let seq = player.seq();
    player.reply(crate::net::ReplayReply::Swing {
        replay: seq,
        camera: camera.to_owned(),
        result: Ok(r),
    });
}

#[test]
fn a_shot_in_cameras_mode_plays_its_clips_with_the_swing_data_under_them() {
    let screen = Screen::new();
    let mut app = app(&[0, 1], true);
    app.shots = vec![shot_row(1, &["camera.0", "camera.1"])];
    app.shots_rev += 1;
    app.set_shots_pane(ShotsPane::Cameras);
    let marks = screen.frame(&mut app, Vec::new());
    let player = app.shot_player.as_ref().expect("the inline player");
    assert_eq!(player.shot_id(), "id1");
    assert!(app.card_clips.is_empty(), "no card row clips");
    let left = marks["player:picture:camera.0"].0;
    let right = marks["player:picture:camera.1"].0;
    assert!(left.right() < right.left(), "side by side");
    assert!(!marks.contains_key("swing:camera.0"), "no report yet");
    give_report(&mut app, "camera.0", report(false));
    let marks = screen.frame(&mut app, Vec::new());
    let swing = marks["swing:camera.0"].0;
    assert!(swing.top() > left.bottom(), "under its clip");
    assert!((swing.left() - left.left()).abs() < 1.0, "in its column");
    assert!(swing.right() <= left.right() + 1.0);
    assert!(marks.contains_key("swing_tempo"));
    assert!(!marks.contains_key("swing:camera.1"));
    // A report that found no swing near the impact: the one line under
    // its clip, no tempo, events or metrics.
    give_report(&mut app, "camera.1", no_swing_report());
    let marks = screen.frame(&mut app, Vec::new());
    let line = marks["swing:camera.1"].0;
    assert!(line.top() > right.bottom(), "under its clip");
    assert!((line.left() - right.left()).abs() < 1.0, "in its column");
    assert!((line.height() - TABLE_ROW_H).abs() < 0.5, "one line");
    assert!(marks["swing:camera.0"].0.height() > line.height() * 3.0);
    assert!(!no_swing_report().swing_found() && report(false).swing_found());
    assert_eq!(NO_SWING, "No swing found in this clip");
    // The Data pane drops the player; the shot's cards come back.
    app.set_shots_pane(ShotsPane::Data);
    screen.frame(&mut app, Vec::new());
    assert!(app.shot_player.is_none());
}

#[test]
fn picking_a_shot_shows_its_clips_and_the_replay_still_opens() {
    let screen = Screen::new();
    let mut app = app(&[0], true);
    app.shots = vec![shot_row(1, &["camera.0"]), shot_row(2, &["camera.0"])];
    app.shots_rev += 1;
    app.set_shots_pane(ShotsPane::Cameras);
    screen.frame(&mut app, Vec::new());
    assert_eq!(
        app.shot_player.as_ref().unwrap().shot_id(),
        "id2",
        "the latest"
    );
    app.select_shot(0);
    let marks = screen.frame(&mut app, Vec::new());
    assert_eq!(app.shot_player.as_ref().unwrap().shot_id(), "id1");
    // The header's Replay opens the modal over the pane, which holds.
    let at = marks["replay"].0.center();
    screen.click(&mut app, at);
    assert_eq!(app.replay.as_ref().map(|r| r.shot_id()), Some("id1"));
    assert!(app.shot_player.is_some());
}

#[test]
fn a_shot_without_clips_shows_each_cameras_status() {
    let screen = Screen::new();
    let mut app = app(&[0, 1], true);
    app.shots = vec![shot_row(1, &[]), shot_row(2, &[])];
    app.shots_rev += 1;
    app.set_shots_pane(ShotsPane::Cameras);
    app.actors
        .get_mut("camera.1")
        .unwrap()
        .observe(ActorStatus::Reconnecting, 0);
    let marks = screen.frame(&mut app, Vec::new());
    assert!(app.shot_player.is_none());
    assert!(marks.contains_key("clip_status:camera.0"));
    assert!(marks.contains_key("clip_status:camera.1"));
    assert_eq!(clip_status_text(true), "Waiting for the clip");
    assert_eq!(clip_status_text(false), "No clip of this shot");
}

/// Put body tracking in `mode`, as saved settings.
fn set_mode(app: &mut FlighthookApp, mode: BodyTrackingMode) {
    let mut saved = app.settings.to_request();
    saved.body_tracking.mode = mode;
    app.settings.load_from(&saved);
}

#[test]
fn live_shows_the_posture_and_readout_in_live_mode_and_the_pictures_alone_otherwise() {
    let screen = Screen::new();
    let mut app = app(&[0, 1], true);
    app.select_live();
    screen.frame(&mut app, Vec::new());
    for mode in BodyTrackingMode::all() {
        set_mode(&mut app, mode);
        // A frame with the models still loading, as the server says in
        // Live mode.
        let id = app.live["camera.0"].id;
        app.live_replies(
            vec![(
                id,
                LiveReply::Frame {
                    seq: 1,
                    jpeg: frame_jpeg(),
                    pose: None,
                    state: PoseState {
                        state: "loading".into(),
                        error: None,
                    },
                },
            )],
            0.0,
        );
        let marks = screen.frame(&mut app, Vec::new());
        let live = mode == BodyTrackingMode::Live;
        assert!(marks.contains_key("camera_live:camera.0"), "{mode:?}");
        assert!(marks.contains_key("camera_live:camera.1"), "{mode:?}");
        assert_eq!(marks.contains_key("posture:camera.0"), live, "{mode:?}");
        assert_eq!(marks.contains_key("posture:camera.1"), live, "{mode:?}");
        assert_eq!(marks.contains_key("pose_readout"), live, "{mode:?}");
        assert_eq!(marks.contains_key("pose_status:camera.0"), live, "{mode:?}");
    }
}

/// Show or hide the skeleton, as saved settings.
fn set_show_skeleton(app: &mut FlighthookApp, show: bool) {
    let mut saved = app.settings.to_request();
    saved.body_tracking.show_skeleton = show;
    app.settings.load_from(&saved);
}

#[test]
fn the_old_body_tracking_keys_change_nothing_on_the_cameras_pane() {
    let screen = Screen::new();
    let mut app = app(&[0], true);
    app.shots = vec![shot_row(1, &["camera.0"])];
    app.shots_rev += 1;
    set_mode(&mut app, BodyTrackingMode::Live);
    let saved = app.settings.saved_body_tracking();
    let keys = [
        Key::B,
        Key::M,
        Key::OpenBracket,
        Key::CloseBracket,
        Key::Comma,
        Key::Period,
        Key::D,
    ];
    // Live, then a shot's clips.
    for live in [true, false] {
        if live {
            app.select_live();
        } else {
            app.select_shot(0);
            app.set_shots_pane(ShotsPane::Cameras);
        }
        screen.frame(&mut app, Vec::new());
        assert!(app.cameras_pane_shown());
        assert_eq!(app.live_shown(), live);
        for key in keys {
            screen.frame(&mut app, press(key));
            screen.frame(&mut app, Vec::new());
        }
        assert_eq!(app.body_tracking(), saved, "live {live}");
        assert!(app.tuning.sent.is_empty(), "live {live}");
        assert!(!app.settings.global_dirty, "live {live}");
        assert_eq!(app.settings.body_tracking.to_section(), Some(saved.clone()));
        assert!(app.pose_draw(Handedness::Right).overlay, "live {live}");
    }
}

#[test]
fn show_skeleton_off_hides_the_skeleton_on_a_live_view() {
    let screen = Screen::new();
    let mut app = app(&[0], true);
    set_mode(&mut app, BodyTrackingMode::Live);
    app.select_live();
    screen.frame(&mut app, Vec::new());
    let id = app.live["camera.0"].id;
    app.live_replies(
        vec![(
            id,
            LiveReply::Frame {
                seq: 1,
                jpeg: frame_jpeg(),
                pose: Some(pose(5.0)),
                state: PoseState {
                    state: "ready".into(),
                    error: None,
                },
            },
        )],
        1.0,
    );
    let marks = screen.frame(&mut app, Vec::new());
    assert!(
        screen.circles_in(marks["camera_live:camera.0"].0) > 0,
        "drawn by default"
    );
    assert!(!screen.texts().iter().any(|t| t.contains("skeleton off")));
    set_show_skeleton(&mut app, false);
    let marks = screen.frame(&mut app, Vec::new());
    assert_eq!(app.live["camera.0"].shown_pose, Some(pose(5.0)));
    assert_eq!(screen.circles_in(marks["camera_live:camera.0"].0), 0);
    assert!(screen.texts().iter().any(|t| t.contains("skeleton off")));
    set_show_skeleton(&mut app, true);
    let marks = screen.frame(&mut app, Vec::new());
    assert!(screen.circles_in(marks["camera_live:camera.0"].0) > 0);
}

#[test]
fn show_skeleton_off_hides_the_skeleton_in_the_player_and_the_replay() {
    let screen = Screen::new();
    let mut app = app(&[0], true);
    app.shots = vec![shot_row(1, &["camera.0"])];
    app.shots_rev += 1;
    app.set_shots_pane(ShotsPane::Cameras);
    for mode in [BodyTrackingMode::Historical, BodyTrackingMode::Live] {
        set_mode(&mut app, mode);
        screen.frame(&mut app, Vec::new());
        assert!(app.pose_draw(Handedness::Right).overlay, "{mode:?}");
        set_show_skeleton(&mut app, false);
        let marks = screen.frame(&mut app, Vec::new());
        assert!(marks.contains_key("player:picture:camera.0"), "{mode:?}");
        assert!(!app.pose_draw(Handedness::Right).overlay, "{mode:?}");
        set_show_skeleton(&mut app, true);
    }
}

#[test]
fn off_shows_no_skeleton_or_swing_data_and_asks_for_no_tracking() {
    let screen = Screen::new();
    let mut app = app(&[0, 1], true);
    set_mode(&mut app, BodyTrackingMode::Off);
    let mut row = shot_row(1, &["camera.0", "camera.1"]);
    row.video[1].pose = false;
    row.video[1].swing = false;
    app.shots = vec![row];
    app.shots_rev += 1;
    app.set_shots_pane(ShotsPane::Cameras);
    screen.frame(&mut app, Vec::new());
    // Files on disk from before: not shown; an untracked clip is not
    // asked for.
    let player = app.shot_player.as_ref().expect("the inline player");
    assert!(!player.asked_to_track("camera.1"));
    give_report(&mut app, "camera.0", report(false));
    let marks = screen.frame(&mut app, Vec::new());
    assert!(
        marks.contains_key("player:picture:camera.0"),
        "the clip plays"
    );
    assert!(!marks.contains_key("swing:camera.0"));
    assert!(!app.pose_draw(Handedness::Right).overlay);
    // Historical: the same report shows, the skeleton is drawn.
    set_mode(&mut app, BodyTrackingMode::Historical);
    let marks = screen.frame(&mut app, Vec::new());
    assert!(marks.contains_key("swing:camera.0"));
    assert!(app.pose_draw(Handedness::Right).overlay);
    let player = app.shot_player.as_ref().expect("the inline player");
    assert!(player.asked_to_track("camera.1"));
    assert!(!player.asked_to_track("camera.0"), "tracked already");
}

#[test]
fn a_new_shot_keeps_live_and_picking_a_shot_leaves_it() {
    let screen = Screen::new();
    let mut app = app(&[0], true);
    app.shots = vec![shot_row(1, &[])];
    app.shots_rev += 1;
    app.select_live();
    screen.frame(&mut app, Vec::new());
    app.shots.push(shot_row(2, &[]));
    app.shots_rev += 1;
    app.observe_shots(1.0);
    screen.frame(&mut app, Vec::new());
    assert!(app.live_selected, "tuning is not interrupted");
    assert_eq!(app.shown_index(), Some(1), "the newest is selected beneath");
    app.select_shot(0);
    assert!(!app.live_selected);
    screen.frame(&mut app, Vec::new());
    assert!(app.live.is_empty());
}

#[test]
fn a_swing_done_later_is_fetched_by_the_player() {
    let screen = Screen::new();
    let mut app = app(&[0], true);
    let mut row = shot_row(1, &["camera.0"]);
    row.video[0].swing = false;
    app.shots = vec![row];
    app.shots_rev += 1;
    app.set_shots_pane(ShotsPane::Cameras);
    screen.frame(&mut app, Vec::new());
    assert!(!app.shot_player.as_ref().unwrap().wants_swing_of("camera.0"));
    app.handle_ui_message(crate::socket::feed::local_msg(
        "camera.0",
        flighthook::UiLocalEvent::ShotSwing {
            keys: vec![ShotKey {
                shot_id: "id1".into(),
                shot_number: 1,
            }],
        },
    ));
    assert!(app.shots[0].video[0].swing);
    assert!(app.shot_player.as_ref().unwrap().wants_swing_of("camera.0"));
}

/// Hand camera `camera`'s clip sidecar (a 1280×720 clip, 5 s at 60 fps)
/// to the inline player.
fn give_sidecar(app: &mut FlighthookApp, camera: &str) {
    let player = app.shot_player.as_mut().expect("a player");
    let frames: Vec<serde_json::Value> = (0..300u64)
        .map(
            |i| serde_json::json!({"t_ns": i * 16_666_667, "offset": 4096 + i * 1000, "len": 1000}),
        )
        .collect();
    let json = serde_json::json!({
        "version": 1, "fourcc": "MJPG", "width": 1280, "height": 720,
        "fps": {"nominal": 60.0, "measured": 60.0},
        "start_ns": 1_000_000_000u64,
        "frames": frames,
        "trigger_index": 180, "impact_index": 180, "trigger_ns": 3_000_000_000i64,
        "impact_ns": 3_000_000_000i64,
        "complete": true, "dropped": 0, "truncated": false,
    });
    let seq = player.seq();
    player.reply(crate::net::ReplayReply::Sidecar {
        replay: seq,
        camera: camera.to_owned(),
        result: stance::sidecar::Sidecar::from_json(&json.to_string()).map_err(|e| e.to_string()),
    });
}

/// Two cameras with a shot and both swing reports, at `w`×`h`: nothing
/// overlaps, the pictures fill their columns on screen and the swing
/// data follows under them; Live fits the same way.
fn layout_at(w: f32, h: f32) {
    let screen = Screen::sized(w, h);
    let mut app = app(&[0, 1], true);
    app.shots = vec![shot_row(1, &["camera.0", "camera.1"])];
    app.shots_rev += 1;
    app.set_shots_pane(ShotsPane::Cameras);
    screen.frame(&mut app, Vec::new());
    give_report(&mut app, "camera.0", report(true));
    give_report(&mut app, "camera.1", report(true));
    give_sidecar(&mut app, "camera.0");
    give_sidecar(&mut app, "camera.1");
    let marks = screen.frame(&mut app, Vec::new());
    let at = format!("{w}x{h}");
    let list = marks["view_selector"].0;
    let toggle = marks["live_toggle"].0;
    let (a, b) = (
        marks["player:picture:camera.0"].0,
        marks["player:picture:camera.1"].0,
    );
    assert!(a.left() > list.right(), "{at}: right of the list");
    assert!(a.top() > toggle.bottom(), "{at}: under the toolbar");
    assert!(a.right() < b.left(), "{at}: side by side");
    assert!(b.right() <= w, "{at}: on screen");
    assert!(
        a.bottom() <= h && b.bottom() <= h,
        "{at}: whole pictures on screen"
    );
    let pane = w - list.right();
    assert!(
        a.width() + b.width() > pane * 0.85,
        "{at}: the pictures fill the pane"
    );
    let scrubber = marks["player:scrubber"].0;
    assert!(
        scrubber.top() >= a.bottom(),
        "{at}: transport under the clips"
    );
    for (camera, pic) in [("camera.0", a), ("camera.1", b)] {
        let swing = marks[format!("swing:{camera}").as_str()].0;
        assert!(
            swing.top() >= scrubber.bottom(),
            "{at}: {camera} data under the transport"
        );
        assert!(swing.left() >= pic.left() - 1.0 && swing.right() <= pic.right() + 1.0);
    }
    let (sa, sb) = (marks["swing:camera.0"].0, marks["swing:camera.1"].0);
    assert!(
        sa.right() < sb.left(),
        "{at}: the data columns don't overlap"
    );

    // Live: pictures, posture tiles and the readout fit on screen.
    app.select_live();
    let marks = screen.frame(&mut app, Vec::new());
    let (a, b) = (
        marks["camera_live:camera.0"].0,
        marks["camera_live:camera.1"].0,
    );
    assert!(
        a.left() > list.right() && a.right() < b.left() && b.right() <= w,
        "{at}"
    );
    let line = marks["pose_readout"].0;
    for camera in ["camera.0", "camera.1"] {
        let tiles = marks[format!("posture:{camera}").as_str()].0;
        assert!(
            tiles.top() >= a.bottom() && tiles.bottom() <= line.top(),
            "{at}"
        );
    }
    assert!(line.bottom() <= h, "{at}: the readout on screen");
}

#[test]
fn the_cameras_pane_fits_1600x900_with_two_cameras() {
    layout_at(1600.0, 900.0);
}

#[test]
fn the_cameras_pane_fits_1024x768_with_two_cameras() {
    layout_at(1024.0, 768.0);
}

fn painted_circles(shapes: &[egui::epaint::ClippedShape]) -> Vec<Pos2> {
    fn collect(shape: &egui::Shape, centers: &mut Vec<Pos2>) {
        match shape {
            egui::Shape::Circle(c) => centers.push(c.center),
            egui::Shape::Vec(v) => v.iter().for_each(|s| collect(s, centers)),
            _ => {}
        }
    }
    let mut centers = Vec::new();
    for clipped in shapes {
        collect(&clipped.shape, &mut centers);
    }
    centers
}

/// Each text painted with the color it shows in.
fn painted_colored(shapes: &[egui::epaint::ClippedShape]) -> Vec<(String, egui::Color32)> {
    fn collect(shape: &egui::Shape, texts: &mut Vec<(String, egui::Color32)>) {
        match shape {
            egui::Shape::Text(t) => {
                let own = t
                    .galley
                    .job
                    .sections
                    .first()
                    .map_or(t.fallback_color, |s| s.format.color);
                let color = if own == egui::Color32::PLACEHOLDER {
                    t.fallback_color
                } else {
                    own
                };
                texts.push((t.galley.text().to_owned(), color));
            }
            egui::Shape::Vec(v) => v.iter().for_each(|s| collect(s, texts)),
            _ => {}
        }
    }
    let mut texts = Vec::new();
    for clipped in shapes {
        collect(&clipped.shape, &mut texts);
    }
    texts
}

fn painted_texts(shapes: &[egui::epaint::ClippedShape]) -> Vec<String> {
    fn collect(shape: &egui::Shape, texts: &mut Vec<String>) {
        match shape {
            egui::Shape::Text(t) => texts.push(t.galley.text().to_owned()),
            egui::Shape::Vec(v) => v.iter().for_each(|s| collect(s, texts)),
            _ => {}
        }
    }
    let mut texts = Vec::new();
    for clipped in shapes {
        collect(&clipped.shape, &mut texts);
    }
    texts
}

/// `report(false)` with its tempo changed to `backswing` and `downswing`
/// (ns); `None` removes it.
fn with_tempo(tempo: Option<(u64, u64)>) -> SwingReportFile {
    let mut r = report(false);
    r.tempo = tempo.map(|(backswing_ns, downswing_ns)| SwingTempo {
        backswing_ns,
        downswing_ns,
        ratio: backswing_ns as f64 / downswing_ns as f64,
    });
    r
}

fn tiles(reports: &[&SwingReportFile]) -> Vec<String> {
    tempo_tiles(reports).into_iter().map(|(_, v)| v).collect()
}

#[test]
fn the_tempo_is_the_average_over_the_cameras_that_have_one() {
    // 3.0 : 1 (900 / 300 ms) and 2.0 : 1 (600 / 300 ms): the mean
    // durations 750 / 300 ms and their ratio.
    let (a, b) = (
        with_tempo(Some((900_000_000, 300_000_000))),
        with_tempo(Some((600_000_000, 300_000_000))),
    );
    assert_eq!(tiles(&[&a, &b]), ["2.5 : 1", "750 ms", "300 ms"]);
    let (a, b) = (
        with_tempo(Some((800_000_000, 200_000_000))),
        with_tempo(Some((600_000_000, 400_000_000))),
    );
    // Mean durations 700 / 300 ms = 2.3 : 1; the mean of 4.0 and 1.5 is
    // 2.75.
    assert_eq!(tiles(&[&a, &b]), ["2.3 : 1", "700 ms", "300 ms"]);
    // A camera without a tempo is left out.
    let none = with_tempo(None);
    assert_eq!(tiles(&[&a, &none]), tiles(&[&a]));
    assert_eq!(tiles(&[&none, &a]), ["4.0 : 1", "800 ms", "200 ms"]);
    // One camera shows its own numbers, ratio as reported.
    let mut one = with_tempo(Some((841_666_667, 300_000_000)));
    one.tempo.as_mut().unwrap().ratio = 2.85;
    assert_eq!(tiles(&[&one]), ["2.9 : 1", "842 ms", "300 ms"]);
    // None has one: dashes.
    assert!(tiles(&[&none, &none]).iter().all(|v| v == NONE));
    // A sum above u64 keeps its mean.
    let big = with_tempo(Some((u64::MAX, u64::MAX)));
    assert_eq!(average_tempo(&[big.tempo.unwrap(); 2]).unwrap().ratio, 1.0);
}

#[test]
fn two_cameras_show_one_tempo_row_across_both_columns() {
    let screen = Screen::new();
    let mut app = app(&[0, 1], true);
    app.shots = vec![shot_row(1, &["camera.0", "camera.1"])];
    app.shots_rev += 1;
    app.set_shots_pane(ShotsPane::Cameras);
    screen.frame(&mut app, Vec::new());
    give_report(&mut app, "camera.0", report(false));
    let marks = screen.frame(&mut app, Vec::new());
    let one = marks["swing_tempo"].0;
    let left = marks["player:picture:camera.0"].0;
    let right = marks["player:picture:camera.1"].0;
    // One camera with data: the row is still the whole width of the
    // columns, above the metrics.
    assert!((one.left() - left.left()).abs() < 1.0);
    assert!((one.right() - right.right()).abs() < 1.0);
    assert!(one.bottom() <= marks["swing:camera.0"].0.top());
    give_report(
        &mut app,
        "camera.1",
        with_tempo(Some((630_000_000, 270_000_000))),
    );
    let marks = screen.frame(&mut app, Vec::new());
    let row = marks["swing_tempo"].0;
    assert!((row.left() - left.left()).abs() < 1.0);
    assert!((row.right() - right.right()).abs() < 1.0, "full width");
    assert!(row.top() > left.bottom(), "under the clips");
    for camera in ["camera.0", "camera.1"] {
        let metrics = marks[&format!("swing:{camera}")].0;
        assert!(metrics.top() >= row.bottom(), "metrics under the row");
    }
    // Both cameras' metrics keep their own column.
    assert!(marks["swing:camera.0"].0.right() <= marks["swing:camera.1"].0.left());
    // Only one tempo row on screen: the average, 690 / 285 ms.
    let texts = screen.texts();
    assert_eq!(texts.iter().filter(|t| *t == "TEMPO").count(), 1);
    assert!(texts.iter().any(|t| t == "690 ms"), "{texts:?}");
    assert!(texts.iter().any(|t| t == "285 ms"), "{texts:?}");
    assert!(texts.iter().any(|t| t == "2.4 : 1"), "{texts:?}");
}

#[test]
fn a_single_camera_keeps_its_own_tempo_and_layout() {
    let screen = Screen::new();
    let mut app = app(&[0], true);
    app.shots = vec![shot_row(1, &["camera.0"])];
    app.shots_rev += 1;
    app.set_shots_pane(ShotsPane::Cameras);
    screen.frame(&mut app, Vec::new());
    give_report(&mut app, "camera.0", report(false));
    let marks = screen.frame(&mut app, Vec::new());
    let row = marks["swing_tempo"].0;
    let metrics = marks["swing:camera.0"].0;
    let picture = marks["player:picture:camera.0"].0;
    assert!((row.left() - picture.left()).abs() < 1.0,);
    assert!((row.width() - picture.width()).abs() < 1.0, "the column");
    assert!(metrics.top() >= row.bottom());
    let texts = screen.texts();
    assert!(texts.iter().any(|t| t == "2.5 : 1"), "{texts:?}");
    // A camera that found no swing has no tempo row at all.
    let mut app = self::app(&[0], true);
    app.shots = vec![shot_row(1, &["camera.0"])];
    app.shots_rev += 1;
    app.set_shots_pane(ShotsPane::Cameras);
    screen.frame(&mut app, Vec::new());
    give_report(&mut app, "camera.0", no_swing_report());
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!marks.contains_key("swing_tempo"));
    assert!(marks.contains_key("swing:camera.0"));
}

/// The swing data under a clip is the tempo tiles and the metrics table:
/// no row of event times (those are the scrubber's markers).
#[test]
fn the_swing_panel_has_no_event_times_row() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let r = report(false);
    let out = ctx.run(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))),
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                tempo_row(ui, &tempo_values(&plain(&[(&r, None)])), 600.0);
                swing_metrics(ui, "camera.0", &r, true, 600.0, None, ReferenceKind::Tour);
            });
        },
    );
    let texts = painted_texts(&out.shapes);
    for gone in ["TAKEAWAY", "-1.15 s", "-0.30 s", "0.00 s", "+0.65 s"] {
        assert!(!texts.iter().any(|t| t == gone), "{gone}: {texts:?}");
    }
    // Finish is the table's column, once.
    assert_eq!(texts.iter().filter(|t| *t == "FINISH").count(), 1);
    // The tempo tiles and the metrics table are still there.
    for kept in [
        "2.5 : 1",
        "750 ms",
        "300 ms",
        "Hip sway",
        "5% torso",
        "Away",
        "12% torso",
        "Toward",
        "Side bend",
        "ADDRESS",
    ] {
        assert!(
            texts.iter().any(|t| t.eq_ignore_ascii_case(kept)),
            "{kept}: {texts:?}"
        );
    }
}

// -----------------------------------------------------------------------
// Grades: colors, words, key positions, the reference
// -----------------------------------------------------------------------

/// A grade of `id` at `position` with `fit`, its value and its words.
fn grade_of(id: &str, position: &str, fit: Fit, value: f64, word: Option<&str>) -> MetricGrade {
    let side = match fit {
        Fit::Both => flighthook::Side::On,
        _ if value < 0.0 => flighthook::Side::Low,
        _ => flighthook::Side::High,
    };
    MetricGrade {
        id: id.into(),
        position: position.into(),
        value,
        unit: "degrees".into(),
        low: 33.0,
        high: 43.0,
        fade: None,
        draw: None,
        z: 0.0,
        z_fade: None,
        z_draw: None,
        fit,
        side,
        direction: (fit != Fit::Both).then(|| "less".to_owned()),
        word: word.map(str::to_owned),
        proxy_sd: false,
        assumed_scale: false,
        club_fallback: false,
        sex_fallback: false,
        confidence: 0.9,
    }
}

fn reference_grades(
    reference: ReferenceKind,
    grades: Vec<MetricGrade>,
    tempo: Vec<MetricGrade>,
) -> ReferenceGrades {
    ReferenceGrades {
        reference,
        grades,
        tempo,
    }
}

#[test]
fn the_five_fits_have_five_colors_apart_from_plain_text() {
    let fits = [Fit::Both, Fit::Fade, Fit::Draw, Fit::Near, Fit::Far];
    let colors: Vec<egui::Color32> = fits.iter().map(|f| grade_color(*f)).collect();
    assert_eq!(
        colors,
        [
            theme::GRADE_BOTH,
            theme::GRADE_FADE,
            theme::GRADE_DRAW,
            theme::GRADE_NEAR,
            theme::GRADE_FAR
        ]
    );
    for (i, a) in colors.iter().enumerate() {
        assert_ne!(*a, theme::TEXT);
        for b in &colors[i + 1..] {
            assert_ne!(a, b);
        }
    }
    // The fade blue is not the headings' blue.
    assert_ne!(theme::GRADE_FADE, theme::HEADING);
}

#[test]
fn a_graded_value_takes_its_color_and_its_word() {
    let shown = |id, unit, v, cm, g: Option<&MetricGrade>| {
        let s = graded_shown(id, unit, v, cm, true, g);
        (joined(&s.text, s.word), s.color)
    };
    // Ungraded: plain, as before, a turn without a word.
    assert_eq!(
        shown("hip_turn", "degrees", 44.0, None, None),
        ("44°".to_owned(), theme::TEXT)
    );
    // A graded turn reads Open or Closed by its signed value, in every fit.
    for fit in [Fit::Both, Fit::Fade, Fit::Draw, Fit::Near, Fit::Far] {
        let g = grade_of(
            "hip_turn",
            "impact",
            fit,
            44.0,
            Some("more open: fade side"),
        );
        assert_eq!(
            shown("hip_turn", "degrees", 44.0, None, Some(&g)),
            ("44° Open".to_owned(), grade_color(fit)),
            "{fit:?}"
        );
    }
    let closed = grade_of("shoulder_turn", "impact", Fit::Near, -12.0, None);
    assert_eq!(
        shown("shoulder_turn", "degrees", 12.0, None, Some(&closed)).0,
        "12° Closed"
    );
    // A shift keeps its own word from address in its color.
    let g = grade_of(
        "hip_sway",
        "impact",
        Fit::Fade,
        2.1,
        Some("further away from the target: fade side"),
    );
    assert_eq!(
        shown("hip_sway", "torso", 0.05, Some(5.334), Some(&g)),
        ("2.1 in Toward".to_owned(), theme::GRADE_FADE)
    );
    // Anything else off its band takes its side's word; inside, none.
    let g = grade_of(
        "spine_forward_bend",
        "impact",
        Fit::Near,
        29.0,
        Some("less than tour: stood up"),
    );
    assert_eq!(
        shown("spine_forward_bend", "degrees", 29.0, None, Some(&g)),
        ("29° Less".to_owned(), theme::GRADE_NEAR)
    );
    let g = grade_of("spine_forward_bend", "impact", Fit::Both, 35.0, None);
    assert_eq!(
        shown("spine_forward_bend", "degrees", 35.0, None, Some(&g)),
        ("35°".to_owned(), theme::GRADE_BOTH)
    );
}

#[test]
fn a_tile_is_colored_only_on_a_key_positions_frame() {
    let fo = face_on_series(true);
    // The one-camera turn has a grade at impact only; hip sway at the top.
    let tour = reference_grades(
        ReferenceKind::Tour,
        vec![
            grade_of(
                "hip_turn",
                "impact",
                Fit::Fade,
                18.0,
                Some("more open: fade side"),
            ),
            grade_of(
                "hip_sway",
                "top",
                Fit::Near,
                -1.8,
                Some("further away from the target than tour: sway"),
            ),
        ],
        Vec::new(),
    );
    let tiles_at = |frame: u64| {
        frame_tiles(
            &[Swing {
                report: &fo,
                t_ns: Some(frame * FRAME_NS),
                grades: Some(&tour),
            }],
            false,
        )
    };
    // On the impact frame and one frame either side: the turn is
    // colored with its word; the hip sway (no impact grade) is plain.
    for frame in [179, 180, 181] {
        let tiles = tiles_at(frame);
        assert_eq!(tiles[0].color, theme::GRADE_FADE, "{frame}");
        assert_eq!(tiles[0].word, Some("Open"), "{frame}");
        assert!(tiles[0].grade.is_some());
        assert_eq!(tiles[2].color, theme::TEXT, "{frame}");
        assert!(tiles[2].grade.is_none());
    }
    // Two frames off: plain, no word.
    for frame in [178, 182, 250] {
        let tiles = tiles_at(frame);
        assert!(
            tiles
                .iter()
                .all(|t| t.color == theme::TEXT && t.grade.is_none()),
            "{frame}"
        );
        assert_eq!(tiles[0].word, None, "{frame}");
    }
    // On the top: the one-camera turn is not graded there, the hip sway
    // is.
    let tiles = tiles_at(150);
    assert_eq!((tiles[0].color, tiles[0].word), (theme::TEXT, None));
    assert_eq!(tiles[2].color, theme::GRADE_NEAR);
    assert_eq!(key_position(&fo, 150 * FRAME_NS), Some(SwingPosition::Top));
    assert_eq!(
        key_position(&fo, 60 * FRAME_NS),
        Some(SwingPosition::Address)
    );
    assert_eq!(key_position(&fo, 100 * FRAME_NS), None);
    // Without grades nothing is colored.
    let plain_tiles = frame_tiles(&plain(&[(&fo, Some(180 * FRAME_NS))]), false);
    assert!(plain_tiles.iter().all(|t| t.color == theme::TEXT));
}

#[test]
fn averaged_tiles_take_the_grade_the_cameras_share() {
    let (a, b) = (face_on_series(true), face_on_series(true));
    let fade = |fit| {
        reference_grades(
            ReferenceKind::Tour,
            vec![grade_of("hip_turn", "impact", fit, 18.0, Some("words"))],
            vec![grade_of("tempo_ratio", "swing", fit, 3.0, Some("words"))],
        )
    };
    let (g1, g2, g3) = (fade(Fit::Fade), fade(Fit::Fade), fade(Fit::Near));
    let at = Some(180 * FRAME_NS);
    let swing = |report, grades| Swing {
        report,
        t_ns: at,
        grades: Some(grades),
    };
    // Two face-on cameras that agree: the color; that disagree: none.
    let agree = frame_tiles(&[swing(&a, &g1), swing(&b, &g2)], false);
    assert_eq!(agree[0].color, theme::GRADE_FADE);
    let differ = frame_tiles(&[swing(&a, &g1), swing(&b, &g3)], false);
    assert_eq!(differ[0].color, theme::TEXT);
    assert!(differ[0].grade.is_none());
    // One of them without grades: none either.
    let half = frame_tiles(
        &[
            swing(&a, &g1),
            Swing {
                report: &b,
                t_ns: at,
                grades: None,
            },
        ],
        false,
    );
    assert_eq!(half[0].color, theme::TEXT);
    // The tempo: the face-on camera's grade over another view's; the
    // first camera's when no face-on camera has one.
    let dtl = down_the_line_series();
    let tempo = tempo_values(&[swing(&dtl, &g3), swing(&a, &g1)]);
    assert_eq!(
        tempo.iter().map(|t| t.color).collect::<Vec<_>>(),
        [theme::GRADE_FADE, theme::TEXT, theme::TEXT],
        "ratio graded, backswing never, downswing not in these grades"
    );
    let tempo = tempo_values(&[
        swing(&dtl, &g3),
        Swing {
            report: &a,
            t_ns: at,
            grades: None,
        },
    ]);
    assert_eq!(tempo[0].color, theme::GRADE_NEAR);
    let tempo = tempo_values(&[swing(&a, &g1), swing(&b, &g3)]);
    assert_eq!(tempo[0].color, theme::TEXT, "two face-on cameras disagree");
}

#[test]
fn the_miss_line_lists_the_tiles_off_their_band() {
    let tile = |label, grade: Option<MetricGrade>| FrameValue {
        id: "x",
        label,
        text: "1".into(),
        word: None,
        color: theme::TEXT,
        grade,
    };
    let tiles = [
        tile(
            "Hip turn",
            Some(grade_of(
                "hip_turn",
                "impact",
                Fit::Fade,
                44.0,
                Some("more open: fade side"),
            )),
        ),
        tile(
            "Shoulder turn",
            Some(grade_of("shoulder_turn", "impact", Fit::Both, 31.0, None)),
        ),
        tile("Head sway", None),
        tile(
            "Forward bend",
            Some(grade_of(
                "spine_forward_bend",
                "impact",
                Fit::Far,
                20.0,
                Some("less than tour: stood up, early extension pattern"),
            )),
        ),
    ];
    let entries = miss_entries(&tiles);
    assert_eq!(
        entries,
        [
            ("Hip turn", "more open: fade side".to_owned(), Fit::Fade),
            (
                "Forward bend",
                "less than tour: stood up, early extension pattern".to_owned(),
                Fit::Far
            ),
        ]
    );
    let job = miss_job(&entries, 400.0);
    assert_eq!(
        job.text,
        "Hip turn more open: fade side \u{b7} Forward bend less than tour: stood up, early extension pattern"
    );
    // Names muted, words in their fit's color.
    let colors: Vec<egui::Color32> = job.sections.iter().map(|s| s.format.color).collect();
    assert_eq!(
        colors,
        [
            theme::TEXT_MUTED,
            theme::GRADE_FADE,
            theme::TEXT_MUTED,
            theme::TEXT_MUTED,
            theme::GRADE_FAR
        ]
    );
    assert_eq!(job.wrap.max_rows, 2, "two lines at most");
    assert!(
        miss_entries(&tiles[1..3]).is_empty(),
        "all inside or ungraded"
    );
}

#[test]
fn a_cells_hover_names_the_band_in_the_units() {
    assert_eq!(band_text(33.0, 43.0, "degrees", true), "33\u{2013}43\u{b0}");
    assert_eq!(band_text(1.5, 5.5, "inches", true), "1.5\u{2013}5.5 in");
    // Rounded after the conversion.
    assert_eq!(band_text(1.5, 5.5, "inches", false), "3.8\u{2013}14.0 cm");
    assert_eq!(band_text(0.2, 0.3, "seconds", true), "0.20\u{2013}0.30 s");
    assert_eq!(band_text(2.5, 3.5, "ratio", true), "2.5\u{2013}3.5");
    let mut hips = grade_of(
        "hip_turn",
        "impact",
        Fit::Near,
        48.0,
        Some("more open than tour: fade or slice pattern"),
    );
    hips.fade = Some([36.0, 46.0]);
    hips.draw = Some([30.0, 40.0]);
    let shown = graded_shown("hip_turn", "degrees", 48.0, None, true, Some(&hips));
    assert_eq!(
        band_tip(ReferenceKind::Tour, &hips, &shown, true),
        "Tour \u{b7} fade 36\u{2013}46\u{b0} \u{b7} draw 30\u{2013}40\u{b0} \u{b7} 48\u{b0} Open \u{b7} more open than tour: fade or slice pattern"
    );
    let bend = grade_of("spine_forward_bend", "impact", Fit::Both, 35.0, None);
    let shown = graded_shown(
        "spine_forward_bend",
        "degrees",
        35.0,
        None,
        true,
        Some(&bend),
    );
    assert_eq!(
        band_tip(ReferenceKind::Amateur, &bend, &shown, true),
        "Amateur 33\u{2013}43\u{b0} \u{b7} 35\u{b0}"
    );
}

#[test]
fn the_table_colors_its_graded_cells_and_widens_to_the_lead_arm_positions() {
    assert_eq!(table_positions(WIDE_TABLE - 1.0).len(), 4);
    assert_eq!(table_positions(WIDE_TABLE), SwingPosition::ALL.as_slice());
    assert_eq!(
        table_positions(400.0)
            .iter()
            .map(|p| p.label())
            .collect::<Vec<_>>(),
        ["Address", "Top", "Impact", "Finish"]
    );
    let r = report(false);
    let tour = reference_grades(
        ReferenceKind::Tour,
        vec![grade_of(
            "lateral_spine_tilt",
            "impact",
            Fit::Draw,
            21.2,
            Some("more: draw side"),
        )],
        Vec::new(),
    );
    let rows = metric_rows(&r, true, table_positions(400.0), Some(&tour));
    let side_bend = &rows.iter().find(|(n, _)| n == "Side bend").unwrap().1;
    let colors: Vec<_> = side_bend.iter().map(|c| c.shown.color).collect();
    assert_eq!(
        colors,
        [theme::TEXT, theme::TEXT, theme::GRADE_DRAW, theme::TEXT],
        "only the graded cell"
    );
    // The cell keeps the table's own text; its hover has the words.
    assert_eq!(side_bend[2].shown.text, "21\u{b0}");
    assert_eq!(side_bend[2].shown.word, None);
    assert_eq!(side_bend[2].worded.word, Some("Less"));
    // The finish column: the shoulder tilt's finish-only value.
    let tilt = &rows.iter().find(|(n, _)| n == "Shoulder tilt").unwrap().1;
    assert_eq!(tilt[3].shown.text, "30\u{b0}");
}

fn give_grades(app: &mut FlighthookApp, camera: &str, grades: flighthook::BodyGrades) {
    let player = app.shot_player.as_mut().expect("a player");
    let seq = player.seq();
    player.reply(crate::net::ReplayReply::Grades {
        replay: seq,
        camera: camera.to_owned(),
        result: Ok(grades),
    });
}

/// Grades of camera `camera`: the hip turn at impact against tour (a
/// fade) and against amateurs (near).
fn hip_grades(camera: &str) -> flighthook::BodyGrades {
    flighthook::BodyGrades {
        shot_id: "id1".into(),
        camera: camera.into(),
        club_class: Some(flighthook::ClubClass::Driver),
        sex: flighthook::Sex::Male,
        hand: Handedness::Right,
        references: vec![
            reference_grades(
                ReferenceKind::Tour,
                vec![grade_of(
                    "hip_turn",
                    "impact",
                    Fit::Fade,
                    18.0,
                    Some("more open: fade side"),
                )],
                Vec::new(),
            ),
            reference_grades(
                ReferenceKind::Amateur,
                vec![grade_of(
                    "hip_turn",
                    "impact",
                    Fit::Near,
                    18.0,
                    Some("more open than amateurs: fade or slice pattern"),
                )],
                Vec::new(),
            ),
        ],
    }
}

/// A shot of a `hand` golfer in the Cameras pane with the face-on
/// camera's report, clip and grades, paused on the impact frame.
fn graded_shot(hand: Handedness) -> (Screen, FlighthookApp) {
    let screen = Screen::new();
    let mut app = app(&[0], true);
    app.set_body_reference(ReferenceKind::Tour);
    let mut row = shot_row(1, &["camera.0"]);
    row.handed = Some(hand);
    app.shots = vec![row];
    app.shots_rev += 1;
    app.set_shots_pane(ShotsPane::Cameras);
    screen.frame(&mut app, Vec::new());
    give_report(&mut app, "camera.0", face_on_series(true));
    give_sidecar(&mut app, "camera.0");
    give_grades(&mut app, "camera.0", hip_grades("camera.0"));
    screen.frame(&mut app, Vec::new());
    seek_to_frame(&mut app, 180);
    screen.frame(&mut app, Vec::new());
    (screen, app)
}

#[test]
fn the_reference_picker_switches_the_colors_and_is_kept() {
    let (screen, mut app) = graded_shot(Handedness::Right);
    let marks = screen.frame(&mut app, Vec::new());
    // Beside the Live switch, in the toolbar.
    let (live, picker) = (marks["live_toggle"].0, marks["body_reference"].0);
    assert!(picker.left() > live.right() && (picker.center().y - live.center().y).abs() < 2.0);
    assert!(
        screen.texts().iter().any(|t| t == "Tour") && screen.texts().iter().any(|t| t == "Amateur")
    );
    assert!(
        !screen.texts().iter().any(|t| t == "Mine"),
        "not before personal bands"
    );
    // Tour: the hips in the fade band, blue, with the turn's word and the
    // miss line.
    assert_eq!(screen.color_of("18°"), Some(theme::GRADE_FADE));
    assert_eq!(screen.color_of("Open"), Some(theme::GRADE_FADE));
    assert!(marks.contains_key("frame_misses"));
    assert!(
        screen
            .texts()
            .iter()
            .any(|t| t.contains("Hip turn more open: fade side")),
        "{:?}",
        screen.texts()
    );
    // Amateur: yellow, its own words.
    let at = marks["body_reference:amateur"].0.center();
    screen.click(&mut app, at);
    assert_eq!(app.body_reference, ReferenceKind::Amateur);
    assert_eq!(
        stored_reference(),
        ReferenceKind::Amateur,
        "kept on this device"
    );
    screen.frame(&mut app, Vec::new());
    assert_eq!(screen.color_of("18°"), Some(theme::GRADE_NEAR));
    assert!(screen.texts().iter().any(|t| t.contains("than amateurs")));
    // Off the key position: plain, and the miss line's words go.
    seek_to_frame(&mut app, 120);
    screen.frame(&mut app, Vec::new());
    assert_eq!(screen.color_of("12°"), Some(theme::TEXT));
    assert!(!screen.texts().iter().any(|t| t.contains("than amateurs")));
    app.set_body_reference(ReferenceKind::Tour);
    // Not in the Data pane.
    app.set_shots_pane(ShotsPane::Data);
    let marks = screen.frame(&mut app, Vec::new());
    assert!(!marks.contains_key("body_reference"));
}

#[test]
fn a_left_handed_golfers_tiles_read_the_same() {
    let (right, mut rh) = graded_shot(Handedness::Right);
    let (left, mut lh) = graded_shot(Handedness::Left);
    right.frame(&mut rh, Vec::new());
    left.frame(&mut lh, Vec::new());
    assert_eq!(row_values(&right), row_values(&left));
    for text in ["18°", "Open"] {
        assert_eq!(right.color_of(text), left.color_of(text), "{text}");
    }
}

/// Answer the Last shot strip being fetched: camera `camera`'s swing
/// report and its grades.
fn answer_strip(app: &mut FlighthookApp, screen: &Screen, camera: &str, report: SwingReportFile) {
    screen.frame(app, Vec::new());
    let seq = app.live_strip.next_seq().expect("a strip being fetched");
    let strip = app.live_strip.find_mut(seq).unwrap();
    assert!(strip.reply(crate::net::ReplayReply::Swing {
        replay: seq,
        camera: camera.to_owned(),
        result: Ok(report),
    }));
    screen.frame(app, Vec::new());
    let strip = app.live_strip.find_mut(seq).unwrap();
    assert!(strip.reply(crate::net::ReplayReply::Grades {
        replay: seq,
        camera: camera.to_owned(),
        result: Ok(hip_grades(camera)),
    }));
}

#[test]
fn live_shows_the_last_shots_swing_under_the_pictures_until_the_next_one_is_in() {
    for (w, h) in [(1600.0, 900.0), (1024.0, 768.0)] {
        let screen = Screen::sized(w, h);
        let mut app = app(&[0], true);
        set_mode(&mut app, BodyTrackingMode::Live);
        app.set_body_reference(ReferenceKind::Tour);
        app.shots = vec![shot_row(1, &["camera.0"])];
        app.shots_rev += 1;
        app.set_shots_pane(ShotsPane::Cameras);
        app.select_live();
        let marks = screen.frame(&mut app, Vec::new());
        assert!(marks.contains_key("camera_live:camera.0"));
        assert!(
            !marks.contains_key("live_last_shot"),
            "nothing before the swing"
        );
        answer_strip(&mut app, &screen, "camera.0", face_on_series(true));
        let marks = screen.frame(&mut app, Vec::new());
        let strip = marks["live_last_shot"].0;
        let picture = marks["camera_live:camera.0"].0;
        assert!(strip.top() >= picture.bottom(), "under the pictures");
        assert!(
            (strip.left() - picture.left()).abs() < 0.5,
            "{strip:?} {picture:?}"
        );
        assert!(strip.bottom() <= h, "on screen at {w}x{h}");
        let readout = marks["pose_readout"].0;
        assert!(readout.top() >= strip.bottom() && readout.bottom() <= h);
        assert!(screen.texts().iter().any(|t| t == "Last shot \u{b7} #1"));
        // The body at impact (frame 180), colored, with the misses.
        assert_eq!(screen.color_of("18°"), Some(theme::GRADE_FADE));
        assert!(
            screen
                .texts()
                .iter()
                .any(|t| t.contains("Hip turn more open: fade side"))
        );
        // The next shot: the strip stays until its swing is in.
        app.shots.push(shot_row(2, &["camera.0"]));
        app.shots_rev += 1;
        screen.frame(&mut app, Vec::new());
        assert!(app.live_selected, "a new shot keeps Live");
        assert!(screen.texts().iter().any(|t| t == "Last shot \u{b7} #1"));
        answer_strip(&mut app, &screen, "camera.0", face_on_series(false));
        screen.frame(&mut app, Vec::new());
        assert!(screen.texts().iter().any(|t| t == "Last shot \u{b7} #2"));
        assert!(!screen.texts().iter().any(|t| t == "Last shot \u{b7} #1"));
        // Body tracking off: no strip.
        set_mode(&mut app, BodyTrackingMode::Off);
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!marks.contains_key("live_last_shot"));
    }
}

/// A shot's clips each have a download icon; Live, which has no
/// clip to download, has none.
#[test]
fn only_a_shots_clips_have_a_download_icon() {
    let screen = Screen::new();
    let mut app = app(&[0, 1], true);
    app.shots = vec![shot_row(1, &["camera.0", "camera.1"])];
    app.shots_rev += 1;
    app.set_shots_pane(ShotsPane::Cameras);
    screen.frame(&mut app, Vec::new());
    let marks = screen.frame(&mut app, Vec::new());
    assert!(marks.contains_key("player:download:camera.0"));
    assert!(marks.contains_key("player:download:camera.1"));
    app.select_live();
    screen.frame(&mut app, Vec::new());
    let marks = screen.frame(&mut app, Vec::new());
    assert!(marks.contains_key("camera_live:camera.0"));
    assert!(!marks.keys().any(|k| k.contains("download")), "{marks:?}");
}

/// The Cameras pane's scrubber marks the swing events once a report is
/// in, in swing order and inside the scrubber, with impact left to the
/// gold tick.
#[test]
fn the_panes_scrubber_marks_the_swing_events_once_reports_are_in() {
    let screen = Screen::new();
    let mut app = app(&[0, 1], true);
    app.shots = vec![shot_row(1, &["camera.0", "camera.1"])];
    app.shots_rev += 1;
    app.set_shots_pane(ShotsPane::Cameras);
    screen.frame(&mut app, Vec::new());
    give_sidecar(&mut app, "camera.0");
    give_sidecar(&mut app, "camera.1");
    let marks = screen.frame(&mut app, Vec::new());
    assert!(marks.contains_key("player:scrubber"));
    let events = |marks: &Marks| -> Vec<String> {
        let mut events: Vec<(f32, String)> = marks
            .iter()
            .filter_map(|(k, (r, _))| {
                k.strip_prefix("player:event:")
                    .map(|n| (r.center().x, n.to_owned()))
            })
            .collect();
        events.sort_by(|a, b| a.0.total_cmp(&b.0));
        events.into_iter().map(|(_, n)| n).collect()
    };
    assert!(events(&marks).is_empty(), "no report yet");
    give_report(&mut app, "camera.0", report(false));
    give_report(&mut app, "camera.1", report(false));
    let marks = screen.frame(&mut app, Vec::new());
    assert_eq!(events(&marks), ["address", "takeaway", "top", "finish"]);
    let scrubber = marks["player:scrubber"].0;
    for name in ["address", "takeaway", "top", "finish"] {
        let rect = marks[&format!("player:event:{name}")].0;
        assert!(rect.intersects(scrubber), "{name}");
    }
}
