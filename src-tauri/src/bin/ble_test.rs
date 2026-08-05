//! Standalone BLE test binary: connects to an ESP32-C5 board advertising as
//! "CaPilot-C5" exposing a Nordic UART Service (NUS), subscribes to the TX
//! characteristic notifications, prints incoming telemetry frames (hex + JSON
//! decode), and sends a health-check command via the RX characteristic.
//!
//! Frame format (from the C5 firmware):
//!   Magic   : 0xCA 0x50            (2 bytes)
//!   Type    : 1 byte               (0x01 = telemetry, ...)
//!   Length  : u16 little-endian    (2 bytes) - payload length
//!   Payload : JSON string          (Len bytes)
//!   CRC16   : u16 little-endian    (2 bytes) - CCITT over everything above
//!
//! Linux/BlueZ notes:
//!   - Requires a Bluetooth adapter (BlueZ) and DBus access. The process must
//!     be able to talk to org.bluez on the system bus (user in `bluetooth`
//!     group, or polkit rules, or run as root).
//!   - The adapter is powered on at start via `bluetoothctl power on`
//!     (non-fatal if it fails).

use std::error::Error;
use std::time::{Duration, Instant};

use btleplug::api::{
    Central, CharPropFlags, Manager as _, Peripheral as _, ScanFilter, WriteType,
};
use btleplug::platform::Manager;
use futures::stream::StreamExt;
use uuid::Uuid;

// --- Nordic UART Service UUIDs -------------------------------------------------
const NUS_SERVICE_UUID: Uuid = Uuid::from_u128(0x6e400001_b5a3_f393_e0a9_e50e24dcca9e);
const NUS_TX_UUID: Uuid = Uuid::from_u128(0x6e400002_b5a3_f393_e0a9_e50e24dcca9e); // notify
const NUS_RX_UUID: Uuid = Uuid::from_u128(0x6e400003_b5a3_f393_e0a9_e50e24dcca9e); // write

// --- Tunables ------------------------------------------------------------------
const TARGET_NAME: &str = "CaPilot-C5";
const SCAN_TIMEOUT: Duration = Duration::from_secs(10);
const COLLECT_TIMEOUT: Duration = Duration::from_secs(15);
const SEND_CMD_AFTER: Duration = Duration::from_secs(3); // send command 3s in

const FRAME_MAGIC: [u8; 2] = [0xCA, 0x50];
const TYPE_TELEMETRY: u8 = 0x01;
const TYPE_COMMAND: u8 = 0x02;

const TEST_CMD_PAYLOAD: &str = r#"{"cmd":"ping"}"#;
// Smaller variant used if the full ping frame fails on the RX write.
const TEST_CMD_PAYLOAD_SHORT: &str = r#"{"p":1}"#;

// --- Telemetry frame -----------------------------------------------------------
struct TelemetryFrame {
    frame_type: u8,
    payload: Vec<u8>,
    crc: u16,
    crc_ok: bool,
}

impl TelemetryFrame {
    /// Build a properly framed command (Magic + Type + Len + Payload + CRC).
    /// CRC is computed over Type+Len+Payload (bytes[2..]), matching the firmware.
    fn command_frame(payload: &[u8]) -> Vec<u8> {
        let mut frame = Vec::with_capacity(7 + payload.len());
        frame.extend_from_slice(&FRAME_MAGIC);
        frame.push(TYPE_COMMAND);
        frame.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        frame.extend_from_slice(payload);
        let crc = crc16_ccitt(&frame[2..]); // skip magic, match firmware
        frame.extend_from_slice(&crc.to_le_bytes());
        frame
    }
}

