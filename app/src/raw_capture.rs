//! Per-shot raw frame capture, shared by every device card.
//!
//! A card records each raw frame its device driver hands over with
//! [`RawCapture::push`]. Outside a shot the frames go to a small pre-roll
//! ring; [`RawCapture::mark_trigger_at`] opens the shot window with the ring
//! as its head, and [`RawCapture::take`] closes it and returns the
//! [`ShotRaw`] that rides on `ShotFinished`. Everything runs on the card's
//! own thread. Which frames may be recorded at all (never identity) is the
//! driver's call: it hands over only those.

use std::collections::VecDeque;
use std::time::Instant;

use flighthook::{RawRecord, ShotRaw};

/// How one actor captures frames.
pub struct RawCaptureConfig {
    /// `ShotRaw.transport` label, e.g. `"flightscope-tcp"`.
    pub transport: &'static str,
    /// Frames kept from before the trigger: a device may deliver some of a
    /// shot's frames before its trigger.
    pub pre_roll_frames: usize,
    /// Cap on non-bulk frames per shot.
    pub max_frames: usize,
    /// Cap on payload bytes of bulk frames per shot.
    pub max_bulk_bytes: usize,
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
    /// Frames before the trigger, each with whether it is bulk.
    ring: VecDeque<(Instant, RawRecord, bool)>,
    window: Option<Window>,
}

impl RawCapture {
    /// A recorder with an empty ring.
    pub fn new(cfg: RawCaptureConfig) -> Self {
        Self {
            ring: VecDeque::with_capacity(cfg.pre_roll_frames),
            cfg,
            window: None,
        }
    }

    /// Record one frame that crossed the transport at `at`; `bulk` charges
    /// it to the bulk byte budget instead of the frame cap. `rec.t_ms` is
    /// filled in by [`take`](Self::take), and a frame pushed after a later
    /// one still lands in time order there.
    pub fn push(&mut self, rec: RawRecord, at: Instant, bulk: bool) {
        if self.window.is_some() {
            self.admit(rec, at, bulk);
        } else if self.cfg.pre_roll_frames > 0 {
            if self.ring.len() == self.cfg.pre_roll_frames {
                self.ring.pop_front();
            }
            self.ring.push_back((at, rec, bulk));
        }
    }

    /// Append a frame to the open window, subject to the per-shot caps.
    fn admit(&mut self, rec: RawRecord, at: Instant, bulk: bool) {
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

    /// Open the shot window as of `at`, the moment of the trigger; the
    /// pre-roll ring becomes its head. A window still open from a shot that
    /// never completed is discarded.
    pub fn mark_trigger_at(&mut self, at: Instant) {
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
        for (t, rec, bulk) in std::mem::take(&mut self.ring) {
            self.admit(rec, t, bulk);
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

    /// A frame named `n`, `len` bytes long.
    fn rec(n: u8, len: usize) -> RawRecord {
        RawRecord {
            label: Some(n.to_string()),
            ..RawRecord::binary(RawDir::Rx, &vec![n; len])
        }
    }

    fn capture(pre_roll: usize, max_frames: usize, max_bulk: usize) -> RawCapture {
        RawCapture::new(RawCaptureConfig {
            transport: "test",
            pre_roll_frames: pre_roll,
            max_frames,
            max_bulk_bytes: max_bulk,
        })
    }

    fn names(raw: &ShotRaw) -> Vec<u8> {
        raw.frames
            .iter()
            .filter_map(|r| r.label.as_deref()?.parse().ok())
            .collect()
    }

    #[test]
    fn pre_roll_ring_is_bounded_and_becomes_the_head() {
        let mut c = capture(3, 16, 0);
        let now = Instant::now();
        for t in 1..=5 {
            c.push(rec(t, 1), now, false);
        }
        c.mark_trigger_at(now);
        c.push(rec(10, 1), now, false);
        let raw = c.take();
        assert_eq!(names(&raw), [3, 4, 5, 10]);
        assert_eq!(raw.transport, "test");
        assert_eq!(raw.dropped, 0);
        assert!(raw.unavailable.is_none());
    }

    #[test]
    fn t_ms_is_negative_before_the_mark_and_zero_based_after() {
        let mut c = capture(4, 16, 0);
        let base = Instant::now();
        c.push(rec(1, 1), base, false);
        c.push(rec(2, 1), base + Duration::from_millis(40), false);
        c.mark_trigger_at(base + Duration::from_millis(100));
        c.push(rec(3, 1), base + Duration::from_millis(100), false);
        c.push(rec(4, 1), base + Duration::from_millis(350), false);
        let t: Vec<i64> = c.take().frames.iter().map(|r| r.t_ms).collect();
        assert_eq!(t, [-100, -60, 0, 250]);
    }

    #[test]
    fn frames_pushed_late_are_returned_in_time_order() {
        let mut c = capture(4, 16, 0);
        let base = Instant::now();
        c.push(rec(2, 1), base + Duration::from_millis(20), false);
        c.push(rec(1, 1), base + Duration::from_millis(10), false);
        c.mark_trigger_at(base + Duration::from_millis(30));
        c.push(rec(4, 1), base + Duration::from_millis(50), false);
        c.push(rec(3, 1), base + Duration::from_millis(40), false);
        assert_eq!(names(&c.take()), [1, 2, 3, 4]);
    }

    #[test]
    fn frame_cap_and_bulk_cap_count_dropped() {
        let mut c = capture(0, 2, 10);
        let now = Instant::now();
        c.mark_trigger_at(now);
        for _ in 0..3 {
            c.push(rec(1, 1), now, false);
        }
        c.push(rec(9, 6), now, true);
        c.push(rec(9, 4), now, true);
        c.push(rec(9, 1), now, true);
        let raw = c.take();
        assert_eq!(names(&raw), [1, 1, 9, 9]);
        assert_eq!(raw.dropped, 2);
    }

    #[test]
    fn take_resets() {
        let mut c = capture(2, 16, 0);
        let now = Instant::now();
        c.mark_trigger_at(now);
        c.push(rec(1, 1), now, false);
        assert_eq!(c.take().frames.len(), 1);
        let again = c.take();
        assert!(again.frames.is_empty());
        assert!(again.unavailable.is_some());
        c.push(rec(2, 1), now, false);
        c.mark_trigger_at(now);
        assert_eq!(names(&c.take()), [2]);
    }

    #[test]
    fn a_second_mark_discards_the_open_window() {
        let mut c = capture(2, 16, 0);
        let now = Instant::now();
        c.mark_trigger_at(now);
        c.push(rec(1, 1), now, false);
        c.mark_trigger_at(now);
        c.push(rec(2, 1), now, false);
        assert_eq!(names(&c.take()), [2]);
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
