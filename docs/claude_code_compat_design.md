# Claude Code 全面兼容设计文档

> **目标**: 让 Fox Agent SDK 完全兼容 Claude Code 的 Skill、Plugin、Hook 生态系统，
> 使应用方无需处理兼容细节，开箱即用。

---

## 1. 现状分析

### 1.1 已有能力

| 能力 | 当前状态 | 差距 |
|------|----------|------|
| Skill 解析 (YAML frontmatter) | 有 | 仅支持基础字段，缺少 `arg`/`model`/`allowed-tools` 等 |
| Skill 加载目录 | 仅 `<workdir>/.claude/skills/` | 不支持全局 skills、不支持嵌套目录递归、不支持 `additionalDirectories` |
| Skill 动态激活/去激活 | 有 (`skill` 工具) | 缺少 `args` 注入、`baseDirectory` 解析 |
| Hook 系统 | **无** | Claude Code 有 12 种 hook 事件 |
| Plugin 系统 | **无** | Claude Code 有 marketplace + plugin 安装机制 |
| 全局配置加载 | 无 | Claude Code 有 `~/.claude/settings.json` |

### 1.2 Claude Code 三层架构

```
┌─────────────────────────────────────────┐
│              Plugin                      │  ← 包装好的功能包
│  ┌─────────────┐  ┌──────────────────┐   │
│  │   Skills     │  │     Hooks         │   │  ← 实际功能
│  │  (.md 文件)   │  │  (shell/python/…) │   │
│  └─────────────┘  └──────────────────┘   │
│  ┌──────────────────────────────────┐    │
│  │        Plugin Config             │    │
│  │   (plugin.json / package.json)   │    │
│  └──────────────────────────────────┘    │
└─────────────────────────────────────────┘
             ↓ 从 Marketplace 安装
┌─────────────────────────────────────────┐
│          Plugin Marketplace              │  ← Git 仓库 / HTTP 服务
│   (index.json → plugin 元数据列表)        │
└─────────────────────────────────────────┘
```

### 1.3 目录规范

> **路径不写死**：所有路径由 `FoxAgentSdkConfig` 驱动，不硬编码 `~/.fox-code`。
> 应用侧只需设置 `storage_dir` 一次（全局数据根目录），SDK 自动推导所有子目录。
> 工作目录下的项目配置使用 `.claude/`（Claude Code 兼容），无需额外配置。

```
{storage_dir}/                            ← FoxAgentSdkConfig.storage_dir (全局数据)
├── settings.json                         ← 全局设置
├── skills/                               ← 全局 skills
│   ├── my-skill.md
│   └── domain/
│       └── sub-skill.md
├── hooks/                                ← 全局 hooks
│   ├── on-tool-use.sh
│   └── on-pre-compact.py
├── plugins/                              ← 已安装的插件
│   └── marketplaces/                     ← marketplace 索引
│       └── official.json
└── AGENTS.md                             ← 全局域指令

<working_dir>/.claude/                    ← Claude Code 兼容的项目级目录
├── settings.local.json
├── skills/
└── hooks/
```

---

## 2. Skills 完整实现

### 2.1 增强的 Skill 数据结构

```rust
/// Claude Code 兼容的 Skill 定义。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    // ── 基础元数据 ──
    pub name: String,                    // 唯一名称
    pub description: String,             // 描述

    // ── Claude Code Frontmatter 字段 ──
    pub version: Option<String>,         // 版本号
    pub model: Option<String>,           // 指定模型 (如 claude-sonnet-4-20250514)
    pub allowed_tools: Vec<String>,      // 预授权工具列表
    pub args: Vec<SkillArg>,             // 参数定义
    pub disable_model_invocation: bool,  // 是否允许模型自动调用

    // ── 内容 ──
    pub prompt: String,                  // 技能指令正文
    pub base_directory: Option<PathBuf>, // 技能所在目录 (用于相对路径引用)

    // ── 来源标记 ──
    pub source: SkillSource,             // 加载来源 (project / global / plugin)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillArg {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SkillSource {
    Project,        // <working_dir>/.claude/skills/
    Global,         // {storage_dir}/skills/
    Additional(PathBuf), // 额外的自定义目录
    Plugin(String), // 来自插件 (值为 plugin name)
}
```

### 2.2 增强的 SkillParser