// --- CRC16 (CCITT, poly 0x1021, init 0xFFFF) ------------------------------------
fn crc16_ccitt(data: &[u8]) -> u16 {
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

/// Try to extract complete frames from a byte buffer, resyncing on garbage.
fn try_extract_frames(buf: &mut Vec<u8>, frames: &mut Vec<TelemetryFrame>) {
    loop {
        if buf.len() < 7 {
            return;
        }
        if buf[0] != FRAME_MAGIC[0] || buf[1] != FRAME_MAGIC[1] {
            // Out of sync: drop one byte and try again.
            println!("  [sync] dropping stray byte 0x{:02X}", buf[0]);
            buf.remove(0);
            continue;
        }
        let len = u16::from_le_bytes([buf[3], buf[4]]) as usize;
        let total = 7 + len;
        if buf.len() < total {
            return; // incomplete frame, wait for more data
        }
        let payload = buf[5..5 + len].to_vec();
        let crc = u16::from_le_bytes([buf[5 + len], buf[6 + len]]);
        // CRC is computed over Type+Len+Payload (bytes[2..5+len]), matching the
        // firmware's crc16_ccitt(&buf[2], idx-2). Magic bytes are NOT included.
        let computed = crc16_ccitt(&buf[2..5 + len]);
        frames.push(TelemetryFrame {
            frame_type: buf[2],
            payload,
            crc,
            crc_ok: crc == computed,
        });
        buf.drain(..total);
    }
}

fn hex_dump(data: &[u8]) {
    for (i, chunk) in data.chunks(16).enumerate() {
        let hex: Vec<String> = chunk.iter().map(|b| format!("{:02X}", b)).collect();
        let ascii: String = chunk
            .iter()
            .map(|&b| {
                if b.is_ascii_graphic() || b == b' ' {
                    b as char
                } else {
                    '.'
                }
            })
            .collect();
        println!("  {:04X}  {:48} |{}|", i * 16, hex.join(" "), ascii);
    }
}

fn print_frame(f: &TelemetryFrame, n: usize) {
    println!(
        "--- Frame #{} type=0x{:02X}{} len={} crc_rx=0x{:04X} crc_ok={} ---",
        n,
        f.frame_type,
        match f.frame_type {
            TYPE_TELEMETRY => " (telemetry)",
            _ => "",
        },
        f.payload.len(),
        f.crc,
        f.crc_ok
    );
    hex_dump(&f.payload);
    if f.crc_ok {
        if let Ok(s) = std::str::from_utf8(&f.payload) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(s) {
                println!("  decoded JSON: {}", serde_json::to_string_pretty(&v).unwrap_or_default());
            } else {
                println!("  payload (not JSON): {}", s);
            }
        } else {
            println!("  payload is not valid UTF-8");
        }
    } else {
        println!("  payload skipped (CRC mismatch)");
    }
}

/// Heuristic ACK detection: frame type 0x02 (ACK), or JSON/payload mentioning
/// "ok"/"ack"/"pong". The C5 firmware echoes a type-0x02 frame with payload "ok".
fn is_ack(f: &TelemetryFrame) -> bool {
    if f.frame_type == TYPE_COMMAND {
        return true; // firmware ACK frames reuse type 0x02
    }
    let s = match std::str::from_utf8(&f.payload) {
        Ok(s) => s.to_lowercase(),
        Err(_) => return false,
    };
    s.contains("ack") || s.contains("\"ok\"") || s.contains("pong") || s == "ok" || s.trim() == "ok"
}

