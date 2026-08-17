use crate::agent_runtime::runtimes::get_adapter;
use crate::persistence::Persistence;
use serde::Serialize;
use std::collections::HashSet;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const MAX_METADATA_BYTES: u64 = 64 * 1024;
const MAX_ITEMS: usize = 500;
const BUILTIN_SOURCE: &str = "内置命令";

#[derive(Clone, Copy)]
struct BuiltinCommand {
    name: &'static str,
    description: &'static str,
}

const CODEX_COMMANDS: &[BuiltinCommand] = &[
    BuiltinCommand {
        name: "permissions",
        description: "设置 Codex 无需询问即可执行的操作权限",
    },
    BuiltinCommand {
        name: "ide",
        description: "附加 IDE 中打开的文件、选区和上下文",
    },
    BuiltinCommand {
        name: "keymap",
        description: "查看或重新映射终端界面的快捷键",
    },
    BuiltinCommand {
        name: "vim",
        description: "开启或关闭 Vim 输入模式",
    },
    BuiltinCommand {
        name: "agent",
        description: "切换当前活动的 Agent 线程",
    },
    BuiltinCommand {
        name: "subagents",
        description: "查看并切换子 Agent 线程",
    },
    BuiltinCommand {
        name: "apps",
        description: "浏览可用的应用和连接器",
    },
    BuiltinCommand {
        name: "plugins",
        description: "浏览和管理插件",
    },
    BuiltinCommand {
        name: "hooks",
        description: "查看和管理生命周期钩子",
    },
    BuiltinCommand {
        name: "clear",
        description: "清空终端并开始一个新对话",
    },
    BuiltinCommand {
        name: "rename",
        description: "重命名当前对话",
    },
    BuiltinCommand {
        name: "archive",
        description: "归档当前会话并退出",
    },
    BuiltinCommand {
        name: "delete",
        description: "永久删除当前会话并退出",
    },
    BuiltinCommand {
        name: "compact",
        description: "压缩当前对话以释放上下文空间",
    },
    BuiltinCommand {
        name: "copy",
        description: "复制最近一次已完成的回复",
    },
    BuiltinCommand {
        name: "diff",
        description: "查看 Git 工作区差异，包括未跟踪文件",
    },
    BuiltinCommand {
        name: "exit",
        description: "退出 Codex CLI",
    },
    BuiltinCommand {
        name: "quit",
        description: "退出 Codex CLI",
    },
    BuiltinCommand {
        name: "experimental",
        description: "查看或切换实验性功能",
    },
    BuiltinCommand {
        name: "approve",
        description: "重新尝试被自动审查拒绝的操作",
    },
    BuiltinCommand {
        name: "memories",
        description: "配置 Codex 的记忆功能",
    },
    BuiltinCommand {
        name: "skills",
        description: "浏览并使用可用技能",
    },
    BuiltinCommand {
        name: "import",
        description: "导入 Claude Code 的设置、项目或对话",
    },
    BuiltinCommand {
        name: "feedback",
        description: "向 OpenAI 发送反馈和诊断日志",
    },
    BuiltinCommand {
        name: "init",
        description: "在当前目录生成 AGENTS.md 指令文件",
    },
    BuiltinCommand {
        name: "logout",
        description: "退出当前 OpenAI 账号",
    },
    BuiltinCommand {
        name: "mcp",
        description: "查看已配置的 MCP 工具和服务器",
    },
    BuiltinCommand {
        name: "mention",
        description: "把文件附加到当前对话",
    },
    BuiltinCommand {
        name: "model",
        description: "选择模型和推理强度",
    },
    BuiltinCommand {
        name: "fast",
        description: "开启或关闭快速服务层（若账号支持）",
    },
    BuiltinCommand {
        name: "plan",
        description: "切换到计划模式",
    },
    BuiltinCommand {
        name: "goal",
        description: "管理当前会话的持久目标",
    },
    BuiltinCommand {
        name: "personality",
        description: "选择 Codex 的交流风格",
    },
    BuiltinCommand {
        name: "ps",
        description: "查看后台终端及其输出",
    },
    BuiltinCommand {
        name: "stop",
        description: "停止所有后台终端",
    },
    BuiltinCommand {
        name: "fork",
        description: "从当前对话创建一个分支会话",
    },
    BuiltinCommand {
        name: "side",
        description: "打开不影响主对话的临时侧边对话",
    },
    BuiltinCommand {
        name: "btw",
        description: "打开不影响主对话的临时侧边对话",
    },
    BuiltinCommand {
        name: "raw",
        description: "查看终端原始滚动输出",
    },
    BuiltinCommand {
        name: "resume",
        description: "恢复一个已保存的对话",
    },
    BuiltinCommand {
        name: "new",
        description: "开始一个新对话",
    },
    BuiltinCommand {
        name: "review",
        description: "请求 Codex 执行代码审查",
    },
    BuiltinCommand {
        name: "status",
        description: "查看会话配置和上下文用量",
    },
    BuiltinCommand {
        name: "usage",
        description: "查看账号使用量",
    },
    BuiltinCommand {
        name: "debug-config",
        description: "查看配置来源和诊断信息",
    },
    BuiltinCommand {
        name: "statusline",
        description: "配置终端状态栏",
    },
    BuiltinCommand {
        name: "title",
        description: "配置终端窗口标题显示字段",
    },
    BuiltinCommand {
        name: "theme",
        description: "选择代码语法高亮主题",
    },
    BuiltinCommand {
        name: "pets",
        description: "配置终端宠物",
    },
    BuiltinCommand {
        name: "pet",
        description: "配置终端宠物",
    },
];

// Codex 0.147.0 deliberately keeps frequently used commands first. Service
// tier commands such as `/fast` are inserted immediately after `/model`.
// Aliases such as `/pet` are accepted by dispatch but omitted from the popup.
const CODEX_DISPLAY_ORDER: &[&str] = &[
    "model",
    "fast",
    "ide",
    "permissions",
    "keymap",
    "vim",
    "experimental",
    "approve",
    "memories",
    "skills",
    "import",
    "hooks",
    "review",
    "rename",
    "new",
    "archive",
    "delete",
    "resume",
    "fork",
    "init",
    "compact",
    "plan",
    "goal",
    "agent",
    "side",
    "btw",
    "copy",
    "raw",
    "diff",
    "mention",
    "status",
    "usage",
    "debug-config",
    "title",
    "statusline",
    "theme",
    "pets",
    "mcp",
    "apps",
    "plugins",
    "logout",
    "quit",
    "exit",
    "feedback",
    "ps",
    "stop",
    "clear",
    "personality",
    "subagents",
];

