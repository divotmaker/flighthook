//! Pureplay Connect v1 wire format: frames, payloads and CRC32C.
//!
//! Pure functions and types, no I/O. A frame is
//!
//! ```text
//! "PL" | version | reserved | type | length (u16 LE) | payload | CRC32C (u32 LE)
//! ```
//!
//! with the CRC32C (Castagnoli) over the header and payload. flighthook
//! sends the reserved byte as 0 and reads a frame whatever its value. All multi-byte
//! values are little endian, floats are 4-byte IEEE 754, and strings are a
//! length byte followed by that many UTF-8 bytes.
//!
//! Payloads are read forward-compatibly: a decoder reads the fields it
//! knows and ignores any bytes after them, so a newer peer may append
//! fields. Enumerations are kept as their raw bytes in the messages, so a
//! value added by a newer peer is passed on rather than rejected; the typed
//! enums ([`ReadyState`], [`ConnectionState`], ...) convert where needed.

use std::fmt;

/// The two magic bytes every frame starts with ("PL").
pub const MAGIC: [u8; 2] = *b"PL";
/// The protocol version flighthook speaks and stamps on every frame.
pub const VERSION: u8 = 1;
/// Header bytes before the payload: magic, version, reserved, type, length.
pub const HEADER_LEN: usize = 7;
/// Trailer bytes after the payload: the CRC32C.
pub const TRAILER_LEN: usize = 4;

/// Message type bytes.
pub mod kind {
    pub const HELLO: u8 = 0x01;
    pub const HEARTBEAT: u8 = 0x02;
    pub const STRICT_MODE_ACK: u8 = 0x04;
    pub const BALL_DATA: u8 = 0x10;
    pub const CLUB_DATA: u8 = 0x11;
    pub const READY_STATE: u8 = 0x12;
    pub const ERROR: u8 = 0x1F;
    pub const HELLO_ACK: u8 = 0x81;
    pub const PING: u8 = 0x82;
    pub const SET_CLUB_INFO: u8 = 0x90;
    pub const SET_DETECT: u8 = 0x91;
    pub const SET_BALL_TYPE: u8 = 0x92;
    pub const SET_SHOT_CONTEXT: u8 = 0x93;
    pub const SET_STRICT_MODE: u8 = 0x94;
}

/// The name of a message type, for logs; `"Unknown"` for a type v1 does not
/// define.
#[must_use]
pub fn kind_name(kind: u8) -> &'static str {
    match kind {
        kind::HELLO => "Hello",
        kind::HEARTBEAT => "Heartbeat",
        kind::STRICT_MODE_ACK => "StrictModeAck",
        kind::BALL_DATA => "BallData",
        kind::CLUB_DATA => "ClubData",
        kind::READY_STATE => "ReadyState",
        kind::ERROR => "Error",
        kind::HELLO_ACK => "HelloAck",
        kind::PING => "Ping",
        kind::SET_CLUB_INFO => "SetClubInfo",
        kind::SET_DETECT => "SetDetect",
        kind::SET_BALL_TYPE => "SetBallType",
        kind::SET_SHOT_CONTEXT => "SetShotContext",
        kind::SET_STRICT_MODE => "SetStrictMode",
        _ => "Unknown",
    }
}

/// DeviceCaps bit: the device runs on a battery.
pub const CAP_BATTERY_POWERED: u16 = 1 << 4;

/// Error code: a generic error.
pub const ERROR_GENERIC: u16 = 0;
/// Error code: the host requires strict mode.
pub const ERROR_STRICT_MODE_REQUIRED: u16 = 2;

/// Heartbeat battery value for "unknown".
pub const BATTERY_UNKNOWN: u8 = 0xFF;

// ---------------------------------------------------------------------------
// CRC32C
// ---------------------------------------------------------------------------

/// Reflected Castagnoli polynomial.
const CRC32C_POLY: u32 = 0x82F6_3B78;

/// Byte-at-a-time lookup table for [`crc32c`].
const CRC32C_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        #[allow(clippy::cast_possible_truncation)]
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ CRC32C_POLY
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

/// CRC32C (Castagnoli) of `bytes`: initial value and final XOR 0xFFFFFFFF.
#[must_use]
pub fn crc32c(bytes: &[u8]) -> u32 {
    let crc = bytes.iter().fold(0xFFFF_FFFF_u32, |crc, &b| {
        CRC32C_TABLE[usize::from((crc as u8) ^ b)] ^ (crc >> 8)
    });
    crc ^ 0xFFFF_FFFF
}

// ---------------------------------------------------------------------------
// Frames
// ---------------------------------------------------------------------------

