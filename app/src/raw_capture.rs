//! Per-shot raw frame capture, shared by every device actor.
//!
//! An actor records each wire frame with [`RawCapture::push`] as it crosses
//! the transport. Outside a shot the frames go to a small pre-roll ring;
//! [`RawCapture::mark_trigger`] opens the shot window with the ring as its
//! head, and [`RawCapture::take`] closes it and returns the [`ShotRaw`] that
//! rides on `ShotFinished`. Everything runs on the actor's own thread.
//!
//! Frames an actor marks as excluded (identity, handshake, keepalive) are
//! dropped before they reach the ring or any budget, so they never appear in
//! an export.

use std::collections::VecDeque;
use std::time::Instant;

use flighthook::{RawRecord, ShotRaw};

/// How one actor captures frames.
pub struct RawCaptureConfig {
    /// `ShotRaw.transport` label, e.g. `"flightscope-tcp"`.
    pub transport: &'static str,
    /// Frames kept from before `mark_trigger()`. BLE devices deliver a
    /// shot's notifications before the client reports the shot; FlightScope
    /// decodes the BALL TRIGGER frame just before its trigger event.
    pub pre_roll_frames: usize,
    /// Cap on non-bulk frames per shot.
    pub max_frames: usize,
    /// Cap on payload bytes of bulk frames per shot.
    pub max_bulk_bytes: usize,
    /// Frames never recorded (identity, handshake, keepalive).
    pub exclude: fn(&RawRecord) -> bool,
    /// Frames charged to the bulk byte budget instead of the frame cap.
    pub is_bulk: fn(&RawRecord) -> bool,
}

/// No frame is excluded.
pub fn exclude_none(_: &RawRecord) -> bool {
    false
}

/// No frame is bulk.
pub fn no_bulk(_: &RawRecord) -> bool {
    false
}

/// An open shot window.
struct Window {
    mark: Instant,
    frames: Vec<(Instant, RawRecord)>,
    frame_count: usize,
    bulk_bytes: usize,
    dropped: u32,
}

/// Raw frame recorder for one device connection.
pub struct RawCapture {
    cfg: RawCaptureConfig,
    ring: VecDeque<(Instant, RawRecord)>,
    window: Option<Window>,
    enabled: bool,
}

impl RawCapture {
    /// A recorder that starts enabled with an empty ring.
    pub fn new(cfg: RawCaptureConfig) -> Self {
        Self {
            ring: VecDeque::with_capacity(cfg.pre_roll_frames),
            cfg,
            window: None,
            enabled: true,
        }
    }

    /// Record one frame now. `rec.t_ms` is filled in by [`take`](Self::take).
    pub fn push(&mut self, rec: RawRecord) {
        self.push_at(rec, Instant::now());
    }

    /// Record one frame that crossed the transport at `at`. A frame pushed
    /// after a later one still lands in time order in [`take`](Self::take).
    pub fn push_at(&mut self, rec: RawRecord, at: Instant) {
        if !self.enabled || (self.cfg.exclude)(&rec) {
            return;
        }
        if self.window.is_some() {
            self.admit(rec, at);
        } else if self.cfg.pre_roll_frames > 0 {
            if self.ring.len() == self.cfg.pre_roll_frames {
                self.ring.pop_front();
            }
            self.ring.push_back((at, rec));
        }
    }

    /// Append a frame to the open window, subject to the per-shot caps.
    fn admit(&mut self, rec: RawRecord, at: Instant) {
        let bulk = (self.cfg.is_bulk)(&rec);
        let len = rec.payload_len();
        let (max_frames, max_bulk) = (self.cfg.max_frames, self.cfg.max_bulk_bytes);
        let Some(w) = self.window.as_mut() else {
            return;
        };
        let fits = if bulk {
            w.bulk_bytes + len <= max_bulk
        } else {
            w.frame_count < max_frames
        };
        if !fits {
            w.dropped = w.dropped.saturating_add(1);
            return;
        }
        if bulk {
            w.bulk_bytes += len;
        } else {
            w.frame_count += 1;
        }
        w.frames.push((at, rec));
    }

    /// Open the shot window now; the pre-roll ring becomes its head. A window
    /// still open from a shot that never completed is discarded.
    pub fn mark_trigger(&mut self) {
        self.mark_trigger_at(Instant::now());
    }

