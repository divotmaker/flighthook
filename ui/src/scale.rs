//! Automatic UI zoom: scales the whole UI so the window shows the layout at a
//! size it is designed for.
//!
//! The layout is designed for windows from [`DESIGN_MIN`] to [`DESIGN_MAX`]
//! points. A bigger window zooms in until the design's largest size fits, a
//! smaller one zooms out until its smallest fits, and anything in between
//! stays at zoom 1. In the native window, Ctrl/Cmd + `=`/`+` and `-` step on
//! top of that and Ctrl/Cmd + `0` goes back to the automatic zoom; in the
//! browser those keys are the browser's own zoom.

use std::ops::RangeInclusive;

use egui::{Key, KeyboardShortcut, Modifiers, Vec2};

/// The largest window, in points, the layout is designed for. A window
/// bigger in both directions zooms in.
pub(crate) const DESIGN_MAX: (f32, f32) = (1920.0, 1080.0);
/// The smallest window, in points, the layout is designed for: the smallest
/// of the Shots tab's fit tests (`FIT_WINDOWS` in `panels/shots.rs`). A
/// window smaller in either direction zooms out.
pub(crate) const DESIGN_MIN: (f32, f32) = (1024.0, 768.0);
/// The largest automatic zoom.
pub(crate) const AUTO_MAX: f32 = 3.0;
/// The smallest automatic zoom. A window much smaller than [`DESIGN_MIN`]
/// (a phone) zooms out only this far and scrolls the rest, so its text
/// stays readable.
pub(crate) const AUTO_MIN: f32 = 0.8;
/// Zoom factor of one manual step.
pub(crate) const STEP: f32 = 1.1;
/// Manual steps either side of the automatic zoom.
pub(crate) const STEPS: RangeInclusive<i32> = -6..=6;
/// The zoom actually applied, after the manual steps.
pub(crate) const ZOOM: RangeInclusive<f32> = 0.5..=4.0;
/// A zoom closer than this to the current one is not applied, so a steady
/// window never sets it again.
const TOLERANCE: f32 = 0.005;
/// Highest effective pixel ratio in the browser.
const WEB_MAX_PIXELS_PER_POINT: f32 = 2.0;
/// The automatic zoom is rounded down to a multiple of `1 / GRID`, so a
/// window being resized changes it (and re-rasterises the text) only now and
/// then, and the rounded zoom still fits the design size.
const GRID: f32 = 32.0;

const ZOOM_IN: [KeyboardShortcut; 2] = [
    KeyboardShortcut::new(Modifiers::COMMAND, Key::Equals),
    KeyboardShortcut::new(Modifiers::COMMAND, Key::Plus),
];
const ZOOM_OUT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Minus);
const ZOOM_RESET: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Num0);

/// The automatic zoom for a window `base` points in size at zoom 1
/// (physical pixels / the monitor's native pixels per point).
///
/// `grow = min(w / 1920, h / 1080)` and `shrink = min(w / 1024, h / 768)`:
/// `grow` (at most [`AUTO_MAX`]) when it is above 1, else `shrink` (at least
/// [`AUTO_MIN`]) when it is below 1, else 1; `grow` and `shrink` are rounded
/// down to a multiple of `1 / GRID` first. Both sides meet 1 at their
/// boundary. A size that is not finite
/// and positive gives 1.
#[must_use]
pub(crate) fn auto_zoom(base: Vec2) -> f32 {
    let (w, h) = (base.x, base.y);
    if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
        return 1.0;
    }
    let grow = (w / DESIGN_MAX.0).min(h / DESIGN_MAX.1);
    let shrink = (w / DESIGN_MIN.0).min(h / DESIGN_MIN.1);
    // The epsilon keeps float noise just under a grid line (2159.99 px for
    // 2160) on that line.
    let on_grid = |z: f32| (z * GRID + 1e-3).floor() / GRID;
    if grow > 1.0 {
        on_grid(grow).min(AUTO_MAX)
    } else if shrink < 1.0 {
        on_grid(shrink).max(AUTO_MIN)
    } else {
        1.0
    }
}

/// `zoom` capped so the effective pixel ratio (`native * zoom`) stays at most
/// 2.0 on a screen whose native ratio is above 2.0: in the browser, text and
/// layout use at most 2 pixels per point, which keeps the glyph atlas small
/// on high-DPI phones. Applied in the browser only.
#[must_use]
fn web_capped(zoom: f32, native: f32) -> f32 {
    if native > WEB_MAX_PIXELS_PER_POINT {
        zoom.min(WEB_MAX_PIXELS_PER_POINT / native)
    } else {
        zoom
    }
}

