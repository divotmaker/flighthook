use super::*;
use crate::panels::devices::Marks;
use crate::types::{ActorStatus, ActorStatusResponse, FlighthookConfig};
use egui::Event;
use std::collections::HashMap;

/// What one pass of the bar drew.
struct Pass {
    marks: Marks,
    /// Every text and its color.
    texts: Vec<(String, Color32)>,
    /// Vertical hairlines drawn (dividers).
    dividers: usize,
    /// Where the area under the bar starts.
    below: f32,
    /// Where the area above a bottom bar ends.
    above: f32,
    /// Every rect filled with the accent color (selection bars, the
    /// nav marker).
    accents: Vec<Rect>,
    /// The y of every horizontal hairline longer than 100 px.
    rules: Vec<f32>,
    /// Every icon drawn: its file name, tint and rect.
    images: Vec<(String, Color32, Rect)>,
    /// Every text and where it was drawn.
    text_rects: Vec<(String, Rect)>,
}

impl Pass {
    fn has_text(&self, text: &str) -> bool {
        self.texts.iter().any(|(t, _)| t == text)
    }

    fn color_of(&self, text: &str) -> Color32 {
        self.texts
            .iter()
            .find(|(t, _)| t == text)
            .unwrap_or_else(|| panic!("{text:?} not drawn: {:?}", self.texts))
            .1
    }

    fn rect(&self, name: &str) -> Rect {
        self.marks
            .get(name)
            .unwrap_or_else(|| panic!("{name} not drawn: {:?}", self.marks.keys()))
            .0
    }

    fn enabled(&self, name: &str) -> bool {
        self.marks[name].1
    }

    /// The tint and rect of the icon drawn from file `file` (no `.png`).
    fn image(&self, file: &str) -> (Color32, Rect) {
        self.images
            .iter()
            .find(|(f, _, _)| f == file)
            .map(|(_, c, r)| (*c, *r))
            .unwrap_or_else(|| panic!("{file} not drawn: {:?}", self.images))
    }

    /// The file name of the icon drawn inside the mark `name`.
    fn icon_in(&self, name: &str) -> (String, Color32) {
        let rect = self.rect(name);
        self.images
            .iter()
            .find(|(_, _, r)| rect.contains_rect(*r))
            .map(|(f, c, _)| (f.clone(), *c))
            .unwrap_or_else(|| panic!("no icon in {name}: {:?}", self.images))
    }

    /// Where text `text` was drawn.
    fn text_rect(&self, text: &str) -> Rect {
        self.text_rects
            .iter()
            .find(|(t, _)| t == text)
            .unwrap_or_else(|| panic!("{text:?} not drawn: {:?}", self.texts))
            .1
    }

    /// Whether an accent bar is drawn inside the mark `name`.
    fn has_accent_in(&self, name: &str) -> bool {
        let rect = self.rect(name);
        self.accents.iter().any(|a| rect.contains_rect(*a))
    }
}

fn flatten(shape: &egui::Shape, pass: &mut Pass, names: &HashMap<egui::TextureId, String>) {
    match shape {
        egui::Shape::Vec(inner) => inner.iter().for_each(|s| flatten(s, pass, names)),
        egui::Shape::Rect(r) if is_accent(r.fill) => pass.accents.push(r.rect),
        egui::Shape::Mesh(m) => {
            if let (Some(name), Some(v)) = (names.get(&m.texture_id), m.vertices.first()) {
                pass.images.push((name.clone(), v.color, m.calc_bounds()));
            }
        }
        egui::Shape::Text(t) => {
            let color = t
                .galley
                .job
                .sections
                .first()
                .map_or(t.fallback_color, |s| s.format.color);
            pass.texts.push((t.galley.text().to_owned(), color));
            pass.text_rects
                .push((t.galley.text().to_owned(), t.visual_bounding_rect()));
        }
        egui::Shape::LineSegment { points, .. } => {
            let (a, b) = (points[0], points[1]);
            if (a.x - b.x).abs() < 0.5 && (a.y - b.y).abs() > 8.0 {
                pass.dividers += 1;
            }
            if (a.y - b.y).abs() < 0.5 && (a.x - b.x).abs() > 100.0 {
                pass.rules.push(a.y);
            }
        }
        _ => {}
    }
}

/// Whether `fill` is the accent color, also while a popup fades it in.
fn is_accent(fill: Color32) -> bool {
    if fill.a() == 0 {
        return false;
    }
    let o = fill.to_opaque();
    let near = |x: u8, y: u8| x.abs_diff(y) <= 4;
    near(o.r(), theme::ACCENT.r())
        && near(o.g(), theme::ACCENT.g())
        && near(o.b(), theme::ACCENT.b())
}

fn pass_at(
    ctx: &egui::Context,
    app: &mut FlighthookApp,
    width: f32,
    time: f64,
    events: Vec<Event>,
) -> Pass {
    ctx.data_mut(|d| d.remove_temp::<Marks>(egui::Id::NULL));
    let input = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width, 900.0))),
        time: Some(time),
        events,
        ..Default::default()
    };
    let (mut below, mut above) = (0.0, 0.0);
    let out = ctx.run(input, |ctx| {
        app.render_header_in_test(ctx);
        below = ctx.available_rect().top();
        above = ctx.available_rect().bottom();
    });
    let mut pass = Pass {
        marks: ctx
            .data(|d| d.get_temp::<Marks>(egui::Id::NULL))
            .unwrap_or_default(),
        texts: Vec::new(),
        dividers: 0,
        below,
        above,
        accents: Vec::new(),
        rules: Vec::new(),
        images: Vec::new(),
        text_rects: Vec::new(),
    };
    let names = icons::loaded(ctx);
    for clipped in &out.shapes {
        flatten(&clipped.shape, &mut pass, &names);
    }
    pass
}

/// The bar settled (the icon font loads on the second pass).
fn settled(app: &mut FlighthookApp, width: f32) -> (egui::Context, Pass) {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    pass_at(&ctx, app, width, 0.0, Vec::new());
    let pass = pass_at(&ctx, app, width, 0.0, Vec::new());
    (ctx, pass)
}

fn pass(app: &mut FlighthookApp, width: f32) -> Pass {
    settled(app, width).1
}

/// Open `menu` and draw it.
fn open(app: &mut FlighthookApp, menu: HeaderMenu, width: f32) -> Pass {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    pass_at(&ctx, app, width, 0.0, Vec::new());
    app.header_menu = Some(menu);
    pass_at(&ctx, app, width, 0.0, Vec::new());
    pass_at(&ctx, app, width, 0.0, Vec::new())
}

const ICON_FLAGS: [&str; 4] = ["energy", "follow", "auto-chip", "shot-type"];

/// An app whose bar shows the icons in `flags` (`ICON_FLAGS` bits).
fn bar_app(flags: u8) -> FlighthookApp {
    let on = |i: usize| flags & (1 << i) != 0;
    let mut app = FlighthookApp::with_pending(crate::net::new_pending());
    let mut config = FlighthookConfig::default();
    config.simulation.auto_chip = on(2);
    if on(3) {
        config
            .mevo
            .insert("0".into(), flighthook::MevoSection::default());
    }
    if on(1) {
        config.golfers = vec![
            flighthook::GolferRecord::new("flighthook.e", "Eric"),
            flighthook::GolferRecord::new("flighthook.s", "Sam"),
        ];
        app.turn_golfer = Some("flighthook.e".into());
    }
    app.settings.load_from(&config);
    if on(0) {
        charge(&mut app, "square.0", "Square Omni", 74);
    }
    app
}

fn charge(app: &mut FlighthookApp, id: &str, name: &str, pct: u8) {
    let mut actor = ActorStatusResponse::new(name, ActorStatus::Connected);
    actor
        .telemetry
        .insert("battery_pct".into(), pct.to_string());
    app.actors.insert(id.into(), actor);
}

const RIGHT_ORDER: [&str; 6] = [
    "energy",
    "follow",
    "auto-chip",
    "shot-type",
    "club",
    "golfer",
];

