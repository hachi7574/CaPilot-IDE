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
}
