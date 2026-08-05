//! ESP frame protocol (DevPlan §8.2).
//!
//! Wire format: `magic(2) | type(1) | len(2 LE) | seq(1) | version(1) | payload(len) | crc16(2 LE)`
//! CRC-16/CCITT (poly 0x1021, init 0xFFFF) computed over `type..payload` (i.e. all bytes
//! after the magic, before the CRC field).

use serde::{Deserialize, Serialize};

pub const MAGIC_0: u8 = 0xCA;
pub const MAGIC_1: u8 = 0x50;
pub const VERSION: u8 = 0x01;

/// Upper bound on a single frame's payload. The wire length field is 16-bit but
/// we never trust it blindly: a corrupt/hostile header declaring a huge length
/// must not let the receive buffer grow without limit.
pub const MAX_FRAME: usize = 16 * 1024;

/// Frame type identifiers (shared IDE ↔ ESP).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum FrameType {
    /// Telemetry (battery/status/uptime…) ESP → IDE
    Telemetry = 0x01,
    /// Command / control frame
    Command = 0x02,
    /// Acknowledgement
    Ack = 0x03,
    /// Audio chunk (fire-and-forget)
    AudioChunk = 0x04,
    /// Heartbeat (5 s bidirectional)
    Heartbeat = 0x05,
}

impl FrameType {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0x01 => Some(Self::Telemetry),
            0x02 => Some(Self::Command),
            0x03 => Some(Self::Ack),
            0x04 => Some(Self::AudioChunk),
            0x05 => Some(Self::Heartbeat),
            _ => None,
        }
    }
}

/// A single decoded frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    pub frame_type: FrameType,
    pub seq: u8,
    pub version: u8,
    pub payload: Vec<u8>,
}

/// Errors from parsing/encoding frames.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("bad magic: expected CA 50, got {0:02X} {1:02X}")]
    BadMagic(u8, u8),
    #[error("bad frame type: 0x{0:02X}")]
    BadType(u8),
    #[error("crc mismatch: rx=0x{rx:04X} computed=0x{computed:04X}")]
    CrcMismatch { rx: u16, computed: u16 },
    #[error("frame too short: {0} bytes")]
    TooShort(usize),
    #[error("frame too large: declared payload {payload_len} bytes exceeds max {max}")]
    FrameTooLarge { payload_len: usize, max: usize },
    #[error("payload too large to encode: {0} bytes (u16 length field)")]
    PayloadTooLarge(usize),
}

