//! Visual theme: palette, egui style, and small styled widgets.
//!
//! [`install`] applies the style once at startup. The dashboard always uses
//! the dark theme, whatever the browser or OS prefers.
//!
//! Icons are glyphs of Microsoft's Codicons font (`assets/fonts/codicon.ttf`,
//! CC BY 4.0; see `codicon-LICENSE.txt` beside it), compiled into the binary
//! and the WASM bundle and drawn with [`icon`].

use egui::{Color32, CornerRadius, FontFamily, FontId, Margin, Stroke, TextStyle, Theme};

// ---------------------------------------------------------------------------
// Palette
// ---------------------------------------------------------------------------

/// Window background (deep charcoal-navy).
pub(crate) const BG: Color32 = Color32::from_rgb(13, 18, 27);
/// Recessed background: text inputs, scroll tracks.
pub(crate) const BG_DEEP: Color32 = Color32::from_rgb(8, 12, 19);
/// Card and bar surface, one step above the background.
pub(crate) const SURFACE: Color32 = Color32::from_rgb(21, 29, 42);
/// Raised surface: idle buttons, table header.
pub(crate) const SURFACE_RAISED: Color32 = Color32::from_rgb(30, 41, 58);
/// Hovered surface.
pub(crate) const SURFACE_HOVER: Color32 = Color32::from_rgb(41, 55, 76);
/// Hairline borders and separators.
pub(crate) const BORDER: Color32 = Color32::from_rgb(40, 52, 71);

/// Primary text.
pub(crate) const TEXT: Color32 = Color32::from_rgb(230, 237, 245);
/// Secondary text: labels, keys, captions.
pub(crate) const TEXT_MUTED: Color32 = Color32::from_rgb(148, 162, 182);
/// Tertiary text: placeholders, missing values.
pub(crate) const TEXT_DIM: Color32 = Color32::from_rgb(92, 106, 126);
/// Text drawn on top of an accent fill.
pub(crate) const ON_ACCENT: Color32 = Color32::from_rgb(6, 26, 15);

/// Primary accent (golf green): selection, primary actions, live state.
pub(crate) const ACCENT: Color32 = Color32::from_rgb(52, 211, 120);
/// Accent hover / pressed shade.
pub(crate) const ACCENT_BRIGHT: Color32 = Color32::from_rgb(98, 230, 152);
/// Secondary accent (bright gold): the latest shot, anything new, pending
/// states. Used at full strength only (text, strokes, dots); darkening or
/// blending it over the navy surfaces turns it brown.
pub(crate) const GOLD: Color32 = Color32::from_rgb(247, 201, 72);
/// Errors and lost connections.
pub(crate) const DANGER: Color32 = Color32::from_rgb(243, 91, 99);
/// Names and section headings.
pub(crate) const HEADING: Color32 = Color32::from_rgb(158, 196, 255);
/// Actor names in the log.
pub(crate) const LOG_ACTOR: Color32 = Color32::from_rgb(190, 170, 235);
/// Fourth device color.
pub(crate) const TEAL: Color32 = Color32::from_rgb(64, 200, 210);

/// Swing numbers inside their band (inside both the fade and the draw band
/// of a shot-shape band): aqua.
pub(crate) const GRADE_BOTH: Color32 = TEAL;
/// Inside the fade band only: a blue more saturated than [`HEADING`], so
/// it stays apart from [`GRADE_BOTH`].
pub(crate) const GRADE_FADE: Color32 = Color32::from_rgb(120, 170, 255);
/// Inside the draw band only: green.
pub(crate) const GRADE_DRAW: Color32 = ACCENT;
/// Outside, within 2 SD of the nearer band: yellow.
pub(crate) const GRADE_NEAR: Color32 = GOLD;
/// Further out: red.
pub(crate) const GRADE_FAR: Color32 = DANGER;

/// Colors of the first four devices in a session, by sorted actor id. Never
/// gold (this shot) or red (errors).
pub(crate) const DEVICE_COLORS: [Color32; 4] = [ACCENT, HEADING, LOG_ACTOR, TEAL];

/// Translucent neutral fill of an idle pill, segmented track or badge.
pub(crate) const VEIL: Color32 = Color32::from_rgba_premultiplied(12, 12, 12, 12);
/// Translucent neutral fill of a hovered pill.
pub(crate) const VEIL_HOVER: Color32 = Color32::from_rgba_premultiplied(26, 26, 26, 26);

/// Status colors for the Telemetry badges.
pub(crate) const STATUS_OK: Color32 = ACCENT;
pub(crate) const STATUS_PENDING: Color32 = GOLD;
pub(crate) const STATUS_DOWN: Color32 = DANGER;
/// A card turned off: neutral, never an alert.
pub(crate) const STATUS_OFF: Color32 = TEXT_MUTED;

/// Corner radius of cards and panels.
pub(crate) const CARD_RADIUS: u8 = 10;