const CLAUDE_COMMANDS: &[BuiltinCommand] = &[
    BuiltinCommand {
        name: "add-dir",
        description: "为当前会话添加额外工作目录",
    },
    BuiltinCommand {
        name: "advisor",
        description: "向只读顾问 Agent 提问",
    },
    BuiltinCommand {
        name: "agents",
        description: "管理自定义子 Agent",
    },
    BuiltinCommand {
        name: "autocompact",
        description: "开启或关闭自动压缩上下文",
    },
    BuiltinCommand {
        name: "autofix-pr",
        description: "自动修复拉取请求中的 CI 失败和评审意见",
    },
    BuiltinCommand {
        name: "background",
        description: "管理后台运行的任务",
    },
    BuiltinCommand {
        name: "batch",
        description: "并行执行大规模代码修改",
    },
    BuiltinCommand {
        name: "branch",
        description: "从当前对话创建一个分支会话",
    },
    BuiltinCommand {
        name: "btw",
        description: "提出不写入对话历史的临时问题",
    },
    BuiltinCommand {
        name: "bug",
        description: "提交 Claude Code 问题报告",
    },
    BuiltinCommand {
        name: "cd",
        description: "切换当前工作目录",
    },
    BuiltinCommand {
        name: "chrome",
        description: "配置 Claude in Chrome 集成",
    },
    BuiltinCommand {
        name: "claude-api",
        description: "获取 Claude API 集成帮助",
    },
    BuiltinCommand {
        name: "clear",
        description: "清空当前对话历史",
    },
    BuiltinCommand {
        name: "code-review",
        description: "审查拉取请求中的代码改动",
    },
    BuiltinCommand {
        name: "color",
        description: "设置当前会话的提示颜色",
    },
    BuiltinCommand {
        name: "compact",
        description: "压缩对话，并可指定压缩重点",
    },
    BuiltinCommand {
        name: "config",
        description: "打开设置界面",
    },
    BuiltinCommand {
        name: "context",
        description: "用彩色网格显示上下文使用情况",
    },
    BuiltinCommand {
        name: "copy",
        description: "复制 Claude 最近一次回复",
    },
    BuiltinCommand {
        name: "cost",
        description: "显示当前会话的令牌和费用统计",
    },
    BuiltinCommand {
        name: "dataviz",
        description: "生成可视化并在浏览器中预览",
    },
    BuiltinCommand {
        name: "debug",
        description: "排查当前会话问题，可附带说明",
    },
    BuiltinCommand {
        name: "deep-research",
        description: "使用 Web 搜索和连接来源执行深度研究",
    },
    BuiltinCommand {
        name: "design-login",
        description: "登录 Anthropic Design",
    },
    BuiltinCommand {
        name: "design-sync",
        description: "同步 Anthropic Design 中的设计",
    },
    BuiltinCommand {
        name: "diff",
        description: "打开交互式 Git 差异查看器",
    },
    BuiltinCommand {
        name: "doctor",
        description: "检查 Claude Code 安装和配置是否正常",
    },
    BuiltinCommand {
        name: "effort",
        description: "调整模型的推理强度",
    },
    BuiltinCommand {
        name: "exit",
        description: "退出 Claude Code",
    },
    BuiltinCommand {
        name: "export",
        description: "把当前对话导出为文本或文件",
    },
    BuiltinCommand {
        name: "fast",
        description: "开启或关闭快速输出模式",
    },
    BuiltinCommand {
        name: "feedback",
        description: "提交 Claude Code 使用反馈",
    },
    BuiltinCommand {
        name: "fewer-permission-prompts",
        description: "减少权限确认提示",
    },
    BuiltinCommand {
        name: "focus",
        description: "将界面切换为专注模式",
    },
    BuiltinCommand {
        name: "fork",
        description: "从当前点分叉出一个新会话",
    },
    BuiltinCommand {
        name: "goal",
        description: "设置并持续推进一个会话目标",
    },
    BuiltinCommand {
        name: "help",
        description: "显示帮助和可用命令",
    },
    BuiltinCommand {
        name: "hooks",
        description: "管理工具事件钩子",
    },
    BuiltinCommand {
        name: "ide",
        description: "管理 IDE 集成并查看连接状态",
    },
    BuiltinCommand {
        name: "import",
        description: "从文件或链接导入对话",
    },
    BuiltinCommand {
        name: "init",
        description: "生成 CLAUDE.md 项目指令文件",
    },
    BuiltinCommand {
        name: "insights",
        description: "生成 Claude Code 使用分析报告",
    },
    BuiltinCommand {
        name: "install-github-app",
        description: "安装 Claude GitHub Actions 应用",
    },
    BuiltinCommand {
        name: "install-slack-app",
        description: "安装 Claude Slack 应用",
    },
    BuiltinCommand {
        name: "keybindings",
        description: "打开快捷键配置",
    },
    BuiltinCommand {
        name: "list-agents",
        description: "列出当前可用的子 Agent",
    },
    BuiltinCommand {
        name: "login",
        description: "登录 Anthropic 账号",
    },
    BuiltinCommand {
        name: "logout",
        description: "退出 Anthropic 账号",
    },
    BuiltinCommand {
        name: "loop",
        description: "按间隔重复运行一条命令或提示词",
    },
    BuiltinCommand {
        name: "mcp",
        description: "管理 MCP 服务器连接和 OAuth 授权",
    },
    BuiltinCommand {
        name: "memory",
        description: "编辑 Claude 的记忆文件",
    },
    BuiltinCommand {
        name: "mobile",
        description: "显示移动端远程控制二维码",
    },
    BuiltinCommand {
        name: "model",
        description: "选择或切换当前模型",
    },
    BuiltinCommand {
        name: "permissions",
        description: "查看或更新权限规则",
    },
    BuiltinCommand {
        name: "plan",
        description: "进入计划模式",
    },
    BuiltinCommand {
        name: "plugin",
        description: "管理 Claude Code 插件",
    },
    BuiltinCommand {
        name: "powerup",
        description: "启用当前可用的增强功能",
    },
    BuiltinCommand {
        name: "radio",
        description: "生成音乐并控制播放",
    },
    BuiltinCommand {
        name: "recap",
        description: "生成项目和会话回顾",
    },
    BuiltinCommand {
        name: "release-notes",
        description: "查看 Claude Code 发行说明",
    },
    BuiltinCommand {
        name: "reload-plugins",
        description: "重新加载已安装插件",
    },
    BuiltinCommand {
        name: "reload-skills",
        description: "重新加载可用技能",
    },
    BuiltinCommand {
        name: "remote-control",
        description: "让 claude.ai 远程控制当前会话",
    },
    BuiltinCommand {
        name: "remote-env",
        description: "配置远程会话的默认环境",
    },
    BuiltinCommand {
        name: "rename",
        description: "重命名当前会话",
    },
    BuiltinCommand {
        name: "resume",
        description: "恢复另一个本地或远程会话",
    },
    BuiltinCommand {
        name: "review",
        description: "审查 GitHub 拉取请求",
    },
    BuiltinCommand {
        name: "rewind",
        description: "回退对话或代码改动",
    },
    BuiltinCommand {
        name: "run",
        description: "运行已配置的自动化任务",
    },
    BuiltinCommand {
        name: "run-skill-generator",
        description: "分析会话并生成可复用技能",
    },
    BuiltinCommand {
        name: "sandbox",
        description: "管理沙箱模式和依赖",
    },
    BuiltinCommand {
        name: "schedule",
        description: "创建定时或周期性任务",
    },
    BuiltinCommand {
        name: "security-review",
        description: "审查当前分支的安全风险",
    },
    BuiltinCommand {
        name: "simplify",
        description: "并行审查并简化最近修改的代码",
    },
    BuiltinCommand {
        name: "skills",
        description: "列出当前可用的技能",
    },
    BuiltinCommand {
        name: "stats",
        description: "查看 Claude Code 使用统计",
    },
    BuiltinCommand {
        name: "status",
        description: "显示版本、模型、账号和连接状态",
    },
    BuiltinCommand {
        name: "statusline",
        description: "配置终端状态栏",
    },
    BuiltinCommand {
        name: "stickers",
        description: "订购 Claude 贴纸",
    },
    BuiltinCommand {
        name: "stop",
        description: "停止正在运行的后台任务",
    },
    BuiltinCommand {
        name: "subtask",
        description: "通过子 Agent 执行复杂任务",
    },
    BuiltinCommand {
        name: "tasks",
        description: "列出和管理后台任务",
    },
    BuiltinCommand {
        name: "team-onboarding",
        description: "为团队配置 Claude Code",
    },
    BuiltinCommand {
        name: "teleport",
        description: "恢复 claude.ai 远程会话",
    },
    BuiltinCommand {
        name: "theme",
        description: "更改界面颜色主题",
    },
    BuiltinCommand {
        name: "tui",
        description: "切换终端界面模式",
    },
    BuiltinCommand {
        name: "ultrareview",
        description: "对当前改动执行多 Agent 深度审查",
    },
    BuiltinCommand {
        name: "upgrade",
        description: "升级 Claude Code 版本或套餐",
    },
    BuiltinCommand {
        name: "usage",
        description: "查看套餐使用限制和重置时间",
    },
    BuiltinCommand {
        name: "usage-credits",
        description: "查看额外用量额度",
    },
    BuiltinCommand {
        name: "verify",
        description: "运行项目验证流程",
    },
    BuiltinCommand {
        name: "voice",
        description: "开启或关闭语音输入",
    },
    BuiltinCommand {
        name: "web-setup",
        description: "配置 Claude 网页端集成",
    },
    BuiltinCommand {
        name: "workflows",
        description: "浏览和运行工作流",
    },
];

