# Local agent CLI inventory (user-prefix)

All installs for this machine go under **user dirs** so they uninstall without sudo.

| Prefix | PATH | How to uninstall |
| --- | --- | --- |
| `$HOME/APP/n` | `$HOME/APP/n/bin` | `npm uninstall -g --prefix "$HOME/APP/n" <pkg>` |
| `$HOME/.local` | `$HOME/.local/bin` | delete the binary / `uv tool uninstall <name>` |
| `$HOME/.kimi-code` | symlink `~/.local/bin/kimi` | `rm -rf ~/.kimi-code ~/.local/bin/kimi` |
| `$HOME/.qoder-cn` | `~/.local/bin/qoderclicn` (+ `qoder` alias) | `rm -rf ~/.qoder-cn ~/.local/bin/qoderclicn ~/.local/bin/qoder` |
| `$HOME/.local/share/trae-cli` | `~/.local/bin/traecli` | `rm -rf ~/.local/share/trae-cli ~/.local/bin/traecli ~/.local/bin/trae-cli` |
| `$HOME/.local/share/hermes-agent` | installer links `hermes` | installer uninstall / `rm -rf ~/.local/share/hermes-agent` |
| `$HOME/.cargo` | `$HOME/.cargo/bin` | `cargo uninstall <crate>` |

CaPilot `ensure_cli_path()` already prepends `~/APP/n/bin`, `~/.local/bin`, `~/.cargo/bin`.

## Installed this session (2026-08-20)

npm `--prefix $HOME/APP/n`:

| runtime | binary | package |
| --- | --- | --- |
| gemini | `gemini` | `@google/gemini-cli` |
| copilot | `copilot` | `@github/copilot` |
| continue | `cn` | `@continuedev/cli` |
| qwen-code | `qwen` | `@qwen-code/qwen-code` |
| kilo | `kilo` | `@kilocode/cli` |
| aug | `auggie` | `@augmentcode/auggie` |
| crush | `crush` | `@charmland/crush` |
| codebuff | `codebuff` | `codebuff` |
| command-code | `command-code` | `command-code` |

Official / user-bin:

| runtime | binary | where |
| --- | --- | --- |
| kimi | `kimi` | `~/.kimi-code/bin` → `~/.local/bin/kimi` |
| trae | `traecli` | `~/.local/share/trae-cli` |
| kiro | `kiro-cli` | `~/.local/bin/kiro-cli` |
| qoder | `qoderclicn` | `~/.qoder-cn` (+ `qoder` alias) |
| aider | `aider` | `uv tool install aider-chat` |
| hermes | `hermes` | `~/.local/share/hermes-agent` (installer) |

Already present: `claude`, `codex`, `opencode`, `dsh`, `codebuddy`, `cline`, `omp`.

**Not installed** (v1 still registers them; Settings shows 未检测 until PATH has the binary): grok, cursor, goose, amp, droid, openclaude, autohand, mimo-code, rovo, openclaw, devin, ante, prime-agent, antigravity.

**Do not install from npm** (wrong packages): `grok-cli`, unofficial `hermes-agent`, `mimo`, reserved `openclaude`, `@ampcode/cli` (Linux bin is `amp.exe`).

## Uninstall this session's npm set

```bash
npm uninstall -g --prefix "$HOME/APP/n" \
  @google/gemini-cli @github/copilot @continuedev/cli \
  @qwen-code/qwen-code @kilocode/cli @augmentcode/auggie \
  @charmland/crush codebuff command-code
uv tool uninstall aider-chat
```
