//! EspTransport abstraction (DevPlan §8.1) — three implementations:
//! `BleUart` (default), `UsbSerial`, `WifiWs`.

use crate::esp::protocol::FrameType;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

/// Which physical link carries ESP traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    Ble,
    Usb,
    Wifi,
}

impl TransportKind {
    #[allow(dead_code)]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Ble => "Bluetooth",
            Self::Usb => "USB",
            Self::Wifi => "WiFi",
        }
    }
}

/// Connection/telemetry status surfaced to the UI.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EspStatus {
    pub connected: bool,
    pub kind: Option<TransportKind>,
    pub name: Option<String>,
    pub address: Option<String>,
    pub rssi: Option<i16>,
    pub battery_pct: Option<u8>,
    pub battery_mv: Option<u32>,
    pub last_seen_ms: Option<u64>,
}

/// A typed event produced by the transport's reader task, forwarded to the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EspEvent {
    Connected { name: String, address: String },
    Disconnected { reason: String },
    Frame { frame_type: FrameType, seq: u8, payload: Vec<u8> },
    Telemetry { battery_pct: u8, battery_mv: u32, extra: serde_json::Value },
    Error { message: String },
}

/// Error type for ESP transport operations.
#[derive(Debug, thiserror::Error)]
pub enum EspError {
    #[error("not connected")]
    NotConnected,
    #[error("scan timeout — device not found")]
    ScanTimeout,
    #[error("no Bluetooth adapter available")]
    NoAdapter,
    #[error("bluetooth: {0}")]
    Ble(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("frame: {0}")]
    Frame(#[from] crate::esp::protocol::FrameError),
}

/// Core transport trait. A transport owns the link and can emit frames.
///
/// Implementations spawn their own reader task (via the supplied event sender)
/// so `read` is push-based rather than polling.
#[async_trait]
#[allow(dead_code)]
pub trait EspTransport: Send + Sync {
    fn kind(&self) -> TransportKind;

    /// Establish the link and start the reader task. The transport pushes
    /// decoded events into `events` as they arrive. Returns once connected.
    async fn connect(&mut self, events: mpsc::Sender<EspEvent>) -> Result<(), EspError>;

    /// Tear down the link and stop the reader task.
    async fn disconnect(&mut self) -> Result<(), EspError>;

    /// Send a fully-encoded frame over the link.
    async fn write_frame(&self, frame: &[u8]) -> Result<(), EspError>;

    /// Convenience: encode + send.
    async fn send(&self, frame_type: FrameType, seq: u8, payload: &[u8]) -> Result<(), EspError> {
        let wire = crate::esp::protocol::encode(frame_type, seq, payload);
        self.write_frame(&wire).await
    }

    /// Current RSSI if the link exposes it (BLE).
    async fn rssi(&self) -> Option<i16> {
        None
    }
}

/// Sequence counter for frames we originate (IDE → ESP).
pub struct SeqCounter(pub u8);

impl SeqCounter {
    pub fn next(&mut self) -> u8 {
        let v = self.0;
        self.0 = self.0.wrapping_add(1);
        v
    }
}