/// One frame read off the wire: its header fields and payload, CRC checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// The frame's version byte.
    pub version: u8,
    /// The frame's reserved byte, 0 from a v1 host.
    pub reserved: u8,
    /// The message type.
    pub kind: u8,
    /// The payload, `Length` bytes.
    pub payload: Vec<u8>,
    /// The whole frame as it arrived, header to CRC.
    pub bytes: Vec<u8>,
}

/// Why a payload could not be encoded into a frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadTooLong(pub usize);

impl fmt::Display for PayloadTooLong {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "payload of {} bytes is over 65535", self.0)
    }
}

/// The frame carrying `payload` as message type `kind`, at [`VERSION`].
///
/// # Errors
///
/// A payload longer than 65535 bytes.
pub fn encode_frame(kind: u8, payload: &[u8]) -> Result<Vec<u8>, PayloadTooLong> {
    let len = u16::try_from(payload.len()).map_err(|_| PayloadTooLong(payload.len()))?;
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len() + TRAILER_LEN);
    out.extend_from_slice(&MAGIC);
    out.push(VERSION);
    out.push(0);
    out.push(kind);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(payload);
    let crc = crc32c(&out);
    out.extend_from_slice(&crc.to_le_bytes());
    Ok(out)
}

/// Splits frames out of a TCP byte stream.
///
/// Feed it bytes as they arrive ([`push`](Self::push)) and take frames out
/// ([`next_frame`](Self::next_frame)). On a bad magic, a header whose
/// version is 0, or a CRC mismatch, it skips one byte and looks again, so it
/// finds the next real frame after any garbage. The skipped bytes are kept
/// until [`take_dropped`](Self::take_dropped) hands them back; the caller
/// takes them after every [`next_frame`](Self::next_frame) pass.
#[derive(Debug, Default)]
pub struct FrameReader {
    buf: Vec<u8>,
    dropped: Vec<u8>,
}

impl FrameReader {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append bytes read from the stream.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// The next complete frame, or `None` until more bytes arrive.
    pub fn next_frame(&mut self) -> Option<Frame> {
        // Bytes before `at` are skipped; they leave the buffer in one drain.
        let mut at = 0;
        let frame = loop {
            let buf = &self.buf[at..];
            if buf.len() < MAGIC.len() {
                // A lone byte that cannot start the magic is garbage.
                if buf.first().is_some_and(|&b| b != MAGIC[0]) {
                    at += 1;
                }
                break None;
            }
            if buf[..2] != MAGIC {
                at += 1;
                continue;
            }
            if buf.len() < HEADER_LEN {
                break None;
            }
            // A header with version 0 is not a frame boundary: skip it rather
            // than wait for a length's worth of bytes behind a chance "PL".
            if buf[2] == 0 {
                at += 1;
                continue;
            }
            let len = usize::from(u16::from_le_bytes([buf[5], buf[6]]));
            let total = HEADER_LEN + len + TRAILER_LEN;
            if buf.len() < total {
                break None;
            }
            let body = &buf[..HEADER_LEN + len];
            let crc = u32::from_le_bytes([
                buf[HEADER_LEN + len],
                buf[HEADER_LEN + len + 1],
                buf[HEADER_LEN + len + 2],
                buf[HEADER_LEN + len + 3],
            ]);
            if crc32c(body) != crc {
                at += 1;
                continue;
            }
            let bytes = buf[..total].to_vec();
            break Some(Frame {
                version: bytes[2],
                reserved: bytes[3],
                kind: bytes[4],
                payload: bytes[HEADER_LEN..HEADER_LEN + len].to_vec(),
                bytes,
            });
        };
        self.dropped.extend_from_slice(&self.buf[..at]);
        let used = at + frame.as_ref().map_or(0, |f| f.bytes.len());
        self.buf.drain(..used);
        frame
    }

    /// The bytes skipped while resynchronising since the last call.
    pub fn take_dropped(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.dropped)
    }
}

// ---------------------------------------------------------------------------
// Enumerations
// ---------------------------------------------------------------------------

/// Whether the device can take a shot: the states flighthook reports.
/// BallDetected (2) is not one: flighthook's readiness already includes the
/// ball, so it reports Ready.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadyState {
    NotReady = 0,
    Ready = 1,
    Processing = 3,
}

impl ReadyState {
    /// The wire byte.
    #[must_use]
    pub fn byte(self) -> u8 {
        self as u8
    }

    /// How the state reads on a card.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::NotReady => "not ready",
            Self::Ready => "ready",
            Self::Processing => "processing",
        }
    }
}