    fn mark_trigger_at(&mut self, at: Instant) {
        if let Some(stale) = self.window.take() {
            tracing::debug!(
                "raw capture: discarding an unfinished shot window ({} frames)",
                stale.frames.len()
            );
        }
        self.window = Some(Window {
            mark: at,
            frames: Vec::new(),
            frame_count: 0,
            bulk_bytes: 0,
            dropped: 0,
        });
        for (t, rec) in std::mem::take(&mut self.ring) {
            self.admit(rec, t);
        }
    }

    /// Close the window and return it, with `t_ms` relative to the trigger.
    /// Resets the recorder.
    pub fn take(&mut self) -> ShotRaw {
        self.ring.clear();
        let Some(w) = self.window.take() else {
            return ShotRaw {
                transport: self.cfg.transport.into(),
                unavailable: Some("no shot window was open".into()),
                ..ShotRaw::default()
            };
        };
        let mut frames = w.frames;
        frames.sort_by_key(|(t, _)| *t);
        let frames = frames
            .into_iter()
            .map(|(t, mut rec)| {
                rec.t_ms = ms_between(w.mark, t);
                rec
            })
            .collect();
        ShotRaw {
            transport: self.cfg.transport.into(),
            frames,
            dropped: w.dropped,
            unavailable: None,
        }
    }

    /// Drop the open window and the pre-roll ring.
    pub fn discard(&mut self) {
        self.ring.clear();
        self.window = None;
    }

    /// Turn recording on or off. Turning it off also clears what is held.
    pub fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
        if !on {
            self.discard();
        }
    }
}

/// Signed milliseconds from `mark` to `t`.
fn ms_between(mark: Instant, t: Instant) -> i64 {
    let ms = |d: std::time::Duration| i64::try_from(d.as_millis()).unwrap_or(i64::MAX);
    if t >= mark {
        ms(t - mark)
    } else {
        -ms(mark - t)
    }
}

/// The byte at `index` of a record's hex payload.
pub fn byte_at(rec: &RawRecord, index: usize) -> Option<u8> {
    let hex = rec.hex.as_deref()?;
    let pair = hex.get(index * 2..index * 2 + 2)?;
    u8::from_str_radix(pair, 16).ok()
}

/// The record's `type` as a byte (`"0xD4"` → `0xD4`).
pub fn type_byte(rec: &RawRecord) -> Option<u8> {
    let t = rec.r#type.as_deref()?;
    u8::from_str_radix(t.strip_prefix("0x")?, 16).ok()
}

/// `0x`-prefixed uppercase type label, e.g. `"0xD4"`.
pub fn type_label(type_id: u8) -> String {
    format!("0x{type_id:02X}")
}