```
Parse pipeline:
  YAML frontmatter → 提取元数据 → 展开模板变量 → 收集 baseDirectory 引用文件

Template variables:
  {{SKILL_DIR}}     → base_directory 的绝对路径
  {{WORKING_DIR}}   → 当前工作目录
  {{ARGS.name}}     → 参数值 (替换为实际参数)

加载路径优先级 (从高到低):
  1. <workdir>/.claude/skills/**
  2. {storage_dir}/skills/**
  3. 额外的自定义目录 (`SkillsConfig.additional_directories`)
  4. 插件自带 skills

递归加载策略:
  - 默认递归扫描 skills/ 下的所有子目录
  - 支持 .skillrc / .skillignore 过滤
  - 监控文件变更 (fs watcher) 实现热加载
```

### 2.3 SkillRegistry 增强

```rust
pub struct SkillRegistry {
    skills: HashMap<String, Skill>,
    source_index: HashMap<SkillSource, Vec<String>>,
    file_watchers: Vec<FsWatcher>,
}

impl SkillRegistry {
    // 新增
    pub fn load_from_global_dir(&mut self, dir: &Path) -> Result<usize>;
    pub fn load_from_config(&mut self, config: &SkillsConfig) -> Result<usize>;
    pub fn unload_source(&mut self, source: &SkillSource);
    pub fn watch_and_reload(&mut self) -> JoinHandle<()>;
    pub fn resolve_args(&self, name: &str, args: &HashMap<String, String>) -> Option<String>;
}
```

### 2.4 SkillsConfig (放入 FoxAgentSdkConfig)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillsConfig {
    /// 是否启用 skills 系统
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// 额外的 skills 目录 (绝对路径)
    #[serde(default)]
    pub additional_directories: Vec<PathBuf>,

    /// 是否加载全局 skills ({storage_dir}/skills/)
    #[serde(default = "default_true")]
    pub load_global: bool,

    /// skills 解析策略: auto (文件变更自动重载) 或 manual
    #[serde(default)]
    pub reload_strategy: ReloadStrategy,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub enum ReloadStrategy {
    #[default]
    Auto,    // fs watcher 监听变更
    Manual,  // 仅在 build() 时加载一次
}
```

---

## 3. Hooks 实现

### 3.1 Hook 概念

Hook 是在 Agent 生命周期的特定事件上执行的脚本/命令。Claude Code 定义了一套完整的 hook 体系。

### 3.2 支持的 Hook 事件

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum HookEvent {
    // ── 会话事件 ──
    SessionStart,      // 会话开始
    SessionEnd,        // 会话结束

    // ── User Prompt Submit ──
    UserPromptSubmit,  // 用户输入提交后 (可修改 prompt)

    // ── Pre-tool-use ──
    PreToolUse,        // tool 执行前 (可修改参数/阻断)

    // ── Post-tool-use ──
    PostToolUse,       // tool 执行后 (可修改输出)

    // ── Notification (单向, 不改变流程) ──
    Notification,      // 通用通知

    // ── Agent 停止 ──
    Stop,              // Agent 停止时 (如错误/预算耗尽)

    // ── 子 Agent 停止 ──
    SubagentStop,      // 子 Agent 完成

    // ── 上下文压缩 ──
    PreCompact,        // 压缩前 (可注入额外上下文)

    // ── 权限审批 ──
    PermissionPrompt,  // 权限提示时 (auto-approve)

    // ── 文件写入前后 ──
    PreFileWrite,      // 文件写入前
    PostFileWrite,     // 文件写入后
}
```

### 3.3 Hook 定义格式

Hook 文件遵循 Claude Code 格式，支持两种类型：

#### 3.3.1 脚本型 Hook (Shell/Python/...)

```jsonc
// .claude/hooks/on-pre-tool-use.json
{
  "hooks": [
    {
      "event": "PreToolUse",
      "command": "python3",
      "args": ["${FOX_CODE_HOOK_DIR}/block-dangerous.py"],
      "matcher": "bash"        // 可选: 仅匹配特定 tool
    }
  ]
}
```

#### 3.3.2 Prompt 型 Hook (LLM)

```jsonc
{
  "hooks": [
    {
      "event": "PreToolUse",
      "prompt": "Review the tool call below. If it would delete important files, respond `block: <reason>`. Otherwise, respond `allow`. $ARGUMENTS"
    }
  ]
}
```

### 3.4 Hook 输入/输出协议