// ---------------------------------------------------------------------------
// Style
// ---------------------------------------------------------------------------

/// Install the theme on `ctx`: dark palette, larger type, roomier spacing,
/// and the icon font.
pub(crate) fn install(ctx: &egui::Context) {
    ctx.set_theme(Theme::Dark);
    ctx.style_mut_of(Theme::Dark, apply);
    add_icon_font(ctx);
}

fn apply(style: &mut egui::Style) {
    style.text_styles = [
        (
            TextStyle::Small,
            FontId::new(12.0, FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(15.0, FontFamily::Proportional)),
        (
            TextStyle::Button,
            FontId::new(15.0, FontFamily::Proportional),
        ),
        (
            TextStyle::Heading,
            FontId::new(22.0, FontFamily::Proportional),
        ),
        (
            TextStyle::Monospace,
            FontId::new(13.5, FontFamily::Monospace),
        ),
    ]
    .into();

    let s = &mut style.spacing;
    s.item_spacing = egui::vec2(8.0, 6.0);
    s.button_padding = egui::vec2(10.0, 4.0);
    s.interact_size.y = 24.0;
    s.window_margin = Margin::same(14);
    s.menu_margin = Margin::same(8);
    s.indent = 20.0;

    let v = &mut style.visuals;
    v.dark_mode = true;
    v.panel_fill = BG;
    v.window_fill = SURFACE;
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.window_corner_radius = CornerRadius::same(CARD_RADIUS);
    v.menu_corner_radius = CornerRadius::same(8);
    v.window_shadow.color = Color32::from_black_alpha(140);
    v.popup_shadow.color = Color32::from_black_alpha(120);
    v.extreme_bg_color = BG_DEEP;
    v.faint_bg_color = Color32::from_rgb(17, 24, 35);
    v.code_bg_color = SURFACE_RAISED;
    v.hyperlink_color = ACCENT;
    v.warn_fg_color = GOLD;
    v.error_fg_color = DANGER;
    v.selection.bg_fill = ACCENT.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, ACCENT_BRIGHT);
    v.striped = false;

    let radius = CornerRadius::same(6);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = SURFACE;
    w.noninteractive.weak_bg_fill = SURFACE;
    w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    w.noninteractive.corner_radius = radius;

    w.inactive.bg_fill = SURFACE_RAISED;
    w.inactive.weak_bg_fill = SURFACE_RAISED;
    w.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    w.inactive.corner_radius = radius;

    w.hovered.bg_fill = SURFACE_HOVER;
    w.hovered.weak_bg_fill = SURFACE_HOVER;
    w.hovered.bg_stroke = Stroke::new(1.0, ACCENT.gamma_multiply(0.6));
    w.hovered.fg_stroke = Stroke::new(1.5, Color32::WHITE);
    w.hovered.corner_radius = radius;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = ACCENT.gamma_multiply(0.45);
    w.active.weak_bg_fill = ACCENT.gamma_multiply(0.45);
    w.active.bg_stroke = Stroke::new(1.0, ACCENT);
    w.active.fg_stroke = Stroke::new(2.0, Color32::WHITE);
    w.active.corner_radius = radius;
    w.active.expansion = 0.0;

    w.open.bg_fill = SURFACE_HOVER;
    w.open.weak_bg_fill = SURFACE_HOVER;
    w.open.bg_stroke = Stroke::new(1.0, ACCENT.gamma_multiply(0.6));
    w.open.fg_stroke = Stroke::new(1.0, TEXT);
    w.open.corner_radius = radius;
}

// ---------------------------------------------------------------------------
// Widgets
// ---------------------------------------------------------------------------

/// A raised card frame.
pub(crate) fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CARD_RADIUS)
        .inner_margin(Margin::same(12))
}

/// A [`card`] the width of `ui` and exactly `height` tall, with a colored
/// stripe down its left edge: the Devices tab's cards. The contents are
/// given the card's inner height to fill, and no more.
pub(crate) fn accent_card<R>(
    ui: &mut egui::Ui,
    accent: Color32,
    height: f32,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    let frame = card().inner_margin(Margin {
        left: 18,
        right: 14,
        top: 12,
        bottom: 12,
    });
    let inner = (height - frame.total_margin().sum().y).max(0.0);
    let card = frame.show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.set_min_height(inner);
        ui.set_max_height(inner);
        add_contents(ui)
    });
    let r = card.response.rect;
    let stripe = egui::Rect::from_min_max(
        egui::pos2(r.left() + 6.0, r.top() + 12.0),
        egui::pos2(r.left() + 9.0, r.bottom() - 12.0),
    );
    ui.painter()
        .rect_filled(stripe, CornerRadius::same(2), accent);
    card
}

/// A pill-shaped toggle: accent fill when selected, a translucent neutral
/// pill otherwise.
pub(crate) fn pill(ui: &mut egui::Ui, selected: bool, text: &str) -> egui::Response {
    pill_with(ui, selected, text, VEIL, egui::Vec2::ZERO, PILL_FONT)
}

