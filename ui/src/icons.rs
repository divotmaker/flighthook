//! The header's own icons: PNGs embedded at 1x, 2x and 3x, each in a
//! normal state (white, tinted by the caller) and a hover state (brand
//! colors, drawn as they are). A texture is decoded the first time it is
//! drawn and kept for the life of the context; the scale follows the UI's
//! pixels per point, so icons stay crisp at every zoom.

use std::collections::HashMap;

use egui::{Color32, Pos2, Rect, TextureHandle, pos2, vec2};

/// Where a club code or golfer initial goes inside an icon, in logical px
/// at the icon's own size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Label {
    /// Center of the text's cap-height box, from the icon's top left.
    pub(crate) center: [f32; 2],
    /// Height of a capital letter.
    pub(crate) cap_height: f32,
    /// Widest the text may be; wider text is drawn smaller.
    pub(crate) max_width: f32,
}

/// One icon: its file name stem, logical size and embedded PNGs.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Asset {
    pub(crate) name: &'static str,
    /// Logical size of the square icon at 1x.
    pub(crate) size: f32,
    /// `[normal, hover]`, each at 1x, 2x and 3x.
    files: [[&'static [u8]; 3]; 2],
    pub(crate) label: Option<Label>,
}

macro_rules! asset {
    ($name:literal, $size:expr) => {
        asset!($name, $size, None)
    };
    ($name:literal, $size:expr, $label:expr) => {
        Asset {
            name: $name,
            size: $size,
            files: [
                [
                    include_bytes!(concat!("../assets/icons/", $name, "-normal@1x.png")),
                    include_bytes!(concat!("../assets/icons/", $name, "-normal@2x.png")),
                    include_bytes!(concat!("../assets/icons/", $name, "-normal@3x.png")),
                ],
                [
                    include_bytes!(concat!("../assets/icons/", $name, "-hover@1x.png")),
                    include_bytes!(concat!("../assets/icons/", $name, "-hover@2x.png")),
                    include_bytes!(concat!("../assets/icons/", $name, "-hover@3x.png")),
                ],
            ],
            label: $label,
        }
    };
}

const fn club_label(x: f32, y: f32, max_width: f32) -> Option<Label> {
    Some(Label {
        center: [x, y],
        cap_height: 7.47,
        max_width,
    })
}

pub(crate) const NAV_SHOTS: Asset = asset!("nav-shots", 24.0);
pub(crate) const NAV_CAMERAS: Asset = asset!("nav-cameras", 24.0);
pub(crate) const NAV_SETTINGS: Asset = asset!("nav-settings", 24.0);
pub(crate) const NAV_LOG: Asset = asset!("nav-log", 24.0);
pub(crate) const TOURNAMENT_OFF: Asset = asset!("tournament-off", 24.0);
pub(crate) const TOURNAMENT_ON: Asset = asset!("tournament-on", 24.0);
pub(crate) const TOURNAMENT_HELD: Asset = asset!("tournament-held", 24.0);
pub(crate) const AUTO_CHIP_OFF: Asset = asset!("auto-chip-off", 24.0);
pub(crate) const AUTO_CHIP_ON: Asset = asset!("auto-chip-on", 24.0);
pub(crate) const ENERGY: Asset = asset!("energy", 24.0);
pub(crate) const ENERGY_FILLED: Asset = asset!("energy-filled", 24.0);
pub(crate) const FOLLOW_ON: Asset = asset!("follow-on", 24.0);
pub(crate) const FOLLOW_OFF: Asset = asset!("follow-off", 24.0);
pub(crate) const SHOT_FULL: Asset = asset!("shot-type-full", 24.0);
pub(crate) const SHOT_CHIP: Asset = asset!("shot-type-chip", 24.0);
pub(crate) const SHOT_PUTT: Asset = asset!("shot-type-putt", 24.0);
pub(crate) const CLUB_NONE: Asset = asset!("club-none", 32.0);
pub(crate) const CLUB_DRIVER: Asset = asset!("club-driver", 32.0, club_label(13.87, 17.76, 16.53));
pub(crate) const CLUB_WOOD: Asset = asset!("club-wood", 32.0, club_label(13.87, 20.16, 15.2));
pub(crate) const CLUB_HYBRID: Asset = asset!("club-hybrid", 32.0, club_label(12.27, 20.16, 13.33));
pub(crate) const CLUB_IRON: Asset = asset!("club-iron", 32.0, club_label(13.07, 19.36, 12.8));
pub(crate) const CLUB_WEDGE: Asset = asset!("club-wedge", 32.0, club_label(12.8, 18.4, 13.33));
pub(crate) const CLUB_PUTTER: Asset = asset!("club-putter", 32.0, club_label(13.33, 21.2, 13.87));
pub(crate) const GOLFER: Asset = asset!(
    "golfer",
    28.0,
    Some(Label {
        center: [14.0, 14.12],
        cap_height: 10.97,
        max_width: 14.0,
    })
);
pub(crate) const GOLFER_NONE: Asset = asset!("golfer-none", 28.0);

/// Every icon, for the test that decodes them all.
#[cfg(test)]
pub(crate) const ALL: [Asset; 25] = [
    NAV_SHOTS,
    NAV_CAMERAS,
    NAV_SETTINGS,
    NAV_LOG,
    TOURNAMENT_OFF,
    TOURNAMENT_ON,
    TOURNAMENT_HELD,
    AUTO_CHIP_OFF,
    AUTO_CHIP_ON,
    ENERGY,
    ENERGY_FILLED,
    FOLLOW_ON,
    FOLLOW_OFF,
    SHOT_FULL,
    SHOT_CHIP,
    SHOT_PUTT,
    CLUB_NONE,
    CLUB_DRIVER,
    CLUB_WOOD,
    CLUB_HYBRID,
    CLUB_IRON,
    CLUB_WEDGE,
    CLUB_PUTTER,
    GOLFER,
    GOLFER_NONE,
];

/// Cap height of the proportional font per point of font size (its
/// capital H is 70 px tall at 100 px).
const CAP_PER_SIZE: f32 = 0.7;

/// The PNG scale (1, 2 or 3) for `pixels_per_point`: rounded up, at most 3.
#[must_use]
pub(crate) fn scale_for(pixels_per_point: f32) -> usize {
    // A hair under a whole number counts as that number.
    (pixels_per_point - 0.01).ceil().clamp(1.0, 3.0) as usize
}

/// Where the decoded textures are kept in the context's data.
const TEXTURES: &str = "header-icons";

/// The decoded textures, by file name.
#[derive(Clone, Default)]
pub(crate) struct Textures(pub(crate) HashMap<String, TextureHandle>);

/// Decode an embedded PNG to an image.
pub(crate) fn decode(bytes: &[u8]) -> Result<egui::ColorImage, png::DecodingError> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(
        png::Transformations::normalize_to_color8() | png::Transformations::ALPHA,
    );
    let mut reader = decoder.read_info()?;
    let mut buf = vec![0; reader.output_buffer_size().unwrap_or(0)];
    let info = reader.next_frame(&mut buf)?;
    let size = [info.width as usize, info.height as usize];
    let pixels = &buf[..info.buffer_size()];
    Ok(match info.color_type {
        png::ColorType::GrayscaleAlpha => {
            let rgba: Vec<u8> = pixels
                .chunks_exact(2)
                .flat_map(|p| [p[0], p[0], p[0], p[1]])
                .collect();
            egui::ColorImage::from_rgba_unmultiplied(size, &rgba)
        }
        _ => egui::ColorImage::from_rgba_unmultiplied(size, pixels),
    })
}