/// The window's size in points at zoom 1, independent of the current zoom,
/// and the monitor's native pixels per point.
#[must_use]
fn base_size(ctx: &egui::Context) -> (Vec2, f32) {
    let ppp = ctx.pixels_per_point();
    let native = ctx
        .native_pixels_per_point()
        .unwrap_or_else(|| ppp / ctx.zoom_factor());
    let physical = ctx.content_rect().size() * ppp;
    (physical / native, native)
}

/// Turns off egui's own keyboard zoom: [`UiScale::apply`] handles the
/// shortcuts.
pub(crate) fn install(ctx: &egui::Context) {
    ctx.options_mut(|o| o.zoom_with_keyboard = false);
}

/// The UI zoom: automatic from the window size, plus the session's manual
/// steps.
#[derive(Debug, Default)]
pub(crate) struct UiScale {
    /// Manual steps of [`STEP`] on top of the automatic zoom.
    steps: i32,
}

impl UiScale {
    /// The zoom for a window `base` points in size at zoom 1.
    #[must_use]
    pub(crate) fn zoom(&self, base: Vec2) -> f32 {
        (auto_zoom(base) * STEP.powi(self.steps)).clamp(*ZOOM.start(), *ZOOM.end())
    }

    pub(crate) fn zoom_in(&mut self) {
        self.steps = (self.steps + 1).min(*STEPS.end());
    }

    pub(crate) fn zoom_out(&mut self) {
        self.steps = (self.steps - 1).max(*STEPS.start());
    }

    /// Back to the automatic zoom.
    pub(crate) fn reset(&mut self) {
        self.steps = 0;
    }

    /// Handles (and consumes) the zoom shortcuts in the native window, then
    /// sets the zoom for the current window when it differs from the applied
    /// one. Call before any layout. A changed zoom discards the pass, so the
    /// next pass lays out at the new zoom.
    pub(crate) fn apply(&mut self, ctx: &egui::Context) {
        if !cfg!(target_arch = "wasm32") {
            self.shortcuts(ctx);
        }
        let (base, native) = base_size(ctx);
        let mut zoom = self.zoom(base);
        if cfg!(target_arch = "wasm32") {
            zoom = web_capped(zoom, native);
        }
        if (zoom - ctx.zoom_factor()).abs() > TOLERANCE {
            ctx.set_zoom_factor(zoom);
            ctx.request_discard("UI zoom changed");
        }
    }

