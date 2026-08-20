# v1 file checklist

Touch only:

- `src-tauri/src/agent_runtime/runtimes/generic.rs` — `V1_RUNTIMES` row (`id` / `name` / `binary` / extra argv)
- `ui/state/store.ts` — `DEFAULT_TEMPLATES` row
- optional `ui/components/layout/SettingsModal.tsx` — `DEFAULT_LAUNCH` preview
- optional `ui/assets/icons/agent-favicons/<id>.png` or `brands/<id>.svg`

`known_runtimes()` and `get_adapter()` consume `V1_RUNTIMES`; do not hand-edit the id list in `mod.rs`.

Do not touch for v1:

- `ui/components/layout/Composer.tsx` (except the existing `PLAIN_COMPOSER` flag)
- `ui/components/terminal/mouseProtocol.ts`
- `ui/state/usage.ts` / `usageContext.ts`
- `src-tauri/src/agent_runtime/status_hooks.rs`
- `src-tauri/src/slash.rs`
- Settings enable-switch / collapsible panel (see SKILL.md “Settings UX deferred”)
