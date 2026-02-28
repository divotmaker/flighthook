pub mod app;
pub mod net;
pub(crate) mod panels;
pub mod types;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

/// Cap pixel ratio on high-DPI mobile devices to prevent WebGL OOM.
/// iOS Safari at 3x DPR creates a ~1170x2532 backing store which can
/// exceed the WebContent process memory budget and get jetsam-killed.
#[cfg(target_arch = "wasm32")]
fn capped_zoom_factor() -> f32 {
    let dpr = web_sys::window().unwrap().device_pixel_ratio() as f32;
    if dpr > 2.0 {
        // Scale egui's zoom so effective DPR stays at 2.0
        2.0 / dpr
    } else {
        1.0
    }
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(start)]
pub fn main() {
    console_error_panic_hook::set_once();
    eframe::WebLogger::init(log::LevelFilter::Warn).ok();

    wasm_bindgen_futures::spawn_local(async {
        let document = web_sys::window().unwrap().document().unwrap();
        let canvas = document
            .get_element_by_id("the_canvas_id")
            .unwrap()
            .dyn_into::<web_sys::HtmlCanvasElement>()
            .unwrap();

        let zoom = capped_zoom_factor();

        eframe::WebRunner::new()
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(move |cc| {
                    cc.egui_ctx.set_zoom_factor(zoom);
                    Ok(Box::new(app::FlighthookApp::new(cc)))
                }),
            )
            .await
            .expect("failed to start eframe");
    });
}
