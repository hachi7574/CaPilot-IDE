//! ACP agent descriptors — config-driven launch specs.
//!
//! Load order:
//! 1. `~/CaPilot/acp-agents.json` (user)
//! 2. Built-in defaults (always include `opencode` → `acp:opencode`)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// One ACP agent entry. Runtime id presented to the UI is `acp:{id}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AcpAgentDescriptor {
    pub id: String,
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// `session` = process cwd = AgentSession.cwd (default).
    #[serde(default = "default_cwd_mode")]
    pub cwd_mode: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_cwd_mode() -> String {
    "session".to_string()
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AcpAgentsFile {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub agents: Vec<AcpAgentDescriptor>,
}

fn default_version() -> u32 {
    1
}

/// Built-in default list. OpenCode is the validation anchor.
pub fn default_descriptors() -> Vec<AcpAgentDescriptor> {
    vec![AcpAgentDescriptor {
        id: "opencode".to_string(),
        name: "OpenCode (ACP)".to_string(),
        command: "opencode".to_string(),
        args: vec!["acp".to_string()],
        env: HashMap::new(),
        cwd_mode: "session".to_string(),
        icon: None,
        enabled: true,
    }]
}

/// Path to the user descriptor file: `~/CaPilot/acp-agents.json`.
pub fn user_descriptors_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join("CaPilot").join("acp-agents.json")
}

/// Load user file if present; on missing/invalid, return empty agents.
pub fn load_user_file(path: &Path) -> AcpAgentsFile {
    let Ok(raw) = fs::read_to_string(path) else {
        return AcpAgentsFile {
            version: 1,
            agents: vec![],
        };
    };
    match serde_json::from_str::<AcpAgentsFile>(&raw) {
        Ok(f) => f,
        Err(e) => {
            log::warn!("failed to parse {}: {e}", path.display());
            AcpAgentsFile {
                version: 1,
                agents: vec![],
            }
        }
    }
}

/// Merge defaults + user: user entries override defaults by `id`.
/// Disabled user entries hide the default of the same id.
pub fn load_all_descriptors() -> Vec<AcpAgentDescriptor> {
    load_all_from(user_descriptors_path().as_path())
}