// dsh-TUI（@deepseek-harness-tui/dsh-tui）的本地命令全集，按分组顺序与上游 README 保持一致。
// `/resume` 等动态选择器命令在 EXCLUDED 中排除；其余命令是叶子项，选中后
// 把 `/name` 发给 PTY，由 TUI 在终端内自行展开（模型、主题、子代理等选择器）。
const DSH_COMMANDS: &[BuiltinCommand] = &[
    // 会话
    BuiltinCommand {
        name: "new",
        description: "开始一个新会话",
    },
    BuiltinCommand {
        name: "resume",
        description: "恢复一个已保存的会话",
    },
    BuiltinCommand {
        name: "clear",
        description: "清空屏幕并开始一个新对话",
    },
    BuiltinCommand {
        name: "compact",
        description: "压缩当前会话以释放上下文空间",
    },
    BuiltinCommand {
        name: "export",
        description: "将当前会话导出为 Markdown",
    },
    // 状态
    BuiltinCommand {
        name: "status",
        description: "查看会话信息和配置",
    },
    BuiltinCommand {
        name: "cost",
        description: "查看 token 用量",
    },
    BuiltinCommand {
        name: "doctor",
        description: "运行环境自检",
    },
    BuiltinCommand {
        name: "config",
        description: "查看配置来源",
    },
    BuiltinCommand {
        name: "init",
        description: "创建 AGENTS.md 指令文件",
    },
    // 模型
    BuiltinCommand {
        name: "model",
        description: "打开模型选择器（会话 fork 续聊）",
    },
    BuiltinCommand {
        name: "thinking",
        description: "显示或隐藏模型思考内容",
    },
    BuiltinCommand {
        name: "tokens",
        description: "查看 token 明细",
    },
    BuiltinCommand {
        name: "theme",
        description: "打开主题选择器",
    },
    BuiltinCommand {
        name: "lang",
        description: "切换中英界面语言",
    },
    // 账号/策略
    BuiltinCommand {
        name: "login",
        description: "查看凭证状态",
    },
    BuiltinCommand {
        name: "logout",
        description: "查看登出说明",
    },
    BuiltinCommand {
        name: "permissions",
        description: "查看权限说明",
    },
    BuiltinCommand {
        name: "add-dir",
        description: "调整文件策略范围",
    },
    BuiltinCommand {
        name: "hooks",
        description: "查看生命周期钩子",
    },
    BuiltinCommand {
        name: "mcp",
        description: "查看 MCP 服务器连接状态",
    },
    BuiltinCommand {
        name: "memory",
        description: "编辑记忆文件",
    },
    // 技能
    BuiltinCommand {
        name: "audit",
        description: "执行代码审计",
    },
    BuiltinCommand {
        name: "bug",
        description: "提交 bug 报告",
    },
    BuiltinCommand {
        name: "review",
        description: "执行代码评审",
    },
    BuiltinCommand {
        name: "practice",
        description: "进行编程练习",
    },
    BuiltinCommand {
        name: "pr_comments",
        description: "查看 PR 评论",
    },
    BuiltinCommand {
        name: "release-notes",
        description: "查看发布说明",
    },
    BuiltinCommand {
        name: "vuln-check",
        description: "执行漏洞检查",
    },
    // 其它
    BuiltinCommand {
        name: "agents",
        description: "打开子代理列表",
    },
    BuiltinCommand {
        name: "vim",
        description: "切换 Vim 输入模式",
    },
    BuiltinCommand {
        name: "terminal-setup",
        description: "打开终端设置向导",
    },
    BuiltinCommand {
        name: "connect",
        description: "查看模型提供商连接",
    },
    BuiltinCommand {
        name: "help",
        description: "显示帮助",
    },
    BuiltinCommand {
        name: "exit",
        description: "退出 dsh TUI",
    },
    // 注册表（DSH 命令注册表插件，随插件并入 `/` 菜单）
    BuiltinCommand {
        name: "plan",
        description: "进入计划模式",
    },
    BuiltinCommand {
        name: "goal",
        description: "管理会话的持久目标",
    },
];

