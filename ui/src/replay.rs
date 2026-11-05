//! Swing replay logic: the cameras' timelines on one host clock, the
//! playhead and its speeds, frame stepping, which bytes of a clip to fetch
//! around the playhead, and the bounded caches of fetched bytes and decoded
//! frames. No egui and no network here: `panels::replay` draws it and feeds
//! it the server's replies.

use std::collections::HashMap;

use stance::sidecar::{Sidecar, SidecarFrame};

/// One camera's clip as its sidecar describes it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Track {
    /// Frame size in pixels.
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// Host time of the first frame (ns).
    pub(crate) start_ns: u64,
    /// Every frame: its time after the first and its bytes in the AVI.
    pub(crate) frames: Vec<SidecarFrame>,
    /// The time the launch monitor reported the shot, host ns.
    pub(crate) trigger_ns: u64,
    /// The estimated impact, host ns, when the clip has one. A clip whose
    /// impact precedes it clamps to its first frame.
    pub(crate) impact_ns: Option<u64>,
}

impl Track {
    /// The track a sidecar describes.
    pub(crate) fn from_sidecar(s: &Sidecar) -> Self {
        Self {
            width: s.width,
            height: s.height,
            start_ns: s.start_ns,
            frames: s.frames.clone(),
            trigger_ns: s.start_ns.saturating_add_signed(s.trigger_ns),
            impact_ns: Some(s.start_ns.saturating_add_signed(s.impact_ns)),
        }
    }

    /// Host time of frame `i` (ns).
    pub(crate) fn time(&self, i: usize) -> u64 {
        self.start_ns
            .saturating_add(self.frames.get(i).map_or(0, |f| f.t_ns))
    }

    /// Host time of the last frame (ns).
    pub(crate) fn end_ns(&self) -> u64 {
        self.time(self.frames.len().saturating_sub(1))
    }

    /// The frame shown at host time `t`: the last frame at or before it, or
    /// the first frame before the clip starts.
    pub(crate) fn frame_at(&self, t: u64) -> usize {
        self.frames
            .partition_point(|f| self.start_ns.saturating_add(f.t_ns) <= t)
            .saturating_sub(1)
    }

    /// The time after the first frame (the sidecar's `t_ns`) of the frame
    /// shown at host time `t` ([`Track::frame_at`]); `None` before the
    /// first frame or after the last, where the clip has no frame of that
    /// moment.
    pub(crate) fn shown_t_ns(&self, t: u64) -> Option<u64> {
        if self.frames.is_empty() || t < self.time(0) || t > self.end_ns() {
            return None;
        }
        self.frames.get(self.frame_at(t)).map(|f| f.t_ns)
    }
}

/// The span every track covers, host ns.
pub(crate) fn span(tracks: &[Track]) -> Option<(u64, u64)> {
    let start = tracks
        .iter()
        .filter(|t| !t.frames.is_empty())
        .map(|t| t.start_ns)
        .min()?;
    let end = tracks
        .iter()
        .filter(|t| !t.frames.is_empty())
        .map(Track::end_ns)
        .max()?;
    Some((start, end))
}

/// The impact the scrubber marks: the first track's estimate. Every camera
/// stamps the same trigger on its own, so the estimates agree within a
/// frame.
pub(crate) fn impact(tracks: &[Track]) -> Option<u64> {
    tracks
        .iter()
        .find(|t| !t.frames.is_empty())
        .and_then(|t| t.impact_ns)
}

/// The mark the first track holds for the launch monitor's report.
fn trigger(tracks: &[Track]) -> Option<u64> {
    tracks
        .iter()
        .find(|t| !t.frames.is_empty())
        .map(|t| t.trigger_ns)
}

/// Where a replay opens: `before_s` seconds before the estimated impact,
/// else before the trigger mark when the clip has no impact estimate, else
/// the clip start. Never before the start of `span` nor after its end.
pub(crate) fn start_point(tracks: &[Track], span: (u64, u64), before_s: f64) -> u64 {
    // Seconds in 0..=10 are far from overflowing; anything else is held to it.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let before = (before_s.clamp(0.0, 10.0) * 1e9) as u64;
    impact(tracks)
        .or_else(|| trigger(tracks))
        .map_or(span.0, |anchor| {
            anchor.saturating_sub(before).clamp(span.0, span.1)
        })
}

/// Playback speed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Speed {
    #[default]
    Full,
    Half,
    Quarter,
    Eighth,
}

impl Speed {
    /// Every speed, in the order the control shows them.
    pub(crate) const ALL: [Speed; 4] = [Speed::Full, Speed::Half, Speed::Quarter, Speed::Eighth];