/// Every button's rect, at three widths and for every combination of
/// the icons that come and go: the right group in role order,
/// anchored to the right edge, 40 px buttons 6 px apart, no divider,
/// widths 86 to 270; the nav the same on the left.
#[test]
fn every_button_has_its_place_at_every_width_and_icon_combination() {
    for flags in 0..16u8 {
        for width in [1600.0, 1024.0, 720.0] {
            let mut app = bar_app(flags);
            let p = pass(&mut app, width);
            let at = format!("flags {flags:04b} at {width}");
            let expected: Vec<&str> = RIGHT_ORDER
                .iter()
                .copied()
                .filter(|name| {
                    ICON_FLAGS
                        .iter()
                        .position(|f| f == name)
                        .is_none_or(|i| flags & (1 << i) != 0)
                })
                .collect();
            let present: Vec<&str> = RIGHT_ORDER
                .iter()
                .copied()
                .filter(|n| p.marks.contains_key(*n))
                .collect();
            assert_eq!(present, expected, "{at}");
            let rects: Vec<Rect> = present.iter().map(|n| p.rect(n)).collect();
            for r in &rects {
                assert_eq!(r.size(), vec2(BUTTON, BUTTON), "{at}");
                assert_eq!(r.center().y, BAR_HEIGHT / 2.0, "{at}");
            }
            for pair in rects.windows(2) {
                assert_eq!(pair[1].left() - pair[0].right(), GAP, "{at}");
            }
            assert_eq!(rects.last().unwrap().right(), width - SIDE_MARGIN, "{at}");
            let group = rects.last().unwrap().right() - rects[0].left();
            assert_eq!(group, group_width(expected.len(), BUTTON, GAP), "{at}");
            let widths = [86.0, 132.0, 178.0, 224.0, 270.0];
            assert!(widths.contains(&group), "{at}: {group}");
            let nav: Vec<Rect> = ["shots", "cameras", "settings", "log"]
                .iter()
                .map(|n| p.rect(&format!("nav:{n}")))
                .collect();
            assert_eq!(nav[0].left(), SIDE_MARGIN, "{at}");
            for pair in nav.windows(2) {
                assert_eq!(pair[1].left() - pair[0].right(), GAP, "{at}");
            }
            assert_eq!(nav[3].right() - nav[0].left(), 178.0, "{at}");
            assert!(nav[3].right() < rects[0].left(), "{at}: the groups touch");
            assert_eq!(p.dividers, 0, "{at}: no divider");
            assert_eq!(p.below, BAR_HEIGHT, "{at}: the bar is one row");
        }
    }
}

/// In the portrait layout the top bar is one row of 52 with all seven
/// right-group buttons (44 px, 6 apart), anchored right while they fit
/// and always on screen; the nav is a bottom bar of 52 with four equal
/// tabs, 44 px tall, across the window.
#[test]
fn the_portrait_header_is_one_row_over_a_bottom_tab_bar() {
    for width in [360.0, 390.0, 430.0] {
        let mut app = bar_app(0b1111);
        app.portrait = true;
        let p = pass(&mut app, width);
        let at = format!("at {width}");
        assert_eq!(p.below, BAR_HEIGHT, "{at}");
        assert_eq!(p.above, 900.0 - BAR_HEIGHT, "{at}");
        let right: Vec<Rect> = RIGHT_ORDER.iter().map(|n| p.rect(n)).collect();
        for r in &right {
            assert_eq!(r.size(), vec2(TOUCH_TARGET, TOUCH_TARGET), "{at}");
            assert_eq!(r.center().y, BAR_HEIGHT / 2.0, "{at}");
            assert!(r.left() >= 0.0 && r.right() <= width, "{at}: {r:?}");
        }
        for pair in right.windows(2) {
            assert_eq!(pair[1].left() - pair[0].right(), GAP, "{at}");
        }
        let last = right.last().unwrap().right();
        assert_eq!(last, (width - SIDE_MARGIN - 294.0).max(GAP) + 294.0, "{at}");
        let nav: Vec<Rect> = ["shots", "cameras", "settings", "log"]
            .iter()
            .map(|n| p.rect(&format!("nav:{n}")))
            .collect();
        assert_eq!(nav[0].left(), 0.0, "{at}");
        assert_eq!(nav[3].right(), width, "{at}");
        for r in &nav {
            assert_eq!(r.size(), vec2(width / 4.0, TOUCH_TARGET), "{at}");
            assert_eq!(r.center().y, 900.0 - BAR_HEIGHT / 2.0, "{at}");
        }
        for pair in nav.windows(2) {
            assert_eq!(pair[1].left(), pair[0].right(), "{at}");
        }
        // Shots is selected: its marker sits at the tab bar's top edge.
        let marker = pos2(nav[0].center().x, 900.0 - BAR_HEIGHT + 1.5);
        assert!(
            p.accents.iter().any(|a| a.center() == marker),
            "{at}: {:?}",
            p.accents
        );
        assert!(!p.marks.contains_key("title"), "{at}: no room for it");
    }
}

/// With few right-group buttons the portrait top bar shows the title,
/// left of the group and clear of it.
#[test]
fn the_portrait_header_shows_the_title_where_it_fits() {
    let mut app = bar_app(0);
    app.portrait = true;
    let p = pass(&mut app, 430.0);
    let title = p.rect("title");
    assert!(title.left() >= TITLE_CLEAR, "{title:?}");
    assert!(
        title.right() + TITLE_CLEAR <= p.rect("club").left(),
        "{title:?}"
    );
    assert_eq!(title.center().y, BAR_HEIGHT / 2.0);
}

/// On a touch screen a tap on a disabled tab shows its reason above
/// the tab, on screen.
#[test]
fn a_tap_on_a_disabled_tab_shows_its_reason_above_it() {
    let width = 390.0;
    let mut app = bar_app(0b1000);
    app.portrait = true;
    app.settings.actors[0].set_dirty(true);
    let (ctx, p) = settled(&mut app, width);
    let tab = p.rect("nav:cameras");
    assert!(!p.enabled("nav:cameras"));
    let at = tab.center();
    let press = |pressed: bool| Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    let finger = Event::Touch {
        device_id: egui::TouchDeviceId(0),
        id: egui::TouchId(0),
        phase: egui::TouchPhase::Start,
        pos: at,
        force: None,
    };
    pass_at(
        &ctx,
        &mut app,
        width,
        0.1,
        vec![finger, Event::PointerMoved(at), press(true)],
    );
    pass_at(
        &ctx,
        &mut app,
        width,
        0.2,
        vec![press(false), Event::PointerGone],
    );
    pass_at(&ctx, &mut app, width, 0.3, Vec::new());
    let p = pass_at(&ctx, &mut app, width, 0.4, Vec::new());
    let tip = p.text_rect(LOCKED_REASON);
    assert!(tip.bottom() < tab.top(), "above the tab: {tip:?} {tab:?}");
    assert!(
        tip.left() >= 0.0 && tip.right() <= width,
        "on screen: {tip:?}"
    );
}

/// Below the narrowest supported width (400 logical px here) the right
/// group starts a gap after the nav and is cut off at the right edge:
/// the groups never overlap.
#[test]
fn a_window_too_narrow_for_both_groups_clips_the_right_group() {
    let mut app = bar_app(0b1111);
    let p = pass(&mut app, 400.0);
    let nav_end = p.rect("nav:log").right();
    for name in RIGHT_ORDER {
        assert!(
            p.rect(name).left() >= nav_end + GAP - 0.01,
            "{name} overlaps the nav: {:?} vs {nav_end}",
            p.rect(name)
        );
    }
    assert!(!p.marks.contains_key("title"));
}

/// The narrowest window, in px, at which a title is drawn: found by
/// widening the window one step at a time.
fn threshold(flags: u8, want: &str, prepare: impl Fn(&mut FlighthookApp)) -> f32 {
    let mut app = bar_app(flags);
    prepare(&mut app);
    let ctx = egui::Context::default();
    theme::install(&ctx);
    pass_at(&ctx, &mut app, 800.0, 0.0, Vec::new());
    let mut width = 520.0;
    loop {
        if pass_at(&ctx, &mut app, width, 0.0, Vec::new())
            .texts
            .iter()
            .any(|(t, _)| t.contains(want))
        {
            return width;
        }
        width += 1.0;
        assert!(width < 1400.0, "no title ever fits");
    }
}

/// Widths here are logical px (the window at zoom 1; the app zooms out
/// below a 1024 px window, so a narrower window has more logical px).
/// The title is there at 1024, and at 720 unless the right group is the
/// widest.
#[test]
fn the_title_shows_at_1024_not_at_720_logical_px() {
    for flags in [0b1100u8, 0b1110, 0b1101, 0b1111] {
        let mut app = bar_app(flags);
        assert!(
            pass(&mut app, 1024.0).marks.contains_key("title"),
            "{flags}"
        );
        // At 720 only the narrower groups leave room for it.
        let at_720 = pass(&mut app, 720.0).marks.contains_key("title");
        assert_eq!(at_720, flags != 0b1111, "{flags:04b} at 720");
    }
}

/// The window a title of width `title` first fits in: 24 px clear of
/// the right group on the half of the bar it sits on.
#[test]
fn the_narrowest_window_follows_the_clearance_rule() {
    for flags in [0b1100u8, 0b1111] {
        let n = 2 + flags.count_ones() as usize;
        let mut app = bar_app(flags);
        let wide = pass(&mut app, 1600.0);
        let title = wide.rect("title").width();
        let found = threshold(flags, "FLIGHTHOOK", |_| {});
        let rule = 2.0 * (SIDE_MARGIN + group_width(n, BUTTON, GAP) + TITLE_CLEAR + title / 2.0);
        assert!(
            (found - rule).abs() <= 1.5,
            "{flags:04b}: {found} vs {rule}"
        );
    }
}