/// A [`pill`] at least `min_size` big, with the label centered: a larger
/// touch target, or pills of equal width in a grid.
pub(crate) fn pill_sized(
    ui: &mut egui::Ui,
    selected: bool,
    text: &str,
    min_size: egui::Vec2,
) -> egui::Response {
    pill_with(ui, selected, text, VEIL, min_size, PILL_FONT)
}

/// A pill that shows a value it does not let you change: outlined, muted
/// label, no hover state or pointer cursor.
pub(crate) fn pill_static(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_string(), FontId::proportional(14.0), TEXT_MUTED);
    let size = galley.size() + egui::vec2(28.0, 12.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        let radius = CornerRadius::same((rect.height() / 2.0) as u8);
        let painter = ui.painter();
        painter.rect(
            rect,
            radius,
            Color32::TRANSPARENT,
            Stroke::new(1.0, BORDER),
            egui::StrokeKind::Inside,
        );
        painter.galley(rect.center() - galley.size() / 2.0, galley, TEXT_MUTED);
    }
    response
}

/// Label size of a pill.
pub(crate) const PILL_FONT: f32 = 14.0;

fn pill_with(
    ui: &mut egui::Ui,
    selected: bool,
    text: &str,
    idle_fill: Color32,
    min_size: egui::Vec2,
    font_size: f32,
) -> egui::Response {
    // Placeholder color: the label color is picked once the hover state
    // is known.
    let galley = ui.painter().layout_no_wrap(
        text.to_string(),
        FontId::proportional(font_size),
        Color32::PLACEHOLDER,
    );
    // Padding in proportion to the label size.
    let pad = egui::vec2(28.0, 12.0) * (font_size / PILL_FONT);
    let size = (galley.size() + pad).max(min_size);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let radius = CornerRadius::same((rect.height() / 2.0) as u8);
        let hovered = response.hovered();
        let (fill, stroke) = match (selected, hovered) {
            (true, false) => (ACCENT, Stroke::NONE),
            (true, true) => (ACCENT_BRIGHT, Stroke::NONE),
            (false, false) => (idle_fill, Stroke::NONE),
            (false, true) => (VEIL_HOVER, Stroke::new(1.0, VEIL_HOVER)),
        };
        let painter = ui.painter();
        painter.rect(rect, radius, fill, stroke, egui::StrokeKind::Inside);
        let text_color = match (selected, hovered) {
            (true, _) => ON_ACCENT,
            (false, true) => TEXT,
            (false, false) => TEXT_MUTED,
        };
        painter.galley(rect.center() - galley.size() / 2.0, galley, text_color);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A row of pills inside a translucent track, one selected. Returns the
/// index of a clicked, unselected option. `alert` names an option that
/// carries a small red notification dot at the top center of its segment,
/// overlapping its top edge, whatever its selection state.
pub(crate) fn segmented(
    ui: &mut egui::Ui,
    options: &[&str],
    selected: usize,
    alert: Option<usize>,
) -> egui::InnerResponse<Option<usize>> {
    segmented_sized(ui, options, selected, alert, PILL_FONT)
}

/// A [`segmented`] control with `font_size` labels, its padding scaled to
/// match.
pub(crate) fn segmented_sized(
    ui: &mut egui::Ui,
    options: &[&str],
    selected: usize,
    alert: Option<usize>,
    font_size: f32,
) -> egui::InnerResponse<Option<usize>> {
    let shown = segmented_pills(ui, options, selected, alert, font_size);
    egui::InnerResponse::new(shown.inner.0, shown.response)
}

/// A [`segmented_sized`] control that also returns each option's pill, in
/// `options` order.
pub(crate) fn segmented_pills(
    ui: &mut egui::Ui,
    options: &[&str],
    selected: usize,
    alert: Option<usize>,
    font_size: f32,
) -> egui::InnerResponse<(Option<usize>, Vec<egui::Response>)> {
    egui::Frame::new()
        .fill(VEIL)
        .corner_radius(CornerRadius::same(255))
        .inner_margin(Margin::same(3))
        .show(ui, |ui| {
            let mut clicked = None;
            let mut pills: Vec<Option<egui::Response>> = vec![None; options.len()];
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.horizontal(|ui| {
                // Inside a right-to-left parent the row fills from the right,
                // so the options go in reversed to read left to right.
                let mut order: Vec<(usize, &&str)> = options.iter().enumerate().collect();
                if ui.layout().prefer_right_to_left() {
                    order.reverse();
                }
                for (i, label) in order {
                    let pill = pill_with(
                        ui,
                        i == selected,
                        label,
                        Color32::TRANSPARENT,
                        egui::Vec2::ZERO,
                        font_size,
                    );
                    if pill.clicked() && i != selected {
                        clicked = Some(i);
                    }
                    if alert == Some(i) {
                        let center = egui::pos2(pill.rect.center().x, pill.rect.top());
                        ui.painter()
                            .circle(center, 4.5, DANGER, Stroke::new(2.0, SURFACE));
                    }
                    pills[i] = Some(pill);
                }
            });
            (clicked, pills.into_iter().flatten().collect())
        })
}

/// A single on/off pill in a [`segmented`] track, so it stands as tall as
/// one: `text` after a `dot` of that color, accent filled while `on`.
/// The inner response is the pill's; the outer one covers the track.
pub(crate) fn switch_pill(
    ui: &mut egui::Ui,
    on: bool,
    text: &str,
    dot: Color32,
) -> egui::InnerResponse<egui::Response> {
    egui::Frame::new()
        .fill(VEIL)
        .corner_radius(CornerRadius::same(255))
        .inner_margin(Margin::same(3))
        .show(ui, |ui| {
            ui.horizontal(|ui| switch_pill_inner(ui, on, text, dot))
                .inner
        })
}

/// The pill of [`switch_pill`], inside its track.
fn switch_pill_inner(ui: &mut egui::Ui, on: bool, text: &str, dot: Color32) -> egui::Response {
    const DOT_RADIUS: f32 = 4.5;
    const DOT_GAP: f32 = 7.0;
    let galley = ui.painter().layout_no_wrap(
        text.to_string(),
        FontId::proportional(PILL_FONT),
        Color32::PLACEHOLDER,
    );
    let pad = egui::vec2(28.0, 12.0);
    let lead = 2.0 * DOT_RADIUS + DOT_GAP;
    let size = galley.size() + pad + egui::vec2(lead, 0.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let radius = CornerRadius::same((rect.height() / 2.0) as u8);
        let hovered = response.hovered();
        let (fill, stroke) = match (on, hovered) {
            (true, false) => (ACCENT, Stroke::NONE),
            (true, true) => (ACCENT_BRIGHT, Stroke::NONE),
            (false, false) => (Color32::TRANSPARENT, Stroke::NONE),
            (false, true) => (VEIL_HOVER, Stroke::new(1.0, VEIL_HOVER)),
        };
        let text_color = match (on, hovered) {
            (true, _) => ON_ACCENT,
            (false, true) => TEXT,
            (false, false) => TEXT_MUTED,
        };
        let painter = ui.painter();
        painter.rect(rect, radius, fill, stroke, egui::StrokeKind::Inside);
        let left = rect.left() + pad.x / 2.0;
        painter.circle_filled(
            egui::pos2(left + DOT_RADIUS, rect.center().y),
            DOT_RADIUS,
            dot,
        );
        let y = rect.center().y - galley.size().y / 2.0;
        painter.galley(egui::pos2(left + lead, y), galley, text_color);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A compact [`segmented`] control with `font_size` labels that fills the
/// available width, its options sharing it equally.
pub(crate) fn segmented_fill(
    ui: &mut egui::Ui,
    options: &[&str],
    selected: usize,
    font_size: f32,
) -> Option<usize> {
    const MARGIN: i8 = 3;
    const GAP: f32 = 2.0;
    let mut clicked = None;
    let n = options.len().max(1) as f32;
    let inner = ui.available_width() - 2.0 * f32::from(MARGIN);
    let width = (inner - GAP * (n - 1.0)) / n;
    egui::Frame::new()
        .fill(VEIL)
        .corner_radius(CornerRadius::same(255))
        .inner_margin(Margin::same(MARGIN))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            ui.horizontal(|ui| {
                for (i, label) in options.iter().enumerate() {
                    let pill = pill_with(
                        ui,
                        i == selected,
                        label,
                        Color32::TRANSPARENT,
                        egui::vec2(width, 0.0),
                        font_size,
                    );
                    if pill.clicked() && i != selected {
                        clicked = Some(i);
                    }
                }
            });
        });
    clicked
}

/// A status badge: neutral translucent pill with a colored outline, dot
/// and label.
pub(crate) fn badge(ui: &mut egui::Ui, text: &str, color: Color32) -> egui::Response {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), FontId::proportional(12.0), color);
    let dot = 7.0;
    let size = galley.size() + egui::vec2(dot + 22.0, 8.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    let radius = CornerRadius::same((rect.height() / 2.0) as u8);
    ui.painter().rect(
        rect,
        radius,
        VEIL,
        Stroke::new(1.0, color),
        egui::StrokeKind::Inside,
    );
    let dot_center = egui::pos2(rect.left() + 10.0 + dot / 2.0 - 1.0, rect.center().y);
    ui.painter().circle_filled(dot_center, dot / 2.0, color);
    let pos = egui::pos2(
        rect.left() + dot + 14.0,
        rect.center().y - galley.size().y / 2.0,
    );
    ui.painter().galley(pos, galley, color);
    response
}