/// The texture of `asset` in its hover or normal state at the scale for
/// the pixels per point of an icon drawn `size` points square; `None` if
/// the PNG doesn't decode.
fn texture(ctx: &egui::Context, asset: &Asset, size: f32, hover: bool) -> Option<TextureHandle> {
    let scale = scale_for(ctx.pixels_per_point() * size / asset.size);
    let state = if hover { "hover" } else { "normal" };
    let key = format!("{}-{state}@{scale}x", asset.name);
    let id = egui::Id::new(TEXTURES);
    if let Some(t) = ctx.data(|d| {
        d.get_temp::<Textures>(id)
            .and_then(|t| t.0.get(&key).cloned())
    }) {
        return Some(t);
    }
    let image = decode(asset.files[usize::from(hover)][scale - 1]).ok()?;
    let handle = ctx.load_texture(&key, image, egui::TextureOptions::LINEAR);
    ctx.data_mut(|d| {
        d.get_temp_mut_or_default::<Textures>(id)
            .0
            .insert(key, handle.clone());
    });
    Some(handle)
}

/// Draw `asset` `size` points square centered on `center`: the hover state
/// as drawn when `hover` (multiplied by `tint`, white for none), else the
/// normal state tinted. Returns the rect drawn in.
pub(crate) fn paint(
    ui: &egui::Ui,
    center: Pos2,
    asset: &Asset,
    size: f32,
    hover: bool,
    tint: Color32,
) -> Rect {
    let rect = Rect::from_center_size(center, vec2(size, size));
    if let Some(t) = texture(ui.ctx(), asset, size, hover) {
        ui.painter().image(
            t.id(),
            rect,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            tint,
        );
    }
    rect
}