/// Replace every occurrence of each identity string in `text` with
/// `<redacted>`, longest first so an address goes before its host part.
/// Strings shorter than four characters are ignored, so a short value cannot
/// shred unrelated text.
pub fn scrub_text(text: &str, scrub: &[String]) -> String {
    let mut by_len: Vec<&String> = scrub.iter().collect();
    by_len.sort_by_key(|s| std::cmp::Reverse(s.len()));
    let mut out = text.to_string();
    for s in by_len {
        if s.chars().count() >= 4 && out.contains(s.as_str()) {
            out = out.replace(s.as_str(), "<redacted>");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use flighthook::RawDir;

    use super::*;

    fn rec(type_id: u8, len: usize) -> RawRecord {
        RawRecord {
            r#type: Some(type_label(type_id)),
            ..RawRecord::binary(RawDir::Rx, &vec![type_id; len])
        }
    }

    fn excluded(r: &RawRecord) -> bool {
        type_byte(r) == Some(0xAA)
    }

    fn bulk(r: &RawRecord) -> bool {
        type_byte(r) == Some(0xEC)
    }

    fn capture(pre_roll: usize, max_frames: usize, max_bulk: usize) -> RawCapture {
        RawCapture::new(RawCaptureConfig {
            transport: "test",
            pre_roll_frames: pre_roll,
            max_frames,
            max_bulk_bytes: max_bulk,
            exclude: excluded,
            is_bulk: bulk,
        })
    }

    fn types(raw: &ShotRaw) -> Vec<u8> {
        raw.frames.iter().filter_map(type_byte).collect()
    }

    #[test]
    fn pre_roll_ring_is_bounded_and_becomes_the_head() {
        let mut c = capture(3, 16, 0);
        for t in 1..=5 {
            c.push(rec(t, 1));
        }
        c.mark_trigger();
        c.push(rec(0x10, 1));
        let raw = c.take();
        assert_eq!(types(&raw), [3, 4, 5, 0x10]);
        assert_eq!(raw.transport, "test");
        assert_eq!(raw.dropped, 0);
        assert!(raw.unavailable.is_none());
    }

    #[test]
    fn t_ms_is_negative_before_the_mark_and_zero_based_after() {
        let mut c = capture(4, 16, 0);
        let base = Instant::now();
        c.push_at(rec(1, 1), base);
        c.push_at(rec(2, 1), base + Duration::from_millis(40));
        c.mark_trigger_at(base + Duration::from_millis(100));
        c.push_at(rec(3, 1), base + Duration::from_millis(100));
        c.push_at(rec(4, 1), base + Duration::from_millis(350));
        let t: Vec<i64> = c.take().frames.iter().map(|r| r.t_ms).collect();
        assert_eq!(t, [-100, -60, 0, 250]);
    }

    #[test]
    fn frames_pushed_late_are_returned_in_time_order() {
        let mut c = capture(4, 16, 0);
        let base = Instant::now();
        c.push_at(rec(2, 1), base + Duration::from_millis(20));
        c.push_at(rec(1, 1), base + Duration::from_millis(10));
        c.mark_trigger_at(base + Duration::from_millis(30));
        c.push_at(rec(4, 1), base + Duration::from_millis(50));
        c.push_at(rec(3, 1), base + Duration::from_millis(40));
        assert_eq!(types(&c.take()), [1, 2, 3, 4]);
    }

    #[test]
    fn frame_cap_and_bulk_cap_count_dropped() {
        let mut c = capture(0, 2, 10);
        c.mark_trigger();
        for _ in 0..3 {
            c.push(rec(1, 1));
        }
        c.push(rec(0xEC, 6));
        c.push(rec(0xEC, 4));
        c.push(rec(0xEC, 1));
        let raw = c.take();
        assert_eq!(types(&raw), [1, 1, 0xEC, 0xEC]);
        assert_eq!(raw.dropped, 2);
    }

    #[test]
    fn excluded_frames_are_never_counted() {
        let mut c = capture(2, 1, 0);
        c.push(rec(0xAA, 1));
        c.push(rec(0xAA, 1));
        c.push(rec(1, 1));
        c.mark_trigger();
        c.push(rec(0xAA, 1));
        let raw = c.take();
        assert_eq!(types(&raw), [1]);
        assert_eq!(raw.dropped, 0);
    }

    #[test]
    fn take_resets() {
        let mut c = capture(2, 16, 0);
        c.mark_trigger();
        c.push(rec(1, 1));
        assert_eq!(c.take().frames.len(), 1);
        let again = c.take();
        assert!(again.frames.is_empty());
        assert!(again.unavailable.is_some());
        c.push(rec(2, 1));
        c.mark_trigger();
        assert_eq!(types(&c.take()), [2]);
    }

    #[test]
    fn a_second_mark_discards_the_open_window() {
        let mut c = capture(2, 16, 0);
        c.mark_trigger();
        c.push(rec(1, 1));
        c.mark_trigger();
        c.push(rec(2, 1));
        assert_eq!(types(&c.take()), [2]);
    }

    #[test]
    fn disabled_capture_records_nothing() {
        let mut c = capture(4, 16, 0);
        c.set_enabled(false);
        c.push(rec(1, 1));
        c.mark_trigger();
        c.push(rec(2, 1));
        assert!(c.take().frames.is_empty());

        c.set_enabled(true);
        c.push(rec(3, 1));
        c.set_enabled(false);
        c.set_enabled(true);
        c.mark_trigger();
        assert!(c.take().frames.is_empty());
    }

    #[test]
    fn byte_and_type_helpers() {
        let r = rec(0xD4, 3);
        assert_eq!(type_byte(&r), Some(0xD4));
        assert_eq!(byte_at(&r, 2), Some(0xD4));
        assert_eq!(byte_at(&r, 3), None);
    }

    #[test]
    fn scrub_replaces_every_identity_string() {
        let scrub = vec![
            "FS M2-000001".to_string(),
            "10.0.0.9".to_string(),
            "ab".into(),
        ];
        assert_eq!(
            scrub_text("FS M2-000001 at 10.0.0.9:5100, ab", &scrub),
            "<redacted> at <redacted>:5100, ab"
        );
    }
}