/// Size of [`on_off_switch`].
pub(crate) const ON_OFF_SWITCH: egui::Vec2 = egui::vec2(30.0, 16.0);

/// A compact on/off switch: a rounded track, accent while `on`, with a knob
/// at its right end while on and its left end while off. Dimmed and inert
/// in a disabled `ui`; the response is clicked when it is flipped.
pub(crate) fn on_off_switch(ui: &mut egui::Ui, on: bool) -> egui::Response {
    let enabled = ui.is_enabled();
    let sense = egui::Sense::click();
    let (rect, response) = ui.allocate_exact_size(ON_OFF_SWITCH, sense);
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let dim = |c: Color32| if enabled { c } else { c.gamma_multiply(0.45) };
    let (track, edge) = if on {
        (ACCENT, ACCENT)
    } else {
        (SURFACE_HOVER, TEXT_DIM)
    };
    let edge = if enabled && response.hovered() {
        if on { ACCENT_BRIGHT } else { TEXT_MUTED }
    } else {
        edge
    };
    let radius = CornerRadius::same((rect.height() / 2.0) as u8);
    ui.painter().rect(
        rect,
        radius,
        dim(track),
        Stroke::new(1.0, dim(edge)),
        egui::StrokeKind::Inside,
    );
    let knob = rect.height() / 2.0 - 3.0;
    let x = if on {
        rect.right() - 3.0 - knob
    } else {
        rect.left() + 3.0 + knob
    };
    let color = if on { ON_ACCENT } else { TEXT_MUTED };
    ui.painter()
        .circle_filled(egui::pos2(x, rect.center().y), knob, dim(color));
    if enabled {
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        response
    }
}

