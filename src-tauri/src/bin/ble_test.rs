//! Standalone BLE test binary: connects to an ESP32-C5 board advertising as
//! "CaPilot-C5" exposing a Nordic UART Service (NUS), subscribes to the TX
//! characteristic, decodes telemetry frames using the REAL IDE protocol module
//! (`capilot_ide_lib::esp::protocol`), and sends a ping command via RX.
//!
//! Frame format (matching `src/esp/protocol.rs`):
//!   Magic   : 0xCA 0x50            (2 bytes)
//!   Type    : 1 byte               (0x01 telemetry, 0x02 command/ack, …)
//!   Length  : u16 little-endian    (2 bytes) — payload length
//!   Seq     : 1 byte               (monotonic)
//!   Version : 1 byte               (0x01)
//!   Payload : JSON string          (Len bytes)
//!   CRC16   : u16 little-endian    (2 bytes) — CCITT over type..payload
//!
//! Linux/BlueZ: requires DBus access to org.bluez (user in `bluetooth` group
//! or run with appropriate polkit rules).

use capilot_ide_lib::esp::protocol::{drain_frames, encode, Frame, FrameType};
use std::error::Error;
use std::time::{Duration, Instant};

use btleplug::api::{Central, CharPropFlags, Manager as _, Peripheral as _, ScanFilter, WriteType};
use btleplug::platform::Manager;
use futures::stream::StreamExt;
use uuid::Uuid;

// --- Nordic UART Service UUIDs -------------------------------------------------
const NUS_SERVICE_UUID: Uuid = Uuid::from_u128(0x6e400001_b5a3_f393_e0a9_e50e24dcca9e);
const NUS_TX_UUID: Uuid = Uuid::from_u128(0x6e400002_b5a3_f393_e0a9_e50e24dcca9e); // notify
const NUS_RX_UUID: Uuid = Uuid::from_u128(0x6e400003_b5a3_f393_e0a9_e50e24dcca9e); // write

const TARGET_NAME: &str = "CaPilot-C5";
const SCAN_TIMEOUT: Duration = Duration::from_secs(10);
const COLLECT_TIMEOUT: Duration = Duration::from_secs(15);
const SEND_CMD_AFTER: Duration = Duration::from_secs(3);

const TEST_CMD_PAYLOAD: &str = r#"{"cmd":"ping"}"#;

fn hex_dump(data: &[u8]) {
    for (i, chunk) in data.chunks(16).enumerate() {
        let hex: Vec<String> = chunk.iter().map(|b| format!("{:02X}", b)).collect();
        let ascii: String = chunk
            .iter()
            .map(|&b| if b.is_ascii_graphic() || b == b' ' { b as char } else { '.' })
            .collect();
        println!("  {:04X}  {:48} |{}|", i * 16, hex.join(" "), ascii);
    }
}

fn print_frame(f: &Frame, n: usize) {
    let type_name = match f.frame_type {
        FrameType::Telemetry => "telemetry",
        FrameType::Command => "command",
        FrameType::Ack => "ack",
        FrameType::AudioChunk => "audio",
        FrameType::Heartbeat => "heartbeat",
    };
    println!(
        "--- Frame #{} type={:?} ({} seq={} ver={} len={}) ---",
        n, f.frame_type, type_name, f.seq, f.version, f.payload.len()
    );
    hex_dump(&f.payload);
    if let Ok(s) = std::str::from_utf8(&f.payload) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(s) {
            println!(
                "  decoded JSON: {}",
                serde_json::to_string_pretty(&v).unwrap_or_default()
            );
        } else {
            println!("  payload: {}", s);
        }
    }
}

fn is_ack(f: &Frame) -> bool {
    f.frame_type == FrameType::Ack || f.frame_type == FrameType::Command
}

