//! BLE UART (NUS) transport — the default ESP link (DevPlan §8.1).
//!
//! Implements `EspTransport` over btleplug + the Nordic UART Service:
//!   NUS Service 6E400001-B5A3-F393-E0A9-E50E24DCCA9E
//!   TX          6E400002 (notify  — ESP → IDE)
//!   RX          6E400003 (write   — IDE → ESP)

use crate::esp::protocol::{drain_frames, Frame};
use crate::esp::transport::{EspError, EspEvent, EspTransport, TransportKind};
use async_trait::async_trait;
use btleplug::api::{Central, Manager as _, Peripheral as _, ScanFilter, WriteType};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures::stream::StreamExt;
use tokio::sync::mpsc;
use uuid::Uuid;

const NUS_SERVICE_UUID: Uuid = Uuid::from_u128(0x6e400001_b5a3_f393_e0a9_e50e24dcca9e);
const NUS_TX_UUID: Uuid = Uuid::from_u128(0x6e400002_b5a3_f393_e0a9_e50e24dcca9e);
const NUS_RX_UUID: Uuid = Uuid::from_u128(0x6e400003_b5a3_f393_e0a9_e50e24dcca9e);

const TARGET_NAME: &str = "CaPilot-C5";
const SCAN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// RAII guard: stops the Bluetooth scan on drop so mid-loop errors don't leave
/// the adapter scanning forever.
struct ScanGuard {
    adapter: Adapter,
}

impl Drop for ScanGuard {
    fn drop(&mut self) {
        let adapter = self.adapter.clone();
        // We're inside the tokio runtime (async fn), so block_on would panic;
        // spawn the async stop instead.
        tokio::spawn(async move {
            let _ = adapter.stop_scan().await;
        });
    }
}

/// RAII guard: disconnects a peripheral on drop. Armed right after a successful
/// `connect()`, so any failure in discover/subscribe tears the device down
/// instead of leaking a connected peripheral.
struct ConnectGuard {
    peripheral: Option<Peripheral>,
}

impl ConnectGuard {
    /// Success path: stop the guard from disconnecting on drop.
    fn disarm(&mut self) {
        self.peripheral = None;
    }
}

impl Drop for ConnectGuard {
    fn drop(&mut self) {
        if let Some(p) = self.peripheral.take() {
            tokio::spawn(async move {
                let _ = p.disconnect().await;
            });
        }
    }
}

pub struct BleUart {
    peripheral: Option<Peripheral>,
    rx_uuid: Option<Uuid>,
    reader_handle: Option<tokio::task::JoinHandle<()>>,
    address: Option<String>,
    rssi: Option<i16>,
}

impl BleUart {
    pub fn new() -> Self {
        Self {
            peripheral: None,
            rx_uuid: None,
            reader_handle: None,
            address: None,
            rssi: None,
        }
    }

    fn power_on_adapter() {
        if let Ok(out) = std::process::Command::new("bluetoothctl")
            .args(["power", "on"])
            .output()
        {
            log::debug!(
                "bluetoothctl power on: {}",
                String::from_utf8_lossy(&out.stdout).trim()
            );
        }
    }

    async fn scan_and_connect() -> Result<Peripheral, EspError> {
        let manager = Manager::new()
            .await
            .map_err(|e| EspError::Ble(e.to_string()))?;
        let adapters = manager
            .adapters()
            .await
            .map_err(|e| EspError::Ble(e.to_string()))?;
        let adapter = adapters.first().ok_or(EspError::NoAdapter)?;

        adapter
            .start_scan(ScanFilter::default())
            .await
            .map_err(|e| EspError::Ble(e.to_string()))?;

        // Stop the scan on every exit path (success or error).
        let _scan_guard = ScanGuard {
            adapter: adapter.clone(),
        };

        let deadline = std::time::Instant::now() + SCAN_TIMEOUT;
        let mut found: Option<Peripheral> = None;
        while std::time::Instant::now() < deadline {
            for p in adapter
                .peripherals()
                .await
                .map_err(|e| EspError::Ble(e.to_string()))?
            {
                let props = p
                    .properties()
                    .await
                    .map_err(|e| EspError::Ble(e.to_string()))?;
                let name_match = props
                    .as_ref()
                    .and_then(|x| x.local_name.clone())
                    .or_else(|| props.as_ref().and_then(|x| x.advertisement_name.clone()))
                    .map(|n| n == TARGET_NAME)
                    .unwrap_or(false);
                let service_match = props
                    .as_ref()
                    .is_some_and(|x| x.services.contains(&NUS_SERVICE_UUID));
                if name_match || service_match {
                    found = Some(p);
                    break;
                }
            }
            if found.is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        }

        found.ok_or(EspError::ScanTimeout)
    }
}

