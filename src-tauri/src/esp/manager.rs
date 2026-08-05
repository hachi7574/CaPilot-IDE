//! ESP manager — Tauri-managed state that owns the active transport, forwards
//! ESP events to the frontend, and tracks the latest telemetry/status.

use crate::esp::ble::BleUart;
use crate::esp::protocol::{encode, FrameError, FrameType};
use crate::esp::transport::{EspError, EspEvent, EspStatus, EspTransport, SeqCounter, TransportKind};
use std::sync::Arc;
use tauri::Emitter;
use tokio::sync::Mutex;

pub const ESP_EVENT: &str = "esp://event";

pub struct EspManager {
    transport: Arc<Mutex<Option<Box<dyn EspTransport>>>>,
    status: Arc<Mutex<EspStatus>>,
    seq: Mutex<SeqCounter>,
    /// Join handle of the event-forwarding task for the current connection.
    /// Aborted on reconnect/disconnect so stale events from a previous
    /// transport can't keep reaching the frontend.
    forward_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl EspManager {
    pub fn new() -> Self {
        Self {
            transport: Arc::new(Mutex::new(None)),
            status: Arc::new(Mutex::new(EspStatus::default())),
            seq: Mutex::new(SeqCounter(0)),
            forward_task: Mutex::new(None),
        }
    }

    /// Current status snapshot for the UI.
    pub async fn status(&self) -> EspStatus {
        self.status.lock().await.clone()
    }

    /// Connect the BLE transport and begin forwarding events.
    pub async fn connect_ble(&self, app: tauri::AppHandle) -> Result<(), EspError> {
        // Stop any previous forward task so stale events can't reach the UI.
        self.abort_forward().await;
        // Tear down any previous transport cleanly (this stops its reader task).
        let mut slot = self.transport.lock().await;
        if let Some(mut old) = slot.take() {
            if let Err(e) = old.disconnect().await {
                log::warn!("previous ESP transport disconnect failed: {e}");
            }
        }
        drop(slot);

        let mut transport = BleUart::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<EspEvent>(128);

        if let Err(e) = transport.connect(tx).await {
            // The old transport is already gone — don't leave the UI with a
            // stale "connected" status.
            let mut status = self.status.lock().await;
            status.connected = false;
            status.name = None;
            status.address = None;
            drop(status);
            return Err(e);
        }

        // Update status: connected.
        let mut status = self.status.lock().await;
        status.connected = true;
        status.kind = Some(TransportKind::Ble);
        drop(status);

        *self.transport.lock().await = Some(Box::new(transport));

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
        *self.forward_task.lock().await = Some(tokio::spawn(forward));

        Ok(())
    }

    /// Disconnect the active transport and stop the forward task.
    pub async fn disconnect(&self) -> Result<(), EspError> {
        self.abort_forward().await;
        let mut slot = self.transport.lock().await;
        if let Some(mut t) = slot.take() {
            if let Err(e) = t.disconnect().await {
                log::warn!("ESP transport disconnect error: {e}");
            }
        }
        let mut status = self.status.lock().await;
        status.connected = false;
        status.battery_pct = None;
        status.last_seen_ms = None;
        Ok(())
    }

    /// Abort the current event-forwarding task (if any).
    async fn abort_forward(&self) {
        if let Some(handle) = self.forward_task.lock().await.take() {
            handle.abort();
        }
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
pub fn encode_frame(frame_type: FrameType, payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    encode(frame_type, 0, payload)
}
