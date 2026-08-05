//! ESP manager — Tauri-managed state that owns the active transport, forwards
//! ESP events to the frontend, and tracks the latest telemetry/status.

use crate::esp::ble::BleUart;
use crate::esp::protocol::{encode, FrameType};
use crate::esp::transport::{EspError, EspEvent, EspStatus, EspTransport, SeqCounter, TransportKind};
use std::sync::Arc;
use tauri::Emitter;
use tokio::sync::Mutex;

pub const ESP_EVENT: &str = "esp://event";

pub struct EspManager {
    transport: Arc<Mutex<Option<Box<dyn EspTransport>>>>,
    status: Arc<Mutex<EspStatus>>,
    seq: Mutex<SeqCounter>,
}

impl EspManager {
    pub fn new() -> Self {
        Self {
            transport: Arc::new(Mutex::new(None)),
            status: Arc::new(Mutex::new(EspStatus::default())),
            seq: Mutex::new(SeqCounter(0)),
        }
    }

    /// Current status snapshot for the UI.
    pub async fn status(&self) -> EspStatus {
        self.status.lock().await.clone()
    }

    /// Connect the BLE transport and begin forwarding events.
    pub async fn connect_ble(&self, app: tauri::AppHandle) -> Result<(), EspError> {
        let mut transport = BleUart::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<EspEvent>(128);

        transport.connect(tx).await?;

        // Update status: connected.
        let mut status = self.status.lock().await;
        status.connected = true;
        status.kind = Some(TransportKind::Ble);
        drop(status);

        // Store transport (stops the old one if present).
        let mut slot = self.transport.lock().await;
        if let Some(mut old) = slot.take() {
            let _ = old.disconnect().await;
        }
        *slot = Some(Box::new(transport));

        // Forward events from the transport channel to the frontend.
        let status_arc = self.status.clone();
        let forward = async move {
            while let Some(evt) = rx.recv().await {
                match &evt {
                    EspEvent::Connected { name, address } => {
                        let mut st = status_arc.lock().await;
                        st.connected = true;
                        st.kind = Some(TransportKind::Ble);
                        st.name = Some(name.clone());
                        st.address = Some(address.clone());
                    }
                    EspEvent::Disconnected { reason } => {
                        let mut st = status_arc.lock().await;
                        st.connected = false;
                        st.last_seen_ms = None;
                        st.battery_pct = None;
                        log::info!("ESP disconnected: {reason}");
                    }
                    EspEvent::Telemetry { battery_pct, battery_mv, .. } => {
                        let mut st = status_arc.lock().await;
                        st.battery_pct = Some(*battery_pct);
                        st.battery_mv = Some(*battery_mv);
                        st.last_seen_ms = Some(
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as u64)
                                .unwrap_or(0),
                        );
                    }
                    _ => {}
                }
                let _ = app.emit(ESP_EVENT, &evt);
            }
        };
        tokio::spawn(forward);

        Ok(())
    }

    /// Disconnect the active transport.
    pub async fn disconnect(&self) -> Result<(), EspError> {
        let mut slot = self.transport.lock().await;
        if let Some(mut t) = slot.take() {
            t.disconnect().await?;
        }
        let mut status = self.status.lock().await;
        status.connected = false;
        status.battery_pct = None;
        status.last_seen_ms = None;
        Ok(())
    }

    /// Send a command frame (type 0x02) with the given payload.
    pub async fn send_command(&self, payload: &[u8]) -> Result<(), EspError> {
        let slot = self.transport.lock().await;
        let t = slot.as_ref().ok_or(EspError::NotConnected)?;
        let seq = self.seq.lock().await.next();
        t.send(FrameType::Command, seq, payload).await
    }

    /// Send an arbitrary raw frame (used for heartbeats etc.).
    #[allow(dead_code)]
    pub async fn send_raw(&self, frame_type: FrameType, payload: &[u8]) -> Result<(), EspError> {
        let slot = self.transport.lock().await;
        let t = slot.as_ref().ok_or(EspError::NotConnected)?;
        let seq = self.seq.lock().await.next();
        t.send(frame_type, seq, payload).await
    }

    /// Convenience: build+send a JSON command.
    #[allow(dead_code)]
    pub async fn send_json(&self, value: &serde_json::Value) -> Result<(), EspError> {
        let payload = serde_json::to_vec(value).map_err(|e| EspError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData, e)))?;
        self.send_command(&payload).await
    }
}

/// Wire helper used by commands.
#[allow(dead_code)]
pub fn encode_frame(frame_type: FrameType, payload: &[u8]) -> Vec<u8> {
    encode(frame_type, 0, payload)
}