/// The title keeps 24 px clear of both groups and sits at the true
/// center of the bar.
#[test]
fn the_title_is_centered_and_keeps_clear_of_the_groups() {
    let mut app = bar_app(0b1111);
    for width in [900.0, 1024.0, 1600.0] {
        let p = pass(&mut app, width);
        let title = p.rect("title");
        assert!((title.center().x - width / 2.0).abs() < 0.01, "{width}");
        assert!(
            title.left() - p.rect("nav:log").right() >= TITLE_CLEAR,
            "{width}"
        );
        assert!(
            p.rect("energy").left() - title.right() >= TITLE_CLEAR,
            "{width}"
        );
    }
}

/// The toast's text, if one shows.
fn toast_text(app: &FlighthookApp) -> Option<&str> {
    app.toast.as_ref().map(|(text, _)| text.as_str())
}

/// A Follow turns switch shows a toast naming the new state, drawn
/// near the bottom of the content.
#[test]
fn a_follow_switch_shows_a_toast_of_the_new_state() {
    let mut app = golfers_app();
    let (ctx, p) = settled(&mut app, 1600.0);
    let at = p.rect("follow").center();
    click(&ctx, &mut app, at);
    assert!(!app.follow_turns);
    assert_eq!(toast_text(&app), Some("Following turns: off."));
    click(&ctx, &mut app, at);
    let on = "Following turns: on.";
    assert_eq!(toast_text(&app), Some(on), "the new toast replaces the old");
    let p = pass_at(&ctx, &mut app, 1600.0, 1.0, Vec::new());
    assert!(!p.has_text("Following turns: off."), "{:?}", p.texts);
    let rect = p.text_rect(on);
    assert!(
        rect.bottom() > 700.0 && rect.width() <= TOAST_WIDTH,
        "{rect:?}"
    );
    assert!((rect.center().x - 800.0).abs() < 8.0, "centered: {rect:?}");
}

/// The toast shows for its time and the fade, then is gone. On a phone
/// it is centered above the tab bar, no wider than the screen.
#[test]
fn the_toast_is_gone_after_its_time() {
    let mut app = golfers_app();
    app.portrait = true;
    let (ctx, p) = settled(&mut app, 390.0);
    click_at(&ctx, &mut app, 390.0, p.rect("follow").center());
    let text = app.follow_switch_text().to_owned();
    for time in [1.0, 2.4, 2.7] {
        let p = pass_at(&ctx, &mut app, 390.0, time, Vec::new());
        assert!(p.has_text(&text), "{time}: {:?}", p.texts);
        let rect = p.text_rect(&text);
        let tab = p.rect("nav:shots");
        assert!(rect.bottom() < tab.top(), "{rect:?} {tab:?}");
        assert!(rect.left() > 0.0 && rect.right() < 390.0, "{rect:?}");
    }
    let p = pass_at(&ctx, &mut app, 390.0, TOAST_SHOW + TOAST_FADE, Vec::new());
    assert!(!p.has_text(&text), "{:?}", p.texts);
    assert_eq!(toast_text(&app), None);
}

/// On a touch screen a tap on the disabled Auto-Chip shows its reason
/// under it until the next tap anywhere; without a touch it does not.
#[test]
fn a_tap_on_disabled_auto_chip_shows_its_reason_until_the_next_tap() {
    let reason = |p: &Pass| {
        p.texts
            .iter()
            .any(|(t, _)| t.starts_with("Sending the chip"))
    };
    for touch in [true, false] {
        let mut app = bar_app(0b0100);
        app.actors.insert(
            "openconnect_client.0".into(),
            ActorStatusResponse::new("GSPro", ActorStatus::Connected),
        );
        app.auto_chip_in_flight = Some(3);
        let (ctx, p) = settled(&mut app, 390.0);
        let at = p.rect("auto-chip").center();
        let press = |pos: Pos2, pressed: bool| Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let finger = |phase| Event::Touch {
            device_id: egui::TouchDeviceId(0),
            id: egui::TouchId(0),
            phase,
            pos: at,
            force: None,
        };
        let mut down = vec![Event::PointerMoved(at), press(at, true)];
        if touch {
            down.insert(0, finger(egui::TouchPhase::Start));
        }
        pass_at(&ctx, &mut app, 390.0, 0.1, down);
        pass_at(
            &ctx,
            &mut app,
            390.0,
            0.2,
            vec![press(at, false), Event::PointerGone],
        );
        let p = pass_at(&ctx, &mut app, 390.0, 0.3, Vec::new());
        assert_eq!(reason(&p), touch, "touch {touch}: shown after the tap");
        let p = pass_at(&ctx, &mut app, 390.0, 5.0, Vec::new());
        assert_eq!(reason(&p), touch, "touch {touch}: stays");
        let away = pos2(200.0, 600.0);
        pass_at(
            &ctx,
            &mut app,
            390.0,
            5.1,
            vec![Event::PointerMoved(away), press(away, true)],
        );
        pass_at(
            &ctx,
            &mut app,
            390.0,
            5.2,
            vec![press(away, false), Event::PointerGone],
        );
        let p = pass_at(&ctx, &mut app, 390.0, 5.3, Vec::new());
        assert!(!reason(&p), "touch {touch}: hidden by the next tap");
    }
}

/// Auto-Chip: a titled dropdown of 1, 3 and 5 Yard; a pick sends the
/// chip as before.
#[test]
fn the_auto_chip_dropdown_is_titled_and_lists_the_distances() {
    let mut app = bar_app(0b0100);
    app.actors.insert(
        "openconnect_client.0".into(),
        ActorStatusResponse::new("GSPro", ActorStatus::Connected),
    );
    let p = open(&mut app, HeaderMenu::AutoChip, 1600.0);
    assert!(p.has_text("AUTO-CHIP"));
    let ys: Vec<f32> = ["1 Yard", "3 Yard", "5 Yard"]
        .iter()
        .map(|l| p.rect(&format!("auto-chip:{l}")).top())
        .collect();
    assert!(ys.windows(2).all(|w| w[0] < w[1]), "{ys:?}");
    assert!(p.rect("auto-chip:1 Yard").top() > p.rect("auto-chip").bottom());
    // It is blocked while a chip is on its way: no dropdown.
    app.auto_chip_in_flight = Some(3);
    let p = open(&mut app, HeaderMenu::AutoChip, 1600.0);
    assert!(!p.enabled("auto-chip"));
    assert!(!p.marks.contains_key("auto-chip:1 Yard"));
}

/// Shot Type's dropdown: a title, the (i) tip and Full, Chip and Putt
/// with the current one first-class; a pick sets the shot type.
#[test]
fn the_shot_type_tip_opens_left_of_the_dropdown() {
    for width in [1600.0, 1024.0, 400.0] {
        let mut app = bar_app(0b1000);
        let ctx = egui::Context::default();
        theme::install(&ctx);
        pass_at(&ctx, &mut app, width, 0.0, Vec::new());
        app.header_menu = Some(HeaderMenu::ShotType);
        pass_at(&ctx, &mut app, width, 0.0, Vec::new());
        let p = pass_at(&ctx, &mut app, width, 0.0, Vec::new());
        assert!(!p.marks.contains_key("shot-type:tip-text"), "only on hover");
        let at = p.rect("shot-type:tip").center();
        pass_at(&ctx, &mut app, width, 0.1, vec![Event::PointerMoved(at)]);
        let p = pass_at(&ctx, &mut app, width, 0.2, Vec::new());
        let tip = p.rect("shot-type:tip-text");
        let title = p.rect("shot-type:tip");
        assert!(!tip.intersects(title), "{width}: covers the title");
        for label in ["Full", "Chip", "Putt"] {
            let row = p.rect(&format!("shot-type:{label}"));
            if width > 1000.0 {
                assert!(tip.right() < row.left(), "{width}: left of {label}");
            } else {
                assert!(tip.top() > row.bottom(), "{width}: under {label}");
            }
            assert!(!tip.intersects(row), "{width}: covers {label}");
        }
        assert!(
            tip.left() >= 0.0 && tip.right() <= width,
            "{width}: on screen: {tip:?}"
        );
        assert!(tip.height() < 70.0, "{width}: two lines: {tip:?}");
    }
}