const OPENCODE_COMMANDS: &[BuiltinCommand] = &[
    BuiltinCommand {
        name: "connect",
        description: "添加或配置模型提供商凭据",
    },
    BuiltinCommand {
        name: "compact",
        description: "压缩当前会话以释放上下文空间",
    },
    BuiltinCommand {
        name: "details",
        description: "显示或隐藏工具执行详情",
    },
    BuiltinCommand {
        name: "editor",
        description: "使用外部编辑器编写消息",
    },
    BuiltinCommand {
        name: "exit",
        description: "退出 OpenCode",
    },
    BuiltinCommand {
        name: "export",
        description: "将当前会话导出为 Markdown",
    },
    BuiltinCommand {
        name: "help",
        description: "打开帮助对话框",
    },
    BuiltinCommand {
        name: "init",
        description: "创建或更新 AGENTS.md 项目指令",
    },
    BuiltinCommand {
        name: "models",
        description: "列出并切换可用模型",
    },
    BuiltinCommand {
        name: "new",
        description: "开始一个新会话",
    },
    BuiltinCommand {
        name: "redo",
        description: "恢复最近撤销的消息和文件改动",
    },
    BuiltinCommand {
        name: "sessions",
        description: "列出并切换历史会话",
    },
    BuiltinCommand {
        name: "share",
        description: "生成当前会话的分享链接",
    },
    BuiltinCommand {
        name: "themes",
        description: "选择终端界面主题",
    },
    BuiltinCommand {
        name: "thinking",
        description: "显示或隐藏模型思考内容",
    },
    BuiltinCommand {
        name: "undo",
        description: "撤销最近一条消息及其文件改动",
    },
    BuiltinCommand {
        name: "unshare",
        description: "撤销当前会话的分享链接",
    },
];

/// One entry in the Composer's runtime-aware `/` picker. `invocation` is the
/// exact text understood by the target CLI; it intentionally differs between
/// providers (for example Codex skills use `$name`).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SlashItem {
    pub name: String,
    pub description: String,
    pub invocation: String,
    pub source: String,
    pub kind: String,
    /// The Composer opens a second-level picker after selecting this item.
    /// Frontends treat a missing field as falsy, so older UI builds keep
    /// rendering the flat catalog.
    pub has_children: bool,
}

/// How a built-in command relates to the static child pickers.
enum ChildSource {
    /// Leaf — selecting it completes the input and closes the menu.
    None,
    /// `agent_list_slash_children` can enumerate its options statically.
    Static,
    /// Needs a dynamic picker/harvest to populate; hidden from the Composer.
    Excluded,
}

/// Per-runtime commands that get a second-level static picker. The child list
/// is produced by `discover_children` without touching the PTY.
///
/// Only commands whose CLI accepts a verbatim inline argument belong here.
/// Codex's built-ins (`supports_inline_args`) and OpenCode's pickers take no
/// inline values, so their rows are absent — those commands stay plain leaves
/// and the CLI opens its own picker in the PTY.
const STATIC_CHILDREN: &[(&str, &[&str])] = &[
    ("claude", &["model", "permissions", "effort", "fast", "mcp"]),
];

/// Per-runtime commands whose options live only in the CLI's own picker
/// (sessions, forks, plugins, ...). Selecting them from the Composer would
/// dead-end, so they are filtered out of the menu. Users can still type them
/// and send them to the terminal manually.
const EXCLUDED: &[(&str, &[&str])] = &[
    (
        "claude",
        &[
            "resume", "fork", "rewind", "memory", "config", "plugins", "add-dir", "cd",
            "background", "tasks",
        ],
    ),
    (
        "codex",
        &["resume", "fork", "apps", "plugins", "hooks", "ps"],
    ),
    ("opencode", &["sessions"]),
    // dsh 的会话恢复走 TUI 内的选择器（`ResumePicker`），Composer 无法枚举
    // 选项；其余命令是叶子项，选中后发给 PTY 由 TUI 自行展开。
    ("dsh", &["resume"]),
];

fn child_source(runtime: &str, name: &str) -> ChildSource {
    if EXCLUDED
        .iter()
        .any(|(r, names)| *r == runtime && names.contains(&name))
    {
        return ChildSource::Excluded;
    }
    if STATIC_CHILDREN
        .iter()
        .any(|(r, names)| *r == runtime && names.contains(&name))
    {
        return ChildSource::Static;
    }
    ChildSource::None
}