/// Diameter of [`loading_ring`] and [`connected_ring`] at 1x.
pub(crate) const STATUS_RING: f32 = 48.0;

/// Line width of [`loading_ring`] and [`connected_ring`].
const STATUS_RING_STROKE: f32 = 4.0;

/// How often a visible [`loading_ring`] is redrawn.
pub(crate) const LOADING_FRAME: std::time::Duration = std::time::Duration::from_micros(33_333);

/// A line through `points` with round ends and joints.
fn round_line(painter: &egui::Painter, points: Vec<egui::Pos2>, color: Color32, width: f32) {
    for p in &points {
        painter.circle_filled(*p, width / 2.0, color);
    }
    painter.add(egui::Shape::line(points, Stroke::new(width, color)));
}

/// A loading spinner [`STATUS_RING`] across centered on `center`, at `time`
/// seconds: a faint ring with an arc in `color` going round it, growing and
/// shrinking as it goes. The caller asks for the next frame.
pub(crate) fn loading_ring(painter: &egui::Painter, center: egui::Pos2, color: Color32, time: f64) {
    use std::f64::consts::TAU;
    let radius = (STATUS_RING - STATUS_RING_STROKE) / 2.0;
    painter.circle_stroke(
        center,
        radius,
        Stroke::new(STATUS_RING_STROKE, color.gamma_multiply(0.18)),
    );
    // One turn every 1.1 s; the arc spans 40 to 250 degrees and back.
    let start = time * TAU / 1.1;
    let span = (40.0 + 105.0 * (1.0 - (time * TAU / 1.6).cos())).to_radians();
    let steps = 32;
    let points = (0..=steps)
        .map(|i| {
            let a = start + span * f64::from(i) / f64::from(steps);
            #[allow(clippy::cast_possible_truncation)]
            let (sin, cos) = (a.sin() as f32, a.cos() as f32);
            center + egui::vec2(cos, sin) * radius
        })
        .collect();
    round_line(painter, points, color, STATUS_RING_STROKE);
}

/// A connected mark [`STATUS_RING`] across centered on `center`: a check in
/// a ring, in the connected green. It does not move.
pub(crate) fn connected_ring(painter: &egui::Painter, center: egui::Pos2) {
    let radius = (STATUS_RING - STATUS_RING_STROKE) / 2.0;
    painter.circle_stroke(center, radius, Stroke::new(STATUS_RING_STROKE, STATUS_OK));
    let at = |x: f32, y: f32| center + egui::vec2(x, y) * radius;
    round_line(
        painter,
        vec![at(-0.42, 0.02), at(-0.12, 0.32), at(0.44, -0.3)],
        STATUS_OK,
        STATUS_RING_STROKE,
    );
}