/// In the portrait layout the dropdowns' rows and club chips are 44 px
/// tall, the Golfer card takes the screen's width less the margins, and
/// the Shot Type tip opens under the (i).
#[test]
fn portrait_dropdowns_are_touch_sized_and_on_screen() {
    let width = 390.0;
    let mut app = golfers_app();
    app.portrait = true;
    let p = open(&mut app, HeaderMenu::Club, width);
    for club in ["club_DR", "club_7I", "club_PW"] {
        assert_eq!(p.rect(club).height(), TOUCH_TARGET, "{club}");
    }
    app.header_menu = None;
    let p = open(&mut app, HeaderMenu::Golfer, width);
    for id in ["flighthook.e", "flighthook.s"] {
        let row = p.rect(&format!("golfer_{id}"));
        assert_eq!(row.height(), TOUCH_TARGET, "{id}");
        assert!(row.left() >= SIDE_MARGIN && row.right() <= width - SIDE_MARGIN);
        assert!(row.width() > 300.0, "{id}: the card's full width: {row:?}");
    }

    let mut app = bar_app(0b1000);
    app.portrait = true;
    let ctx = egui::Context::default();
    theme::install(&ctx);
    pass_at(&ctx, &mut app, width, 0.0, Vec::new());
    app.header_menu = Some(HeaderMenu::ShotType);
    pass_at(&ctx, &mut app, width, 0.0, Vec::new());
    let p = pass_at(&ctx, &mut app, width, 0.0, Vec::new());
    for label in ["Full", "Chip", "Putt"] {
        let row = p.rect(&format!("shot-type:{label}"));
        assert_eq!(row.height(), TOUCH_TARGET, "{label}");
    }
    let info = p.rect("shot-type:tip");
    pass_at(
        &ctx,
        &mut app,
        width,
        0.1,
        vec![Event::PointerMoved(info.center())],
    );
    let p = pass_at(&ctx, &mut app, width, 0.2, Vec::new());
    let tip = p.rect("shot-type:tip-text");
    assert!(tip.top() > info.bottom(), "under the (i): {tip:?} {info:?}");
    assert!(
        tip.left() >= 0.0 && tip.right() <= width,
        "on screen: {tip:?}"
    );
}

/// On a touch screen a tap on the Shot Type (i) shows its tip until the
/// next press; the dropdown stays open.
#[test]
fn a_tap_on_the_shot_type_info_shows_its_tip() {
    let width = 390.0;
    let mut app = bar_app(0b1000);
    app.portrait = true;
    let ctx = egui::Context::default();
    theme::install(&ctx);
    pass_at(&ctx, &mut app, width, 0.0, Vec::new());
    app.header_menu = Some(HeaderMenu::ShotType);
    pass_at(&ctx, &mut app, width, 0.0, Vec::new());
    let p = pass_at(&ctx, &mut app, width, 0.0, Vec::new());
    let at = p.rect("shot-type:tip").center();
    let press = |pos: Pos2, pressed: bool| Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    let finger = Event::Touch {
        device_id: egui::TouchDeviceId(0),
        id: egui::TouchId(0),
        phase: egui::TouchPhase::Start,
        pos: at,
        force: None,
    };
    pass_at(
        &ctx,
        &mut app,
        width,
        0.1,
        vec![finger, Event::PointerMoved(at), press(at, true)],
    );
    pass_at(
        &ctx,
        &mut app,
        width,
        0.2,
        vec![press(at, false), Event::PointerGone],
    );
    let p = pass_at(&ctx, &mut app, width, 0.3, Vec::new());
    assert!(
        p.marks.contains_key("shot-type:tip-text"),
        "shown after the tap"
    );
    assert_eq!(app.header_menu, Some(HeaderMenu::ShotType));
    let p = pass_at(&ctx, &mut app, width, 5.0, Vec::new());
    assert!(p.marks.contains_key("shot-type:tip-text"), "stays");
    let row = p.rect("shot-type:Full").center();
    pass_at(
        &ctx,
        &mut app,
        width,
        5.1,
        vec![Event::PointerMoved(row), press(row, true)],
    );
    let p = pass_at(&ctx, &mut app, width, 5.2, Vec::new());
    assert!(
        !p.marks.contains_key("shot-type:tip-text"),
        "hidden by the next press"
    );
}

#[test]
fn the_shot_type_dropdown_lists_full_chip_and_putt() {
    let mut app = bar_app(0b1000);
    app.current_mode = "chipping".into();
    let p = open(&mut app, HeaderMenu::ShotType, 1600.0);
    assert!(p.has_text("SHOT TYPE"));
    assert!(p.marks.contains_key("shot-type:tip"));
    let ys: Vec<f32> = ["Full", "Chip", "Putt"]
        .iter()
        .map(|l| p.rect(&format!("shot-type:{l}")).top())
        .collect();
    assert!(ys.windows(2).all(|w| w[0] < w[1]), "{ys:?}");
    // The button shows the current type's icon, lit while open.
    assert_eq!(p.icon_in("shot-type").0, "shot-type-chip-hover@1x");
    // A pick closes the dropdown and changes the type.
    let ctx = egui::Context::default();
    theme::install(&ctx);
    pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new());
    app.header_menu = Some(HeaderMenu::ShotType);
    pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new());
    let at = pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new())
        .rect("shot-type:Putt")
        .center();
    click(&ctx, &mut app, at);
    assert_eq!(app.current_mode, "putting");
    assert_eq!(app.header_menu, None);
}

/// A click at `at`: move, press, release.
fn click(ctx: &egui::Context, app: &mut FlighthookApp, at: Pos2) {
    click_at(ctx, app, 1600.0, at);
}

/// A click at `at` on a window `width` wide.
fn click_at(ctx: &egui::Context, app: &mut FlighthookApp, width: f32, at: Pos2) {
    let button = |pressed| Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    pass_at(ctx, app, width, 0.0, vec![Event::PointerMoved(at)]);
    pass_at(ctx, app, width, 0.0, vec![button(true)]);
    pass_at(ctx, app, width, 0.0, vec![button(false)]);
    pass_at(ctx, app, width, 0.0, Vec::new());
}

fn golfers_app() -> FlighthookApp {
    let mut app = bar_app(0b0010);
    let mut config = app.settings.saved_config_ref().cloned().unwrap();
    let mut eric = flighthook::GolferRecord::new("flighthook.e", "Eric");
    eric.bag = vec![Club::Driver, Club::PitchingWedge];
    config.golfers = vec![eric, flighthook::GolferRecord::new("flighthook.s", "Sam")];
    app.settings.load_from(&config);
    app.selected_club = Some(Club::Iron7);
    app
}

/// The Club dropdown: a title and the club pills, the golfer up's bag
/// with the club in play always shown; the button shows the code.
#[test]
fn the_club_dropdown_is_titled_and_follows_the_bag() {
    let mut app = golfers_app();
    let p = pass(&mut app, 1600.0);
    assert!(p.has_text("7I"), "the code in the slot");
    let p = open(&mut app, HeaderMenu::Club, 1600.0);
    assert!(p.has_text("CLUB"));
    for club in ["club_DR", "club_7I", "club_PW"] {
        assert!(p.marks.contains_key(club), "{club}");
    }
    assert!(!p.marks.contains_key("club_SW"), "not in the bag");
    // No club: the iron with a dash, no code.
    app.selected_club = None;
    app.header_menu = None;
    let p = pass(&mut app, 1600.0);
    assert_eq!(p.icon_in("club").0, "club-none-normal@1x");
    assert!(!p.has_text("7I"));
}

/// The Golfer card: the golfer's name and initial, every golfer, a
/// link to Settings; no hand control.
#[test]
fn the_golfer_card_holds_the_name_and_the_list() {
    let mut app = golfers_app();
    let p = pass(&mut app, 1600.0);
    assert!(p.has_text("E"), "the initial: {:?}", p.texts);
    let p = open(&mut app, HeaderMenu::Golfer, 1600.0);
    assert!(p.has_text("Eric") && p.has_text("Sam"), "{:?}", p.texts);
    for text in ["Right-handed", "Left-handed"] {
        assert!(!p.has_text(text), "{text}: {:?}", p.texts);
    }
    assert!(!p.marks.contains_key("hand"));
    assert!(!p.marks.contains_key("golfer:hand-caption"));
    assert!(p.marks.contains_key("golfer_flighthook.e"));
    assert!(p.marks.contains_key("golfer_flighthook.s"));
    assert!(p.marks.contains_key("golfer:settings"));
    // The link opens Settings and closes the card.
    let ctx = egui::Context::default();
    theme::install(&ctx);
    pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new());
    app.header_menu = Some(HeaderMenu::Golfer);
    pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new());
    let at = pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new())
        .rect("golfer:settings")
        .center();
    app.active_tab = Tab::Shots;
    click(&ctx, &mut app, at);
    assert_eq!(app.active_tab, Tab::Devices);
    assert_eq!(
        app.settings_section,
        crate::panels::settings_nav::SettingsSection::Golfers,
        "Settings opens on Golfers"
    );
    assert_eq!(app.header_menu, None);
    // With no golfer: "No golfer" and "No golfers".
    let mut none = bar_app(0);
    let p = open(&mut none, HeaderMenu::Golfer, 1600.0);
    assert!(
        p.has_text("No golfer") && p.has_text("No golfers"),
        "{:?}",
        p.texts
    );
}