#[derive(Default)]
struct Frontmatter {
    name: Option<String>,
    description: Option<String>,
    user_invocable: Option<bool>,
    slash: Option<bool>,
}

fn clean_scalar(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        value[1..value.len() - 1].trim().to_string()
    } else {
        value.to_string()
    }
}

fn parse_bool(value: &str) -> Option<bool> {
    match clean_scalar(value).to_ascii_lowercase().as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// Read only the small metadata prefix. Skill bodies may contain executable
/// instructions and can be very large; the picker needs only safe text labels.
fn read_frontmatter(path: &Path) -> Frontmatter {
    let Ok(file) = File::open(path) else {
        return Frontmatter::default();
    };
    let mut bytes = Vec::new();
    if file
        .take(MAX_METADATA_BYTES)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return Frontmatter::default();
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return Frontmatter::default();
    }

    let mut meta = Frontmatter::default();
    let mut block_key: Option<String> = None;
    let mut block_lines: Vec<String> = Vec::new();

    let flush_block =
        |key: &mut Option<String>, values: &mut Vec<String>, meta: &mut Frontmatter| {
            if key.as_deref() == Some("description") && !values.is_empty() {
                meta.description = Some(values.join(" "));
            }
            *key = None;
            values.clear();
        };

    for line in lines {
        if line.trim() == "---" {
            flush_block(&mut block_key, &mut block_lines, &mut meta);
            break;
        }
        if block_key.is_some() && (line.starts_with(' ') || line.starts_with('\t')) {
            let value = line.trim();
            if !value.is_empty() {
                block_lines.push(value.to_string());
            }
            continue;
        }
        flush_block(&mut block_key, &mut block_lines, &mut meta);
        let Some((raw_key, raw_value)) = line.trim().split_once(':') else {
            continue;
        };
        let key = raw_key.trim().to_ascii_lowercase();
        let value = raw_value.trim();
        if matches!(value, ">" | ">-" | "|" | "|-") {
            block_key = Some(key);
            continue;
        }
        match key.as_str() {
            "name" => meta.name = Some(clean_scalar(value)),
            "description" => meta.description = Some(clean_scalar(value)),
            "user-invocable" => meta.user_invocable = parse_bool(value),
            // OpenCode V2 accepts both a top-level `slash` field and the
            // nested `metadata.opencode/slash` spelling. The simple parser sees
            // the nested key after indentation is trimmed.
            "slash" | "opencode/slash" => meta.slash = parse_bool(value),
            _ => {}
        }
    }
    meta
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
}

fn clipped_description(description: Option<String>) -> String {
    let description = description
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut chars = description.chars();
    let clipped: String = chars.by_ref().take(240).collect();
    if chars.next().is_some() {
        format!("{clipped}…")
    } else {
        clipped
    }
}

fn chinese_description(description: Option<String>, name: &str, kind: &str) -> String {
    let description = clipped_description(description);
    if description
        .chars()
        .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
    {
        return description;
    }
    if kind == "skill" {
        format!("加载并执行「{name}」技能")
    } else {
        format!("运行自定义命令「/{name}」")
    }
}

fn builtin_commands(runtime: &str) -> &'static [BuiltinCommand] {
    match runtime {
        "claude" => CLAUDE_COMMANDS,
        "codex" => CODEX_COMMANDS,
        "opencode" => OPENCODE_COMMANDS,
        "dsh" => DSH_COMMANDS,
        _ => &[],
    }
}

fn append_builtin_commands(runtime: &str, items: &mut Vec<SlashItem>, seen: &mut HashSet<String>) {
    let append =
        |command: &BuiltinCommand, items: &mut Vec<SlashItem>, seen: &mut HashSet<String>| {
            // Dynamic-picker commands are not statically enumerable; keep them
            // out of the menu so selecting one never dead-ends.
            if matches!(child_source(runtime, command.name), ChildSource::Excluded) {
                return;
            }
            push_item(
                items,
                seen,
                SlashItem {
                    name: command.name.to_string(),
                    description: command.description.to_string(),
                    invocation: format!("/{}", command.name),
                    source: BUILTIN_SOURCE.to_string(),
                    kind: "command".to_string(),
                    has_children: matches!(
                        child_source(runtime, command.name),
                        ChildSource::Static
                    ),
                },
            );
        };

    if runtime == "codex" {
        for name in CODEX_DISPLAY_ORDER {
            if let Some(command) = CODEX_COMMANDS.iter().find(|command| command.name == *name) {
                append(command, items, seen);
            }
        }
        return;
    }

    for command in builtin_commands(runtime) {
        append(command, items, seen);
    }
}

fn push_item(items: &mut Vec<SlashItem>, seen: &mut HashSet<String>, item: SlashItem) {
    if items.len() >= MAX_ITEMS || !seen.insert(item.invocation.clone()) {
        return;
    }
    items.push(item);
}

fn scan_skill_root(
    root: &Path,
    source: &str,
    invocation: impl Fn(&str) -> String,
    items: &mut Vec<SlashItem>,
    seen: &mut HashSet<String>,
) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let mut paths = entries
        .flatten()
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    paths.sort();
    for directory in paths {
        let skill_file = directory.join("SKILL.md");
        if !skill_file.is_file() {
            continue;
        }
        let meta = read_frontmatter(&skill_file);
        // Claude uses this field to hide model-only reference skills from its
        // interactive command menu.
        if meta.user_invocable == Some(false) || meta.slash == Some(false) {
            continue;
        }
        let fallback = directory
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        let name = meta
            .name
            .as_deref()
            .filter(|name| valid_name(name))
            .unwrap_or(fallback);
        if !valid_name(name) {
            continue;
        }
        push_item(
            items,
            seen,
            SlashItem {
                name: name.to_string(),
                description: chinese_description(meta.description, name, "skill"),
                invocation: invocation(name),
                source: source.to_string(),
                kind: "skill".to_string(),
                has_children: false,
            },
        );
    }
}