/// CRC-16/CCITT (poly 0x1021, init 0xFFFF), matching the C5 firmware.
pub fn crc16_ccitt(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &byte in data {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

/// Encode a frame into the wire format. Errors if the payload doesn't fit the
/// 16-bit length field (instead of silently wrapping).
pub fn encode(frame_type: FrameType, seq: u8, payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    if payload.len() > u16::MAX as usize {
        return Err(FrameError::PayloadTooLarge(payload.len()));
    }
    let mut out = Vec::with_capacity(9 + payload.len());
    out.push(MAGIC_0);
    out.push(MAGIC_1);
    out.push(frame_type as u8);
    out.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    out.push(seq);
    out.push(VERSION);
    out.extend_from_slice(payload);
    let crc = crc16_ccitt(&out[2..]);
    out.extend_from_slice(&crc.to_le_bytes());
    Ok(out)
}

/// Try to decode one frame from a buffer. On success returns the frame and
/// the number of bytes consumed (header + payload + crc). The caller should
/// drain that many bytes and retry for any further complete frames.
pub fn try_decode(buf: &[u8]) -> Result<(Frame, usize), FrameError> {
    // Need at least magic(2)+type(1)+len(2)+seq(1)+ver(1)+crc(2) = 9 bytes
    if buf.len() < 9 {
        return Err(FrameError::TooShort(buf.len()));
    }
    if buf[0] != MAGIC_0 || buf[1] != MAGIC_1 {
        return Err(FrameError::BadMagic(buf[0], buf[1]));
    }
    let frame_type = FrameType::from_u8(buf[2]).ok_or(FrameError::BadType(buf[2]))?;
    let payload_len = u16::from_le_bytes([buf[3], buf[4]]) as usize;
    let seq = buf[5];
    let version = buf[6];
    let total = 9 + payload_len;
    if payload_len > MAX_FRAME || total > MAX_FRAME {
        return Err(FrameError::FrameTooLarge { payload_len, max: MAX_FRAME });
    }
    if buf.len() < total {
        return Err(FrameError::TooShort(buf.len()));
    }
    let payload = buf[7..7 + payload_len].to_vec();
    let crc_rx = u16::from_le_bytes([buf[total - 2], buf[total - 1]]);
    // CRC covers type..payload (bytes[2..total-2])
    let computed = crc16_ccitt(&buf[2..total - 2]);
    if crc_rx != computed {
        return Err(FrameError::CrcMismatch {
            rx: crc_rx,
            computed,
        });
    }
    Ok((
        Frame {
            frame_type,
            seq,
            version,
            payload,
        },
        total,
    ))
}

/// Extract all complete frames from a streaming buffer, resyncing on garbage.
/// Returns decoded frames and leaves any partial trailing bytes in `buf`.
///
/// The buffer is never allowed to grow past `MAX_FRAME` on an incomplete frame:
/// a hostile/corrupt length field triggers a linear resync instead of buffering
/// forever.
pub fn drain_frames(buf: &mut Vec<u8>, out: &mut Vec<Frame>) {
    loop {
        if buf.len() < 2 {
            return;
        }
        if buf[0] != MAGIC_0 || buf[1] != MAGIC_1 {
            // Out of sync: skip to the next magic in one linear scan instead of
            // dropping one byte at a time (O(n²) on hostile input).
            resync(buf);
            continue;
        }
        match try_decode(buf) {
            Ok((frame, consumed)) => {
                out.push(frame);
                buf.drain(..consumed);
            }
            Err(FrameError::TooShort(_)) => {
                // Incomplete frame. If the buffer has already outgrown MAX_FRAME
                // the declared length is bogus — resync rather than buffer forever.
                if buf.len() > MAX_FRAME {
                    resync(buf);
                    continue;
                }
                return; // wait for more data
            }
            Err(_) => {
                // Bad type / CRC / oversize declared length at this offset — drop
                // the corrupt prefix and resync.
                resync(buf);
            }
        }
    }
}

/// Drop leading garbage until `buf` begins with the `CA 50` magic, or trim to a
/// single trailing `0xCA` that could start the magic on the next chunk. Runs in
/// O(n) (memchr-like) rather than `remove(0)` one byte at a time.
fn resync(buf: &mut Vec<u8>) {
    // Callers guarantee buf[0] is not a usable frame start, so scan from 1.
    let mut i = 1;
    while i + 1 < buf.len() {
        if buf[i] == MAGIC_0 && buf[i + 1] == MAGIC_1 {
            buf.drain(..i);
            return;
        }
        i += 1;
    }
    // No complete magic left — preserve a lone trailing 0xCA as a potential
    // prefix of the next frame; otherwise drop everything.
    if buf.last() == Some(&MAGIC_0) {
        let keep = buf.split_off(buf.len() - 1);
        *buf = keep;
    } else {
        buf.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_bytes(payload: &[u8]) -> Vec<u8> {
        encode(FrameType::Telemetry, 1, payload).unwrap()
    }

    #[test]
    fn encode_decode_roundtrip() {
        let payload = b"{\"batt_pct\":87}";
        let wire = frame_bytes(payload);
        let (frame, consumed) = try_decode(&wire).unwrap();
        assert_eq!(consumed, wire.len());
        assert_eq!(frame.frame_type, FrameType::Telemetry);
        assert_eq!(frame.seq, 1);
        assert_eq!(frame.payload, payload);
    }

    #[test]
    fn encode_rejects_oversized_payload() {
        let big = vec![0u8; u16::MAX as usize + 1];
        assert!(matches!(
            encode(FrameType::Command, 0, &big),
            Err(FrameError::PayloadTooLarge(_))
        ));
    }

    #[test]
    fn try_decode_rejects_oversized_length() {
        // Header declaring a payload beyond MAX_FRAME.
        let mut wire = vec![MAGIC_0, MAGIC_1, FrameType::Command as u8, 0xFF, 0xFF, 0, VERSION];
        wire.resize(9 + MAX_FRAME * 4, 0);
        assert!(matches!(
            try_decode(&wire),
            Err(FrameError::FrameTooLarge { .. })
        ));
    }

    #[test]
    fn drain_frames_resyncs_on_garbage() {
        let mut buf = vec![0xDE, 0xAD, 0xBE, 0xEF];
        buf.extend_from_slice(&frame_bytes(b"one"));
        buf.extend_from_slice(&frame_bytes(b"two"));
        let mut frames = Vec::new();
        drain_frames(&mut buf, &mut frames);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].payload, b"one");
        assert_eq!(frames[1].payload, b"two");
        assert!(buf.is_empty());
    }

    #[test]
    fn drain_frames_caps_runaway_buffer() {
        // A header declaring a huge length never completes → the buffer must not
        // grow unboundedly; drain_frames should drop the bogus data.
        let mut buf = vec![MAGIC_0, MAGIC_1, FrameType::Telemetry as u8, 0xFF, 0x7F, 0, VERSION];
        buf.extend(std::iter::repeat_n(0xAA, MAX_FRAME));
        let mut frames = Vec::new();
        drain_frames(&mut buf, &mut frames);
        assert!(frames.is_empty());
        assert!(buf.len() <= 1); // at most a trailing partial magic
    }
}