/// A long golfer name wraps inside the Golfer card, between the avatar
/// and the gear, still centered on the avatar; never out of the card
/// or the window.
#[test]
fn a_long_golfer_name_wraps_inside_the_card() {
    const LONG: &str = "Christopher Montgomery-Wellington III";
    for width in [1600.0, 400.0] {
        let mut app = golfers_app();
        let mut config = app.settings.saved_config_ref().cloned().unwrap();
        config.golfers[0].name = LONG.to_owned();
        app.settings.load_from(&config);
        let p = open(&mut app, HeaderMenu::Golfer, width);
        assert!(
            p.texts.iter().any(|t| t.0.contains("Christopher")),
            "{width}: the name"
        );
        let avatar = p.rect("golfer:avatar");
        let name = p.rect("golfer:name");
        let gear = p.rect("golfer:settings");
        let row = p.rect("golfer_flighthook.e");
        assert!(name.left() > avatar.right(), "{width}: right of the avatar");
        assert!(
            name.right() <= gear.left() && name.right() <= row.right(),
            "{width}: inside the card, clear of the gear: {name:?} {gear:?} {row:?}"
        );
        assert!(name.right() <= width, "{width}: inside the window");
        assert!(
            name.height() > 30.0,
            "{width}: wraps to a second line: {name:?}"
        );
        assert!(
            (name.center().y - avatar.center().y).abs() <= 1.0,
            "{width}: centered on the avatar"
        );
        assert!(
            row.top() > name.bottom(),
            "{width}: the golfers below the name"
        );
    }
}

/// The Golfer card's layout: the name centered on the avatar, the gear
/// in the top right corner and a hairline between the avatar and the
/// golfers.
#[test]
fn the_golfer_card_is_laid_out_as_drawn() {
    for width in [1600.0, 1024.0] {
        let mut app = golfers_app();
        let p = open(&mut app, HeaderMenu::Golfer, width);
        let avatar = p.rect("golfer:avatar");
        let name = p.rect("golfer:name");
        assert!(
            (name.center().y - avatar.center().y).abs() <= 1.0,
            "{width}: name centered on the avatar"
        );
        let gear = p.rect("golfer:settings");
        let row = p.rect("golfer_flighthook.e");
        assert!(gear.top() <= avatar.top() + 0.5, "{width}: gear at the top");
        assert!(
            (gear.right() - row.right()).abs() <= 1.0,
            "{width}: gear at the right"
        );
        assert!(
            p.rules
                .iter()
                .any(|y| *y > avatar.bottom() && *y < row.top()),
            "{width}: a hairline before GOLFERS: {:?}",
            p.rules
        );
    }
}

/// The row of the golfer up carries the accent bar, also when the
/// turn names a game id linked to a saved golfer.
#[test]
fn the_golfer_up_is_highlighted_also_by_a_linked_game_id() {
    let mut app = golfers_app();
    let mut config = app.settings.saved_config_ref().cloned().unwrap();
    config.golfers[0]
        .identifiers
        .push("pureplay.local-3".into());
    app.settings.load_from(&config);
    for up in ["flighthook.e", "pureplay.local-3"] {
        app.turn_golfer = Some(up.into());
        let p = open(&mut app, HeaderMenu::Golfer, 1600.0);
        assert!(p.has_accent_in("golfer_flighthook.e"), "{up} up");
        assert!(!p.has_accent_in("golfer_flighthook.s"), "{up} up");
        assert!(
            !p.marks.contains_key("golfer_pureplay.local-3"),
            "the linked id has no row of its own"
        );
    }
}

/// A game golfer's row names its game by the id's namespace.
#[test]
fn a_game_golfers_row_names_its_game() {
    let mut app = golfers_app();
    app.turn_golfer = Some("othergame.player-9".into());
    let p = open(&mut app, HeaderMenu::Golfer, 1600.0);
    assert!(p.marks.contains_key("golfer_othergame.player-9"));
    assert!(p.has_text("othergame"), "{:?}", p.texts);
    assert!(!p.has_text("Pureplay"), "{:?}", p.texts);
    assert!(p.has_accent_in("golfer_othergame.player-9"));
    app.turn_golfer = Some("pureplay.player-7".into());
    let p = open(&mut app, HeaderMenu::Golfer, 1600.0);
    assert!(p.has_text("Pureplay"), "{:?}", p.texts);
}

/// Follow turns is on whenever fewer than two golfers are known, so it
/// is on when it shows again.
#[test]
fn follow_turns_is_on_when_it_shows_again() {
    let mut app = golfers_app();
    let (ctx, p) = settled(&mut app, 1600.0);
    click(&ctx, &mut app, p.rect("follow").center());
    assert!(!app.follow_turns);
    let mut config = app.settings.saved_config_ref().cloned().unwrap();
    let sam = config.golfers.pop().unwrap();
    app.settings.load_from(&config);
    assert!(!pass(&mut app, 1600.0).marks.contains_key("follow"));
    assert!(app.follow_turns, "on while hidden");
    config.golfers.push(sam);
    app.settings.load_from(&config);
    assert!(pass(&mut app, 1600.0).marks.contains_key("follow"));
    assert!(app.follow_turns, "on when it shows again");
}

/// Follow turns: shown with two or more known golfers (a game's count),
/// on by default; a click flips the session state.
#[test]
fn follow_turns_shows_with_two_golfers_and_flips_on_a_click() {
    let mut one = bar_app(0);
    assert!(!pass(&mut one, 1600.0).marks.contains_key("follow"));
    // One saved golfer and one a game names: two.
    let config = FlighthookConfig {
        golfers: vec![flighthook::GolferRecord::new("flighthook.e", "Eric")],
        ..Default::default()
    };
    one.settings.load_from(&config);
    assert!(!pass(&mut one, 1600.0).marks.contains_key("follow"));
    let game = flighthook::GolferProfile::new("pureplay.player-7".parse().unwrap(), "Guest");
    let msg = flighthook::BusMessage::new("pp", game);
    one.bus_states.insert(msg.state_key().unwrap(), msg);
    assert!(pass(&mut one, 1600.0).marks.contains_key("follow"));
    assert!(one.follow_turns, "on by default");

    let mut app = golfers_app();
    let (ctx, p) = settled(&mut app, 1600.0);
    let at = p.rect("follow").center();
    click(&ctx, &mut app, at);
    assert!(!app.follow_turns);
    click(&ctx, &mut app, at);
    assert!(app.follow_turns);
}

/// Whether Shot Type shows, per card type: one test per card type.
macro_rules! shot_type_row {
    ($name:ident, $shown:expr, |$c:ident| $add:expr) => {
        #[test]
        fn $name() {
            let mut app = FlighthookApp::with_pending(crate::net::new_pending());
            let mut $c = FlighthookConfig::default();
            $add;
            app.settings.load_from(&$c);
            assert_eq!(
                pass(&mut app, 1600.0).marks.contains_key("shot-type"),
                $shown
            );
            // Turned off, the card steers nothing.
            let mut off = $c.clone();
            for s in off.mevo.values_mut() {
                s.enabled = false;
            }
            for s in off.trackball.values_mut() {
                s.enabled = false;
            }
            for s in off.mock_monitor.values_mut() {
                s.enabled = false;
            }
            app.settings.load_from(&off);
            assert!(!pass(&mut app, 1600.0).marks.contains_key("shot-type"));
        }
    };
}

shot_type_row!(a_mevo_card_shows_shot_type, true, |c| c
    .mevo
    .insert("0".into(), flighthook::MevoSection::default())
    .map(drop)
    .unwrap_or_default());
shot_type_row!(an_r10_card_hides_shot_type, false, |c| c
    .r10
    .insert("0".into(), flighthook::R10Section::default())
    .map(drop)
    .unwrap_or_default());
shot_type_row!(an_omni_card_hides_shot_type, false, |c| c
    .square
    .insert("0".into(), flighthook::SquareSection::default())
    .map(drop)
    .unwrap_or_default());
shot_type_row!(an_exputt_card_hides_shot_type, false, |c| c
    .exputt
    .insert("0".into(), flighthook::ExPuttSection::default())
    .map(drop)
    .unwrap_or_default());
shot_type_row!(a_trackball_card_shows_shot_type, true, |c| c
    .trackball
    .insert("0".into(), flighthook::TrackballSection::default())
    .map(drop)
    .unwrap_or_default());
shot_type_row!(a_mock_monitor_shows_shot_type, true, |c| c
    .mock_monitor
    .insert("0".into(), flighthook::MockMonitorSection::default())
    .map(drop)
    .unwrap_or_default());
shot_type_row!(an_open_connect_server_hides_shot_type, false, |c| c
    .openconnect_server
    .insert("0".into(), flighthook::OpenConnectServerSection::default())
    .map(drop)
    .unwrap_or_default());
shot_type_row!(a_pureplay_accessory_hides_shot_type, false, |c| c
    .pureplay_accessory
    .insert("0".into(), flighthook::PureplayAccessorySection::default())
    .map(drop)
    .unwrap_or_default());

