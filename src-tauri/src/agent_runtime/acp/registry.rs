//! Merge ACP descriptors into `runtime_list_available`.

use super::descriptor::{self, AcpAgentDescriptor};
use super::acp_runtime_id;
use crate::agent_runtime::adapter::{
    ModelInfo, PermissionModeInfo, RuntimeInfo, ThinkingOptionInfo,
};

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

/// Static OpenCode ACP model catalog for Composer (session/new returns the live
/// list; this seeds the picker before the first session). Default is Go flash
/// (reliable under Console rate limits); Zen free remain selectable.
fn opencode_acp_models() -> Vec<ModelInfo> {
    // (id, display name, provider group, is_default)
    const ENTRIES: &[(&str, &str, &str, bool)] = &[
        (
            "opencode-go/deepseek-v4-flash",
            "DeepSeek V4 Flash (Go)",
            "OpenCode Go",
            true,
        ),
        (
            "opencode/deepseek-v4-flash-free",
            "DeepSeek V4 Flash Free",
            "OpenCode Zen",
            false,
        ),
        (
            "opencode/nemotron-3.5-lightning-free",
            "Nemotron 3.5 Lightning Free",
            "OpenCode Zen",
            false,
        ),
        (
            "opencode/hy3-free",
            "Hy3 Free",
            "OpenCode Zen",
            false,
        ),
        (
            "opencode/mimo-v2.5-free",
            "MiMo V2.5 Free",
            "OpenCode Zen",
            false,
        ),
        (
            "opencode/laguna-s-2.1-free",
            "Laguna S 2.1 Free",
            "OpenCode Zen",
            false,
        ),
        (
            "opencode/nemotron-3-ultra-free",
            "Nemotron 3 Ultra Free",
            "OpenCode Zen",
            false,
        ),
        (
            "opencode/big-pickle",
            "Big Pickle",
            "OpenCode Zen",
            false,
        ),
        (
            "opencode-go/deepseek-v4-pro",
            "DeepSeek V4 Pro",
            "OpenCode Go",
            false,
        ),
        (
            "opencode-go/glm-5.1",
            "GLM-5.1",
            "OpenCode Go",
            false,
        ),
        (
            "opencode-go/kimi-k2.7-code",
            "Kimi K2.7 Code",
            "OpenCode Go",
            false,
        ),
        (
            "opencode-go/minimax-m2.7",
            "MiniMax-M2.7",
            "OpenCode Go",
            false,
        ),
        (
            "opencode-go/qwen3.7-plus",
            "Qwen3.7 Plus",
            "OpenCode Go",
            false,
        ),
    ];
    ENTRIES
        .iter()
        .map(|(id, name, provider, is_default)| ModelInfo {
            id: (*id).into(),
            name: (*name).into(),
            provider: (*provider).into(),
            is_default: *is_default,
            efforts: None,
        })
        .collect()
}

fn opencode_acp_thinking() -> Vec<ThinkingOptionInfo> {
    vec![
        ThinkingOptionInfo {
            id: "low".into(),
            label: "Low".into(),
            description: "Lower effort (OpenCode config option effort)".into(),
        },
        ThinkingOptionInfo {
            id: "high".into(),
            label: "High".into(),
            description: "Higher effort".into(),
        },
        ThinkingOptionInfo {
            id: "max".into(),
            label: "Max".into(),
            description: "Maximum effort".into(),
        },
    ]
}

/// Convert one descriptor to RuntimeInfo (`id = acp:{desc.id}`).
pub fn descriptor_to_runtime_info(desc: &AcpAgentDescriptor) -> RuntimeInfo {
    let available = descriptor::command_available(&desc.command);
    let is_opencode = desc.id == "opencode" || desc.command == "opencode";
    RuntimeInfo {
        id: acp_runtime_id(&desc.id),
        name: desc.name.clone(),
        available,
        authenticated: available, // MVP: no separate auth probe
        version: None,
        models: if is_opencode {
            opencode_acp_models()
        } else {
            vec![]
        },
        permission_modes: acp_permission_modes(),
        thinking_options: if is_opencode {
            opencode_acp_thinking()
        } else {
            vec![]
        },
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
        assert!(
            oc.models
                .iter()
                .any(|m| m.id == "opencode-go/deepseek-v4-flash" && m.is_default),
            "go flash must be default (zen free rate-limits): {:?}",
            oc.models
        );
        assert!(!oc.thinking_options.is_empty());
    }

    #[test]
    fn pick_bootstrap_prefers_go_flash() {
        use crate::agent_runtime::acp::host::pick_bootstrap_model;
        let catalog = vec![
            "opencode/big-pickle".into(),
            "opencode-go/deepseek-v4-flash".into(),
            "opencode/deepseek-v4-flash-free".into(),
        ];
        assert_eq!(
            pick_bootstrap_model(&catalog, None).as_deref(),
            Some("opencode-go/deepseek-v4-flash")
        );
        // preferred missing from catalog → fall through to go flash
        assert_eq!(
            pick_bootstrap_model(&catalog, Some("opencode-go/deepseek-v4-pro")).as_deref(),
            Some("opencode-go/deepseek-v4-flash"),
        );
        // When preferred is in catalog, honor it.
        assert_eq!(
            pick_bootstrap_model(&catalog, Some("opencode/deepseek-v4-flash-free")).as_deref(),
            Some("opencode/deepseek-v4-flash-free")
        );
    }
}
