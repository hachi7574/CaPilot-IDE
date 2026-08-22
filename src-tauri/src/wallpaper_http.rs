//! Loopback HTTP server for wallpaper videos on Linux.
//!
//! WebKitGTK's `<video>` is backed by GStreamer, which only opens URIs it
//! understands (`file://`, real `http(s)://`). Custom schemes (`asset://`,
//! `tauri://`, `capilot-media://`) and `blob:` all decode to zero frames —
//! confirmed 2026-08-21 by `scripts/webkit-video-probe.py`.
//!
//! This server binds `127.0.0.1:0`, always sends `Accept-Ranges: bytes`, and
//! serves the requested range untruncated (Tauri's `asset://` caps 206 at 1 MiB
//! and omits Accept-Ranges on the first 200). Windows / macOS never start it.

use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::thread;

const MAX_BYTES: u64 = 80 * 1024 * 1024;

static PORT: OnceLock<u16> = OnceLock::new();
static RESOURCE_DIR: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Still-image / looping-video extensions. Keep in sync with
/// `WALLPAPER_IMAGE_EXTS` / `WALLPAPER_VIDEO_EXTS` in `ui/state/themes.ts`.
const WALLPAPER_READ_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "webp", "gif", "bmp", "mp4", "webm", "mov", "m4v",
];

pub fn wallpaper_ext_ok(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| {
            WALLPAPER_READ_EXTS
                .iter()
                .any(|ok| e.eq_ignore_ascii_case(ok))
        })
}

/// Same allow-list as `fs_*`, plus the packaged resource dir.
pub fn wallpaper_bytes_path_ok(
    resolved: &Path,
    resource_dir: Option<&Path>,
) -> Result<bool, String> {
    if !wallpaper_ext_ok(resolved) {
        return Ok(false);
    }
    if crate::persistence::path_is_allowed(resolved)? {
        return Ok(true);
    }
    if let Some(root) = resource_dir {
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        if crate::persistence::path_is_within(resolved, &root) {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn wallpaper_media_mime(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "m4v" | "mp4" => "video/mp4",
        _ => "application/octet-stream",
    }
}

pub fn parse_single_byte_range(header: &str, len: u64) -> Option<(u64, u64)> {
    let spec = header.strip_prefix("bytes=")?;
    let spec = spec.split(',').next()?.trim();
    if let Some(suffix) = spec.strip_prefix('-') {
        let n: u64 = suffix.parse().ok()?;
        if n == 0 || len == 0 {
            return None;
        }
        let start = len.saturating_sub(n);
        return Some((start, len - 1));
    }
    let (start_s, end_s) = spec.split_once('-')?;
    let start: u64 = start_s.parse().ok()?;
    if start >= len {
        return None;
    }
    let end = if end_s.is_empty() {
        len - 1
    } else {
        end_s.parse::<u64>().ok()?.min(len - 1)
    };
    if end < start {
        return None;
    }
    Some((start, end))
}

/// Bind `127.0.0.1:0` and spawn the accept loop. Idempotent.
/// This module is compiled on Linux only; Windows / macOS play `asset://` natively.
pub fn start(resource_dir: Option<PathBuf>) -> Result<u16, String> {
    if let Some(port) = PORT.get() {
        return Ok(*port);
    }
    let _ = RESOURCE_DIR.set(resource_dir);
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("wallpaper http bind: {e}"))?;
    listener
        .set_nonblocking(false)
        .map_err(|e| format!("wallpaper http: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("wallpaper http addr: {e}"))?
        .port();
    let _ = PORT.set(port);
    thread::Builder::new()
        .name("wallpaper-http".into())
        .spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(s) => {
                        thread::spawn(move || {
                            if let Err(e) = handle_client(s) {
                                log::debug!("wallpaper http: {e}");
                            }
                        });
                    }
                    Err(e) => log::warn!("wallpaper http accept: {e}"),
                }
            }
        })
        .map_err(|e| format!("wallpaper http spawn: {e}"))?;
    log::info!("wallpaper http listening on 127.0.0.1:{port}");
    Ok(port)
}

pub fn port() -> Option<u16> {
    PORT.get().copied()
}

/// `http://127.0.0.1:<port>/wallpaper/<percent-encoded-abs-path>`
pub fn url_for(abs: &Path) -> Result<String, String> {
    let port = port().ok_or_else(|| "wallpaper http server is not running".to_string())?;
    let path = abs.to_string_lossy();
    let encoded = percent_encoding::utf8_percent_encode(&path, percent_encoding::NON_ALPHANUMERIC);
    Ok(format!("http://127.0.0.1:{port}/wallpaper/{encoded}"))
}

fn handle_client(mut stream: TcpStream) -> Result<(), String> {
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .ok();
    let mut buf = Vec::with_capacity(4096);
    let mut tmp = [0u8; 1024];
    loop {
        let n = stream.read(&mut tmp).map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 64 * 1024 {
            break;
        }
    }
    let header_end = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| "incomplete http headers".to_string())?;
    let header_text = std::str::from_utf8(&buf[..header_end]).map_err(|e| e.to_string())?;
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");
    if method != "GET" && method != "HEAD" {
        return write_status(&mut stream, 405, "Method Not Allowed", b"");
    }
    let mut range: Option<String> = None;
    for line in lines {
        let (name, rest) = match line.split_once(':') {
            Some(p) => p,
            None => continue,
        };
        if name.eq_ignore_ascii_case("range") {
            range = Some(rest.trim().to_string());
        }
    }
    serve_wallpaper(&mut stream, method == "HEAD", path, range.as_deref())
}