/// A remote launch monitor is not counted: the bar cannot know.
#[test]
fn a_remote_launch_monitor_does_not_show_shot_type() {
    let mut app = bar_app(0);
    let msg = flighthook::BusMessage::new(
        "remote.lm",
        flighthook::BusActor::new("Remote", 5.0).with_role(flighthook::roles::LAUNCH_MONITOR),
    );
    app.bus_states.insert(msg.state_key().unwrap(), msg);
    assert!(!pass(&mut app, 1600.0).marks.contains_key("shot-type"));
}

/// Per-mode routing counts: Chip, Putt, Full alone and "Any".
fn routed(edit: fn(&mut flighthook::OpenConnectClientSection)) -> bool {
    let mut app = FlighthookApp::with_pending(crate::net::new_pending());
    let mut config = FlighthookConfig::default();
    let mut client = flighthook::OpenConnectClientSection::default();
    edit(&mut client);
    config.openconnect_client.insert("0".into(), client);
    app.settings.load_from(&config);
    pass(&mut app, 1600.0).marks.contains_key("shot-type")
}

#[test]
fn routing_chips_to_a_monitor_shows_shot_type() {
    assert!(routed(|c| c.chipping_monitor = Some("r10.0".into())));
}

#[test]
fn routing_putts_to_a_monitor_shows_shot_type() {
    assert!(routed(|c| c.putting_monitor = Some("exputt.0".into())));
}

#[test]
fn routing_only_full_swings_to_a_monitor_hides_shot_type() {
    assert!(!routed(|c| c.full_monitor = Some("r10.0".into())));
}

#[test]
fn routing_to_any_monitor_hides_shot_type() {
    assert!(!routed(|_| {}));
}

/// The energy icon: not drawn without a charge; the lowest charge sets
/// its color (normal from 20 %, yellow from 10 to 19, red below 10).
#[test]
fn the_energy_icon_needs_a_charge_and_the_lowest_one_colors_it() {
    let mut none = bar_app(0);
    none.actors.insert(
        "r10.0".into(),
        ActorStatusResponse::new("R10", ActorStatus::Connected),
    );
    assert!(!pass(&mut none, 1600.0).marks.contains_key("energy"));
    let outline = "energy-normal@1x".to_owned();
    let filled = "energy-filled-normal@1x".to_owned();
    for (pct, icon) in [
        (74u8, (outline.clone(), theme::TEXT_MUTED)),
        (25, (outline.clone(), theme::TEXT_MUTED)),
        (20, (outline.clone(), theme::TEXT_MUTED)),
        (19, (filled.clone(), theme::GOLD)),
        (15, (filled.clone(), theme::GOLD)),
        (10, (filled.clone(), theme::GOLD)),
        (9, (filled.clone(), theme::DANGER)),
        (5, (filled.clone(), theme::DANGER)),
    ] {
        let mut app = bar_app(0);
        charge(&mut app, "square.0", "Square Omni", pct);
        let p = pass(&mut app, 1600.0);
        assert_eq!(p.icon_in("energy"), icon, "{pct}%");
    }
    // The lowest of two decides.
    let mut app = bar_app(0);
    charge(&mut app, "square.0", "Square Omni", 74);
    charge(&mut app, "mevo.0", "Mevo+", 8);
    assert_eq!(
        pass(&mut app, 1600.0).icon_in("energy"),
        (filled, theme::DANGER)
    );
}

/// The bar after the pointer rests on the mark `name`.
fn hovering(app: &mut FlighthookApp, name: &str, ppp: f32) -> Pass {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    ctx.set_pixels_per_point(ppp);
    pass_at(&ctx, app, 1600.0, 0.0, Vec::new());
    let at = pass_at(&ctx, app, 1600.0, 0.0, Vec::new())
        .rect(name)
        .center();
    pass_at(&ctx, app, 1600.0, 0.1, vec![Event::PointerMoved(at)]);
    pass_at(&ctx, app, 1600.0, 0.2, Vec::new())
}

/// Every button draws its own icon: the normal file tinted (muted at
/// rest, dim while disabled), the hover file untinted while hovered,
/// selected or a toggle that is on.
#[test]
fn each_button_draws_its_icon_for_its_state() {
    let normal = |f: &str| (format!("{f}-normal@1x"), theme::TEXT_MUTED);
    let lit = |f: &str| (format!("{f}-hover@1x"), Color32::WHITE);
    let mut app = bar_app(0b1110);
    let p = pass(&mut app, 1600.0);
    // The nav: Shots selected, Cameras disabled (none configured).
    assert_eq!(p.icon_in("nav:shots"), lit("nav-shots"));
    assert_eq!(
        p.icon_in("nav:cameras"),
        ("nav-cameras-normal@1x".to_owned(), theme::TEXT_DIM)
    );
    assert_eq!(p.icon_in("nav:settings"), normal("nav-settings"));
    assert_eq!(p.icon_in("nav:log"), normal("nav-log"));
    // Follow is on; Auto-Chip is off (no simulator) and disabled.
    assert_eq!(p.icon_in("follow"), lit("follow-on"));
    assert_eq!(
        p.icon_in("auto-chip"),
        ("auto-chip-off-normal@1x".to_owned(), theme::TEXT_DIM)
    );
    assert_eq!(p.icon_in("shot-type"), normal("shot-type-full"));
    assert_eq!(p.icon_in("club"), normal("club-none"));
    assert_eq!(p.icon_in("golfer"), normal("golfer"));
    // Auto-Chip on, Follow off, nobody up.
    app.actors.insert(
        "openconnect_client.0".into(),
        ActorStatusResponse::new("GSPro", ActorStatus::Connected),
    );
    app.follow_turns = false;
    app.turn_golfer = None;
    app.viewed_golfer = None;
    let p = pass(&mut app, 1600.0);
    // Auto-Chip is an action: muted at rest even when usable.
    assert_eq!(p.icon_in("auto-chip"), normal("auto-chip-on"));
    assert_eq!(p.icon_in("follow"), normal("follow-off"));
    assert_eq!(p.icon_in("golfer"), normal("golfer-none"));
    // Hovered, each takes its hover file.
    for (name, file) in [
        ("nav:log", "nav-log"),
        ("follow", "follow-off"),
        ("shot-type", "shot-type-full"),
        ("club", "club-none"),
        ("golfer", "golfer-none"),
    ] {
        assert_eq!(
            hovering(&mut app, name, 1.0).icon_in(name),
            lit(file),
            "{name}"
        );
    }
    // Each shot type has its icon.
    for (mode, file) in [
        ("chipping", "shot-type-chip"),
        ("putting", "shot-type-putt"),
    ] {
        app.current_mode = mode.into();
        assert_eq!(pass(&mut app, 1600.0).icon_in("shot-type"), normal(file));
    }
}

/// The outline bolt lights up on hover; the filled bolt keeps its
/// warning color in both states.
#[test]
fn the_filled_energy_icon_keeps_its_tint_on_hover() {
    let mut app = bar_app(0);
    charge(&mut app, "square.0", "Square Omni", 74);
    assert_eq!(
        hovering(&mut app, "energy", 1.0).icon_in("energy"),
        ("energy-hover@1x".to_owned(), Color32::WHITE)
    );
    charge(&mut app, "square.0", "Square Omni", 5);
    assert_eq!(
        hovering(&mut app, "energy", 1.0).icon_in("energy"),
        ("energy-filled-hover@1x".to_owned(), theme::DANGER)
    );
}

/// Each club picker row has its head; no club is the dashed iron.
#[test]
fn each_club_takes_the_head_of_its_picker_row() {
    for (club, head) in [
        (Some(Club::Driver), "club-driver"),
        (Some(Club::MiniDriver), "club-driver"),
        (Some(Club::Wood3), "club-wood"),
        (Some(Club::Hybrid4), "club-hybrid"),
        (Some(Club::Iron7), "club-iron"),
        (Some(Club::PitchingWedge), "club-wedge"),
        (Some(Club::LobWedge), "club-wedge"),
        (Some(Club::Putter), "club-putter"),
        (None, "club-none"),
    ] {
        assert_eq!(club_icon(club).name, head, "{club:?}");
    }
}

