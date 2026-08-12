//! Rate-limit usage fetching — powers the status-bar "剩余用量" readout and the
//! Settings → 已安装 → ⚙ availability check.
//!
//! Strategy mirrors `docs/reference/rate-limit-usage-fetching.md`, scoped to the
//! two runtimes CaPilot surfaces quota for:
//! - **codex**: JSON-RPC over the `codex app-server` stdio channel
//!   (`account/rateLimits/read`). Auth is auto-discovered from `~/.codex/auth.json`,
//!   so no manual configuration is needed.
//! - **opencode**: HTTP fetch of the opencode.ai workspace usage page using the
//!   user-pasted `auth` cookie. The workspace id is filled manually (best-effort
//!   auto probe only).
//!
//! Settings live in the app KV `settings` table under `usage_enabled`
//! (`{"codex": true, ...}`) and `usage_config` (`{"opencode": {auth_cookie, workspace_id}}`).

use crate::persistence::{Persistence, SessionsDb};
use reqwest::header::COOKIE;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// Settings KV key: `{"<runtime>": bool}` — whether the status bar shows usage.
pub const USAGE_ENABLED_KEY: &str = "usage_enabled";
/// Settings KV key: `{"<runtime>": UsageConfig}` — per-runtime fetch config.
pub const USAGE_CONFIG_KEY: &str = "usage_config";