/// The launch monitor's connection, as the device reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected = 0,
    Connecting = 1,
    Connected = 2,
    Reconnecting = 3,
    Error = 4,
}

impl ConnectionState {
    /// The wire byte.
    #[must_use]
    pub fn byte(self) -> u8 {
        self as u8
    }
}

/// The player's hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handedness {
    Right = 0,
    Left = 1,
}

impl Handedness {
    /// The hand for a wire byte; `None` for a value v1 does not define.
    #[must_use]
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            0 => Some(Self::Right),
            1 => Some(Self::Left),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Payloads
// ---------------------------------------------------------------------------

/// Ball flight reading (BallData, 0x10). Fields whose [`valid`](Self::valid)
/// bit is clear are sent as 0.0.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BallData {
    pub shot_id: u32,
    pub valid: u16,
    pub ball_speed_ms: f32,
    pub vla: f32,
    pub hla: f32,
    pub total_spin_rpm: f32,
    pub back_spin_rpm: f32,
    pub side_spin_rpm: f32,
    pub spin_axis_deg: f32,
}

impl BallData {
    pub const BALL_SPEED: u16 = 1 << 0;
    pub const VLA: u16 = 1 << 1;
    pub const HLA: u16 = 1 << 2;
    pub const TOTAL_SPIN: u16 = 1 << 3;
    pub const BACK_SPIN: u16 = 1 << 4;
    pub const SIDE_SPIN: u16 = 1 << 5;
    pub const SPIN_AXIS: u16 = 1 << 6;
    /// Every spin bit.
    #[cfg(test)]
    pub const SPIN: u16 = Self::TOTAL_SPIN | Self::BACK_SPIN | Self::SIDE_SPIN | Self::SPIN_AXIS;
    /// Payload size in v1.
    pub const LEN: usize = 34;

    fn fields(&self) -> [f32; 7] {
        [
            self.ball_speed_ms,
            self.vla,
            self.hla,
            self.total_spin_rpm,
            self.back_spin_rpm,
            self.side_spin_rpm,
            self.spin_axis_deg,
        ]
    }
}

/// Club reading (ClubData, 0x11), paired with the BallData of the same
/// [`shot_id`](Self::shot_id).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ClubData {
    pub shot_id: u32,
    pub valid: u16,
    pub club_head_speed_ms: f32,
    pub smash_factor: f32,
    pub dynamic_loft_deg: f32,
    pub face_angle_deg: f32,
    pub club_path_deg: f32,
    pub attack_angle_deg: f32,
    pub impact_point_x_mm: f32,
    pub impact_point_y_mm: f32,
}

impl ClubData {
    pub const CLUB_HEAD_SPEED: u16 = 1 << 0;
    pub const SMASH_FACTOR: u16 = 1 << 1;
    pub const DYNAMIC_LOFT: u16 = 1 << 2;
    pub const FACE_ANGLE: u16 = 1 << 3;
    pub const CLUB_PATH: u16 = 1 << 4;
    pub const ATTACK_ANGLE: u16 = 1 << 5;
    pub const IMPACT_POINT_X: u16 = 1 << 6;
    pub const IMPACT_POINT_Y: u16 = 1 << 7;
    /// Payload size in v1.
    pub const LEN: usize = 38;

    fn fields(&self) -> [f32; 8] {
        [
            self.club_head_speed_ms,
            self.smash_factor,
            self.dynamic_loft_deg,
            self.face_angle_deg,
            self.club_path_deg,
            self.attack_angle_deg,
            self.impact_point_x_mm,
            self.impact_point_y_mm,
        ]
    }
}

/// A Pureplay Connect message, either direction.
///
/// Enumerated fields are raw bytes: see [`ReadyState`],
/// [`ConnectionState`], [`Handedness`] for v1's values.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// Device identity and capabilities.
    Hello {
        protocol_version: u8,
        device_caps: u16,
        device_name: String,
        firmware: String,
    },
    /// Liveness reply to a Ping.
    Heartbeat {
        connection_state: u8,
        ready_state: u8,
        battery: u8,
    },
    /// Reply to SetStrictMode.
    StrictModeAck {
        accepted: bool,
        reason: String,
    },
    BallData(BallData),
    ClubData(ClubData),
    /// Ready state transition.
    ReadyState(u8),
    /// Device-side error / nack (also sent by the host before it closes a
    /// link).
    Error {
        code: u16,
        message: String,
    },
    /// The host's handshake reply.
    HelloAck {
        protocol_version: u8,
        host_caps: u16,
    },
    /// Keepalive request.
    Ping,
    SetClubInfo {
        handedness: u8,
        club: u8,
    },
    /// 0 = disarm, 1 = arm.
    SetDetect {
        mode: u8,
    },
    /// 0 = marker, 1 = markerless.
    SetBallType {
        ball_type: u8,
    },
    /// Distance from the ball to the coming shot's target, metres; 0 or
    /// less, or NaN, means unknown.
    SetShotContext {
        distance_to_target_m: f32,
    },
    /// 1 = a recorded round is in progress, 0 = it is over.
    SetStrictMode {
        strict: u8,
    },
}