/// A clickable chip the size of a `badge`, with an up arrow: neutral
/// translucent pill with an accent outline, arrow and label, brighter on
/// hover. For jumping back to the top of a list.
pub(crate) fn jump_chip(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(
        text.to_string(),
        FontId::proportional(12.0),
        Color32::PLACEHOLDER,
    );
    let arrow = 8.0;
    let size = galley.size() + egui::vec2(arrow + 28.0, 8.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let hovered = response.hovered();
        let (fill, color) = if hovered {
            (VEIL_HOVER, ACCENT_BRIGHT)
        } else {
            (VEIL, ACCENT)
        };
        let radius = CornerRadius::same((rect.height() / 2.0) as u8);
        let painter = ui.painter();
        painter.rect(
            rect,
            radius,
            fill,
            Stroke::new(1.0, color),
            egui::StrokeKind::Inside,
        );
        let stroke = Stroke::new(1.5, color);
        let x = rect.left() + 10.0 + arrow / 2.0;
        let (top, bottom) = (rect.center().y - 4.5, rect.center().y + 4.5);
        painter.line_segment([egui::pos2(x, top + 0.5), egui::pos2(x, bottom)], stroke);
        painter.line(
            vec![
                egui::pos2(x - arrow / 2.0, top + arrow / 2.0),
                egui::pos2(x, top),
                egui::pos2(x + arrow / 2.0, top + arrow / 2.0),
            ],
            stroke,
        );
        let pos = egui::pos2(
            rect.left() + arrow + 16.0,
            rect.center().y - galley.size().y / 2.0,
        );
        painter.galley(pos, galley, color);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A clickable chip with a Codicons glyph before its label: the
/// [`jump_chip`] look with `icon` in place of the arrow.
pub(crate) fn icon_chip(ui: &mut egui::Ui, icon: Icon, text: &str) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(
        text.to_string(),
        FontId::proportional(12.0),
        Color32::PLACEHOLDER,
    );
    let glyph = 14.0;
    let size = galley.size() + egui::vec2(glyph + 26.0, 8.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let family = icon_family();
    let loaded = ui.fonts(|f| f.definitions().families.contains_key(&family));
    if !loaded {
        add_icon_font(ui.ctx());
    }
    if ui.is_rect_visible(rect) {
        let (fill, color) = if response.hovered() {
            (VEIL_HOVER, ACCENT_BRIGHT)
        } else {
            (VEIL, ACCENT)
        };
        let radius = CornerRadius::same((rect.height() / 2.0) as u8);
        let painter = ui.painter();
        painter.rect(
            rect,
            radius,
            fill,
            Stroke::new(1.0, color),
            egui::StrokeKind::Inside,
        );
        if loaded {
            let icon = painter.layout_no_wrap(
                icon.codepoint().to_string(),
                FontId::new(glyph, family),
                color,
            );
            let at = egui::pos2(rect.left() + 10.0, rect.center().y - icon.size().y / 2.0);
            painter.galley(at, icon, color);
        }
        let pos = egui::pos2(
            rect.left() + glyph + 16.0,
            rect.center().y - galley.size().y / 2.0,
        );
        painter.galley(pos, galley, color);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A clickable chip the size of a `badge`, at least `min_size` big: the
/// [`jump_chip`] look without the arrow (neutral translucent pill, accent
/// outline and label, brighter on hover). Disabled, it is dimmed and takes
/// no clicks.
pub(crate) fn chip_button(
    ui: &mut egui::Ui,
    text: &str,
    enabled: bool,
    min_size: egui::Vec2,
) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(
        text.to_string(),
        FontId::proportional(12.0),
        Color32::PLACEHOLDER,
    );
    let size = (galley.size() + egui::vec2(24.0, 8.0)).max(min_size);
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    if ui.is_rect_visible(rect) {
        let (fill, color) = match (enabled, response.hovered()) {
            (false, _) => (Color32::TRANSPARENT, TEXT_DIM),
            (true, false) => (VEIL, ACCENT),
            (true, true) => (VEIL_HOVER, ACCENT_BRIGHT),
        };
        let radius = CornerRadius::same((rect.height() / 2.0) as u8);
        let painter = ui.painter();
        painter.rect(
            rect,
            radius,
            fill,
            Stroke::new(1.0, color),
            egui::StrokeKind::Inside,
        );
        painter.galley(rect.center() - galley.size() / 2.0, galley, color);
    }
    if enabled {
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        response
    }
}

/// A [`chip_button`] with a down chevron after the label, for a button that
/// opens a menu. At least `min_size` big; disabled, it is dimmed and takes no
/// clicks.
pub(crate) fn menu_chip(
    ui: &mut egui::Ui,
    text: &str,
    enabled: bool,
    min_size: egui::Vec2,
) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(
        text.to_string(),
        FontId::proportional(12.0),
        Color32::PLACEHOLDER,
    );
    let chevron = 8.0;
    let size = (galley.size() + egui::vec2(chevron + 30.0, 8.0)).max(min_size);
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    if ui.is_rect_visible(rect) {
        let (fill, color) = match (enabled, response.hovered()) {
            (false, _) => (Color32::TRANSPARENT, TEXT_DIM),
            (true, false) => (VEIL, ACCENT),
            (true, true) => (VEIL_HOVER, ACCENT_BRIGHT),
        };
        let radius = CornerRadius::same((rect.height() / 2.0) as u8);
        let painter = ui.painter();
        painter.rect(
            rect,
            radius,
            fill,
            Stroke::new(1.0, color),
            egui::StrokeKind::Inside,
        );
        let label_width = galley.size().x;
        let left = rect.center().x - (label_width + 6.0 + chevron) / 2.0;
        let pos = egui::pos2(left, rect.center().y - galley.size().y / 2.0);
        painter.galley(pos, galley, color);
        let x = left + label_width + 6.0 + chevron / 2.0;
        let y = rect.center().y;
        painter.line(
            vec![
                egui::pos2(x - chevron / 2.0, y - chevron / 4.0),
                egui::pos2(x, y + chevron / 4.0),
                egui::pos2(x + chevron / 2.0, y - chevron / 4.0),
            ],
            Stroke::new(1.5, color),
        );
    }
    if enabled {
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        response
    }
}

// ---------------------------------------------------------------------------
// Icons
// ---------------------------------------------------------------------------

/// Codicons 0.0.46-24 (font 1.15), unmodified.
const CODICON_TTF: &[u8] = include_bytes!("../assets/fonts/codicon.ttf");

/// Name of the icon font and of its font family.
const ICON_FONT: &str = "codicon";

/// Point size icons are drawn at: the 16 px grid Codicons are designed on.
const ICON_SIZE: f32 = 16.0;

/// A Codicons glyph, by its Codicons name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Icon {
    /// `add`.
    Add,
    /// `trash`.
    Trash,
    /// `play`.
    Play,
    /// `debug-pause`.
    Pause,
    /// `chevron-left`.
    ChevronLeft,
    /// `chevron-right`.
    ChevronRight,
    /// `device-camera-video`.
    Video,
    /// `refresh`.
    Refresh,
    /// `download`.
    Download,
    /// `export`.
    Export,
}

impl Icon {
    /// The glyph's codepoint in the Codicons font (its private use area).
    pub(crate) const fn codepoint(self) -> char {
        match self {
            Icon::Add => '\u{ea60}',
            Icon::Trash => '\u{ea81}',
            Icon::Play => '\u{eb2c}',
            Icon::Pause => '\u{ead1}',
            Icon::ChevronLeft => '\u{eab5}',
            Icon::ChevronRight => '\u{eab6}',
            Icon::Video => '\u{ead9}',
            Icon::Refresh => '\u{eb37}',
            Icon::Download => '\u{ec74}',
            Icon::Export => '\u{ebac}',
        }
    }
}

fn icon_family() -> FontFamily {
    FontFamily::Name(ICON_FONT.into())
}

/// Register the Codicons font as its own family, once per context; it
/// takes effect from the next pass.
fn add_icon_font(ctx: &egui::Context) {
    use egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};
    let added = egui::Id::new("flighthook.icon_font");
    if ctx.data(|d| d.get_temp::<bool>(added)).is_some() {
        return;
    }
    ctx.data_mut(|d| d.insert_temp(added, true));
    ctx.add_font(FontInsert::new(
        ICON_FONT,
        egui::FontData::from_static(CODICON_TTF),
        vec![InsertFontFamily {
            family: icon_family(),
            priority: FontPriority::Highest,
        }],
    ));
}