const RPC_TIMEOUT: Duration = Duration::from_secs(8);
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ── Serialized shapes (sent to the frontend) ────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageWindow {
    /// "5h" | "7d" | "30d" (derived from the provider window length).
    pub label: String,
    pub window_minutes: i64,
    /// Percent of the window already used (0..100), when the provider reports it.
    pub used_pct: Option<f64>,
    /// 100 - used_pct — the "剩余用量" the status bar shows.
    pub remaining_pct: Option<f64>,
    /// Epoch seconds the window resets, when known.
    pub resets_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeUsage {
    pub runtime: String,
    pub available: bool,
    /// Human-readable reason when unavailable (shown in the settings check).
    pub error: Option<String>,
    /// e.g. codex "plus".
    pub plan_type: Option<String>,
    pub windows: Vec<UsageWindow>,
    pub checked_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageCheck {
    pub available: bool,
    pub message: String,
}

impl RuntimeUsage {
    fn error(runtime: &str, message: impl Into<String>) -> Self {
        Self {
            runtime: runtime.to_string(),
            available: false,
            error: Some(message.into()),
            plan_type: None,
            windows: vec![],
            checked_at: now_ms(),
        }
    }
}

// ── Settings loaders ────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UsageConfig {
    /// opencode.ai `auth` cookie value (bare token, or a full `k=v` pair).
    pub auth_cookie: String,
    /// Workspace id from `https://opencode.ai/workspace/<id>/go`. Empty → probe.
    pub workspace_id: String,
}

pub fn load_usage_enabled(db: &SessionsDb) -> HashMap<String, bool> {
    match db.get_setting(USAGE_ENABLED_KEY) {
        Ok(Some(raw)) => serde_json::from_str(&raw).unwrap_or_default(),
        _ => HashMap::new(),
    }
}

pub fn load_usage_config(db: &SessionsDb) -> HashMap<String, UsageConfig> {
    match db.get_setting(USAGE_CONFIG_KEY) {
        Ok(Some(raw)) => serde_json::from_str(&raw).unwrap_or_default(),
        _ => HashMap::new(),
    }
}

// ── Command bodies ──────────────────────────────────────────────

/// Status-bar path: only fetches when the runtime is enabled in settings.
pub async fn fetch(runtime: &str, persistence: &Persistence) -> Result<RuntimeUsage, String> {
    let (enabled, config) = {
        let db = persistence.db().lock().map_err(|e| e.to_string())?;
        let enabled = load_usage_enabled(&db);
        let config = load_usage_config(&db).get(runtime).cloned().unwrap_or_default();
        (enabled, config)
    };
    if !enabled.get(runtime).copied().unwrap_or(false) {
        return Ok(RuntimeUsage::error(
            runtime,
            "未启用（设置 → 已安装 → ⚙ → 用量统计）",
        ));
    }
    Ok(fetch_with_config(runtime, config).await)
}

/// Settings-check path: fetches regardless of the enable toggle so the user can
/// verify config viability before enabling.
pub async fn check(runtime: &str, persistence: &Persistence) -> Result<UsageCheck, String> {
    if runtime != "codex" && runtime != "opencode" {
        return Err(format!("unsupported runtime: {runtime}"));
    }
    let config = {
        let db = persistence.db().lock().map_err(|e| e.to_string())?;
        load_usage_config(&db).get(runtime).cloned().unwrap_or_default()
    };
    let usage = fetch_with_config(runtime, config).await;
    Ok(usage_to_check(usage))
}

fn usage_to_check(usage: RuntimeUsage) -> UsageCheck {
    if usage.available {
        let mut message = "可用".to_string();
        // Headline the 7d/weekly window when present, matching the status bar.
        if let Some(w) = usage
            .windows
            .iter()
            .find(|w| w.label == "7d")
            .or(usage.windows.first())
        {
            if let Some(rp) = w.remaining_pct {
                message = format!("可用 · {} 剩余 {:.0}%", w.label, rp);
            } else {
                message = format!("可用 · {} 窗口", w.label);
            }
        }
        if let Some(plan) = &usage.plan_type {
            message.push_str(&format!("（{plan}）"));
        }
        UsageCheck { available: true, message }
    } else {
        UsageCheck {
            available: false,
            message: usage.error.unwrap_or_else(|| "不可用".into()),
        }
    }
}

async fn fetch_with_config(runtime: &str, config: UsageConfig) -> RuntimeUsage {
    match runtime {
        "codex" => {
            let rpc: Result<Value, String> =
                match tauri::async_runtime::spawn_blocking(|| codex_rpc("account/rateLimits/read"))
                    .await
                {
                    Ok(r) => r,
                    Err(e) => return RuntimeUsage::error("codex", format!("codex 用量任务失败: {e}")),
                };
            match rpc {
                Ok(v) => parse_codex_usage(v),
                Err(e) => RuntimeUsage::error("codex", e),
            }
        }
        "opencode" => match fetch_opencode(&config).await {
            Ok(windows) => RuntimeUsage {
                runtime: "opencode".into(),
                available: true,
                error: None,
                plan_type: None,
                windows,
                checked_at: now_ms(),
            },
            Err(e) => RuntimeUsage::error("opencode", e),
        },
        other => RuntimeUsage::error(other, format!("unsupported runtime: {other}")),
    }
}

// ── codex: app-server JSON-RPC ──────────────────────────────────

/// Run one JSON-RPC call against the codex app-server over stdio. Mirrors
/// `runtimes/codex.rs::discover_models`: write `initialize` then `method`,
/// read lines until the `"id":2` response, then kill the child.
fn codex_rpc(method: &str) -> Result<Value, String> {
    let mut child = Command::new("codex")
        .args(["app-server", "--listen", "stdio://"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("无法启动 codex app-server: {e}"))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "codex stdin 不可用".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "codex stdout 不可用".to_string())?;

    let initialize = serde_json::json!({
        "id": 1,
        "method": "initialize",
        "params": { "clientInfo": { "name": "capilot-ide", "version": env!("CARGO_PKG_VERSION") } }
    });
    let request = serde_json::json!({ "id": 2, "method": method });
    if writeln!(stdin, "{initialize}").is_err()
        || writeln!(stdin, "{request}").is_err()
        || stdin.flush().is_err()
    {
        let _ = child.kill();
        return Err("写入 codex app-server 失败".to_string());
    }
    // Keep `stdin` alive (do not drop) until the child is killed — closing the
    // pipe early makes some app-server versions exit before responding.

    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
        {
            if sender.send(line).is_err() {
                break;
            }
        }
    });

    let deadline = std::time::Instant::now() + RPC_TIMEOUT;
    let mut response = None;
    while let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) {
        let Ok(line) = receiver.recv_timeout(remaining) else {
            break;
        };
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if value.get("id").and_then(Value::as_i64) == Some(2) {
            response = Some(value);
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    drop(stdin);

    response.ok_or_else(|| "codex 未返回用量数据（未登录或 app-server 不可用）".to_string())
}

fn parse_codex_usage(value: Value) -> RuntimeUsage {
    let mut usage = RuntimeUsage::error("codex", "codex 用量数据为空");
    let Some(limits) = value
        .get("result")
        .and_then(|r| r.get("rateLimits"))
    else {
        usage.error = Some("codex 未返回 rateLimits".into());
        return usage;
    };
    usage.plan_type = limits
        .get("planType")
        .and_then(Value::as_str)
        .map(str::to_string);

    for slot in ["primary", "secondary"] {
        if let Some(w) = limits.get(slot).and_then(Value::as_object).and_then(window_from_codex) {
            usage.windows.push(w);
        }
    }
    usage.available = !usage.windows.is_empty();
    if usage.available {
        usage.error = None;
    }
    usage
}

fn window_from_codex(obj: &serde_json::Map<String, Value>) -> Option<UsageWindow> {
    let minutes = obj.get("windowDurationMins").and_then(Value::as_i64)?;
    let used_pct = obj.get("usedPercent").and_then(Value::as_f64);
    let remaining_pct = used_pct.map(|u| (100.0 - u).clamp(0.0, 100.0));
    Some(UsageWindow {
        label: window_label(minutes),
        window_minutes: minutes,
        used_pct,
        remaining_pct,
        resets_at: obj.get("resetsAt").and_then(Value::as_i64),
    })
}

fn window_label(minutes: i64) -> String {
    match minutes {
        300 => "5h".into(),
        10080 => "7d".into(),
        43200 => "30d".into(),
        other if other % 1440 == 0 => format!("{}d", other / 1440),
        other if other % 60 == 0 => format!("{}h", other / 60),
        other => format!("{other}min"),
    }
}

// ── opencode: HTTP workspace-page scrape ────────────────────────

async fn fetch_opencode(config: &UsageConfig) -> Result<Vec<UsageWindow>, String> {
    let cookie = config.auth_cookie.trim();
    if cookie.is_empty() {
        return Err(
            "未配置 opencode.ai 登录 Cookie（浏览器登录 opencode.ai 后复制 auth 值粘贴到设置中）"
                .into(),
        );
    }
    let cookie_header = if cookie.contains('=') {
        cookie.to_string()
    } else {
        format!("auth={cookie}")
    };

    // reqwest is built with `rustls-no-provider` (no bundled crypto backend), so
    // a process-global rustls provider must be installed before any Client can be
    // built. tauri-plugin-updater installs one lazily when it first runs; install
    // the ring provider here too so usage fetching works even before that.
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    let client = reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .user_agent(format!("capilot-ide/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("HTTP 客户端创建失败: {e}"))?;

    let workspace = if !config.workspace_id.trim().is_empty() {
        config.workspace_id.trim().to_string()
    } else {
        discover_workspace_id(&client, &cookie_header).await?
    };

    let url = format!("https://opencode.ai/workspace/{workspace}/go");
    let resp = client
        .get(&url)
        .header(COOKIE, &cookie_header)
        .send()
        .await
        .map_err(|e| format!("请求 opencode.ai 用量页失败: {e}"))?;
    let final_url = resp.url().clone();
    let body = resp
        .text()
        .await
        .map_err(|e| format!("读取用量页响应失败: {e}"))?;

    // An invalid/expired cookie redirects to the auth page — the body then
    // carries no usage markers. Surface that instead of a cryptic parse error.
    // (opencode.ai sends a 302 to `/auth/authorize` → auth.opencode.ai, which
    // reqwest follows to a URL containing `/authorize`.)
    if final_url.as_str().contains("/login")
        || final_url.as_str().contains("/signin")
        || final_url.as_str().contains("/authorize")
    {
        return Err("opencode.ai 未通过 Cookie 验证（已跳转登录页），Cookie 可能已过期".into());
    }

    let mut windows = Vec::new();
    push_window(&mut windows, "rollingUsage", &body, 300);
    push_window(&mut windows, "weeklyUsage", &body, 10080);
    push_window(&mut windows, "monthlyUsage", &body, 43200);
    if windows.is_empty() {
        return Err("无法从用量页解析出用量数据（Cookie 可能已过期，或 workspace id 不正确）".into());
    }
    Ok(windows)
}

/// Best-effort SST server-fn probe for the workspace id. The fn id can drift
/// with opencode.ai; failure falls back to manual entry.
async fn discover_workspace_id(
    client: &reqwest::Client,
    cookie_header: &str,
) -> Result<String, String> {
    let url = "https://opencode.ai/_server?id=def3997db14b88bdcb8d29e6a14f32d9";
    let body = match client.get(url).header(COOKIE, cookie_header).send().await {
        Ok(resp) => resp.text().await.unwrap_or_default(),
        Err(_) => String::new(),
    };
    for prefix in ["wrk_", "wk_"] {
        let mut start = 0;
        while let Some(rel) = body[start..].find(prefix) {
            let idx = start + rel;
            let rest = &body[idx + prefix.len()..];
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
                .unwrap_or(rest.len());
            if end >= 8 {
                return Ok(format!("{prefix}{}", &rest[..end]));
            }
            start = idx + 1;
        }
    }
    Err(
        "未能自动发现 workspace id，请在设置中填写（https://opencode.ai/workspace/<id>/go 中的 <id>）"
            .into(),
    )
}

fn push_window(windows: &mut Vec<UsageWindow>, key: &str, body: &str, minutes: i64) {
    if let Some((used_pct, remaining_pct, resets_at)) = extract_usage_pct(body, key) {
        windows.push(UsageWindow {
            label: window_label(minutes),
            window_minutes: minutes,
            used_pct,
            remaining_pct,
            resets_at,
        });
    }
}

/// Extract usage for `key` from the serialized page. Returns
/// `(used_pct, remaining_pct, resets_at)`. Handles the shapes seen in the wild:
/// - SolidStart hydration (`rollingUsage:$R[36]={status:"ok",resetInSec:NNN,usagePercent:NN}`)
///   — the actual opencode.ai page format,
/// - React Flight escaped JSON (`\"rollingUsage\":{...}`) inside a string,
/// - raw JSON (`"rollingUsage":{...}`),
/// - a bare number (treated as used %).
fn extract_usage_pct(body: &str, key: &str) -> Option<(Option<f64>, Option<f64>, Option<i64>)> {
    // SolidStart serializes the subscription payload as a JS object assignment
    // `key:$R[<n>]={...}` (unquoted keys, no JSON). Try that first.
    let solid = format!("{key}:$R[");
    if let Some(idx) = body.find(&solid) {
        let rest = &body[idx + solid.len()..];
        if let Some(eq) = rest.find("]=") {
            if let Some(r) = solid_usage_pct(&rest[eq + 2..]) {
                return Some(r);
            }
        }
    }
    // JSON needles — escaped React Flight string first, then raw.
    for needle in [format!("\\\"{key}\\\":"), format!("\"{key}\":")] {
        if let Some(idx) = body.find(&needle) {
            let rest = body[idx + needle.len()..].trim_start().to_string();
            return parse_after_colon(&rest);
        }
    }
    None
}

/// Parse the value that follows a `key:` colon (JSON object or bare number).
fn parse_after_colon(rest: &str) -> Option<(Option<f64>, Option<f64>, Option<i64>)> {
    let rest = rest.trim_start();
    if rest.is_empty() {
        return None;
    }
    let first = rest.as_bytes()[0];
    if first.is_ascii_digit() || first == b'-' {
        let end = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
            .unwrap_or(rest.len());
        let n: f64 = rest[..end].parse().ok()?;
        return Some((Some(n.clamp(0.0, 100.0)), Some((100.0 - n).clamp(0.0, 100.0)), None));
    }
    // Object form. Inside a React Flight string the keys/quotes are escaped
    // (`{\"remaining\":...}`); outside they are raw. A `\"` sequence anywhere
    // before the value's closing brace marks the escaped form — de-escape first
    // so brace-balancing sees real quotes.
    let escaped = rest.contains("\\\"");
    let window = if escaped {
        deescape_json_string(rest)
    } else {
        rest.to_string()
    };
    let window = window.trim_start().to_string();
    if !window.starts_with('{') {
        return None;
    }
    let end = balanced_braces(&window)?;
    let obj: Value = serde_json::from_str(&window[..=end]).ok()?;
    object_usage_pct(&obj)
}

/// Parse a SolidStart object literal `{status:"ok",resetInSec:NNN,usagePercent:NN}`
/// into a usage window. `usagePercent` is the *used* percent; `resetInSec` is
/// seconds until the window resets.
fn solid_usage_pct(s: &str) -> Option<(Option<f64>, Option<f64>, Option<i64>)> {
    let end = balanced_braces(s)?;
    let fields = solid_fields(&s[..=end]);
    let field = |name: &str| fields.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
    let used: f64 = field("usagePercent")?.parse().ok()?;
    let reset_in_sec: Option<i64> = field("resetInSec").and_then(|v| v.parse().ok());
    let remaining = 100.0 - used;
    Some((
        Some(used.clamp(0.0, 100.0)),
        Some(remaining.clamp(0.0, 100.0)),
        reset_in_sec.map(|s| now_epoch() + s),
    ))
}

/// Parse a flat SolidStart `{name:value,name:value}` object body into `(name,
/// value)` pairs. String values keep their quotes stripped.
fn solid_fields(s: &str) -> Vec<(String, String)> {
    let s = s.trim();
    let Some(inner) = s.strip_prefix('{').and_then(|t| t.strip_suffix('}')) else {
        return Vec::new();
    };
    let mut fields = Vec::new();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    let mut start = 0usize;
    for (i, b) in inner.bytes().enumerate() {
        if in_str {
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' | b'[' => depth += 1,
            b'}' | b']' => depth -= 1,
            b',' if depth == 0 => {
                push_solid_field(&mut fields, &inner[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    push_solid_field(&mut fields, &inner[start..]);
    fields
}

fn push_solid_field(fields: &mut Vec<(String, String)>, part: &str) {
    let part = part.trim();
    if let Some(colon) = part.find(':') {
        let name = part[..colon].trim().trim_matches('"').to_string();
        let value = part[colon + 1..].trim().trim_matches('"').to_string();
        fields.push((name, value));
    }
}

fn object_usage_pct(v: &Value) -> Option<(Option<f64>, Option<f64>, Option<i64>)> {
    let obj = v.as_object()?;
    let num = |keys: &[&str]| -> Option<f64> {
        keys.iter().find_map(|k| obj.get(*k).and_then(Value::as_f64))
    };
    let remaining = num(&["remaining", "remainingPct", "remaining_pct"]);
    let limit = num(&["limit", "total", "quota"]);
    let used = num(&["used", "usedPct", "used_pct", "percent", "percentage", "usagePercent"]);
    let resets_at = num(&["resetsAt", "resets_at"])
        .map(|s| s as i64)
        .or_else(|| num(&["resetInSec", "reset_in_sec"]).map(|s| now_epoch() + s as i64));
    if let (Some(rem), Some(lim)) = (remaining, limit) {
        if lim > 0.0 {
            let remaining_pct = (rem / lim * 100.0).clamp(0.0, 100.0);
            return Some((Some((100.0 - remaining_pct).clamp(0.0, 100.0)), Some(remaining_pct), resets_at));
        }
    }
    if let Some(used) = used {
        if used <= 100.0 {
            return Some((Some(used), Some((100.0 - used).clamp(0.0, 100.0)), resets_at));
        }
    }
    None
}

/// Minimal JSON string de-escape (`\"` → `"`, `\\` → `\`, `\n`, `\uXXXX`).
fn deescape_json_string(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('/') => out.push('/'),
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                if let Ok(code) = u32::from_str_radix(&hex, 16) {
                    if let Some(ch) = char::from_u32(code) {
                        out.push(ch);
                    }
                }
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => break,
        }
    }
    out
}

/// Index of the closing `}` of the first balanced `{...}` in `s` (string-aware).
fn balanced_braces(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    if bytes.first() != Some(&b'{') {
        return None;
    }
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    for (i, &b) in bytes.iter().enumerate() {
        if in_str {
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_labels() {
        assert_eq!(window_label(300), "5h");
        assert_eq!(window_label(10080), "7d");
        assert_eq!(window_label(43200), "30d");
        assert_eq!(window_label(60), "1h");
        assert_eq!(window_label(90), "90min");
    }

    #[test]
    fn parses_real_codex_rate_limits_response() {
        let json = r#"{"id":2,"result":{"rateLimits":{"limitId":"codex","limitName":null,
            "primary":{"usedPercent":29,"windowDurationMins":10080,"resetsAt":1787034723},
            "secondary":null,"credits":{"hasCredits":false,"unlimited":false,"balance":"0"},
            "individualLimit":null,"spendControlReached":false,"planType":"plus","rateLimitReachedType":null},
            "rateLimitsByLimitId":{},"rateLimitResetCredits":{"availableCount":0,"credits":[]}}}"#;
        let usage = parse_codex_usage(serde_json::from_str(json).unwrap());
        assert!(usage.available);
        assert_eq!(usage.plan_type.as_deref(), Some("plus"));
        assert_eq!(usage.windows.len(), 1);
        let w = &usage.windows[0];
        assert_eq!(w.label, "7d");
        assert_eq!(w.window_minutes, 10080);
        assert_eq!(w.used_pct.unwrap(), 29.0);
        assert_eq!(w.remaining_pct.unwrap(), 71.0);
        assert_eq!(w.resets_at, Some(1787034723));
    }

    #[test]
    fn codex_error_when_rate_limits_missing() {
        let json = r#"{"id":2,"result":{}}"#;
        let usage = parse_codex_usage(serde_json::from_str(json).unwrap());
        assert!(!usage.available);
        assert!(usage.error.is_some());
    }

    #[test]
    fn parses_opencode_object_in_escaped_flight_string() {
        // React Flight pushes JSON as an escaped string inside the script body.
        let body = r#"self.__next_f.push([1,"{\"id\":42,\"rollingUsage\":{\"remaining\":7000,\"limit\":10000}}"])"#;
        let (used, remaining, _) = extract_usage_pct(body, "rollingUsage").unwrap();
        assert!((used.unwrap() - 30.0).abs() < 0.01);
        assert!((remaining.unwrap() - 70.0).abs() < 0.01);
    }

    #[test]
    fn parses_opencode_bare_number() {
        let body = r#"{"weeklyUsage":25}"#;
        let (used, remaining, _) = extract_usage_pct(body, "weeklyUsage").unwrap();
        assert_eq!(used.unwrap(), 25.0);
        assert_eq!(remaining.unwrap(), 75.0);
    }

    #[test]
    fn parses_opencode_solidstart_hydration() {
        // Real opencode.ai page (SolidStart): unquoted keys, `$R[n]` refs, and
        // `key:$R[<n>]={...}` assignments for the usage payload.
        let body = r#"$R[28]($R[18],$R[34]={mine:!0,useBalance:!1,region:$R[35]=["us","eu","sg","cn"],rollingUsage:$R[36]={status:"ok",resetInSec:14097,usagePercent:0},weeklyUsage:$R[37]={status:"ok",resetInSec:389276,usagePercent:9},monthlyUsage:$R[38]={status:"ok",resetInSec:1864698,usagePercent:52}});"#;
        let (used, remaining, resets_at) = extract_usage_pct(body, "weeklyUsage").unwrap();
        assert!((used.unwrap() - 9.0).abs() < 0.01);
        assert!((remaining.unwrap() - 91.0).abs() < 0.01);
        assert!(resets_at.is_some());
        // rollingUsage is 0 % — must parse, not be treated as missing/falsy.
        let (used0, remaining0, resets0) = extract_usage_pct(body, "rollingUsage").unwrap();
        assert_eq!(used0.unwrap(), 0.0);
        assert_eq!(remaining0.unwrap(), 100.0);
        assert!(resets0.is_some());
        let (used_m, remaining_m, _) = extract_usage_pct(body, "monthlyUsage").unwrap();
        assert!((used_m.unwrap() - 52.0).abs() < 0.01);
        assert!((remaining_m.unwrap() - 48.0).abs() < 0.01);
    }

    #[tokio::test]
    #[ignore = "manual: set CAPILOT_OC_TEST_COOKIE (Cookie header value) + CAPILOT_OC_TEST_WORKSPACE"]
    async fn fetch_opencode_end_to_end() {
        let (Ok(cookie), Ok(workspace)) = (
            std::env::var("CAPILOT_OC_TEST_COOKIE"),
            std::env::var("CAPILOT_OC_TEST_WORKSPACE"),
        ) else {
            eprintln!("skipping: set CAPILOT_OC_TEST_COOKIE + CAPILOT_OC_TEST_WORKSPACE");
            return;
        };
        let config = UsageConfig {
            auth_cookie: cookie,
            workspace_id: workspace,
        };
        let windows = fetch_opencode(&config)
            .await
            .expect("fetch_opencode should succeed with a valid cookie");
        assert!(!windows.is_empty(), "expected at least one usage window");
        for w in &windows {
            println!(
                "{} used={:?} remaining={:?} resets_at={:?}",
                w.label, w.used_pct, w.remaining_pct, w.resets_at
            );
        }
    }

    #[test]
    fn balanced_braces_ignores_strings() {
        assert_eq!(balanced_braces(r#"{"a":{"b":"}"}}"#), Some(14));
        assert_eq!(balanced_braces("{}"), Some(1));
        assert_eq!(balanced_braces("{"), None);
    }
}