    /// The control's label.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Speed::Full => "1×",
            Speed::Half => "½",
            Speed::Quarter => "¼",
            Speed::Eighth => "⅛",
        }
    }

    /// Host time played per second of wall time.
    pub(crate) fn factor(self) -> f64 {
        match self {
            Speed::Full => 1.0,
            Speed::Half => 0.5,
            Speed::Quarter => 0.25,
            Speed::Eighth => 0.125,
        }
    }
}

/// The playhead over a clip span.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Playback {
    /// Host time shown (ns).
    pub(crate) playhead: u64,
    /// Where play restarts once the playhead reached the end.
    pub(crate) start: u64,
    pub(crate) playing: bool,
    pub(crate) speed: Speed,
    /// Playing past the end of the clip goes on from the start point
    /// instead of stopping.
    pub(crate) looping: bool,
}

impl Playback {
    /// Paused at `at`, which is also where play restarts from.
    pub(crate) fn paused_at(at: u64) -> Self {
        Self {
            playhead: at,
            start: at,
            playing: false,
            speed: Speed::default(),
            looping: true,
        }
    }

    /// Advance by `dt` seconds of wall time while playing. At the end of
    /// `span` it goes on from the start point when looping, else stops
    /// there. The playhead follows the wall clock however
    /// long a screen update took, so a slow update skips frames instead of
    /// slowing the clip down.
    pub(crate) fn advance(&mut self, dt: f64, span: (u64, u64)) {
        if !self.playing {
            return;
        }
        // Wall steps are a frame long at most; the cast cannot overflow.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let step = (dt.max(0.0) * 1e9 * self.speed.factor()) as u64;
        self.playhead = self.playhead.saturating_add(step);
        if self.playhead >= span.1 {
            if self.looping {
                self.playhead = self.start.clamp(span.0, span.1);
            } else {
                self.playhead = span.1;
                self.playing = false;
            }
        }
    }

    /// Play from where it stands, or from the start point once it reached
    /// the end.
    pub(crate) fn toggle(&mut self, span: (u64, u64)) {
        if !self.playing && self.playhead >= span.1 {
            self.playhead = self.start.clamp(span.0, span.1);
        }
        self.playing = !self.playing;
    }

    /// Move to `t`, clamped to `span`.
    pub(crate) fn seek(&mut self, t: u64, span: (u64, u64)) {
        self.playhead = t.clamp(span.0, span.1);
    }

    /// Pause and move one frame: to the next frame time of any camera after
    /// the playhead (`forward`), or the latest one before it. Every step
    /// changes at least one camera's picture.
    pub(crate) fn step(&mut self, tracks: &[Track], forward: bool) {
        self.playing = false;
        let times = tracks
            .iter()
            .flat_map(|t| (0..t.frames.len()).map(move |i| t.time(i)));
        let next = if forward {
            times.filter(|t| *t > self.playhead).min()
        } else {
            times.filter(|t| *t < self.playhead).max()
        };
        if let Some(t) = next {
            self.playhead = t;
        }
    }
}

/// How far behind the playhead a fetch window starts (ns).
pub(crate) const WINDOW_BEHIND_NS: u64 = 100_000_000;
/// How far ahead of the playhead a fetch window reaches (ns).
pub(crate) const WINDOW_AHEAD_NS: u64 = 400_000_000;
/// Most bytes one range request asks for.
pub(crate) const WINDOW_MAX_BYTES: u64 = 8 << 20;

/// A run of consecutive frames fetched with one range request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Window {
    /// First frame.
    pub(crate) first: usize,
    /// Last frame (inclusive).
    pub(crate) last: usize,
}

impl Window {
    /// Its bytes in the AVI: `(offset, end)`, `end` exclusive.
    pub(crate) fn bytes(self, track: &Track) -> (u64, u64) {
        (
            track.frames[self.first].offset,
            track.frames[self.last].end(),
        )
    }

    pub(crate) fn contains(self, frame: usize) -> bool {
        (self.first..=self.last).contains(&frame)
    }
}

/// The window to fetch for frame `frame`: from [`WINDOW_BEHIND_NS`] before
/// it to [`WINDOW_AHEAD_NS`] after, cut to [`WINDOW_MAX_BYTES`] (always at
/// least the frame itself).
pub(crate) fn window_for(track: &Track, frame: usize) -> Option<Window> {
    if frame >= track.frames.len() {
        return None;
    }
    let t = track.time(frame);
    let from = t.saturating_sub(WINDOW_BEHIND_NS);
    let to = t.saturating_add(WINDOW_AHEAD_NS);
    let mut first = frame;
    while first > 0 && track.time(first - 1) >= from {
        first -= 1;
    }
    let mut last = frame;
    while last + 1 < track.frames.len() && track.time(last + 1) <= to {
        last += 1;
    }
    let size = |a: usize, b: usize| {
        Window { first: a, last: b }
            .bytes(track)
            .1
            .saturating_sub(track.frames[a].offset)
    };
    while last > frame && size(first, last) > WINDOW_MAX_BYTES {
        last -= 1;
    }
    while first < frame && size(first, last) > WINDOW_MAX_BYTES {
        first += 1;
    }
    Some(Window { first, last })
}