/// Draw `text` (a club code or an initial) in `asset`'s label block, the
/// icon drawn `size` points square in `rect`; text wider than the block's
/// width is drawn smaller to fit.
pub(crate) fn paint_label(ui: &egui::Ui, rect: Rect, asset: &Asset, text: &str, color: Color32) {
    let Some(label) = asset.label else { return };
    let k = rect.width() / asset.size;
    let mut font = egui::FontId::proportional(label.cap_height * k / CAP_PER_SIZE);
    let painter = ui.painter();
    let mut galley = painter.layout_no_wrap(text.to_owned(), font.clone(), color);
    let max = label.max_width * k;
    if galley.size().x > max {
        font.size *= max / galley.size().x;
        galley = painter.layout_no_wrap(text.to_owned(), font.clone(), color);
    }
    let cap = font.size * CAP_PER_SIZE;
    let baseline = galley
        .rows
        .first()
        .and_then(|r| r.glyphs.first().map(|g| r.pos.y + g.pos.y))
        .unwrap_or(galley.size().y);
    let center = rect.min + vec2(label.center[0], label.center[1]) * k;
    let at = pos2(
        center.x - galley.size().x / 2.0,
        center.y + cap / 2.0 - baseline,
    );
    painter.galley(at, galley, color);
}

/// The file name (without `.png`) of every texture loaded so far, by id.
#[cfg(test)]
pub(crate) fn loaded(ctx: &egui::Context) -> HashMap<egui::TextureId, String> {
    ctx.data(|d| d.get_temp::<Textures>(egui::Id::new(TEXTURES)))
        .unwrap_or_default()
        .0
        .into_iter()
        .map(|(name, t)| (t.id(), name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every PNG the code names decodes, at its logical size times its
    /// scale.
    #[test]
    fn every_icon_file_decodes_at_its_size() {
        for asset in &ALL {
            for (state, files) in asset.files.iter().enumerate() {
                for (i, bytes) in files.iter().enumerate() {
                    let image = decode(bytes)
                        .unwrap_or_else(|e| panic!("{} {state} @{}x: {e}", asset.name, i + 1));
                    let side = (asset.size as usize) * (i + 1);
                    assert_eq!(
                        image.size,
                        [side, side],
                        "{} {state} @{}x",
                        asset.name,
                        i + 1
                    );
                }
            }
        }
    }

    /// The scale rounds the pixels per point up and stops at 3.
    #[test]
    fn the_scale_follows_the_pixels_per_point() {
        for (ppp, scale) in [(1.0, 1), (1.5, 2), (2.0, 2), (3.0, 3), (4.0, 3), (0.8, 1)] {
            assert_eq!(scale_for(ppp), scale, "{ppp}");
        }
    }

    /// The scale follows the drawn size: a 44 point avatar of a 28 point
    /// icon at zoom 1 takes the 2x file, a 16 point icon of a 24 point one
    /// the 1x file.
    #[test]
    fn the_scale_follows_the_drawn_size() {
        for (ppp, size, base, scale) in [
            (1.0, 44.0, 28.0, 2),
            (1.0, 28.0, 28.0, 1),
            (1.0, 16.0, 24.0, 1),
            (2.0, 24.0, 24.0, 2),
            (2.0, 44.0, 28.0, 3),
        ] {
            assert_eq!(scale_for(ppp * size / base), scale, "{ppp} {size} {base}");
        }
    }
}