fn scan_command_root(
    root: &Path,
    source: &str,
    items: &mut Vec<SlashItem>,
    seen: &mut HashSet<String>,
) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let mut paths = entries
        .flatten()
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    paths.sort();
    for path in paths {
        if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        let meta = read_frontmatter(&path);
        let fallback = path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        let name = meta
            .name
            .as_deref()
            .filter(|name| valid_name(name))
            .unwrap_or(fallback);
        if !valid_name(name) {
            continue;
        }
        push_item(
            items,
            seen,
            SlashItem {
                name: name.to_string(),
                description: chinese_description(meta.description, name, "command"),
                invocation: format!("/{name}"),
                source: source.to_string(),
                kind: "command".to_string(),
                has_children: false,
            },
        );
    }
}

/// Provider project configuration is inherited only as far as the nearest git
/// root. CaPilot's managed projects are git-initialized, so their per-project
/// skills are found even though a default session starts in `agents/<id>`.
fn project_ancestors(cwd: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut current = cwd.to_path_buf();
    let home = crate::persistence::user_home().ok();
    loop {
        out.push(current.clone());
        if current.join(".git").exists() || home.as_ref() == Some(&current) {
            break;
        }
        let Some(parent) = current.parent() else {
            break;
        };
        current = parent.to_path_buf();
    }
    out
}