所有 hook 通过 stdin 接收 JSON，通过 stdout 返回 JSON：

**Input (stdin)**:
```jsonc
{
  "session_id": "abc123",
  "event": "PreToolUse",
  "tool_name": "bash",
  "tool_input": { "command": "rm -rf /" },
  "working_dir": "/home/user/project",
  "hook_event_name": "PreToolUse"
}
```

**Output (stdout)**:
```jsonc
// 允许:
{ "continue": true }

// 允许 + 修改:
{ "continue": true, "modified_input": { "command": "rm -rf /tmp/cache" } }

// 阻断:
{ "continue": false, "reason": "This would delete all files" }

// 通知 (不阻塞):
{ "continue": true, "systemMessage": "Formatted all changed files" }
```

### 3.5 HookManager

```rust
pub struct HookManager {
    hooks: HashMap<HookEvent, Vec<HookDefinition>>,
    timeout: Duration,           // hook 执行超时
    max_concurrent: usize,       // 同一事件最大并发 hook 数
}

impl HookManager {
    /// 加载所有 hook 定义 (全局 + 项目)
    pub fn load_all(&mut self, config: &HooksConfig) -> Result<usize>;

    /// 执行指定事件的所有 hook
    pub async fn execute(&self, event: HookEvent, ctx: HookContext)
        -> Result<HookResult>;

    /// 执行 prompt 型 hook (调用 LLM)
    pub async fn execute_prompt_hook(&self, hook: &HookDefinition, ctx: &HookContext)
        -> Result<HookResult>;
}
```

### 3.6 HooksConfig (放入 FoxAgentSdkConfig)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HooksConfig {
    /// 是否启用 hooks
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// hooks 脚本超时 (秒)
    #[serde(default = "default_hook_timeout")]
    pub timeout_secs: u64,          // default: 30

    /// 单个事件最大并发 hook 数
    #[serde(default = "default_hook_max_concurrent")]
    pub max_concurrent: usize,      // default: 5

    /// 额外的 hooks 目录
    #[serde(default)]
    pub additional_directories: Vec<PathBuf>,

    /// 是否加载全局 hooks ({storage_dir}/hooks/)
    #[serde(default = "default_true")]
    pub load_global: bool,
}
```

---

## 4. Plugin 系统实现

### 4.1 Plugin 概念

Plugin 是 Skills + Hooks + 配置 的打包分发单元。Plugin 通过 Marketplace 索引发现和安装。

### 4.2 Plugin 目录结构

```
my-plugin/                      ← 本地 / Git 仓库根目录
├── plugin.json                 ← Plugin 清单
├── skills/
│   └── my-feature.md
├── hooks/
│   └── post-tool-use.sh
├── AGENTS.md                   ← plugin 级域指令
└── README.md
```

### 4.3 plugin.json 格式

```jsonc
{
  "name": "my-plugin",
  "version": "1.0.0",
  "description": "A useful plugin",
  "author": "author-name",
  "repository": "https://github.com/user/my-plugin",
  "license": "MIT",
  "entry": {
    "skills": ["skills/"],       // skill 目录路径 (相对于 plugin root)
    "hooks": ["hooks/"],         // hook 目录路径
    "agents_md": "AGENTS.md"     // 域指令文件
  },
  "dependencies": {
    "other-plugin": "^1.0"
  },
  "min_sdk_version": "0.1.0"
}
```

### 4.4 Marketplace 格式

```jsonc
// marketplace/index.json
{
  "name": "fox-agent-marketplace",
  "version": "1.0.0",
  "description": "Official Fox Agent Plugin Marketplace",
  "plugins": [
    {
      "name": "code-review",
      "version": "1.2.0",
      "description": "Automated code review with best practices",
      "repository": "https://github.com/user/code-review-plugin",
      "source": "github",
      "tags": ["code", "review", "quality"]
    }
  ]
}
```

### 4.5 PluginManager

```rust
pub struct PluginManager {
    installed: HashMap<String, InstalledPlugin>,
    marketplaces: Vec<MarketplaceConfig>,
    plugin_dir: PathBuf,        // {storage_dir}/plugins/
}

impl PluginManager {
    // ── Marketplace ──
    pub fn add_marketplace(&mut self, config: MarketplaceConfig);
    pub async fn refresh_marketplace(&self, name: &str) -> Result<MarketplaceIndex>;
    pub async fn search(&self, query: &str) -> Result<Vec<PluginEntry>>;

