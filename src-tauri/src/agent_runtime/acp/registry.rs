//! Merge ACP descriptors into `runtime_list_available`.

use super::descriptor::{self, AcpAgentDescriptor};
use super::acp_runtime_id;
use crate::agent_runtime::adapter::{PermissionModeInfo, RuntimeInfo};

/// CaPilot-generic permission modes for ACP (MVP: ask only is meaningful;
/// auto/yolo reserved for Phase 3 policy).
fn acp_permission_modes() -> Vec<PermissionModeInfo> {
    vec![
        PermissionModeInfo {
            id: "ask".into(),
            label: "Ask".into(),
            description: "Confirm every tool permission in the UI".into(),
            requires_confirmation: true,
        },
        PermissionModeInfo {
            id: "auto".into(),
            label: "Auto".into(),
            description: "Allow safe tools; ask on destructive (Phase 3)".into(),
            requires_confirmation: false,
        },
        PermissionModeInfo {
            id: "yolo".into(),
            label: "Yolo".into(),
            description: "Allow all permissions (dangerous)".into(),
            requires_confirmation: false,
        },
    ]
}

/// Convert one descriptor to RuntimeInfo (`id = acp:{desc.id}`).
pub fn descriptor_to_runtime_info(desc: &AcpAgentDescriptor) -> RuntimeInfo {
    let available = descriptor::command_available(&desc.command);
    RuntimeInfo {
        id: acp_runtime_id(&desc.id),
        name: desc.name.clone(),
        available,
        authenticated: available, // MVP: no separate auth probe
        version: None,
        models: vec![],
        permission_modes: acp_permission_modes(),
        thinking_options: vec![],
        transport: "acp".to_string(),
    }
}

/// All enabled ACP runtimes for the picker.
pub fn list_runtime_infos() -> Vec<RuntimeInfo> {
    descriptor::load_all_descriptors()
        .iter()
        .map(descriptor_to_runtime_info)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_list_has_acp_opencode() {
        let list = list_runtime_infos();
        let oc = list.iter().find(|r| r.id == "acp:opencode");
        assert!(oc.is_some(), "expected acp:opencode in {list:?}");
        let oc = oc.unwrap();
        assert_eq!(oc.transport, "acp");
        assert_eq!(oc.name, "OpenCode (ACP)");
        assert!(!oc.permission_modes.is_empty());
    }
}