fn discover(runtime: &str, cwd: &Path) -> Vec<SlashItem> {
    let home = crate::persistence::user_home().ok();
    let ancestors = project_ancestors(cwd);
    let mut items = Vec::new();
    let mut seen = HashSet::new();

    // Native CLIs route built-ins before extension, custom, file and skill
    // commands. Insert them first so the Composer mirrors that collision rule.
    append_builtin_commands(runtime, &mut items, &mut seen);

    match runtime {
        "claude" => {
            // Claude precedence: personal skills override project skills.
            if let Some(home) = &home {
                scan_skill_root(
                    &home.join(".claude/skills"),
                    "个人",
                    |name| format!("/{name}"),
                    &mut items,
                    &mut seen,
                );
                scan_command_root(
                    &home.join(".claude/commands"),
                    "个人命令",
                    &mut items,
                    &mut seen,
                );
            }
            for dir in &ancestors {
                scan_skill_root(
                    &dir.join(".claude/skills"),
                    "项目",
                    |name| format!("/{name}"),
                    &mut items,
                    &mut seen,
                );
                scan_command_root(
                    &dir.join(".claude/commands"),
                    "项目命令",
                    &mut items,
                    &mut seen,
                );
            }
        }
        "codex" => {
            // Current Codex uses `.agents/skills`; CODEX_HOME/skills remains
            // included because released CLIs keep bundled/system skills there.
            for dir in &ancestors {
                scan_skill_root(
                    &dir.join(".agents/skills"),
                    "项目",
                    |name| format!("${name}"),
                    &mut items,
                    &mut seen,
                );
            }
            if let Some(home) = &home {
                scan_skill_root(
                    &home.join(".agents/skills"),
                    "个人",
                    |name| format!("${name}"),
                    &mut items,
                    &mut seen,
                );
                let codex_home = std::env::var_os("CODEX_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home.join(".codex"));
                scan_skill_root(
                    &codex_home.join("skills"),
                    "Codex",
                    |name| format!("${name}"),
                    &mut items,
                    &mut seen,
                );
                scan_skill_root(
                    &codex_home.join("skills/.system"),
                    "Codex 内置",
                    |name| format!("${name}"),
                    &mut items,
                    &mut seen,
                );
            }
            scan_skill_root(
                Path::new("/etc/codex/skills"),
                "系统",
                |name| format!("${name}"),
                &mut items,
                &mut seen,
            );
        }
        "opencode" => {
            // OpenCode's stable TUI exposes custom commands as `/name`.
            // Agent skills are model-loaded and are not slash commands there.
            for dir in &ancestors {
                scan_command_root(
                    &dir.join(".opencode/commands"),
                    "项目命令",
                    &mut items,
                    &mut seen,
                );
            }
            if let Some(home) = &home {
                let config_home = std::env::var_os("XDG_CONFIG_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home.join(".config"));
                scan_command_root(
                    &config_home.join("opencode/commands"),
                    "个人命令",
                    &mut items,
                    &mut seen,
                );
            }
        }
        _ => {}
    }

    if runtime == "opencode" {
        // OpenCode's current TUI presents the combined command list by name.
        items.sort_by(|a, b| a.invocation.cmp(&b.invocation));
    }
    items
}

/// Resolve runtime/cwd from the persisted session rather than accepting paths
/// from the webview. This keeps filesystem discovery scoped to a real agent.
#[tauri::command]
pub fn agent_list_slash_items(
    persistence: tauri::State<'_, Arc<Persistence>>,
    id: String,
) -> Result<Vec<SlashItem>, String> {
    let session = persistence
        .db_tolerant()
        .ok_or_else(|| "persistence unavailable".to_string())?
        .get(&id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("agent not found: {id}"))?;
    Ok(discover(&session.runtime, &session.cwd))
}

/// Enumerate the static options for a built-in command's second-level picker.
/// `parent` is the built-in command name (`model`, `effort`, `mcp`, ...). All
/// children are leaves for now; the stack mechanism supports deeper levels.
///
/// `invocation` is always the verbatim argument the target CLI accepts after
/// `/parent` — never CaPilot's internal id vocabulary. The adapters' internal
/// ids (`list_thinking_options`, `list_permission_modes`) stay for the spawn
/// flags and settings panel; they are not valid interactive-command text.
fn discover_children(runtime: &str, _cwd: &Path, parent: &str) -> Vec<SlashItem> {
    let mut items = Vec::new();
    let mut seen = HashSet::new();

    // (invocation, Chinese description) pairs for a static child list.
    let mut push_static = |pairs: &[(&str, &str)], source: &str| {
        for (invocation, description) in pairs {
            push_item(
                &mut items,
                &mut seen,
                SlashItem {
                    name: invocation.to_string(),
                    description: description.to_string(),
                    invocation: invocation.to_string(),
                    source: source.to_string(),
                    kind: "command".to_string(),
                    has_children: false,
                },
            );
        }
    };

    match parent {
        "model" => {
            // Only Claude's `/model <id>` accepts an inline model id; Codex opens
            // an interactive picker instead.
            if runtime == "claude" {
                for model in get_adapter(runtime).list_models() {
                    push_item(
                        &mut items,
                        &mut seen,
                        SlashItem {
                            name: model.id.clone(),
                            description: format!("{} · {}", model.name, model.provider),
                            invocation: model.id,
                            source: "模型".to_string(),
                            kind: "command".to_string(),
                            has_children: false,
                        },
                    );
                }
            }
        }
        "permissions" => {
            // Claude accepts `/permissions <mode>`. `bypassPermissions` is
            // gated behind --dangerously-skip-permissions, so it is omitted.
            match runtime {
                "claude" => push_static(
                    &[
                        ("default", "默认权限模式：敏感操作逐一询问"),
                        ("acceptEdits", "自动接受文件编辑，其他操作仍询问"),
                        ("plan", "只读规划模式：不执行修改"),
                        ("auto", "由 Claude 自动决策权限（原生自动模式）"),
                    ],
                    "权限模式",
                ),
                _ => {}
            }
        }
        "effort" => {
            // Claude Code v2.1.205+ `/effort [level|auto]`; `auto` resets.
            match runtime {
                "claude" => push_static(
                    &[
                        ("low", "低思考强度，响应更快"),
                        ("medium", "中等思考强度"),
                        ("high", "高思考强度"),
                        ("xhigh", "极高思考强度"),
                        ("max", "最大思考强度（会话级）"),
                        ("ultracode", "Ultracode 模式（会话级）"),
                        ("auto", "由模型自动选择"),
                    ],
                    "思考强度",
                ),
                _ => {}
            }
        }
        "fast" => {
            // claude `/fast [on|off]`.
            push_static(&[("on", "快速模式"), ("off", "快速模式")], "开关");
        }
        "mcp" => {
            match runtime {
                // claude `/mcp [reconnect <server>|enable|disable [<server>|all]]`:
                // a bare server name is invalid, so offer the actions and let the
                // CLI's own server list finish the command in the PTY.
                "claude" => push_static(
                    &[
                        ("enable", "启用 MCP 服务器（随后在终端选择服务器）"),
                        ("disable", "禁用 MCP 服务器（随后在终端选择服务器）"),
                        ("reconnect", "重连已断开的服务器（随后在终端选择服务器）"),
                    ],
                    "MCP 动作",
                ),
                _ => {}
            }
        }
        _ => {}
    }

    items
}

/// Resolve the second-level picker for a built-in command. Runtime/cwd come
/// from the persisted session, never from the webview.
#[tauri::command]
pub fn agent_list_slash_children(
    persistence: tauri::State<'_, Arc<Persistence>>,
    id: String,
    parent: String,
) -> Result<Vec<SlashItem>, String> {
    let session = persistence
        .db_tolerant()
        .ok_or_else(|| "persistence unavailable".to_string())?
        .get(&id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("agent not found: {id}"))?;
    Ok(discover_children(&session.runtime, &session.cwd, &parent))
}

#[cfg(test)]
mod tests {
    use super::{builtin_commands, discover, discover_children, read_frontmatter};
    use std::collections::HashSet;
    use std::fs;
    use std::path::PathBuf;

    fn fixture_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "capilot-slash-{name}-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ))
    }

    #[test]
    fn parses_frontmatter_and_multiline_description() {
        let root = fixture_root("frontmatter");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("SKILL.md");
        fs::write(
            &path,
            "---\nname: review\ndescription: >-\n  Review changes and\n  flag risks.\nuser-invocable: false\n---\nbody",
        )
        .unwrap();
        let meta = read_frontmatter(&path);
        assert_eq!(meta.name.as_deref(), Some("review"));
        assert_eq!(
            meta.description.as_deref(),
            Some("Review changes and flag risks.")
        );
        assert_eq!(meta.user_invocable, Some(false));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn opencode_discovers_project_commands() {
        let root = fixture_root("opencode");
        let commands = root.join(".opencode/commands");
        fs::create_dir_all(&commands).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(
            commands.join("test.md"),
            "---\ndescription: Run the test suite\n---\nRun tests",
        )
        .unwrap();
        let items = discover("opencode", &root);
        let item = items
            .iter()
            .find(|item| item.invocation == "/test")
            .unwrap();
        assert_eq!(item.kind, "command");
        assert_eq!(item.description, "运行自定义命令「/test」");
        assert!(items.iter().any(|item| {
            item.invocation == "/models"
                && item.source == "内置命令"
                && item.description == "列出并切换可用模型"
        }));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn builtin_catalog_is_runtime_specific_and_chinese() {
        let root = fixture_root("builtins");
        fs::create_dir_all(root.join(".git")).unwrap();

        let codex = discover("codex", &root);
        assert_eq!(
            codex.first().map(|item| item.invocation.as_str()),
            Some("/model")
        );
        assert_eq!(
            codex.get(1).map(|item| item.invocation.as_str()),
            Some("/fast")
        );
        assert!(!codex.iter().any(|item| item.invocation == "/pet"));
        assert!(codex.iter().any(|item| {
            item.invocation == "/model"
                && item.source == "内置命令"
                && item.description == "选择模型和推理强度"
        }));
        assert!(!codex.iter().any(|item| item.invocation == "/connect"));

        let opencode = discover("opencode", &root);
        assert_eq!(
            opencode.first().map(|item| item.invocation.as_str()),
            Some("/compact")
        );
        assert!(opencode.iter().any(|item| item.invocation == "/connect"));
        assert!(opencode.iter().any(|item| item.invocation == "/models"));
        assert!(opencode.iter().any(|item| item.invocation == "/exit"));
        assert!(!opencode
            .iter()
            .any(|item| item.invocation == "/permissions"));

        let claude = discover("claude", &root);
        assert_eq!(
            claude.first().map(|item| item.invocation.as_str()),
            Some("/advisor")
        );

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn every_builtin_has_a_unique_name_and_chinese_description() {
        for runtime in ["claude", "codex", "opencode"] {
            let mut names = HashSet::new();
            for command in builtin_commands(runtime) {
                assert!(names.insert(command.name), "{runtime}: {}", command.name);
                assert!(
                    command
                        .description
                        .chars()
                        .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
                    "{runtime}: {}",
                    command.name
                );
            }
        }
    }

    #[test]
    fn dsh_builtin_catalog_is_complete_and_resume_is_excluded() {
        let root = fixture_root("dsh");
        fs::create_dir_all(root.join(".git")).unwrap();

        // 目录全集：会话 + 状态 + 模型 + 策略 + 技能 + 其它 + 注册表。
        let catalog = builtin_commands("dsh");
        assert!(catalog.len() >= 30);
        for command in catalog {
            assert!(
                command
                    .description
                    .chars()
                    .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
                "dsh: {}",
                command.name
            );
        }
        let names = catalog
            .iter()
            .map(|command| command.name)
            .collect::<HashSet<_>>();
        assert_eq!(names.len(), catalog.len(), "dsh names must be unique");
        for expected in [
            "new",
            "resume",
            "clear",
            "compact",
            "export",
            "status",
            "cost",
            "model",
            "thinking",
            "permissions",
            "mcp",
            "review",
            "exit",
        ] {
            assert!(
                names.contains(expected),
                "dsh catalog should contain /{expected}"
            );
        }

        // `/resume` 走 TUI 内选择器，从 Composer 菜单排除；其余是叶子项。
        let menu = discover("dsh", &root);
        assert!(!menu.iter().any(|item| item.invocation == "/resume"));
        assert!(menu.iter().any(|item| item.invocation == "/model"));
        let model = menu
            .iter()
            .find(|item| item.invocation == "/model")
            .unwrap();
        assert!(
            !model.has_children,
            "dsh /model opens the TUI's own picker and takes no inline arg"
        );
        assert!(!menu.iter().any(|item| item.invocation == "/sessions"));
        assert!(menu.iter().any(|item| item.invocation == "/compact"));

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn dynamic_picker_commands_are_excluded_from_menu() {
        let root = fixture_root("excluded");
        fs::create_dir_all(root.join(".git")).unwrap();

        let claude = discover("claude", &root);
        for excluded in [
            "/resume",
            "/fork",
            "/rewind",
            "/memory",
            "/config",
            "/plugins",
            "/cd",
            "/background",
            "/tasks",
        ] {
            assert!(
                !claude.iter().any(|item| item.invocation == excluded),
                "claude menu should not contain {excluded}"
            );
        }
        assert!(claude.iter().any(|item| item.invocation == "/model"));
        assert!(claude.iter().any(|item| item.invocation == "/clear"));

        let opencode = discover("opencode", &root);
        assert!(!opencode.iter().any(|item| item.invocation == "/sessions"));
        assert!(opencode.iter().any(|item| item.invocation == "/models"));

        let codex = discover("codex", &root);
        assert!(!codex.iter().any(|item| item.invocation == "/resume"));
        assert!(!codex.iter().any(|item| item.invocation == "/apps"));
        assert!(codex.iter().any(|item| item.invocation == "/model"));

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn static_children_are_marked_and_leaves_are_not() {
        let root = fixture_root("has_children");
        fs::create_dir_all(root.join(".git")).unwrap();

        let claude = discover("claude", &root);
        for parent in ["/model", "/permissions", "/effort", "/fast", "/mcp"] {
            assert!(
                claude
                    .iter()
                    .find(|item| item.invocation == parent)
                    .unwrap_or_else(|| panic!("claude menu should contain {parent}"))
                    .has_children,
                "claude {parent} should have a second-level picker"
            );
        }
        // `/agents` and `/theme` take no inline argument; they stay leaves.
        for leaf in ["/agents", "/theme", "/clear"] {
            assert!(
                !claude
                    .iter()
                    .find(|item| item.invocation == leaf)
                    .unwrap()
                    .has_children,
                "claude {leaf} should be a leaf"
            );
        }

        // Codex built-ins take no inline arguments, so none has children.
        let codex = discover("codex", &root);
        for leaf in ["/model", "/fast", "/permissions", "/vim", "/theme", "/mcp"] {
            assert!(
                !codex
                    .iter()
                    .find(|item| item.invocation == leaf)
                    .unwrap()
                    .has_children,
                "codex {leaf} should be a leaf"
            );
        }

        // OpenCode's `/models` and `/themes` are interactive pickers.
        let opencode = discover("opencode", &root);
        assert!(!opencode
            .iter()
            .find(|item| item.invocation == "/models")
            .unwrap()
            .has_children);
        assert!(!opencode
            .iter()
            .find(|item| item.invocation == "/themes")
            .unwrap()
            .has_children);
        assert!(!opencode
            .iter()
            .find(|item| item.invocation == "/connect")
            .unwrap()
            .has_children);

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn children_use_cli_native_values() {
        let root = fixture_root("children");
        fs::create_dir_all(root.join(".git")).unwrap();

        let invocations = |runtime: &str, parent: &str| {
            discover_children(runtime, &root, parent)
                .iter()
                .map(|item| item.invocation.clone())
                .collect::<Vec<_>>()
        };

        assert_eq!(invocations("claude", "fast"), ["on", "off"]);

        // `/effort` must emit Claude's native levels, never the internal
        // speed vocabulary (`fast`, `mid`).
        let effort = invocations("claude", "effort");
        assert_eq!(
            effort,
            ["low", "medium", "high", "xhigh", "max", "ultracode", "auto"]
        );

        // `/permissions` must emit native modes; internal `ask`/`yolo` and the
        // launch-flag-gated `bypassPermissions` are excluded.
        let permissions = invocations("claude", "permissions");
        assert_eq!(permissions, ["default", "acceptEdits", "plan", "auto"]);

        // `/mcp` offers actions (claude), not server names.
        assert_eq!(invocations("claude", "mcp"), ["enable", "disable", "reconnect"]);

        let models = discover_children("claude", &root, "model");
        assert_eq!(models.len(), 3);
        assert!(models.iter().all(|item| item.kind == "command"));
        assert!(models.iter().all(|item| item.invocation.starts_with("claude-")));

        // Removed parents return empty rather than stale internal-id text.
        assert!(discover_children("codex", &root, "model").is_empty());
        assert!(discover_children("claude", &root, "theme").is_empty());
        assert!(discover_children("claude", &root, "agents").is_empty());

        // Unknown parent → empty, not an error.
        assert!(discover_children("claude", &root, "unknown").is_empty());

        fs::remove_dir_all(&root).unwrap();
    }
}