/// An icon button `height` tall and as wide: the glyph in muted text at
/// rest and in `hover` when hovered. Disabled, it is dimmed and takes no
/// clicks. Until the icon font is loaded (the pass after [`install`]), the
/// space is left blank.
pub(crate) fn icon(
    ui: &mut egui::Ui,
    icon: Icon,
    height: f32,
    hover: Color32,
    enabled: bool,
) -> egui::Response {
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(height, height), sense);
    let family = icon_family();
    let loaded = ui.fonts(|f| f.definitions().families.contains_key(&family));
    if !loaded {
        add_icon_font(ui.ctx());
    } else if ui.is_rect_visible(rect) {
        let color = match (enabled, response.hovered()) {
            (false, _) => TEXT_DIM,
            (true, false) => TEXT_MUTED,
            (true, true) => hover,
        };
        let galley = ui.painter().layout_no_wrap(
            icon.codepoint().to_string(),
            FontId::new(ICON_SIZE, family),
            color,
        );
        ui.painter()
            .galley(rect.center() - galley.size() / 2.0, galley, color);
    }
    if enabled {
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        response
    }
}

/// A filled primary button (accent fill, dark label).
pub(crate) fn primary_button(text: &str, size: f32) -> egui::Button<'static> {
    egui::Button::new(
        egui::RichText::new(text.to_string())
            .size(size)
            .strong()
            .color(ON_ACCENT),
    )
    .fill(ACCENT)
    .corner_radius(CornerRadius::same(6))
}

/// A filled destructive button (alert red fill, white label).
pub(crate) fn danger_button(text: &str, size: f32) -> egui::Button<'static> {
    egui::Button::new(
        egui::RichText::new(text.to_string())
            .size(size)
            .strong()
            .color(Color32::WHITE),
    )
    .fill(DANGER)
    .corner_radius(CornerRadius::same(6))
}

