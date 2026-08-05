//! ESP frame protocol (DevPlan §8.2).
//!
//! Wire format: `magic(2) | type(1) | len(2 LE) | seq(1) | version(1) | payload(len) | crc16(2 LE)`
//! CRC-16/CCITT (poly 0x1021, init 0xFFFF) computed over `type..payload` (i.e. all bytes
//! after the magic, before the CRC field).

use serde::{Deserialize, Serialize};

pub const MAGIC_0: u8 = 0xCA;
pub const MAGIC_1: u8 = 0x50;
pub const VERSION: u8 = 0x01;

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

/// Encode a frame into the wire format.
pub fn encode(frame_type: FrameType, seq: u8, payload: &[u8]) -> Vec<u8> {
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
    out
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
pub fn drain_frames(buf: &mut Vec<u8>, out: &mut Vec<Frame>) {
    loop {
        if buf.len() < 2 {
            return;
        }
        if buf[0] != MAGIC_0 || buf[1] != MAGIC_1 {
            // Out of sync: drop one byte.
            buf.remove(0);
            continue;
        }
        match try_decode(buf) {
            Ok((frame, consumed)) => {
                out.push(frame);
                buf.drain(..consumed);
            }
            Err(FrameError::TooShort(_)) => return, // wait for more data
            Err(_) => {
                // Bad magic/type/crc at this offset — drop one byte and resync.
                buf.remove(0);
            }
        }
    }
}