// --- main ----------------------------------------------------------------------
#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    println!("=== CaPilot-C5 NUS BLE Test ===");

    // Ensure the adapter is powered on (best effort; may require sudo/group).
    if let Ok(out) = std::process::Command::new("bluetoothctl")
        .args(["power", "on"])
        .output()
    {
        println!("bluetoothctl power on -> {}", String::from_utf8_lossy(&out.stdout).trim());
    } else {
        println!("note: could not run bluetoothctl (is bluez installed?)");
    }

    // --- 1. Adapter manager ----------------------------------------------------
    let manager = Manager::new().await?;
    let adapters = manager.adapters().await?;
    if adapters.is_empty() {
        eprintln!("No Bluetooth adapters found. Is BlueZ running?");
        std::process::exit(1);
    }
    let adapter = &adapters[0];
    println!("Adapter: {}", adapter.adapter_info().await?);

    // --- 2. Scan for the target --------------------------------------------------
    adapter.start_scan(ScanFilter::default()).await?;
    println!(
        "Scanning for '{}' (up to {}s)...",
        TARGET_NAME,
        SCAN_TIMEOUT.as_secs()
    );

    let scan_deadline = Instant::now() + SCAN_TIMEOUT;
    let mut target = None;
    while Instant::now() < scan_deadline {
        for p in adapter.peripherals().await? {
            let props = p.properties().await?;
            let local_name = props.as_ref().and_then(|x| x.local_name.clone());
            let adv_name = props.as_ref().and_then(|x| x.advertisement_name.clone());
            let name_match = [&local_name, &adv_name]
                .into_iter()
                .flatten()
                .any(|n| n == TARGET_NAME);
            let service_match = props
                .as_ref()
                .map_or(false, |x| x.services.contains(&NUS_SERVICE_UUID));
            if name_match || service_match {
                println!(
                    "Found target: address={} local_name={:?} adv_name={:?} rssi={:?}",
                    p.address(),
                    local_name,
                    adv_name,
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

    // --- 3. Connect ---------------------------------------------------------------
    println!("Connecting to {}...", peripheral.address());
    peripheral.connect().await?;
    println!("Connected.");

    peripheral.discover_services().await?;

    // --- 4. Find NUS characteristics ----------------------------------------------
    let characteristics = peripheral.characteristics();
    let tx = characteristics
        .iter()
        .find(|c| c.uuid == NUS_TX_UUID)
        .ok_or("NUS TX characteristic (6E400002) not found")?;
    let rx = characteristics
        .iter()
        .find(|c| c.uuid == NUS_RX_UUID)
        .ok_or("NUS RX characteristic (6E400003) not found")?;

    println!("TX char 6E400002: notify={} read={}",
        tx.properties.contains(CharPropFlags::NOTIFY),
        tx.properties.contains(CharPropFlags::READ));
    println!("RX char 6E400003: write={} write_no_resp={}",
        rx.properties.contains(CharPropFlags::WRITE),
        rx.properties.contains(CharPropFlags::WRITE_WITHOUT_RESPONSE));

    // --- 5. Subscribe to TX notifications ------------------------------------------
    let mut notifications = peripheral.notifications().await?; // must exist before subscribe
    peripheral.subscribe(tx).await?;
    println!("Subscribed to TX notifications. Collecting for {}s...", COLLECT_TIMEOUT.as_secs());

    // --- 6. Collect telemetry + send test command ----------------------------------
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
                    println!("--- Notification {} bytes ---", n.value.len());
                    hex_dump(&n.value);
                    buf.extend_from_slice(&n.value);
                    let mut frames = Vec::new();
                    try_extract_frames(&mut buf, &mut frames);
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
                // 250ms tick: check if it's time to send the test command
                if !sent_cmd && start.elapsed() >= SEND_CMD_AFTER {
                    sent_cmd = true;
                    // Try full ping frame with response first
                    let cmd = TelemetryFrame::command_frame(TEST_CMD_PAYLOAD.as_bytes());
                    println!(">>> Sending test command via RX ({} bytes):", cmd.len());
                    hex_dump(&cmd);
                    match peripheral.write(rx, &cmd, WriteType::WithResponse).await {
                        Ok(_) => {
                            println!(">>> Command written, waiting for ACK...");
                        }
                        Err(e) => {
                            println!(">>> WithResponse write failed: {e}");
                            println!(">>> Retrying with shorter frame + WithoutResponse...");
                            let cmd2 =
                                TelemetryFrame::command_frame(TEST_CMD_PAYLOAD_SHORT.as_bytes());
                            hex_dump(&cmd2);
                            match peripheral.write(rx, &cmd2, WriteType::WithoutResponse).await {
                                Ok(_) => println!(">>> Short command written, waiting for ACK..."),
                                Err(e2) => println!(">>> Short WithoutResponse write failed: {e2}"),
                            }
                        }
                    }
                }
            }
        }
    }

    // --- 7. Summary -----------------------------------------------------------------
    println!("=== Summary ===");
    println!("Telemetry frames received: {}", frame_count);
    println!("Test command sent: {}", sent_cmd);
    println!("ACK received: {}", saw_ack);
    if buf.len() > 0 {
        println!("Note: {} partial bytes left in buffer (incomplete frame).", buf.len());
    }

    peripheral.unsubscribe(tx).await?;
    peripheral.disconnect().await?;
    println!("Disconnected. Done.");
    Ok(())
}