/// Small-caps style label: uppercase, letter-spaced, muted.
pub(crate) fn caps(text: &str, size: f32, color: Color32) -> egui::RichText {
    egui::RichText::new(text.to_uppercase())
        .size(size)
        .color(color)
        .extra_letter_spacing(1.0)
}

/// Linear blend from `a` to `b` (`t` in 0..=1).
pub(crate) fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgba_unmultiplied(
        l(a.r(), b.r()),
        l(a.g(), b.g()),
        l(a.b(), b.b()),
        l(a.a(), b.a()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One pass over a 400×200 screen, drawing `add`.
    fn frame(ctx: &egui::Context, add: impl FnMut(&mut egui::Ui)) -> egui::FullOutput {
        let mut add = add;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(400.0, 200.0),
            )),
            ..Default::default()
        };
        ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| add(ui));
        })
    }

    #[test]
    fn trash_is_the_codicons_trash_glyph() {
        // `trash` in Codicons' mapping.json (60033) and codicon.csv (EA81).
        assert_eq!(Icon::Trash.codepoint(), '\u{ea81}');
        let ctx = egui::Context::default();
        install(&ctx);
        let _ = frame(&ctx, |_| {});
        let font = FontId::new(ICON_SIZE, icon_family());
        assert!(ctx.fonts_mut(|f| f.has_glyph(&font, Icon::Trash.codepoint())));
    }

    #[test]
    fn add_is_the_codicons_add_glyph() {
        // `add` in Codicons' mapping.json (60000) and codicon.csv (EA60).
        assert_eq!(Icon::Add.codepoint(), '\u{ea60}');
        let ctx = egui::Context::default();
        install(&ctx);
        let _ = frame(&ctx, |_| {});
        let font = FontId::new(ICON_SIZE, icon_family());
        assert!(ctx.fonts_mut(|f| f.has_glyph(&font, Icon::Add.codepoint())));
    }

    #[test]
    fn replay_icons_are_codicons_glyphs() {
        // Codepoints from Codicons' mapping.json: play 60204, debug-pause
        // 60113, chevron-left 60085, chevron-right 60086,
        // device-camera-video 60121, refresh 60215, download 60532, export 60332.
        let icons = [
            (Icon::Play, '\u{eb2c}'),
            (Icon::Pause, '\u{ead1}'),
            (Icon::ChevronLeft, '\u{eab5}'),
            (Icon::ChevronRight, '\u{eab6}'),
            (Icon::Video, '\u{ead9}'),
            (Icon::Refresh, '\u{eb37}'),
            (Icon::Download, '\u{ec74}'),
            (Icon::Export, '\u{ebac}'),
        ];
        let ctx = egui::Context::default();
        install(&ctx);
        let _ = frame(&ctx, |_| {});
        let font = FontId::new(ICON_SIZE, icon_family());
        for (icon, codepoint) in icons {
            assert_eq!(icon.codepoint(), codepoint, "{icon:?}");
            assert!(ctx.fonts_mut(|f| f.has_glyph(&font, codepoint)), "{icon:?}");
        }
    }

    #[test]
    fn a_second_install_adds_the_icon_font_once() {
        let ctx = egui::Context::default();
        install(&ctx);
        install(&ctx);
        let _ = frame(&ctx, |_| {});
        let entries = ctx.fonts(|f| {
            f.definitions()
                .families
                .get(&icon_family())
                .map_or(0, |names| names.iter().filter(|n| *n == ICON_FONT).count())
        });
        assert_eq!(entries, 1);
    }

    #[test]
    fn icon_draws_once_the_font_is_loaded() {
        let glyphs = |out: &egui::FullOutput| {
            out.shapes
                .iter()
                .filter(|s| matches!(s.shape, egui::Shape::Text(_)))
                .count()
        };
        // Without `install` the space is kept blank, and the font is loaded
        // for the next pass.
        let ctx = egui::Context::default();
        let out = frame(&ctx, |ui| {
            let r = icon(ui, Icon::Trash, 20.0, DANGER, true);
            assert_eq!(r.rect.size(), egui::vec2(20.0, 20.0));
        });
        assert_eq!(glyphs(&out), 0);
        let out = frame(&ctx, |ui| {
            let _ = icon(ui, Icon::Trash, 20.0, DANGER, true);
        });
        assert_eq!(glyphs(&out), 1);
    }

    #[test]
    fn disabled_icon_takes_no_clicks() {
        let ctx = egui::Context::default();
        install(&ctx);
        let _ = frame(&ctx, |ui| {
            let on = icon(ui, Icon::Trash, 20.0, DANGER, true);
            let off = icon(ui, Icon::Trash, 20.0, DANGER, false);
            assert!(on.sense.senses_click());
            assert!(!off.sense.senses_click());
        });
    }
}