    // ── Install / Remove ──
    pub async fn install(&mut self, entry: &PluginEntry) -> Result<InstalledPlugin>;
    pub async fn install_from_path(&mut self, path: &Path) -> Result<InstalledPlugin>;
    pub async fn remove(&mut self, name: &str) -> Result<()>;
    pub async fn update(&mut self, name: &str) -> Result<InstalledPlugin>;

    // ── Lifecycle ──
    pub fn active_skills(&self) -> Vec<Skill>;
    pub fn active_hooks(&self) -> Vec<HookDefinition>;
    pub fn active_agents_md(&self) -> Vec<PathBuf>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplaceConfig {
    pub name: String,
    pub url: String,              // Git URL or HTTP URL to index.json
    pub source: MarketplaceSource,
    /// Auto-update interval in hours (0 = disabled)
    pub auto_update_hours: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MarketplaceSource {
    GitHub { owner: String, repo: String, branch: Option<String> },
    Git { url: String, branch: Option<String> },
    Http { url: String },
    Local { path: PathBuf },
}
```

### 4.6 PluginsConfig (放入 FoxAgentSdkConfig)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginsConfig {
    /// 是否启用插件系统
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// 预配置的 marketplaces
    #[serde(default)]
    pub marketplaces: Vec<MarketplaceConfig>,

    /// 自动更新检查间隔 (小时, 0 = 禁用)
    #[serde(default)]
    pub auto_update_hours: u64,

    /// 启动时安装的插件列表 (名称列表)
    #[serde(default)]
    pub preinstall: Vec<String>,
}
```

---

## 5. Agent Loop 集成

### 5.1 Hook 在 Agent Loop 中的触点

```
┌──────────────────────────────────────────────────┐
│                   Agent Loop                      │
│                                                   │
│  ┌──────────────┐  ┌──────────────┐              │
│  │ SessionStart │  │ UserPrompt   │              │
│  │   hook       │  │ Submit hook  │              │
│  └──────────────┘  └──────┬───────┘              │
│                           │                       │
│  ┌───────────────────────▼────────────────────┐  │
│  │              Model Inference                │  │
│  │  ┌──────────────────────────────────────┐  │  │
│  │  │  Tool call detected                  │  │  │
│  │  │  ┌─────────────────────────────┐     │  │  │
│  │  │  │  PreToolUse hook            │     │  │  │
│  │  │  │    → 可修改参数/阻断         │     │  │  │
│  │  │  ├─────────────────────────────┤     │  │  │
│  │  │  │  PermissionPrompt hook      │     │  │  │
│  │  │  │    → 可自动批准              │     │  │  │
│  │  │  ├─────────────────────────────┤     │  │  │
│  │  │  │  执行 tool                  │     │  │  │
│  │  │  ├─────────────────────────────┤     │  │  │
│  │  │  │  PostToolUse hook           │     │  │  │
│  │  │  │    → 可修改输出/格式化      │     │  │  │
│  │  │  └─────────────────────────────┘     │  │  │
│  │  └──────────────────────────────────────┘  │  │
│  └────────────────────────────────────────────┘  │
│                                                   │
│  ┌──────────────┐  ┌──────────────┐              │
│  │  PreCompact  │  │  Stop hook   │              │
│  │    hook      │  │              │              │
│  └──────────────┘  └──────────────┘              │
└──────────────────────────────────────────────────┘
```

### 5.2 Hook 执行器嵌入 Harness

```rust
// harness.rs
pub struct Harness {
    // ... 现有字段 ...
    pub hook_manager: HookManager,
    pub plugin_manager: Option<Arc<PluginManager>>,
}

impl Harness {
    /// Agent loop 内部调用, 在 tool 执行前触发 hook 链
    pub async fn run_pre_tool_hooks(&self, tool_name: &str, input: &Value)
        -> Result<HookDecision> { ... }

    /// Agent loop 内部调用, 在 tool 执行后触发
    pub async fn run_post_tool_hooks(&self, tool_name: &str, output: &ToolOutput)
        -> Result<HookDecision> { ... }

    /// Build system prompt with all active skill prompts + plugin AGENTS.md
    pub async fn build_enhanced_system_prompt(&self, ...) -> SplitPrompt { ... }
}
```

---

## 6. 配置扩展

### 6.1 FoxAgentSdkConfig 新增字段

```rust
pub struct FoxAgentSdkConfig {
    // ... 现有字段 ...
    pub skills: SkillsConfig,    // 新增
    pub hooks: HooksConfig,      // 新增
    pub plugins: PluginsConfig,  // 新增
}
```

### 6.2 agent.toml 示例扩展

```toml
# ── Skills ──────────────────────────────────────────────────
[skills]
enabled = true
load_global = true
# additional_directories = ["/path/to/custom/skills"]
reload_strategy = "Auto"      # "Auto" | "Manual"

# ── Hooks ───────────────────────────────────────────────────
[hooks]
enabled = true
timeout_secs = 30
max_concurrent = 5
load_global = true

# ── Plugins ─────────────────────────────────────────────────
[plugins]
enabled = true
auto_update_hours = 24
preinstall = ["code-review"]   # 启动时自动安装

[[plugins.marketplaces]]
name = "official"
source = "GitHub"
owner = "your-org"
repo = "fox-agent-marketplace"
auto_update_hours = 12
```

---

## 7. 开发计划

### Phase 1: Skills 增强 (预计 3 天)

| 任务 | 说明 |
|------|------|
| T1 | 增强 `Skill` 结构体 (arg/model/source 等) |
| T2 | 增强 `SkillParser` (模板变量展开、递归扫描、嵌套目录) |
| T3 | `SkillRegistry` 全局加载 + 多源管理 |
| T4 | `SkillsConfig` 放入 `FoxAgentSdkConfig` |
| T5 | Skill 热加载 (fs watcher) |
| T6 | 单元测试 |

### Phase 2: Hooks 实现 (预计 4 天)

| 任务 | 说明 |
|------|------|
| T7 | `HookManager` + `HookDefinition` + `HookEvent` |
| T8 | 脚本型 hook 执行器 (Shell/Python) |
| T9 | Prompt 型 hook 执行器 |
| T10 | Hook 在 Agent Loop 中的触点集成 |
| T11 | `HooksConfig` 放入 `FoxAgentSdkConfig` |
| T12 | 单元测试 + 集成测试 |

### Phase 3: Plugin 系统 (预计 5 天)

| 任务 | 说明 |
|------|------|
| T13 | `PluginManager` + `plugin.json` 解析 |
| T14 | Marketplace 索引下载与缓存 |
| T15 | Plugin 安装/更新/卸载 (Git clone / local copy / HTTP download) |
| T16 | Plugin 依赖解析 |
| T17 | Plugin 自动更新调度 |
| T18 | `PluginsConfig` 放入 `FoxAgentSdkConfig` |
| T19 | 单元测试 + 集成测试 |

### Phase 4: 集成与文档 (预计 2 天)

| 任务 | 说明 |
|------|------|
| T20 | Builder 集成 (自动加载 plugin skills/hooks) |
| T21 | 更新 agent.toml.example |
| T22 | 更新 PRD |
| T23 | 端到端测试 |

**总计: 约 14 个工作日**

---

## 8. 验收标准

### AC1: Skills 完全兼容
- Given: `.claude/skills/` 下有合法 skill 文件
- When: Agent 启动
- Then: Skill 被自动加载到 `SkillRegistry`，可通过 `/skillname` 命令激活

### AC2: 多源 Skill 加载
- Given: 项目 skills + 全局 skills + plugin skills
- When: Agent 启动
- Then: 全部加载，按优先级去重

### AC3: Hook 脚本执行
- Given: `{storage_dir}/hooks/` 或 `.claude/hooks/` 下有 `PreToolUse` hook
- When: Agent 调用工具
- Then: Hook 脚本被执行，结果影响工具调用 (阻断/修改/放行)

### AC4: Prompt Hook
- Given: 定义了 prompt 型 hook
- When: After hook 事件触发
- Then: SDK 调用 LLM 评估 prompt hook，根据结果决定行为

### AC5: Plugin 安装
- Given: Marketplace 中有可用 plugin
- When: `PluginManager.install("plugin-name")`
- Then: Plugin 被下载到 `{storage_dir}/plugins/`，其 skills/hooks 自动生效

### AC6: 零代码兼容
- Given: 已有 Claude Code 的 skills 目录和 hook 目录
- When: Fox Agent 应用加载该配置
- Then: Skills 和 Hooks 直接可用，无需修改文件