/// The club code and the golfer's initial are drawn inside their
/// icons, in the tint at rest and in the background color on hover.
#[test]
fn the_club_code_and_the_initial_are_drawn_inside_their_icons() {
    let mut app = bar_app(0b0010);
    app.selected_club = Some(Club::Iron7);
    let p = pass(&mut app, 1600.0);
    let (_, head) = p.image("club-iron-normal@1x");
    assert_eq!(head.size(), vec2(32.0, 32.0));
    assert!(head.contains_rect(p.text_rect("7I")), "{head:?}");
    assert_eq!(p.color_of("7I"), theme::TEXT_MUTED);
    let (_, ring) = p.image("golfer-normal@1x");
    assert_eq!(ring.size(), vec2(28.0, 28.0));
    assert!(ring.contains_rect(p.text_rect("E")), "{ring:?}");
    assert_eq!(p.color_of("E"), theme::TEXT_MUTED);
    let p = hovering(&mut app, "club", 1.0);
    assert!(
        p.image("club-iron-hover@1x")
            .1
            .contains_rect(p.text_rect("7I"))
    );
    assert_eq!(p.color_of("7I"), theme::BG);
    let p = hovering(&mut app, "golfer", 1.0);
    assert_eq!(p.color_of("E"), theme::BG);
    // A wide code shrinks to the head's label width.
    app.selected_club = Some(Club::Wood3);
    let p = pass(&mut app, 1600.0);
    assert!(
        p.text_rect("3W").width() <= 15.2 + 0.5,
        "{:?}",
        p.text_rect("3W")
    );
}

/// At UI zoom 2 the bar draws the 2x files, at 4 the 3x files.
#[test]
fn the_icons_follow_the_zoom() {
    let mut app = bar_app(0);
    for (ppp, file) in [(2.0, "nav-log-hover@2x"), (4.0, "nav-log-hover@3x")] {
        assert_eq!(
            hovering(&mut app, "nav:log", ppp).icon_in("nav:log").0,
            file
        );
    }
}

/// The Batteries dropdown: a title, one row per device in card order
/// with its name and percent, colored by the same thresholds.
#[test]
fn the_batteries_dropdown_lists_the_devices_in_card_order() {
    let mut app = bar_app(0);
    charge(&mut app, "square.0", "Square Omni", 74);
    charge(&mut app, "mevo.0", "Mevo+", 18);
    charge(&mut app, "r10.0", "R10", 7);
    let p = open(&mut app, HeaderMenu::Energy, 1600.0);
    assert!(p.has_text("BATTERIES"));
    let order: Vec<&str> = ["mevo.0", "r10.0", "square.0"].into();
    let ys: Vec<f32> = order
        .iter()
        .map(|id| p.rect(&format!("battery:{id}")).top())
        .collect();
    assert!(ys.windows(2).all(|w| w[0] < w[1]), "card order: {ys:?}");
    for (text, color) in [
        ("18%", theme::STATUS_PENDING),
        ("7%", theme::STATUS_DOWN),
        ("74%", theme::STATUS_OK),
    ] {
        assert_eq!(p.color_of(text), color, "{text}");
    }
    for name in ["Mevo+", "R10", "Square Omni"] {
        assert!(p.has_text(name), "{name}");
    }
}

/// Settings with unsaved edits lock the nav, with the reason as the
/// hover text; the toggle's click still works unlocked.
#[test]
fn unsaved_settings_edits_lock_the_nav() {
    let mut app = bar_app(0b1000);
    let p = pass(&mut app, 1600.0);
    for n in ["shots", "cameras", "settings", "log"] {
        let enabled = p.enabled(&format!("nav:{n}"));
        // Cameras is off anyway while no camera is configured.
        assert_eq!(enabled, n != "cameras", "{n}");
    }
    app.settings.actors[0].set_dirty(true);
    assert!(app.settings.locked());
    let p = pass(&mut app, 1600.0);
    for n in ["shots", "cameras", "settings", "log"] {
        assert!(!p.enabled(&format!("nav:{n}")), "{n} locked");
    }
    // The right group still answers.
    assert!(p.enabled("club"));
}

/// The red dot sits on the Settings icon while a card is red.
#[test]
fn the_alert_dot_rides_the_settings_icon() {
    let mut app = bar_app(0);
    assert!(!pass(&mut app, 1600.0).marks.contains_key("alert_dot"));
    let mut actor = ActorStatusResponse::new("Range Bridge", ActorStatus::Connected);
    actor.observe(ActorStatus::Connected, 1_000);
    actor.observe(ActorStatus::Disconnected, 1_000);
    app.actors.insert("openconnect_server.0".into(), actor);
    app.now_ms = 1_001;
    let p = pass(&mut app, 1600.0);
    let settings = p.rect("nav:settings");
    assert!(settings.contains_rect(p.rect("alert_dot")));
    assert!(
        p.rect("alert_dot").center().x > settings.center().x,
        "top right"
    );
}

/// The nav icons switch what the content shows: Shots and Cameras the
/// Shots tab's pane, Settings the device, output and global cards.
#[test]
fn the_nav_icons_switch_the_content() {
    let mut app = bar_app(0);
    assert_eq!(app.active_tab, Tab::Shots, "the app opens on Shots");
    let (ctx, p) = settled(&mut app, 1600.0);
    click(&ctx, &mut app, p.rect("nav:settings").center());
    assert_eq!(app.active_tab, Tab::Devices);
    click(&ctx, &mut app, p.rect("nav:log").center());
    assert_eq!(app.active_tab, Tab::Log);
    click(&ctx, &mut app, p.rect("nav:shots").center());
    assert_eq!(app.active_tab, Tab::Shots);
    assert_eq!(app.shots_pane, ShotsPane::Data);
    // Cameras is disabled while none is configured, with the reason.
    assert!(!p.enabled("nav:cameras"));
}

/// The texts drawn while the pointer rests on the mark `name`.
fn hover_texts(app: &mut FlighthookApp, name: &str) -> Vec<String> {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    pass_at(&ctx, app, 1600.0, 0.0, Vec::new());
    let at = pass_at(&ctx, app, 1600.0, 0.0, Vec::new())
        .rect(name)
        .center();
    pass_at(&ctx, app, 1600.0, 0.1, vec![Event::PointerMoved(at)]);
    pass_at(&ctx, app, 1600.0, 2.0, Vec::new());
    pass_at(&ctx, app, 1600.0, 2.1, Vec::new())
        .texts
        .into_iter()
        .map(|(t, _)| t)
        .collect()
}

/// Every button of the right group, and Cameras with none configured,
/// shows its hover text.
#[test]
fn every_right_group_button_has_its_hover_text() {
    let mut app = golfers_app();
    charge(&mut app, "square.0", "Square Omni", 74);
    app.actors.insert(
        "openconnect_client.0".into(),
        ActorStatusResponse::new("GSPro", ActorStatus::Connected),
    );
    let mut config = app.settings.saved_config_ref().cloned().unwrap();
    config.simulation.auto_chip = true;
    config
        .mevo
        .insert("0".into(), flighthook::MevoSection::default());
    app.settings.load_from(&config);
    app.turn_golfer = Some("flighthook.e".into());
    app.current_mode = "full".into();
    let cases: [(&str, &str); 8] = [
        ("energy", "Square Omni 74%"),
        (
            "follow",
            "Following turns: on. The view moves to the golfer up.",
        ),
        ("auto-chip", AUTO_CHIP_HINT),
        ("shot-type", "Shot Type: Full"),
        ("club", "Club: 7 Iron"),
        ("golfer", "Golfer: Eric"),
        ("nav:cameras", NO_CAMERAS),
        ("nav:log", "Log"),
    ];
    for (name, want) in cases {
        let texts = hover_texts(&mut app, name);
        assert!(
            texts.iter().any(|t| t == want),
            "{name}: {want:?} not in {texts:?}"
        );
    }
}

/// A row of the Auto-Chip dropdown sends that chip and closes it.
#[test]
fn an_auto_chip_row_sends_the_chip() {
    let mut app = bar_app(0b0100);
    app.actors.insert(
        "openconnect_client.0".into(),
        ActorStatusResponse::new("GSPro", ActorStatus::Connected),
    );
    let ctx = egui::Context::default();
    theme::install(&ctx);
    pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new());
    app.header_menu = Some(HeaderMenu::AutoChip);
    pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new());
    let at = pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new())
        .rect("auto-chip:3 Yard")
        .center();
    click(&ctx, &mut app, at);
    assert_eq!(app.auto_chip_in_flight, Some(3));
    assert_eq!(app.header_menu, None);
}

/// With a camera configured, the Cameras icon opens the Shots tab on
/// its Cameras pane and takes the marker.
#[test]
fn the_cameras_icon_opens_the_cameras_pane() {
    let mut app = bar_app(0);
    let mut config = app.settings.saved_config_ref().cloned().unwrap();
    config.camera.insert(
        "0".into(),
        flighthook::CameraSection {
            camera: flighthook::CameraSection::TEST_PATTERN.into(),
            ..Default::default()
        },
    );
    app.settings.load_from(&config);
    app.cameras_supported = true;
    app.active_tab = Tab::Log;
    let (ctx, p) = settled(&mut app, 1600.0);
    assert!(p.enabled("nav:cameras"));
    click(&ctx, &mut app, p.rect("nav:cameras").center());
    assert_eq!(app.active_tab, Tab::Shots);
    assert_eq!(app.shots_pane, ShotsPane::Cameras);
    let p = pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new());
    let cameras = p.rect("nav:cameras");
    assert!(
        p.accents
            .iter()
            .any(|a| a.center().x == cameras.center().x && a.top() >= cameras.bottom()),
        "the marker under Cameras"
    );
}