fn serve_wallpaper(
    stream: &mut TcpStream,
    head: bool,
    url_path: &str,
    range: Option<&str>,
) -> Result<(), String> {
    let encoded = url_path
        .strip_prefix("/wallpaper/")
        .ok_or_else(|| "not a wallpaper path".to_string())?;
    let decoded = percent_encoding::percent_decode_str(encoded)
        .decode_utf8()
        .map_err(|_| "bad encoding".to_string())?
        .into_owned();
    let resolved = match Path::new(&decoded).canonicalize() {
        Ok(p) => p,
        Err(_) => return write_status(stream, 404, "Not Found", b""),
    };
    let resource = RESOURCE_DIR.get().and_then(|o| o.as_deref());
    match wallpaper_bytes_path_ok(&resolved, resource) {
        Ok(true) => {}
        _ => return write_status(stream, 403, "Forbidden", b""),
    }
    if !resolved.is_file() {
        return write_status(stream, 404, "Not Found", b"");
    }
    let mut file = match std::fs::File::open(&resolved) {
        Ok(f) => f,
        Err(_) => return write_status(stream, 404, "Not Found", b""),
    };
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    if len > MAX_BYTES {
        return write_status(stream, 413, "Payload Too Large", b"");
    }
    let mime = wallpaper_media_mime(&resolved);

    if let Some(range_hdr) = range {
        let Some((start, end)) = parse_single_byte_range(range_hdr, len) else {
            let hdr = format!(
                "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{len}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(hdr.as_bytes()).map_err(|e| e.to_string())?;
            return Ok(());
        };
        let nbytes = end - start + 1;
        log::info!(
            "wallpaper http {} {} range {}-{} / {}",
            if head { "HEAD" } else { "GET" },
            resolved.display(),
            start,
            end,
            len
        );
        let mut body = vec![0u8; nbytes as usize];
        file.seek(SeekFrom::Start(start)).map_err(|e| e.to_string())?;
        file.read_exact(&mut body).map_err(|e| e.to_string())?;
        let hdr = format!(
            "HTTP/1.1 206 Partial Content\r\nContent-Type: {mime}\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {start}-{end}/{len}\r\nContent-Length: {nbytes}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(hdr.as_bytes()).map_err(|e| e.to_string())?;
        if !head {
            stream.write_all(&body).map_err(|e| e.to_string())?;
        }
        return Ok(());
    }

    log::info!(
        "wallpaper http {} {} 200 len={}",
        if head { "HEAD" } else { "GET" },
        resolved.display(),
        len
    );
    let hdr = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nAccept-Ranges: bytes\r\nContent-Length: {len}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(hdr.as_bytes()).map_err(|e| e.to_string())?;
    if !head {
        let mut body = Vec::with_capacity(len as usize);
        file.read_to_end(&mut body).map_err(|e| e.to_string())?;
        stream.write_all(&body).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn write_status(
    stream: &mut TcpStream,
    code: u16,
    reason: &str,
    body: &[u8],
) -> Result<(), String> {
    let hdr = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(hdr.as_bytes()).map_err(|e| e.to_string())?;
    if !body.is_empty() {
        stream.write_all(body).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_single_byte_range_covers_webkit_shapes() {
        assert_eq!(parse_single_byte_range("bytes=0-1023", 5000), Some((0, 1023)));
        assert_eq!(parse_single_byte_range("bytes=100-", 5000), Some((100, 4999)));
        assert_eq!(parse_single_byte_range("bytes=-200", 5000), Some((4800, 4999)));
        assert_eq!(parse_single_byte_range("bytes=0-999999", 5000), Some((0, 4999)));
        assert_eq!(parse_single_byte_range("bytes=5000-5010", 5000), None);
        assert_eq!(parse_single_byte_range("bytes=200-100", 5000), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn loopback_serves_range_and_accept_ranges() {
        let dir = std::env::temp_dir().join(format!(
            "capilot-wp-http-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("clip.mp4");
        let payload: Vec<u8> = (0u8..=255).cycle().take(4096).collect();
        std::fs::write(&file, &payload).unwrap();

        // Point HOME at the temp dir so path_is_allowed admits the file.
        let _guard = crate::agent_runtime::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        std::env::set_var("HOME", &dir);
        std::env::set_var("USERPROFILE", &dir);
        std::env::set_var("CAPILOT_HOME", &dir);

        let port = start(None).unwrap();
        let url = url_for(&file.canonicalize().unwrap()).unwrap();
        assert!(url.starts_with(&format!("http://127.0.0.1:{port}/wallpaper/")));

        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let req = format!(
            "GET /wallpaper/{} HTTP/1.1\r\nHost: 127.0.0.1\r\nRange: bytes=10-19\r\nConnection: close\r\n\r\n",
            percent_encoding::utf8_percent_encode(
                &file.canonicalize().unwrap().to_string_lossy(),
                percent_encoding::NON_ALPHANUMERIC
            )
        );
        stream.write_all(req.as_bytes()).unwrap();
        let mut resp = Vec::new();
        stream.read_to_end(&mut resp).unwrap();
        let text = String::from_utf8_lossy(&resp);
        assert!(text.starts_with("HTTP/1.1 206"), "{text}");
        assert!(text.contains("Accept-Ranges: bytes"), "{text}");
        assert!(text.contains("Content-Range: bytes 10-19/4096"), "{text}");
        let split = resp.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let body = &resp[split + 4..];
        assert_eq!(body, &payload[10..20]);

        std::env::remove_var("HOME");
        std::env::remove_var("USERPROFILE");
        std::env::remove_var("CAPILOT_HOME");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
