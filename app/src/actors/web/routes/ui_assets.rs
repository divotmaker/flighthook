//! The embedded dashboard (built by `make ui` in flighthook/ui/): `/`, the JS and WASM bundles and the icons.

use super::*;

pub(super) const UI_HTML: &str = include_str!("../../../../../ui/dist/index.html");
pub(super) const UI_JS: &[u8] = include_bytes!("../../../../../ui/dist/flighthook-ui.js");
pub(super) const UI_WASM: &[u8] = include_bytes!("../../../../../ui/dist/flighthook-ui_bg.wasm");
pub(super) const UI_FAVICON_ICO: &[u8] = include_bytes!("../../../../../ui/dist/favicon.ico");
pub(super) const UI_FAVICON_PNG: &[u8] = include_bytes!("../../../../../ui/dist/favicon-32.png");
pub(super) const UI_APPLE_TOUCH_ICON: &[u8] =
    include_bytes!("../../../../../ui/dist/apple-touch-icon.png");

/// GET / — serve the egui dashboard HTML.
pub async fn get_ui_html() -> Response {
    Response::builder()
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        )
        .body(Body::from(UI_HTML))
        .unwrap()
}

/// GET /flighthook-ui.js — serve the WASM JS glue.
pub async fn get_ui_js() -> Response {
    Response::builder()
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/javascript"),
        )
        .body(Body::from(UI_JS))
        .unwrap()
}

/// GET /flighthook-ui_bg.wasm — serve the WASM binary.
pub async fn get_ui_wasm() -> Response {
    Response::builder()
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/wasm"),
        )
        .body(Body::from(UI_WASM))
        .unwrap()
}

/// GET /favicon.ico — the browser tab icon.
pub async fn get_favicon_ico() -> Response {
    icon_response("image/x-icon", UI_FAVICON_ICO)
}

/// GET /favicon-32.png — the tab icon for browsers that prefer PNG.
pub async fn get_favicon_png() -> Response {
    icon_response("image/png", UI_FAVICON_PNG)
}

/// GET /apple-touch-icon.png — the home-screen icon on iOS.
pub async fn get_apple_touch_icon() -> Response {
    icon_response("image/png", UI_APPLE_TOUCH_ICON)
}

pub(super) fn icon_response(content_type: &'static str, body: &'static [u8]) -> Response {
    Response::builder()
        .header(header::CONTENT_TYPE, HeaderValue::from_static(content_type))
        .body(Body::from(body))
        .unwrap()
}