/// One camera's fetched bytes: whole windows, the oldest-used dropped once
/// they hold more than `max_bytes`.
#[derive(Debug, Default)]
pub(crate) struct ByteCache {
    chunks: Vec<(Window, Vec<u8>, u64)>,
    max_bytes: usize,
    tick: u64,
}

impl ByteCache {
    pub(crate) fn new(max_bytes: usize) -> Self {
        Self {
            chunks: Vec::new(),
            max_bytes,
            tick: 0,
        }
    }

    /// Keep `bytes`, the reply for `window`. A reply shorter than the window
    /// keeps the frames it holds whole.
    pub(crate) fn insert(&mut self, window: Window, bytes: Vec<u8>) {
        self.tick += 1;
        self.chunks.retain(|(w, _, _)| *w != window);
        self.chunks.push((window, bytes, self.tick));
        while self.held() > self.max_bytes && self.chunks.len() > 1 {
            let oldest = self
                .chunks
                .iter()
                .enumerate()
                .min_by_key(|(_, (_, _, used))| *used)
                .map(|(i, _)| i);
            if let Some(i) = oldest {
                self.chunks.remove(i);
            }
        }
    }

    /// Bytes held.
    pub(crate) fn held(&self) -> usize {
        self.chunks.iter().map(|(_, b, _)| b.len()).sum()
    }

    /// Whether frame `frame` is held.
    pub(crate) fn has(&self, frame: usize) -> bool {
        self.chunks.iter().any(|(w, _, _)| w.contains(frame))
    }

    /// Frame `frame`'s JPEG, if a held window has all of it.
    pub(crate) fn frame(&mut self, track: &Track, frame: usize) -> Option<&[u8]> {
        self.tick += 1;
        let f = track.frames.get(frame)?;
        let (w, bytes, used) = self.chunks.iter_mut().find(|(w, b, _)| {
            w.contains(frame)
                && track
                    .frames
                    .get(w.first)
                    .is_some_and(|first| f.end().saturating_sub(first.offset) <= b.len() as u64)
        })?;
        *used = self.tick;
        let base = track.frames[w.first].offset;
        let start = usize::try_from(f.offset.saturating_sub(base)).ok()?;
        let end = usize::try_from(f.end().saturating_sub(base)).ok()?;
        bytes.get(start..end)
    }
}

/// Least-recently-used map with a fixed capacity.
#[derive(Debug)]
pub(crate) struct Lru<K, V> {
    entries: HashMap<K, (V, u64)>,
    capacity: usize,
    tick: u64,
}

impl<K: std::hash::Hash + Eq + Clone, V> Lru<K, V> {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            capacity: capacity.max(1),
            tick: 0,
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Most entries it holds.
    #[cfg(test)]
    pub(crate) fn capacity(&self) -> usize {
        self.capacity
    }

    /// Whether `key` is held (not marked as used).
    pub(crate) fn contains(&self, key: &K) -> bool {
        self.entries.contains_key(key)
    }

    /// The value for `key`, marked as used.
    pub(crate) fn get(&mut self, key: &K) -> Option<&V> {
        self.tick += 1;
        let tick = self.tick;
        self.entries.get_mut(key).map(|(v, used)| {
            *used = tick;
            &*v
        })
    }

    /// Insert, dropping the least recently used entry when full.
    pub(crate) fn insert(&mut self, key: K, value: V) {
        self.tick += 1;
        if !self.entries.contains_key(&key) && self.entries.len() >= self.capacity {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(k, _)| k.clone());
            if let Some(k) = oldest {
                self.entries.remove(&k);
            }
        }
        self.entries.insert(key, (value, self.tick));
    }
}