pub fn load_all_from(user_path: &Path) -> Vec<AcpAgentDescriptor> {
    let mut by_id: HashMap<String, AcpAgentDescriptor> = HashMap::new();
    for d in default_descriptors() {
        by_id.insert(d.id.clone(), d);
    }
    let user = load_user_file(user_path);
    for d in user.agents {
        by_id.insert(d.id.clone(), d);
    }
    let mut out: Vec<AcpAgentDescriptor> = by_id.into_values().filter(|d| d.enabled).collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// Look up one enabled descriptor by short id (without `acp:` prefix).
pub fn find_descriptor(short_id: &str) -> Option<AcpAgentDescriptor> {
    load_all_descriptors()
        .into_iter()
        .find(|d| d.id == short_id)
}

/// Best-effort: is `command` resolvable on PATH (or absolute and executable)?
pub fn command_available(command: &str) -> bool {
    if command.contains('/') {
        let p = Path::new(command);
        return p.is_file();
    }
    // `command -v` via `sh` is more portable than which(1).
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {}", shell_escape(command)))
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn shell_escape(s: &str) -> String {
    // Minimal single-quote escape for PATH lookup only.
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Write the full agents file (Settings CRUD — Phase 4; exposed early for tests).
pub fn write_user_file(path: &Path, file: &AcpAgentsFile) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let raw = serde_json::to_string_pretty(file).map_err(|e| e.to_string())?;
    fs::write(path, raw).map_err(|e| e.to_string())
}

/// Validate a descriptor before writing (id / command non-empty; id is a short
/// slug without `acp:` prefix).
pub fn validate_descriptor(d: &AcpAgentDescriptor) -> Result<(), String> {
    let id = d.id.trim();
    if id.is_empty() {
        return Err("descriptor id is required".into());
    }
    if id.contains(':') || id.contains('/') || id.contains('\\') || id.contains(' ') {
        return Err(
            "descriptor id must be a short slug (no 'acp:' prefix, no path separators)".into(),
        );
    }
    if d.command.trim().is_empty() {
        return Err("descriptor command is required".into());
    }
    if d.name.trim().is_empty() {
        return Err("descriptor name is required".into());
    }
    Ok(())
}

/// Upsert one entry into the user file (does not rewrite built-in defaults).
/// When the id matches a default, writing a user override shadows it.
pub fn upsert_user_descriptor(desc: AcpAgentDescriptor) -> Result<AcpAgentDescriptor, String> {
    validate_descriptor(&desc)?;
    let path = user_descriptors_path();
    let mut file = load_user_file(&path);
    let mut desc = desc;
    desc.id = desc.id.trim().to_string();
    desc.name = desc.name.trim().to_string();
    desc.command = desc.command.trim().to_string();
    if let Some(slot) = file.agents.iter_mut().find(|a| a.id == desc.id) {
        *slot = desc.clone();
    } else {
        file.agents.push(desc.clone());
    }
    file.version = 1;
    write_user_file(&path, &file)?;
    Ok(desc)
}

/// Remove a user override by id. If only the built-in default existed, write an
/// `enabled: false` shadow so the default disappears from the merged list.
/// Returns true when the file was modified.
pub fn remove_user_descriptor(short_id: &str) -> Result<bool, String> {
    let id = short_id.trim();
    if id.is_empty() {
        return Err("id is required".into());
    }
    let path = user_descriptors_path();
    let mut file = load_user_file(&path);
    let before = file.agents.len();
    file.agents.retain(|a| a.id != id);
    let removed = file.agents.len() != before;

    let is_default = default_descriptors().iter().any(|d| d.id == id);
    if is_default {
        // Shadow the built-in so it no longer appears until the user re-adds it.
        file.agents.push(AcpAgentDescriptor {
            id: id.to_string(),
            name: id.to_string(),
            command: String::new(),
            args: vec![],
            env: HashMap::new(),
            cwd_mode: "session".to_string(),
            icon: None,
            enabled: false,
        });
        write_user_file(&path, &file)?;
        return Ok(true);
    }

    if removed {
        write_user_file(&path, &file)?;
    }
    Ok(removed)
}

/// List every descriptor as stored for Settings: merged view + disabled user
/// shadows so the UI can show "off" defaults. Each entry includes whether the
/// command is currently on PATH.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpAgentListItem {
    #[serde(flatten)]
    pub descriptor: AcpAgentDescriptor,
    pub available: bool,
    /// Full runtime id (`acp:{id}`).
    pub runtime_id: String,
    pub is_default: bool,
}

/// Settings list: defaults + user overrides (including disabled).
pub fn list_for_settings() -> Vec<AcpAgentListItem> {
    let mut by_id: HashMap<String, AcpAgentDescriptor> = HashMap::new();
    let defaults = default_descriptors();
    let default_ids: std::collections::HashSet<String> =
        defaults.iter().map(|d| d.id.clone()).collect();
    for d in defaults {
        by_id.insert(d.id.clone(), d);
    }
    let user = load_user_file(&user_descriptors_path());
    for d in user.agents {
        by_id.insert(d.id.clone(), d);
    }
    let mut out: Vec<AcpAgentListItem> = by_id
        .into_values()
        .map(|d| {
            let available = d.enabled && command_available(&d.command);
            let is_default = default_ids.contains(&d.id);
            let runtime_id = format!("acp:{}", d.id);
            AcpAgentListItem {
                descriptor: d,
                available,
                runtime_id,
                is_default,
            }
        })
        .collect();
    out.sort_by(|a, b| a.descriptor.id.cmp(&b.descriptor.id));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_runtime::ENV_LOCK;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn defaults_include_opencode_acp() {
        let d = default_descriptors();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].id, "opencode");
        assert_eq!(d[0].command, "opencode");
        assert_eq!(d[0].args, vec!["acp".to_string()]);
        assert!(d[0].env.is_empty(), "must not inject OPENCODE_TUI_CONFIG");
    }

    #[test]
    fn merge_user_override() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("capilot-acp-desc-{stamp}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("acp-agents.json");
        write_user_file(
            &path,
            &AcpAgentsFile {
                version: 1,
                agents: vec![AcpAgentDescriptor {
                    id: "opencode".to_string(),
                    name: "Custom OC".to_string(),
                    command: "opencode".to_string(),
                    args: vec!["acp".to_string(), "--cwd".to_string(), "/tmp".to_string()],
                    env: HashMap::new(),
                    cwd_mode: "session".to_string(),
                    icon: None,
                    enabled: true,
                }],
            },
        )
        .unwrap();
        let all = load_all_from(&path);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "Custom OC");
        assert_eq!(all[0].args.len(), 3);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn disabled_hides_default() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("capilot-acp-desc-off-{stamp}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("acp-agents.json");
        write_user_file(
            &path,
            &AcpAgentsFile {
                version: 1,
                agents: vec![AcpAgentDescriptor {
                    id: "opencode".to_string(),
                    name: "Off".to_string(),
                    command: "opencode".to_string(),
                    args: vec!["acp".to_string()],
                    env: HashMap::new(),
                    cwd_mode: "session".to_string(),
                    icon: None,
                    enabled: false,
                }],
            },
        )
        .unwrap();
        let all = load_all_from(&path);
        assert!(all.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_rejects_bad_id() {
        let mut d = default_descriptors().remove(0);
        d.id = "acp:opencode".into();
        assert!(validate_descriptor(&d).is_err());
        d.id = "ok".into();
        d.command = "".into();
        assert!(validate_descriptor(&d).is_err());
        d.command = "opencode".into();
        assert!(validate_descriptor(&d).is_ok());
    }
}