/// Why a payload could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// A message type v1 does not define.
    UnknownKind(u8),
    /// The payload is shorter than the type's fields.
    Truncated { kind: u8, need: usize, got: usize },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownKind(k) => write!(f, "unknown message type 0x{k:02X}"),
            Self::Truncated { kind, need, got } => write!(
                f,
                "{} (0x{kind:02X}) payload too short: {got} bytes, need {need}",
                kind_name(*kind)
            ),
        }
    }
}

/// Reads fields off the front of a payload.
struct Reader<'a> {
    kind: u8,
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(kind: u8, bytes: &'a [u8]) -> Self {
        Self { kind, bytes, at: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        let end = self.at + n;
        let got = self.bytes.get(self.at..end).ok_or(DecodeError::Truncated {
            kind: self.kind,
            need: end,
            got: self.bytes.len(),
        })?;
        self.at = end;
        Ok(got)
    }

    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, DecodeError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, DecodeError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn f32(&mut self) -> Result<f32, DecodeError> {
        let b = self.take(4)?;
        Ok(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// A length byte and that many UTF-8 bytes; invalid UTF-8 is replaced.
    fn string(&mut self) -> Result<String, DecodeError> {
        let n = usize::from(self.u8()?);
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
}

/// Append a length-prefixed string, cut at 255 bytes on a character
/// boundary.
fn put_string(out: &mut Vec<u8>, s: &str) {
    let mut end = s.len().min(255);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    #[allow(clippy::cast_possible_truncation)]
    out.push(end as u8);
    out.extend_from_slice(&s.as_bytes()[..end]);
}

impl Message {
    /// The message type byte.
    #[must_use]
    pub fn kind(&self) -> u8 {
        match self {
            Self::Hello { .. } => kind::HELLO,
            Self::Heartbeat { .. } => kind::HEARTBEAT,
            Self::StrictModeAck { .. } => kind::STRICT_MODE_ACK,
            Self::BallData(_) => kind::BALL_DATA,
            Self::ClubData(_) => kind::CLUB_DATA,
            Self::ReadyState(_) => kind::READY_STATE,
            Self::Error { .. } => kind::ERROR,
            Self::HelloAck { .. } => kind::HELLO_ACK,
            Self::Ping => kind::PING,
            Self::SetClubInfo { .. } => kind::SET_CLUB_INFO,
            Self::SetDetect { .. } => kind::SET_DETECT,
            Self::SetBallType { .. } => kind::SET_BALL_TYPE,
            Self::SetShotContext { .. } => kind::SET_SHOT_CONTEXT,
            Self::SetStrictMode { .. } => kind::SET_STRICT_MODE,
        }
    }

    /// The payload bytes.
    #[must_use]
    pub fn payload(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Self::Hello {
                protocol_version,
                device_caps,
                device_name,
                firmware,
            } => {
                out.push(*protocol_version);
                out.extend_from_slice(&device_caps.to_le_bytes());
                put_string(&mut out, device_name);
                put_string(&mut out, firmware);
            }
            Self::Heartbeat {
                connection_state,
                ready_state,
                battery,
            } => out.extend_from_slice(&[*connection_state, *ready_state, *battery]),
            Self::StrictModeAck { accepted, reason } => {
                out.push(u8::from(*accepted));
                put_string(&mut out, reason);
            }
            Self::BallData(b) => {
                out.reserve(BallData::LEN);
                out.extend_from_slice(&b.shot_id.to_le_bytes());
                out.extend_from_slice(&b.valid.to_le_bytes());
                for v in b.fields() {
                    out.extend_from_slice(&v.to_le_bytes());
                }
            }
            Self::ClubData(c) => {
                out.reserve(ClubData::LEN);
                out.extend_from_slice(&c.shot_id.to_le_bytes());
                out.extend_from_slice(&c.valid.to_le_bytes());
                for v in c.fields() {
                    out.extend_from_slice(&v.to_le_bytes());
                }
            }
            Self::ReadyState(s) => out.push(*s),
            Self::Error { code, message } => {
                out.extend_from_slice(&code.to_le_bytes());
                put_string(&mut out, message);
            }
            Self::HelloAck {
                protocol_version,
                host_caps,
            } => {
                out.push(*protocol_version);
                out.extend_from_slice(&host_caps.to_le_bytes());
                out.push(0);
            }
            Self::Ping => {}
            Self::SetClubInfo { handedness, club } => out.extend_from_slice(&[*handedness, *club]),
            Self::SetDetect { mode } => out.push(*mode),
            Self::SetBallType { ball_type } => out.push(*ball_type),
            Self::SetShotContext {
                distance_to_target_m,
            } => out.extend_from_slice(&distance_to_target_m.to_le_bytes()),
            Self::SetStrictMode { strict } => out.push(*strict),
        }
        out
    }

    /// The whole frame: header, payload, CRC.
    #[must_use]
    pub fn to_frame(&self) -> Vec<u8> {
        // Every v1 payload is far below the 65535-byte limit (strings are cut
        // at 255 bytes).
        encode_frame(self.kind(), &self.payload()).unwrap_or_default()
    }

    /// Decode a payload of message type `kind`. Bytes after the fields v1
    /// defines are ignored.
    ///
    /// # Errors
    ///
    /// A type v1 does not define, or a payload too short for its fields.
    pub fn decode(kind: u8, payload: &[u8]) -> Result<Self, DecodeError> {
        let mut r = Reader::new(kind, payload);
        Ok(match kind {
            kind::HELLO => Self::Hello {
                protocol_version: r.u8()?,
                device_caps: r.u16()?,
                device_name: r.string()?,
                firmware: r.string()?,
            },
            kind::HEARTBEAT => Self::Heartbeat {
                connection_state: r.u8()?,
                ready_state: r.u8()?,
                battery: r.u8()?,
            },
            kind::STRICT_MODE_ACK => Self::StrictModeAck {
                accepted: r.u8()? != 0,
                reason: r.string()?,
            },
            kind::BALL_DATA => Self::BallData(BallData {
                shot_id: r.u32()?,
                valid: r.u16()?,
                ball_speed_ms: r.f32()?,
                vla: r.f32()?,
                hla: r.f32()?,
                total_spin_rpm: r.f32()?,
                back_spin_rpm: r.f32()?,
                side_spin_rpm: r.f32()?,
                spin_axis_deg: r.f32()?,
            }),
            kind::CLUB_DATA => Self::ClubData(ClubData {
                shot_id: r.u32()?,
                valid: r.u16()?,
                club_head_speed_ms: r.f32()?,
                smash_factor: r.f32()?,
                dynamic_loft_deg: r.f32()?,
                face_angle_deg: r.f32()?,
                club_path_deg: r.f32()?,
                attack_angle_deg: r.f32()?,
                impact_point_x_mm: r.f32()?,
                impact_point_y_mm: r.f32()?,
            }),
            kind::READY_STATE => Self::ReadyState(r.u8()?),
            kind::ERROR => Self::Error {
                code: r.u16()?,
                message: r.string()?,
            },
            kind::HELLO_ACK => {
                let protocol_version = r.u8()?;
                let host_caps = r.u16()?;
                r.u8()?;
                Self::HelloAck {
                    protocol_version,
                    host_caps,
                }
            }
            kind::PING => Self::Ping,
            kind::SET_CLUB_INFO => Self::SetClubInfo {
                handedness: r.u8()?,
                club: r.u8()?,
            },
            kind::SET_DETECT => Self::SetDetect { mode: r.u8()? },
            kind::SET_BALL_TYPE => Self::SetBallType { ball_type: r.u8()? },
            kind::SET_SHOT_CONTEXT => Self::SetShotContext {
                distance_to_target_m: r.f32()?,
            },
            kind::SET_STRICT_MODE => Self::SetStrictMode { strict: r.u8()? },
            other => return Err(DecodeError::UnknownKind(other)),
        })
    }
}

/// Space-separated upper-case hex of `bytes`, for audit logs.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let _ = write!(out, "{b:02X}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bytes from the spec's hex dump (spaces and line breaks ignored).
    fn bytes(hex: &str) -> Vec<u8> {
        let digits: Vec<u8> = hex.bytes().filter(u8::is_ascii_hexdigit).collect();
        digits
            .chunks(2)
            .map(|p| u8::from_str_radix(std::str::from_utf8(p).expect("ascii"), 16).expect("hex"))
            .collect()
    }

    /// `msg` encodes to exactly `wire`, and `wire` decodes back to `msg`.
    fn golden(msg: &Message, wire: &str) {
        let wire = bytes(wire);
        assert_eq!(hex(&msg.to_frame()), hex(&wire), "{msg:?} encodes");
        let mut reader = FrameReader::new();
        reader.push(&wire);
        let frame = reader.next_frame().expect("a frame");
        assert!(reader.take_dropped().is_empty());
        assert_eq!(frame.bytes, wire);
        assert_eq!(frame.version, VERSION);
        assert_eq!(frame.kind, msg.kind());
        let back = Message::decode(frame.kind, &frame.payload).expect("decodes");
        assert_eq!(&back, msg, "decodes back");
        assert!(reader.next_frame().is_none());
    }

    // -- Spec section 10 worked examples ------------------------------------

    #[test]
    fn ball_data_example() {
        golden(
            &Message::BallData(BallData {
                shot_id: 1,
                valid: 0x007F,
                ball_speed_ms: 64.0,
                vla: 12.5,
                hla: -1.5,
                total_spin_rpm: 2500.0,
                back_spin_rpm: 2450.0,
                side_spin_rpm: -300.0,
                spin_axis_deg: 6.98,
            }),
            "50 4C 01 00 10 22 00 01 00 00 00 7F 00 00 00 80 42 00 00 48 41
             00 00 C0 BF 00 40 1C 45 00 20 19 45 00 00 96 C3 29 5C DF 40 0A 14 D6 31",
        );
    }

    #[test]
    fn club_data_example() {
        golden(
            &Message::ClubData(ClubData {
                shot_id: 1,
                valid: 0x00FF,
                club_head_speed_ms: 47.5,
                smash_factor: 1.347,
                dynamic_loft_deg: 14.2,
                face_angle_deg: -0.8,
                club_path_deg: 2.1,
                attack_angle_deg: -3.4,
                impact_point_x_mm: 5.0,
                impact_point_y_mm: -2.0,
            }),
            "50 4C 01 00 11 26 00 01 00 00 00 FF 00 00 00 3E 42 7F 6A AC 3F
             33 33 63 41 CD CC 4C BF 66 66 06 40 9A 99 59 C0 00 00 A0 40 00 00 00 C0 5D D2 D9 F1",
        );
    }

    #[test]
    fn hello_example() {
        golden(
            &Message::Hello {
                protocol_version: 1,
                device_caps: CAP_BATTERY_POWERED,
                device_name: "ACME LM".into(),
                firmware: "1.0".into(),
            },
            "50 4C 01 00 01 0F 00 01 10 00 07 41 43 4D 45 20 4C 4D 03 31 2E 30 C0 C9 B1 D0",
        );
    }

    #[test]
    fn hello_ack_example() {
        golden(
            &Message::HelloAck {
                protocol_version: 1,
                host_caps: 0,
            },
            "50 4C 01 00 81 04 00 01 00 00 00 64 CA B4 DA",
        );
    }

    #[test]
    fn ping_example() {
        golden(&Message::Ping, "50 4C 01 00 82 00 00 D0 A9 7C 7D");
    }

    #[test]
    fn set_shot_context_examples() {
        golden(
            &Message::SetShotContext {
                distance_to_target_m: 142.5,
            },
            "50 4C 01 00 93 04 00 00 80 0E 43 6B C8 7B 66",
        );
        golden(
            &Message::SetShotContext {
                distance_to_target_m: 0.0,
            },
            "50 4C 01 00 93 04 00 00 00 00 00 EA C3 AC 4B",
        );
    }

    #[test]
    fn set_strict_mode_examples() {
        golden(
            &Message::SetStrictMode { strict: 1 },
            "50 4C 01 00 94 01 00 01 61 7C D8 FB",
        );
        golden(
            &Message::SetStrictMode { strict: 0 },
            "50 4C 01 00 94 01 00 00 62 FF B3 09",
        );
    }

    #[test]
    fn strict_mode_ack_examples() {
        golden(
            &Message::StrictModeAck {
                accepted: true,
                reason: String::new(),
            },
            "50 4C 01 00 04 02 00 01 00 FA A6 89 42",
        );
        golden(
            &Message::StrictModeAck {
                accepted: false,
                reason: "Not supported".into(),
            },
            "50 4C 01 00 04 0F 00 00 0D 4E 6F 74 20 73 75 70 70 6F 72 74 65 64 1B 9D FD 27",
        );
    }

    #[test]
    fn error_example() {
        golden(
            &Message::Error {
                code: ERROR_STRICT_MODE_REQUIRED,
                message: "Strict mode required".into(),
            },
            "50 4C 01 00 1F 17 00 02 00 14 53 74 72 69 63 74 20 6D 6F 64 65 20 72 65 71 75
             69 72 65 64 31 B3 14 C5",
        );
    }

    // -- Every other message type round-trips --------------------------------

    #[test]
    fn every_message_type_round_trips() {
        let messages = [
            Message::Heartbeat {
                connection_state: ConnectionState::Connected.byte(),
                ready_state: ReadyState::Ready.byte(),
                battery: 87,
            },
            Message::ReadyState(ReadyState::Processing.byte()),
            Message::SetClubInfo {
                handedness: 1,
                club: 22,
            },
            Message::SetDetect { mode: 1 },
            Message::SetBallType { ball_type: 1 },
        ];
        for msg in messages {
            let frame = msg.to_frame();
            let mut reader = FrameReader::new();
            reader.push(&frame);
            let f = reader.next_frame().expect("frame");
            assert_eq!(Message::decode(f.kind, &f.payload), Ok(msg.clone()));
        }
    }

    #[test]
    fn crc32c_check_value() {
        // The standard CRC-32C check value.
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
        assert_eq!(crc32c(b""), 0);
    }

    // -- Framing -------------------------------------------------------------

    #[test]
    fn frames_split_across_reads_come_out_whole() {
        let a = Message::Ping.to_frame();
        let b = Message::SetDetect { mode: 1 }.to_frame();
        let stream: Vec<u8> = a.iter().chain(&b).copied().collect();
        let mut reader = FrameReader::new();
        let mut got = Vec::new();
        for byte in &stream {
            reader.push(std::slice::from_ref(byte));
            while let Some(f) = reader.next_frame() {
                got.push(f.kind);
            }
        }
        assert_eq!(got, [kind::PING, kind::SET_DETECT]);
        assert!(reader.take_dropped().is_empty());
    }

    #[test]
    fn garbage_before_a_frame_is_skipped_a_byte_at_a_time() {
        // A stray "PL" with version 0 is not taken for a header.
        let mut stream = vec![0x00, 0x50, 0x13, 0x50, 0x4C, 0x00, 0x07];
        stream.extend(Message::Ping.to_frame());
        let mut reader = FrameReader::new();
        reader.push(&stream);
        let f = reader.next_frame().expect("frame after the garbage");
        assert_eq!(f.kind, kind::PING);
        assert_eq!(
            reader.take_dropped(),
            [0x00, 0x50, 0x13, 0x50, 0x4C, 0x00, 0x07]
        );
        assert!(reader.next_frame().is_none());
        assert!(reader.buf.is_empty());
    }

    /// Bytes with no frame behind them are handed back by `take_dropped`
    /// and not kept: a peer that sends only garbage holds no memory.
    #[test]
    fn garbage_with_no_frame_after_it_is_handed_back_and_not_kept() {
        let mut reader = FrameReader::new();
        for _ in 0..100 {
            reader.push(&[0x13; 4096]);
            assert!(reader.next_frame().is_none());
            assert_eq!(reader.take_dropped().len(), 4096);
            assert!(reader.buf.is_empty());
            assert!(reader.dropped.is_empty());
        }
        // A trailing 'P' may start the magic: it waits, and nothing else.
        reader.push(&[0x13, 0x13, b'P']);
        assert!(reader.next_frame().is_none());
        assert_eq!(reader.take_dropped(), [0x13, 0x13]);
        assert_eq!(reader.buf, [b'P']);
    }

    /// A frame whose reserved byte is set (with a CRC over it) is a frame.
    #[test]
    fn a_frame_with_the_reserved_byte_set_is_read() {
        let mut frame = Message::SetDetect { mode: 1 }.to_frame();
        frame[3] = 0x05;
        let body = frame.len() - TRAILER_LEN;
        let crc = crc32c(&frame[..body]);
        frame[body..].copy_from_slice(&crc.to_le_bytes());
        let mut reader = FrameReader::new();
        reader.push(&frame);
        let f = reader.next_frame().expect("frame");
        assert_eq!(f.reserved, 0x05);
        assert_eq!(
            Message::decode(f.kind, &f.payload),
            Ok(Message::SetDetect { mode: 1 })
        );
        assert!(reader.take_dropped().is_empty());
    }

    /// A chance header waiting on a 64 KiB length that turns out to fail
    /// its CRC resyncs on the frame behind it in one pass.
    #[test]
    fn a_long_resync_finds_the_frame_behind_a_failed_one() {
        let mut stream = vec![0x50, 0x4C, 0x01, 0x00, 0x10, 0xFF, 0xFF];
        stream.resize(HEADER_LEN + 0xFFFF + TRAILER_LEN, 0x13);
        let ping = Message::Ping.to_frame();
        stream.extend(&ping);
        let mut reader = FrameReader::new();
        reader.push(&stream);
        let f = reader.next_frame().expect("the Ping");
        assert_eq!(f.kind, kind::PING);
        assert_eq!(reader.take_dropped().len(), stream.len() - ping.len());
        assert!(reader.buf.is_empty());
    }

    #[test]
    fn a_frame_with_a_bad_crc_is_dropped_and_the_next_one_found() {
        let mut bad = Message::SetDetect { mode: 1 }.to_frame();
        let last = bad.len() - 1;
        bad[last] ^= 0xFF;
        let good = Message::SetDetect { mode: 0 }.to_frame();
        let mut reader = FrameReader::new();
        reader.push(&bad);
        reader.push(&good);
        let f = reader.next_frame().expect("the good frame");
        assert_eq!(
            Message::decode(f.kind, &f.payload),
            Ok(Message::SetDetect { mode: 0 })
        );
        assert_eq!(reader.take_dropped(), bad);
        assert!(reader.next_frame().is_none());
    }

    #[test]
    fn a_corrupted_payload_byte_fails_the_crc() {
        let mut frame = Message::SetShotContext {
            distance_to_target_m: 142.5,
        }
        .to_frame();
        frame[8] ^= 0x01;
        let mut reader = FrameReader::new();
        reader.push(&frame);
        assert!(reader.next_frame().is_none());
    }

    #[test]
    fn an_incomplete_frame_waits_for_the_rest() {
        let frame = Message::SetClubInfo {
            handedness: 0,
            club: 0,
        }
        .to_frame();
        let mut reader = FrameReader::new();
        reader.push(&frame[..frame.len() - 1]);
        assert!(reader.next_frame().is_none());
        reader.push(&frame[frame.len() - 1..]);
        assert!(reader.next_frame().is_some());
        assert!(reader.take_dropped().is_empty());
    }

    // -- Forward compatibility ----------------------------------------------

    #[test]
    fn appended_payload_fields_are_skipped() {
        // A newer host appends a field after SetShotContext's distance.
        let mut payload = 142.5_f32.to_le_bytes().to_vec();
        payload.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
        let frame = encode_frame(kind::SET_SHOT_CONTEXT, &payload).expect("frame");
        let mut reader = FrameReader::new();
        reader.push(&frame);
        let f = reader.next_frame().expect("frame");
        assert_eq!(
            Message::decode(f.kind, &f.payload),
            Ok(Message::SetShotContext {
                distance_to_target_m: 142.5
            })
        );

        // And a longer BallData still reads its 34-byte prefix.
        let ball = BallData {
            shot_id: 7,
            valid: BallData::BALL_SPEED,
            ball_speed_ms: 50.0,
            ..BallData::default()
        };
        let mut payload = Message::BallData(ball).payload();
        payload.extend_from_slice(&1.0_f32.to_le_bytes());
        assert_eq!(
            Message::decode(kind::BALL_DATA, &payload),
            Ok(Message::BallData(ball))
        );
    }

    #[test]
    fn an_unknown_type_is_framed_but_not_decoded() {
        let frame = encode_frame(0xA5, &[1, 2, 3]).expect("frame");
        let mut reader = FrameReader::new();
        reader.push(&frame);
        let f = reader.next_frame().expect("frame");
        assert_eq!(f.kind, 0xA5);
        assert_eq!(
            Message::decode(f.kind, &f.payload),
            Err(DecodeError::UnknownKind(0xA5))
        );
        assert_eq!(kind_name(0xA5), "Unknown");
    }

    #[test]
    fn a_short_payload_is_an_error() {
        assert_eq!(
            Message::decode(kind::SET_CLUB_INFO, &[0]),
            Err(DecodeError::Truncated {
                kind: kind::SET_CLUB_INFO,
                need: 2,
                got: 1
            })
        );
        assert!(Message::decode(kind::BALL_DATA, &[0; 33]).is_err());
        assert!(Message::decode(kind::HELLO_ACK, &[1, 0, 0]).is_err());
    }

    #[test]
    fn a_long_string_is_cut_at_255_bytes_on_a_character_boundary() {
        let name = "é".repeat(200); // 400 bytes
        let msg = Message::Hello {
            protocol_version: 1,
            device_caps: 0,
            device_name: name,
            firmware: String::new(),
        };
        let payload = msg.payload();
        assert_eq!(payload[3], 254);
        let Ok(Message::Hello { device_name, .. }) = Message::decode(kind::HELLO, &payload) else {
            panic!("decodes");
        };
        assert_eq!(device_name, "é".repeat(127));
    }
}