/// The centre of a `src`-sized picture (width, height) cut to the aspect of
/// `dst`: `[x, y, width, height]` in source pixels.
pub(crate) fn crop(src: [usize; 2], dst: [usize; 2]) -> [usize; 4] {
    let [sw, sh] = src;
    let [dw, dh] = [dst[0].max(1), dst[1].max(1)];
    // Source pixels counts and sizes stay far inside f64's exact range.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let scaled = |n: usize, num: usize, den: usize| -> usize {
        ((n as f64 * num as f64 / den as f64).round() as usize).max(1)
    };
    if sw * dh > dw * sh {
        // Wider than the card: cut the sides.
        let w = scaled(sh, dw, dh).min(sw);
        [(sw - w) / 2, 0, w, sh]
    } else {
        // Taller: cut the top and bottom.
        let h = scaled(sw, dh, dw).min(sh);
        [0, (sh - h) / 2, sw, h]
    }
}

/// The `crop` (`[x, y, width, height]`) of an RGB picture `width` pixels
/// wide, scaled to `dst` (width, height): each pixel the average of the
/// source pixels under it, or the nearest one when scaling up.
pub(crate) fn crop_scale(rgb: &[u8], width: usize, crop: [usize; 4], dst: [usize; 2]) -> Vec<u8> {
    let [x0, y0, cw, ch] = crop;
    let [dw, dh] = dst;
    let mut out = vec![0; dw * dh * 3];
    let span = |i: usize, n: usize, d: usize| {
        let a = i * n / d;
        (a, ((i + 1) * n / d).max(a + 1).min(n))
    };
    for dy in 0..dh {
        let (ya, yb) = span(dy, ch, dh);
        for dx in 0..dw {
            let (xa, xb) = span(dx, cw, dw);
            let mut sum = [0_u32; 3];
            let mut n = 0_u32;
            for y in ya..yb {
                let row = (y0 + y) * width;
                for x in xa..xb {
                    let at = (row + x0 + x) * 3;
                    if let Some(px) = rgb.get(at..at + 3) {
                        for (s, &v) in sum.iter_mut().zip(px) {
                            *s += u32::from(v);
                        }
                        n += 1;
                    }
                }
            }
            let at = (dy * dw + dx) * 3;
            for (o, s) in out[at..at + 3].iter_mut().zip(sum) {
                // An average of bytes is a byte.
                #[allow(clippy::cast_possible_truncation)]
                {
                    *o = (s / n.max(1)) as u8;
                }
            }
        }
    }
    out
}

/// The pixel size of a picture `size` points (width, height) at
/// `pixels_per_point`: each side rounded up to the next even number.
pub(crate) fn card_pixels(size: [f32; 2], pixels_per_point: f32) -> [usize; 2] {
    size.map(|points| {
        // A card's side in pixels is far inside usize.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let n = (points * pixels_per_point).ceil().max(2.0) as usize;
        n + n % 2
    })
}