    /// Ctrl/Cmd + `=`/`+`, `-` and `0`, consumed. With Alt held (`AltGr` on
    /// Windows) they are left alone.
    fn shortcuts(&mut self, ctx: &egui::Context) {
        ctx.input_mut(|i| {
            if i.modifiers.alt {
                return;
            }
            if i.consume_shortcut(&ZOOM_RESET) {
                self.reset();
            }
            for shortcut in &ZOOM_IN {
                while i.consume_shortcut(shortcut) {
                    self.zoom_in();
                }
            }
            while i.consume_shortcut(&ZOOM_OUT) {
                self.zoom_out();
            }
        });
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    fn zoom_at(w: f32, h: f32) -> f32 {
        auto_zoom(egui::vec2(w, h))
    }

    #[test]
    fn auto_zoom_fits_the_design_sizes() {
        // 4K at 100 %, maximized: the height limits it (1.898), on the grid.
        assert_eq!(zoom_at(3840.0, 2050.0), 60.0 / 32.0);
        assert!((zoom_at(3840.0, 2160.0) - 2.0).abs() < 1e-6);
        // 4K at 200 %: unchanged.
        assert_eq!(zoom_at(1920.0, 1025.0), 1.0);
        // 4K at 300 %: the height limits it (0.889), on the grid.
        assert_eq!(zoom_at(1280.0, 683.0), 28.0 / 32.0);
        for (w, h) in [
            (1920.0, 1080.0),
            (1600.0, 900.0),
            (1440.0, 900.0),
            (1280.0, 800.0),
            (1024.0, 768.0),
        ] {
            assert_eq!(zoom_at(w, h), 1.0, "{w}x{h}");
        }
        // 1.296 and 0.879, on the grid.
        assert_eq!(zoom_at(2560.0, 1400.0), 41.0 / 32.0);
        assert_eq!(zoom_at(900.0, 700.0), 28.0 / 32.0);
        // Rounded down, so the design size still fits.
        for (w, h) in [(3840.0, 2050.0), (2560.0, 1400.0), (1280.0, 683.0)] {
            let z = zoom_at(w, h);
            assert!(w / z >= 1024.0 && h / z >= 768.0, "{w}x{h}: {z}");
        }
    }

    #[test]
    fn auto_zoom_is_clamped() {
        assert_eq!(zoom_at(20_000.0, 20_000.0), AUTO_MAX);
        assert_eq!(zoom_at(100.0, 100.0), AUTO_MIN);
        assert_eq!(zoom_at(1.0, 1.0), AUTO_MIN);
    }

    #[test]
    fn auto_zoom_steps_by_one_grid_cell_at_both_boundaries() {
        let eps = 0.01;
        for (w, h) in [DESIGN_MAX, DESIGN_MIN] {
            for (dw, dh) in [(eps, eps), (-eps, -eps), (eps, -eps), (-eps, eps)] {
                let z = zoom_at(w + dw, h + dh);
                assert!(
                    (z - 1.0).abs() <= 1.0 / GRID + 1e-6,
                    "{w}+{dw} x {h}+{dh}: {z}"
                );
            }
        }
        // Wide but short or tall but narrow: neither limit applies.
        assert_eq!(zoom_at(3000.0, 900.0), 1.0);
        assert_eq!(zoom_at(1200.0, 1500.0), 1.0);
    }

    #[test]
    fn auto_zoom_of_garbage_is_one() {
        for (w, h) in [
            (0.0, 0.0),
            (-1920.0, 1080.0),
            (1920.0, -1.0),
            (f32::NAN, 1080.0),
            (1920.0, f32::INFINITY),
            (f32::INFINITY, f32::INFINITY),
        ] {
            assert_eq!(zoom_at(w, h), 1.0, "{w}x{h}");
        }
    }

    #[test]
    fn manual_steps_multiply_and_clamp() {
        let base = egui::vec2(1600.0, 900.0);
        let mut scale = UiScale::default();
        assert_eq!(scale.zoom(base), 1.0);
        scale.zoom_in();
        assert!((scale.zoom(base) - 1.1).abs() < 1e-6);
        scale.zoom_out();
        scale.zoom_out();
        assert!((scale.zoom(base) - 1.0 / 1.1).abs() < 1e-6);
        for _ in 0..20 {
            scale.zoom_in();
        }
        assert_eq!(scale.steps, 6);
        assert!((scale.zoom(base) - 1.1_f32.powi(6)).abs() < 1e-5);
        for _ in 0..20 {
            scale.zoom_out();
        }
        assert_eq!(scale.steps, -6);
        assert!((scale.zoom(base) - 1.1_f32.powi(-6)).abs() < 1e-5);
        scale.reset();
        assert_eq!(scale.zoom(base), 1.0);
    }

    #[test]
    fn final_zoom_is_clamped() {
        let mut scale = UiScale::default();
        for _ in 0..6 {
            scale.zoom_in();
        }
        // 3.0 * 1.1^6 = 5.3, clamped.
        assert_eq!(scale.zoom(egui::vec2(20_000.0, 20_000.0)), 4.0);
        scale.reset();
        for _ in 0..6 {
            scale.zoom_out();
        }
        // 0.8 / 1.1^6 = 0.45, clamped.
        assert_eq!(scale.zoom(egui::vec2(100.0, 100.0)), 0.5);
    }

    #[test]
    fn web_cap_keeps_the_pixel_ratio_at_two() {
        assert_eq!(web_capped(1.0, 1.0), 1.0);
        assert_eq!(web_capped(1.9, 2.0), 1.9);
        assert!((web_capped(1.0, 3.0) - 2.0 / 3.0).abs() < 1e-6);
        assert!((web_capped(0.6, 3.0) - 0.6).abs() < 1e-6);
        assert!((web_capped(2.0, 4.0) - 0.5).abs() < 1e-6);
    }

    /// A window of `physical` pixels on a monitor of `native` pixels per
    /// point, driven like eframe does: the screen rect in points from the
    /// zoom egui applied last.
    struct Window {
        ctx: egui::Context,
        scale: UiScale,
        physical: Vec2,
        native: f32,
    }

    impl Window {
        fn new(physical: Vec2, native: f32) -> Self {
            let ctx = egui::Context::default();
            install(&ctx);
            Self {
                ctx,
                scale: UiScale::default(),
                physical,
                native,
            }
        }

        /// One frame with `events`; returns whether it discarded a pass.
        fn frame(&mut self, events: Vec<egui::Event>) -> bool {
            let ppp = self.native * self.ctx.zoom_factor();
            let mut input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    self.physical / ppp,
                )),
                events,
                ..Default::default()
            };
            input
                .viewports
                .entry(egui::ViewportId::ROOT)
                .or_default()
                .native_pixels_per_point = Some(self.native);
            let mut leftover = Vec::new();
            let out = self.ctx.run(input, |ctx| {
                self.scale.apply(ctx);
                // What a text field would see after apply().
                leftover = ctx.input(|i| i.events.clone());
            });
            assert!(
                !leftover
                    .iter()
                    .any(|e| matches!(e, egui::Event::Key { pressed: true, .. })),
                "zoom keys left for the widgets: {leftover:?}"
            );
            out.platform_output.num_completed_passes > 1
        }

        fn zoom(&self) -> f32 {
            self.ctx.zoom_factor()
        }

        /// The window's size in points as laid out.
        fn points(&self) -> Vec2 {
            self.ctx.content_rect().size()
        }
    }

    fn key(key: Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::COMMAND,
        }
    }

    #[test]
    fn a_4k_window_fits_at_every_display_scale() {
        let physical = egui::vec2(3840.0, 2050.0);
        // 100 %: zoomed to the 200 % look.
        let mut w = Window::new(physical, 1.0);
        assert!(w.frame(Vec::new()));
        assert_eq!(w.zoom(), 60.0 / 32.0);
        assert!(
            (w.points().y - 2050.0 / 1.875).abs() < 1.0,
            "{:?}",
            w.points()
        );
        // A steady window keeps it and never re-sets it.
        assert!(!w.frame(Vec::new()));
        assert_eq!(w.zoom(), 60.0 / 32.0);

        // 200 %: untouched.
        let mut w = Window::new(physical, 2.0);
        assert!(!w.frame(Vec::new()));
        assert_eq!(w.zoom(), 1.0);
        assert!((w.points() - egui::vec2(1920.0, 1025.0)).length() < 1.0);

        // 300 %: zoomed out to the design's smallest height.
        let mut w = Window::new(physical, 3.0);
        assert!(w.frame(Vec::new()));
        assert_eq!(w.zoom(), 28.0 / 32.0);
        assert!(w.points().y >= 768.0, "{:?}", w.points());
        assert!(!w.frame(Vec::new()));
    }

    #[test]
    fn resizing_and_changing_monitor_refit_on_the_next_frame() {
        let mut w = Window::new(egui::vec2(3840.0, 2050.0), 1.0);
        w.frame(Vec::new());
        w.physical = egui::vec2(1600.0, 900.0);
        assert!(w.frame(Vec::new()));
        assert_eq!(w.zoom(), 1.0);
        w.physical = egui::vec2(3840.0, 2160.0);
        w.frame(Vec::new());
        assert!((w.zoom() - 2.0).abs() < 1e-3);
        w.native = 2.0;
        w.frame(Vec::new());
        assert_eq!(w.zoom(), 1.0);
    }

    #[test]
    fn shortcuts_step_and_reset_the_zoom() {
        let mut w = Window::new(egui::vec2(1600.0, 900.0), 1.0);
        w.frame(Vec::new());
        assert_eq!(w.zoom(), 1.0);
        w.frame(vec![key(Key::Equals)]);
        assert!((w.zoom() - 1.1).abs() < 1e-5);
        w.frame(vec![key(Key::Plus)]);
        assert!((w.zoom() - 1.21).abs() < 1e-5);
        w.frame(vec![key(Key::Minus)]);
        w.frame(vec![key(Key::Minus)]);
        w.frame(vec![key(Key::Minus)]);
        assert!((w.zoom() - 1.0 / 1.1).abs() < 1e-5);
        w.frame(vec![key(Key::Num0)]);
        assert_eq!(w.zoom(), 1.0);
        // The steps stay on top of the automatic zoom when the window changes.
        w.frame(vec![key(Key::Equals)]);
        w.physical = egui::vec2(3840.0, 2160.0);
        w.frame(Vec::new());
        assert!((w.zoom() - 2.2).abs() < 1e-3, "{}", w.zoom());
        w.frame(vec![key(Key::Num0)]);
        assert!((w.zoom() - 2.0).abs() < 1e-3);
    }
}