#[async_trait]
impl EspTransport for BleUart {
    fn kind(&self) -> TransportKind {
        TransportKind::Ble
    }

    async fn connect(
        &mut self,
        events: mpsc::Sender<EspEvent>,
    ) -> Result<(), EspError> {
        Self::power_on_adapter();

        let peripheral = Self::scan_and_connect().await?;
        self.address = Some(peripheral.address().to_string());
        if let Ok(props) = peripheral.properties().await {
            self.rssi = props.and_then(|p| p.rssi);
        }

        peripheral
            .connect()
            .await
            .map_err(|e| EspError::Ble(format!("connect: {e}")))?;

        // From here on the peripheral is connected: if anything below fails,
        // the guard tears it down instead of leaking it.
        let mut guard = ConnectGuard {
            peripheral: Some(peripheral.clone()),
        };

        peripheral
            .discover_services()
            .await
            .map_err(|e| EspError::Ble(format!("discover_services: {e}")))?;

        let chars = peripheral.characteristics();
        let tx = chars
            .iter()
            .find(|c| c.uuid == NUS_TX_UUID)
            .ok_or_else(|| EspError::Ble("NUS TX char not found".into()))?;
        let rx = chars
            .iter()
            .find(|c| c.uuid == NUS_RX_UUID)
            .ok_or_else(|| EspError::Ble("NUS RX char not found".into()))?;

        // Notification stream must be created before subscribe.
        let mut notifications = peripheral
            .notifications()
            .await
            .map_err(|e| EspError::Ble(format!("notifications: {e}")))?;
        peripheral
            .subscribe(tx)
            .await
            .map_err(|e| EspError::Ble(format!("subscribe: {e}")))?;

        self.rx_uuid = Some(rx.uuid);
        self.peripheral = Some(peripheral.clone());

        let evt_sink = events.clone();
        let addr = self.address.clone().unwrap_or_default();
        let _ = evt_sink.send(EspEvent::Connected {
            name: TARGET_NAME.to_string(),
            address: addr,
        }).await;

        let mut frame_buf: Vec<u8> = Vec::new();
        self.reader_handle = Some(tokio::spawn(async move {
            while let Some(n) = notifications.next().await {
                frame_buf.extend_from_slice(&n.value);
                let mut frames: Vec<Frame> = Vec::new();
                drain_frames(&mut frame_buf, &mut frames);
                for f in frames {
                    let _ = evt_sink.send(EspEvent::Frame {
                        frame_type: f.frame_type,
                        seq: f.seq,
                        payload: f.payload.clone(),
                    }).await;
                    // Convenience typed telemetry event.
                    if let Ok(payload_str) = std::str::from_utf8(&f.payload) {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(payload_str) {
                            let _ = evt_sink.send(EspEvent::Telemetry {
                                battery_pct: v.get("batt_pct").and_then(|x| x.as_u64()).unwrap_or(0) as u8,
                                battery_mv: v.get("batt_mv").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
                                extra: v,
                            }).await;
                        }
                    }
                }
            }
            let _ = evt_sink
                .send(EspEvent::Disconnected { reason: "notification stream ended".into() })
                .await;
        }));

        // Everything succeeded — don't disconnect on drop.
        guard.disarm();
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), EspError> {
        if let Some(handle) = self.reader_handle.take() {
            handle.abort();
        }
        if let Some(peripheral) = self.peripheral.take() {
            if let Err(e) = peripheral.disconnect().await {
                log::warn!("BLE disconnect error: {e}");
            }
        }
        self.rx_uuid = None;
        Ok(())
    }

    async fn write_frame(&self, frame: &[u8]) -> Result<(), EspError> {
        let Some(peripheral) = self.peripheral.as_ref() else {
            return Err(EspError::NotConnected);
        };
        let Some(rx_uuid) = self.rx_uuid else {
            return Err(EspError::NotConnected);
        };
        let chars = peripheral.characteristics();
        let char = chars
            .iter()
            .find(|c| c.uuid == rx_uuid)
            .cloned()
            .ok_or_else(|| EspError::Ble("RX char not found".into()))?;
        peripheral
            .write(&char, frame, WriteType::WithResponse)
            .await
            .map_err(|e| EspError::Ble(format!("write: {e}")))
    }

    async fn rssi(&self) -> Option<i16> {
        self.rssi
    }
}