// --- main ----------------------------------------------------------------------
#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    println!("=== CaPilot-C5 NUS BLE Test (IDE protocol.rs) ===");

    if let Ok(out) = std::process::Command::new("bluetoothctl")
        .args(["power", "on"])
        .output()
    {
        println!("bluetoothctl power on -> {}", String::from_utf8_lossy(&out.stdout).trim());
    }

    let manager = Manager::new().await?;
    let adapters = manager.adapters().await?;
    if adapters.is_empty() {
        eprintln!("No Bluetooth adapters found. Is BlueZ running?");
        std::process::exit(1);
    }
    let adapter = &adapters[0];
    println!("Adapter: {}", adapter.adapter_info().await?);

    adapter.start_scan(ScanFilter::default()).await?;
    println!("Scanning for '{}' (up to {}s)...", TARGET_NAME, SCAN_TIMEOUT.as_secs());

    let scan_deadline = Instant::now() + SCAN_TIMEOUT;
    let mut target = None;
    while Instant::now() < scan_deadline {
        for p in adapter.peripherals().await? {
            let props = p.properties().await?;
            let name_match = props
                .as_ref()
                .and_then(|x| x.local_name.clone())
                .or_else(|| props.as_ref().and_then(|x| x.advertisement_name.clone()))
                .map(|n| n == TARGET_NAME)
                .unwrap_or(false);
            let service_match = props
                .as_ref()
                .map_or(false, |x| x.services.contains(&NUS_SERVICE_UUID));
            if name_match || service_match {
                println!(
                    "Found target: address={} rssi={:?}",
                    p.address(),
                    props.as_ref().and_then(|x| x.rssi)
                );
                target = Some(p);
                break;
            }
        }
        if target.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    adapter.stop_scan().await?;

    let Some(peripheral) = target else {
        eprintln!("Target '{}' not found within {}s.", TARGET_NAME, SCAN_TIMEOUT.as_secs());
        std::process::exit(1);
    };

    println!("Connecting to {}...", peripheral.address());
    peripheral.connect().await?;
    println!("Connected.");

    peripheral.discover_services().await?;
    let characteristics = peripheral.characteristics();
    let tx = characteristics.iter().find(|c| c.uuid == NUS_TX_UUID).ok_or("TX char not found")?;
    let rx = characteristics.iter().find(|c| c.uuid == NUS_RX_UUID).ok_or("RX char not found")?;

    println!(
        "TX char 6E400002: notify={} read={}",
        tx.properties.contains(CharPropFlags::NOTIFY),
        tx.properties.contains(CharPropFlags::READ)
    );
    println!(
        "RX char 6E400003: write={} write_no_resp={}",
        rx.properties.contains(CharPropFlags::WRITE),
        rx.properties.contains(CharPropFlags::WRITE_WITHOUT_RESPONSE)
    );

    let mut notifications = peripheral.notifications().await?;
    peripheral.subscribe(tx).await?;
    println!("Subscribed to TX notifications. Collecting for {}s...", COLLECT_TIMEOUT.as_secs());

    let start = Instant::now();
    let mut buf: Vec<u8> = Vec::new();
    let mut frame_count: usize = 0;
    let mut sent_cmd = false;
    let mut saw_ack = false;

    loop {
        let elapsed = start.elapsed();
        if elapsed >= COLLECT_TIMEOUT {
            break;
        }
        match tokio::time::timeout(Duration::from_millis(250), notifications.next()).await {
            Ok(Some(n)) => {
                if n.uuid == NUS_TX_UUID {
                    buf.extend_from_slice(&n.value);
                    let mut frames: Vec<Frame> = Vec::new();
                    drain_frames(&mut buf, &mut frames);
                    for f in frames {
                        frame_count += 1;
                        print_frame(&f, frame_count);
                        if is_ack(&f) {
                            saw_ack = true;
                            println!(">>> ACK detected in frame #{}", frame_count);
                        }
                    }
                }
            }
            Ok(None) => {
                println!("Notification stream ended.");
                break;
            }
            Err(_elapsed) => {
                if !sent_cmd && start.elapsed() >= SEND_CMD_AFTER {
                    sent_cmd = true;
                    let cmd = encode(FrameType::Command, 0, TEST_CMD_PAYLOAD.as_bytes());
                    println!(">>> Sending test command via RX ({} bytes):", cmd.len());
                    hex_dump(&cmd);
                    match peripheral.write(rx, &cmd, WriteType::WithResponse).await {
                        Ok(_) => println!(">>> Command written, waiting for ACK..."),
                        Err(e) => println!(">>> WithResponse write failed: {e}"),
                    }
                }
            }
        }
    }

    println!("=== Summary ===");
    println!("Telemetry frames received: {}", frame_count);
    println!("Test command sent: {}", sent_cmd);
    println!("ACK received: {}", saw_ack);
    if !buf.is_empty() {
        println!("Note: {} partial bytes left in buffer.", buf.len());
    }

    peripheral.unsubscribe(tx).await?;
    peripheral.disconnect().await?;
    println!("Disconnected. Done.");
    Ok(())
}