/// Escape closes the open dropdown.
#[test]
fn escape_closes_an_open_dropdown() {
    let mut app = golfers_app();
    let (ctx, p) = settled(&mut app, 1600.0);
    click(&ctx, &mut app, p.rect("club").center());
    assert_eq!(app.header_menu, Some(HeaderMenu::Club));
    pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new());
    let key = |pressed| Event::Key {
        key: egui::Key::Escape,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    pass_at(&ctx, &mut app, 1600.0, 0.0, vec![key(true)]);
    pass_at(&ctx, &mut app, 1600.0, 0.0, vec![key(false)]);
    pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new());
    assert_eq!(app.header_menu, None);
}

/// One dropdown at a time: opening another closes the first.
#[test]
fn one_dropdown_is_open_at_a_time() {
    let mut app = golfers_app();
    let (ctx, p) = settled(&mut app, 1600.0);
    click(&ctx, &mut app, p.rect("club").center());
    assert_eq!(app.header_menu, Some(HeaderMenu::Club));
    let p = pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new());
    click(&ctx, &mut app, p.rect("golfer").center());
    assert_eq!(app.header_menu, Some(HeaderMenu::Golfer));
}

/// `golfers_app` with Eric up and Follow turns off, viewing `viewed`.
fn viewing(viewed: &str) -> FlighthookApp {
    let mut app = golfers_app();
    app.turn_golfer = Some("flighthook.e".into());
    app.follow_turns = false;
    app.refresh_shot_caches();
    app.set_view(Some(viewed.into()));
    app
}

/// Follow turns' three hover texts, and the Club, Shot Type and
/// Auto-Chip hover texts naming the golfer up while the view is on
/// someone else.
#[test]
fn hover_texts_follow_the_view() {
    let setup = |app: &mut FlighthookApp| {
        app.actors.insert(
            "openconnect_client.0".into(),
            ActorStatusResponse::new("GSPro", ActorStatus::Connected),
        );
        let mut config = app.settings.saved_config_ref().cloned().unwrap();
        config.simulation.auto_chip = true;
        config
            .mevo
            .insert("0".into(), flighthook::MevoSection::default());
        app.settings.load_from(&config);
        app.current_mode = "full".into();
    };
    let mut other = viewing("flighthook.s");
    setup(&mut other);
    let chip = format!("{AUTO_CHIP_HINT} (Eric, up)");
    let cases: [(&str, &str); 5] = [
        ("follow", "Following turns: off. Viewing Sam; Eric is up."),
        ("auto-chip", chip.as_str()),
        ("shot-type", "Shot Type: Full (Eric, up)"),
        ("club", "Club: 7 Iron (Eric, up)"),
        ("golfer", "Golfer: Sam"),
    ];
    for (name, want) in cases {
        let texts = hover_texts(&mut other, name);
        assert!(
            texts.iter().any(|t| t == want),
            "{name}: {want:?} not in {texts:?}"
        );
    }
    let mut up = viewing("flighthook.e");
    setup(&mut up);
    let cases: [(&str, &str); 5] = [
        (
            "follow",
            "Following turns: off. The view stays on Eric when the turn changes.",
        ),
        ("auto-chip", AUTO_CHIP_HINT),
        ("shot-type", "Shot Type: Full"),
        ("club", "Club: 7 Iron"),
        ("golfer", "Golfer: Eric"),
    ];
    for (name, want) in cases {
        let texts = hover_texts(&mut up, name);
        assert!(
            texts.iter().any(|t| t == want),
            "{name}: {want:?} not in {texts:?}"
        );
    }
}

/// The Golfer icon shows the golfer up's initial while Follow turns is
/// on and the viewed golfer's while it is off; the card marks the
/// viewed golfer's row and, while off, the golfer up's with the gold
/// dot.
#[test]
fn the_golfer_icon_and_card_follow_the_view() {
    let mut on = golfers_app();
    on.turn_golfer = Some("flighthook.e".into());
    on.refresh_shot_caches();
    let p = open(&mut on, HeaderMenu::Golfer, 1600.0);
    assert!(p.has_text("E"), "{:?}", p.texts);
    assert!(p.has_accent_in("golfer_flighthook.e"));
    assert!(
        !p.marks.keys().any(|k| k.starts_with("golfer:up_")),
        "no dot while on"
    );
    let mut off = viewing("flighthook.s");
    let p = open(&mut off, HeaderMenu::Golfer, 1600.0);
    assert!(p.has_text("S") && p.has_text("Sam"), "{:?}", p.texts);
    assert!(p.has_accent_in("golfer_flighthook.s"), "the viewed row");
    assert!(!p.has_accent_in("golfer_flighthook.e"));
    let dot = p.rect("golfer:up_flighthook.e");
    assert!(
        p.rect("golfer_flighthook.e").contains_rect(dot),
        "on Eric's row"
    );
    assert!(!p.marks.contains_key("golfer:up_flighthook.s"));
    // Off and viewing the golfer up: the dot stays on the golfer up.
    let mut same = viewing("flighthook.e");
    let p = open(&mut same, HeaderMenu::Golfer, 1600.0);
    assert!(p.marks.contains_key("golfer:up_flighthook.e"));
    assert!(p.has_text("E"));
}

/// The gear opens Settings on Golfers, but unsaved edits keep their
/// section on show; the card closes either way.
#[test]
fn the_golfer_gear_keeps_the_section_of_unsaved_edits() {
    use crate::panels::settings_nav::SettingsSection;
    let mut app = golfers_app();
    app.settings_section = SettingsSection::General;
    app.settings.global_dirty = true;
    assert!(app.settings.locked());
    let (ctx, _) = settled(&mut app, 1600.0);
    app.header_menu = Some(HeaderMenu::Golfer);
    pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new());
    let at = pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new())
        .rect("golfer:settings")
        .center();
    click(&ctx, &mut app, at);
    assert_eq!(app.settings_section, SettingsSection::General);
    assert_eq!(app.header_menu, None);
}

/// Follow turns off then on with a click: the view snaps to the golfer
/// up at once.
#[test]
fn turning_follow_on_snaps_the_view_to_the_golfer_up() {
    let mut app = viewing("flighthook.s");
    assert!(app.viewing_other());
    let (ctx, p) = settled(&mut app, 1600.0);
    click(&ctx, &mut app, p.rect("follow").center());
    assert!(app.follow_turns);
    assert_eq!(app.viewed_golfer.as_deref(), Some("flighthook.e"));
    assert!(!app.viewing_other());
}
/// The Golfer card lists a departed guest and "No golfer" after the
/// known golfers; a click on either moves only the view and sends
/// nothing.
#[test]
fn view_only_rows_follow_the_known_golfers_and_send_nothing() {
    let mut app = golfers_app();
    app.turn_golfer = Some("flighthook.e".into());
    let row = |n: u32, golfer: Option<&str>| {
        let mut row = crate::types::ShotRow::from(crate::types::ShotData {
            shot_number: n,
            shot_id: Some(format!("s{n}")),
            ..crate::types::ShotData::default()
        });
        row.golfer = golfer.map(str::to_owned);
        row
    };
    app.shots = vec![
        row(1, None),
        row(2, Some("pureplay.player-7")),
        row(3, Some("flighthook.e")),
    ];
    app.golfer_names
        .insert("pureplay.player-7".into(), "Pat Guest".into());
    app.shots_rev += 1;
    app.refresh_shot_caches();
    for follow in [true, false] {
        app.follow_turns = follow;
        app.view_hold = None;
        app.refresh_shot_caches();
        let p = open(&mut app, HeaderMenu::Golfer, 1600.0);
        assert!(p.has_text("Pat Guest") && p.has_text("No golfer"));
        let sam = p.rect("golfer_flighthook.s");
        let pat = p.rect("golfer_pureplay.player-7");
        let none = p.rect("golfer:none");
        assert!(sam.bottom() <= pat.top() && pat.bottom() <= none.top());
        assert!(p.icon_in("golfer:none").0.starts_with("golfer-none"));
        assert!(
            p.icon_in("golfer_pureplay.player-7")
                .0
                .starts_with("golfer-normal")
        );
        for (name, want) in [
            ("golfer_pureplay.player-7", Some("pureplay.player-7")),
            ("golfer:none", None),
        ] {
            let ctx = egui::Context::default();
            theme::install(&ctx);
            app.header_menu = Some(HeaderMenu::Golfer);
            pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new());
            let p = pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new());
            click(&ctx, &mut app, p.rect(name).center());
            app.refresh_shot_caches();
            assert_eq!(app.viewed_golfer.as_deref(), want, "{name} follow {follow}");
            assert!(app.sent_selects.is_empty(), "{name} follow {follow}");
            assert_eq!(app.turn_golfer.as_deref(), Some("flighthook.e"));
        }
    }
}