/// Decode an MJPEG frame to RGB: `(width, height, pixels)`. Webcam frames
/// without Huffman tables get the standard ones first.
pub(crate) fn decode(jpeg: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
    use zune_jpeg::JpegDecoder;
    use zune_jpeg::zune_core::bytestream::ZCursor;
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;

    let jpeg = stance::jpeg::ensure_huffman(jpeg);
    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(jpeg.as_ref()), options);
    let pixels = decoder.decode().ok()?;
    let (w, h) = decoder.dimensions()?;
    (pixels.len() == w * h * 3).then_some((w, h, pixels))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const MS: u64 = 1_000_000;

    /// A track starting at `start` ms with `n` frames `period` ms apart,
    /// each `len` bytes, packed one after another from offset 232 with 8
    /// bytes of chunk header between them.
    pub(crate) fn track(start: u64, period: u64, n: u64, len: u32) -> Track {
        let mut offset = 232;
        let frames = (0..n)
            .map(|i| {
                let f = SidecarFrame {
                    t_ns: i * period * MS,
                    offset,
                    len,
                };
                offset += u64::from(len) + 8;
                f
            })
            .collect();
        Track {
            width: 64,
            height: 48,
            start_ns: start * MS,
            frames,
            trigger_ns: (start + period * n * 3 / 4) * MS,
            impact_ns: Some((start + period * n / 2) * MS),
        }
    }

    #[test]
    fn the_frame_shown_is_known_only_inside_the_clip() {
        // A 30 fps camera from 1010 ms to 1010 + 33 × 49 ms.
        let slow = track(1010, 33, 50, 100);
        assert_eq!(slow.shown_t_ns(1005 * MS), None, "before its first frame");
        assert_eq!(slow.shown_t_ns(1010 * MS), Some(0));
        // Between frames: the frame on screen, the last one at or before.
        assert_eq!(slow.shown_t_ns(1100 * MS), Some(66 * MS));
        assert_eq!(
            slow.shown_t_ns(1100 * MS).map(|t| slow.start_ns + t),
            Some(slow.time(slow.frame_at(1100 * MS)))
        );
        assert_eq!(slow.shown_t_ns(slow.end_ns()), Some(33 * 49 * MS));
        assert_eq!(
            slow.shown_t_ns(slow.end_ns() + 1),
            None,
            "after its last frame"
        );
        let mut empty = slow;
        empty.frames.clear();
        assert_eq!(empty.shown_t_ns(1010 * MS), None);
    }

    #[test]
    fn two_cameras_sync_on_host_time() {
        // A 120 fps camera that started at 1000 ms and a 30 fps one at 1010 ms.
        let fast = track(1000, 8, 200, 100);
        let slow = track(1010, 33, 50, 100);
        let tracks = [fast.clone(), slow.clone()];
        assert_eq!(span(&tracks), Some((1000 * MS, (1010 + 33 * 49) * MS)));
        // Before the slow camera starts, it shows its first frame.
        assert_eq!(slow.frame_at(1005 * MS), 0);
        // Each camera shows its last frame at or before the playhead.
        let t = 1100 * MS;
        assert_eq!(fast.frame_at(t), 12); // 1096 ms
        assert_eq!(slow.frame_at(t), 2); // 1076 ms
        assert_eq!(fast.frame_at(1104 * MS), 13); // exactly on a frame
        // Past its end, the last frame.
        assert_eq!(fast.frame_at(9000 * MS), 199);
        assert_eq!(impact(&tracks), fast.impact_ns);
    }

    #[test]
    fn sidecar_impact_becomes_host_time() {
        let json = r#"{"version":1,"fourcc":"MJPG","width":8,"height":8,
            "fps":{"nominal":null,"measured":100.0},"start_ns":5000000000,
            "frames":[{"t_ns":0,"offset":232,"len":10},{"t_ns":10000000,"offset":250,"len":10}],
            "trigger_index":1,"impact_index":0,"trigger_ns":10000000,"impact_ns":-2000000,
            "complete":true,"dropped":0,"truncated":false}"#;
        let t = Track::from_sidecar(&Sidecar::from_json(json).unwrap());
        assert_eq!(t.start_ns, 5_000_000_000);
        assert_eq!(t.impact_ns, Some(4_998_000_000));
        assert_eq!(t.trigger_ns, 5_010_000_000);
        assert_eq!(t.end_ns(), 5_010_000_000);
    }

    #[test]
    fn speeds_scale_the_playhead_and_play_stops_at_the_end() {
        let span = (0, 1000 * MS);
        let mut p = Playback::paused_at(0);
        p.looping = false;
        p.advance(0.1, span);
        assert_eq!(p.playhead, 0, "paused");
        p.toggle(span);
        for (speed, ms) in [
            (Speed::Full, 100),
            (Speed::Half, 50),
            (Speed::Quarter, 25),
            (Speed::Eighth, 12),
        ] {
            p.speed = speed;
            let before = p.playhead;
            p.advance(0.1, span);
            assert_eq!((p.playhead - before) / MS, ms, "{speed:?}");
        }
        p.speed = Speed::Full;
        p.advance(5.0, span);
        assert_eq!(p.playhead, span.1);
        assert!(!p.playing);
        // Play again from the start point (here the clip start).
        p.toggle(span);
        assert!(p.playing);
        assert_eq!(p.playhead, 0);
        p.seek(5000 * MS, span);
        assert_eq!(p.playhead, span.1);
        assert_eq!(Speed::ALL.map(Speed::label), ["1×", "½", "¼", "⅛"]);
    }

    #[test]
    fn a_replay_starts_before_the_impact_and_never_before_the_clip() {
        // 10 ms frames from 1000 ms to 3000 ms; the impact at 2000 ms.
        let t = track(1000, 10, 200, 50);
        let tracks = [t.clone()];
        let span = span(&tracks).unwrap();
        assert_eq!(t.impact_ns, Some(2000 * MS));
        // 2 s before the impact is exactly the clip start.
        assert_eq!(start_point(&tracks, span, 2.0), 1000 * MS);
        // Less than that is later.
        assert_eq!(start_point(&tracks, span, 0.5), 1500 * MS);
        assert_eq!(start_point(&tracks, span, 0.0), 2000 * MS);
        // More than the clip holds clamps to the clip start.
        assert_eq!(start_point(&tracks, span, 7.5), span.0);
        // Out-of-range or non-finite values stay in the clip.
        assert_eq!(start_point(&tracks, span, -1.0), 2000 * MS);
        assert_eq!(start_point(&tracks, span, f64::INFINITY), span.0);
        assert_eq!(start_point(&tracks, span, f64::NAN), 2000 * MS);
        // An impact that precedes the clip clamps to its start.
        let mut early = t.clone();
        early.impact_ns = Some(500 * MS);
        let tracks = [early];
        assert_eq!(start_point(&tracks, span, 1.0), span.0);
        assert_eq!(start_point(&tracks, span, 0.0), span.0);
    }

    #[test]
    fn a_replay_without_an_impact_starts_before_the_trigger_then_at_the_clip_start() {
        let mut t = track(1000, 10, 200, 50);
        t.impact_ns = None;
        t.trigger_ns = 2500 * MS;
        let tracks = [t.clone()];
        let span = span(&tracks).unwrap();
        assert_eq!(impact(&tracks), None);
        assert_eq!(start_point(&tracks, span, 1.0), 1500 * MS);
        assert_eq!(start_point(&tracks, span, 9.0), span.0);
        // No tracks with frames: the clip start.
        assert_eq!(start_point(&[], (7, 9), 2.0), 7);
        t.frames.clear();
        assert_eq!(start_point(&[t], (7, 9), 2.0), 7);
    }

    #[test]
    fn play_after_the_end_restarts_from_the_start_point() {
        let span = (1000 * MS, 3000 * MS);
        let mut p = Playback::paused_at(1500 * MS);
        p.looping = false;
        p.toggle(span);
        assert_eq!(p.playhead, 1500 * MS, "play from where it stands");
        p.advance(5.0, span);
        assert_eq!((p.playhead, p.playing), (span.1, false));
        p.toggle(span);
        assert!(p.playing);
        assert_eq!(p.playhead, 1500 * MS, "not the clip start");
        // Scrubbing to the very start works, and playing on from there goes on.
        p.playing = false;
        p.seek(0, span);
        assert_eq!(p.playhead, span.0);
        p.toggle(span);
        assert_eq!(p.playhead, span.0);
        assert!(p.playing);
        // Pausing mid-clip and playing again does not jump back.
        p.advance(0.5, span);
        p.toggle(span);
        p.toggle(span);
        assert_eq!(p.playhead, span.0 + 500 * MS);
        // A frame step keeps the restart point.
        let t = track(1000, 10, 200, 50);
        p.step(&[t], true);
        assert!(!p.playing);
        p.toggle(span);
        p.advance(5.0, span);
        assert_eq!((p.playhead, p.playing), (span.1, false));
        p.toggle(span);
        assert_eq!(p.playhead, 1500 * MS);
    }

    #[test]
    fn looping_is_on_by_default_and_wraps_to_the_start_point_at_the_current_speed() {
        let span = (1000 * MS, 3000 * MS);
        let mut p = Playback::paused_at(1500 * MS);
        assert!(p.looping);
        p.speed = Speed::Half;
        p.toggle(span);
        // Half speed: 3.2 s of wall time plays 1.6 s, to 3100 ms: past the end.
        p.advance(3.2, span);
        assert_eq!(p.playhead, 1500 * MS, "back at the start point");
        assert!(p.playing, "and still playing");
        assert_eq!(p.speed, Speed::Half);
        p.advance(0.2, span);
        assert_eq!(p.playhead, 1600 * MS, "at the current speed");
        // A start point at the clip start wraps there.
        let mut p = Playback::paused_at(span.0);
        p.toggle(span);
        p.advance(5.0, span);
        assert_eq!((p.playhead, p.playing), (span.0, true));
    }

    #[test]
    fn without_looping_play_stops_at_the_end_and_restarts_from_the_start_point() {
        let span = (1000 * MS, 3000 * MS);
        let mut p = Playback::paused_at(1500 * MS);
        p.looping = false;
        p.toggle(span);
        p.advance(5.0, span);
        assert_eq!((p.playhead, p.playing), (span.1, false));
        p.toggle(span);
        assert_eq!((p.playhead, p.playing), (1500 * MS, true));
        // Turning looping on mid-play wraps at the next end.
        p.looping = true;
        p.advance(5.0, span);
        assert_eq!((p.playhead, p.playing), (1500 * MS, true));
    }

    #[test]
    fn a_slow_update_skips_frames_and_keeps_time() {
        // 120 fps: a 100 ms update (a slow phone decode) moves 12 frames.
        let t = track(0, 8, 600, 100);
        let span = span(std::slice::from_ref(&t)).unwrap();
        let mut p = Playback::paused_at(0);
        p.toggle(span);
        p.advance(0.1, span);
        assert_eq!(t.frame_at(p.playhead), 12);
        p.advance(0.016, span);
        assert_eq!(t.frame_at(p.playhead), 14);
    }

    #[test]
    fn speed_labels_have_glyphs() {
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let _ = ctx.run(egui::RawInput::default(), |_| {});
        let font = egui::FontId::proportional(14.0);
        for label in Speed::ALL.map(Speed::label) {
            for c in label.chars() {
                assert!(ctx.fonts_mut(|f| f.has_glyph(&font, c)), "{c}");
            }
        }
    }

    #[test]
    fn a_frame_step_changes_some_camera_and_pauses() {
        let fast = track(0, 10, 10, 100); // 0, 10, ... 90 ms
        let slow = track(5, 30, 4, 100); // 5, 35, 65, 95 ms
        let tracks = [fast, slow];
        let mut p = Playback::paused_at(0);
        p.playing = true;
        let mut seen = Vec::new();
        for _ in 0..6 {
            p.step(&tracks, true);
            seen.push(p.playhead / MS);
        }
        assert!(!p.playing);
        assert_eq!(seen, [5, 10, 20, 30, 35, 40]);
        p.step(&tracks, false);
        assert_eq!(p.playhead / MS, 35);
        p.step(&tracks, false);
        assert_eq!(p.playhead / MS, 30);
        // Nothing before the first frame: it stays.
        p.playhead = 0;
        p.step(&tracks, false);
        assert_eq!(p.playhead, 0);
    }

    #[test]
    fn a_window_covers_the_playhead_and_stays_in_bounds() {
        // 120 fps, 40 kB frames: the window runs 100 ms back, 400 ms ahead.
        let t = track(0, 8, 600, 40_000);
        let w = window_for(&t, 100).unwrap();
        assert!(w.contains(100));
        assert_eq!(t.time(w.first), (100 - 12) * 8 * MS); // 704 ms
        assert_eq!(w.last, 150); // 1200 ms
        let (start, end) = w.bytes(&t);
        assert_eq!(start, t.frames[w.first].offset);
        assert_eq!(end, t.frames[150].offset + 40_000);
        let req = crate::net::range_request("http://h/c.avi", start, end);
        assert_eq!(
            req.headers.get("Range"),
            Some(format!("bytes={start}-{}", end - 1).as_str())
        );
        // Clamped at both ends of the clip.
        let w = window_for(&t, 0).unwrap();
        assert_eq!(w.first, 0);
        let w = window_for(&t, 599).unwrap();
        assert_eq!(w.last, 599);
        assert!(window_for(&t, 600).is_none());
    }

    #[test]
    fn a_window_never_asks_for_more_than_its_byte_cap() {
        // 2 MB frames: only a few fit the cap, but the frame itself always does.
        let t = track(0, 8, 100, 2_000_000);
        let w = window_for(&t, 50).unwrap();
        assert!(w.contains(50));
        let (start, end) = w.bytes(&t);
        assert!(end - start <= WINDOW_MAX_BYTES, "{}", end - start);
        let huge = track(0, 8, 10, 9_000_000);
        let w = window_for(&huge, 5).unwrap();
        assert_eq!((w.first, w.last), (5, 5));
    }

    #[test]
    fn the_byte_cache_slices_frames_and_drops_the_oldest_window() {
        let t = track(0, 10, 30, 10);
        let window = |first, last| Window { first, last };
        let bytes = |w: Window| {
            let (a, b) = w.bytes(&t);
            (a..b).map(|i| (i % 251) as u8).collect::<Vec<u8>>()
        };
        // Room for about two windows of ten frames (each ~172 bytes).
        let mut cache = ByteCache::new(400);
        let w1 = window(0, 9);
        cache.insert(w1, bytes(w1));
        let f3 = t.frames[3];
        let expect: Vec<u8> = (f3.offset..f3.end()).map(|i| (i % 251) as u8).collect();
        assert_eq!(cache.frame(&t, 3), Some(expect.as_slice()));
        assert!(cache.frame(&t, 12).is_none());
        let w2 = window(10, 19);
        cache.insert(w2, bytes(w2));
        // Using w1 makes w2 the oldest.
        assert!(cache.frame(&t, 0).is_some());
        let w3 = window(20, 29);
        cache.insert(w3, bytes(w3));
        assert!(cache.held() <= 400);
        assert!(cache.has(0) && !cache.has(15) && cache.has(25));
        // A short reply keeps only the frames it holds whole.
        let mut short = ByteCache::new(10_000);
        let mut b = bytes(w1);
        b.truncate(40);
        short.insert(w1, b);
        assert!(short.frame(&t, 1).is_some());
        assert!(short.frame(&t, 2).is_none());
    }

    #[test]
    fn the_lru_keeps_its_capacity_and_drops_the_least_used() {
        let mut lru = Lru::new(3);
        for k in 0..3 {
            lru.insert(k, k * 10);
        }
        assert_eq!(lru.get(&0), Some(&0));
        lru.insert(3, 30);
        assert_eq!(lru.len(), 3);
        assert!(lru.get(&1).is_none(), "the least recently used goes");
        assert_eq!(lru.get(&0), Some(&0));
        assert_eq!(lru.get(&3), Some(&30));
        // Replacing a key never evicts.
        lru.insert(3, 31);
        assert_eq!(lru.len(), 3);
        assert_eq!(lru.get(&2), Some(&20));
    }

    #[test]
    fn a_crop_keeps_the_centre_at_the_card_aspect() {
        // A 16:9 picture into a square card: the sides go.
        assert_eq!(crop([1280, 720], [200, 200]), [280, 0, 720, 720]);
        // Into a tall card (1:2): narrower still.
        assert_eq!(crop([1280, 720], [100, 200]), [460, 0, 360, 720]);
        // A tall picture into a wide card: the top and bottom go.
        assert_eq!(crop([720, 1280], [200, 100]), [0, 460, 720, 360]);
        // A 4:3 picture into a 16:9 card.
        assert_eq!(crop([640, 480], [320, 180]), [0, 60, 640, 360]);
        // The same aspect: all of it.
        assert_eq!(crop([640, 480], [320, 240]), [0, 0, 640, 480]);
        // Every crop fills the card's aspect within a pixel and stays inside.
        for src in [[1280, 720], [720, 1280], [640, 480], [1920, 1080], [33, 7]] {
            for dst in [[200, 200], [150, 260], [400, 120], [2, 2], [302, 418]] {
                let [x, y, w, h] = crop(src, dst);
                assert!(x + w <= src[0] && y + h <= src[1], "{src:?} {dst:?}");
                let aspect = |a: usize, b: usize| a as f64 / b as f64;
                let error = (aspect(w, h) - aspect(dst[0], dst[1])).abs() * h as f64;
                assert!(error <= 1.0, "{src:?} {dst:?}: {w}x{h}");
                // Centred.
                assert!((src[0] - w) / 2 == x && (src[1] - h) / 2 == y);
            }
        }
    }

    #[test]
    fn a_crop_scales_down_by_averaging() {
        // A 4x2 picture: the left half black, the right half white, with a
        // red pixel; cropped to its right 2x2 and scaled to 1x1.
        let mut rgb = vec![0_u8; 4 * 2 * 3];
        for y in 0..2 {
            for x in 2..4 {
                let at = (y * 4 + x) * 3;
                rgb[at..at + 3].copy_from_slice(&[255, 255, 255]);
            }
        }
        rgb[(3) * 3..(3) * 3 + 3].copy_from_slice(&[255, 0, 0]);
        assert_eq!(crop_scale(&rgb, 4, [2, 0, 2, 2], [1, 1]), [255, 191, 191]);
        // The whole picture to 2x1: black, then the right half's average.
        assert_eq!(
            crop_scale(&rgb, 4, [0, 0, 4, 2], [2, 1]),
            [0, 0, 0, 255, 191, 191]
        );
        // Scaling up repeats pixels.
        let up = crop_scale(&rgb, 4, [3, 0, 1, 1], [2, 2]);
        assert_eq!(up, [255, 0, 0].repeat(4));
        assert_eq!(crop_scale(&rgb, 4, [0, 0, 4, 2], [6, 4]).len(), 6 * 4 * 3);
    }

    #[test]
    fn card_pixels_round_up_to_even() {
        assert_eq!(card_pixels([182.8, 216.0], 1.0), [184, 216]);
        assert_eq!(card_pixels([182.8, 216.0], 2.0), [366, 432]);
        assert_eq!(card_pixels([100.0, 101.0], 1.0), [100, 102]);
        assert_eq!(card_pixels([150.0, 150.0], 1.25), [188, 188]);
        assert_eq!(card_pixels([0.0, -3.0], 1.0), [2, 2]);
    }

    #[test]
    fn webcam_frames_decode_to_rgb() {
        use stance::{Source, TestPattern, TestPatternConfig};
        let mut pattern = TestPattern::new(TestPatternConfig {
            width: 64,
            height: 48,
            paced: false,
            ..TestPatternConfig::default()
        })
        .unwrap();
        let frame = pattern
            .next_frame(std::time::Duration::ZERO)
            .unwrap()
            .unwrap();
        let (w, h, rgb) = decode(&frame.jpeg).unwrap();
        assert_eq!((w, h, rgb.len()), (64, 48, 64 * 48 * 3));
        assert!(decode(b"not a jpeg").is_none());
    }
}
