# Fox Agent SDK 应用开发指南

面向基于 Fox Agent SDK 构建 AI Agent 应用的开发者的完整指南。涵盖从
Agent 生命周期管理到高级配置的全部内容。

---

## 目录

1. [Agent 构建](#1-agent-构建)
2. [Agent 执行](#2-agent-执行)
3. [会话管理](#3-会话管理)
4. [工具系统](#4-工具系统)
5. [权限与安全](#5-权限与安全)
6. [记忆系统](#6-记忆系统)
7. [规划系统](#7-规划系统)
8. [上下文压缩](#8-上下文压缩)
9. [运行治理](#9-运行治理)
10. [事件录制与回放](#10-事件录制与回放)
11. [MCP 集成](#11-mcp-集成)
12. [域自适应 — 让 Agent 适配任意领域](#12-域自适应--让-agent-适配任意领域)
13. [故障排查](#13-故障排查)

---

## 1. Agent 构建

### 1.1 最小示例

```rust
use fox_agent_sdk::{AgentBuilder, ProviderConfig, TurnOutcome};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("DEEPSEEK_API_KEY")?;
    let mut agent = AgentBuilder::new()
        .provider_config(ProviderConfig::deepseek(api_key))
        .model_id("deepseek-reasoner")
        .build()
        .await?;

    let outcome = agent.run_once("你是谁？").await?;
    println!("{:?}", outcome);
    Ok(())
}
```

### 1.2 Builder 配置选项

| 方法 | 默认值 | 说明 |
|------|--------|------|
| `.provider_config(config)` | 无（必须设置） | 选择 Provider（DeepSeek/OpenAI/Anthropic） |
| `.model_id(id)` | `"deepseek-reasoner"` | 模型标识符 |
| `.working_dir(path)` | `None` | 工具执行的工作目录 |
| `.with_default_tools()` | 不注册任何工具 | 注册所有内置工具 |
| `.with_tool(tool)` | - | 注册自定义工具 |
| `.with_system_prompt(text)` | 内置 `system.md` | 覆盖系统提示词 |
| `.with_safety_policy(config)` | `SafetyConfig::default()` | 权限策略 |
| `.with_session_store(store)` | `InMemorySessionStore` | 会话持久化后端 |
| `.with_planning_store(store)` | `InMemoryPlanningStore` | 规划持久化后端 |
| `.with_mcp_server(config)` | - | 接入 MCP 服务器 |
| `.with_global_agents_md_path(path)` | `~/.fox-agent/AGENTS.md` | 全局/领域级 AGENTS.md 路径 |
| `.build()` | - | 构建 Agent |

### 1.3 多 Provider 配置

```rust
// DeepSeek（最小配置）
let config = ProviderConfig::deepseek(api_key);

// OpenAI
let config = ProviderConfig::new(
    "openai",
    "https://api.openai.com/v1".to_string(),
    api_key,
);

// Anthropic
let config = ProviderConfig::new(
    "anthropic",
    "https://api.anthropic.com/v1".to_string(),
    api_key,
);

// 通过 builder 组装
let mut agent = AgentBuilder::new()
    .provider_config(config)
    .model_id("claude-sonnet-4-20250514")
    .build()
    .await?;
```

### 1.4 使用 MockProvider 测试

不调用真实 LLM，使用确定性脚本：

```rust
use std::sync::Arc;
use fox_agent_sdk::{MockProvider, StreamEvent, AgentBuilder};

let provider = Arc::new(MockProvider::new("mock"));

// 推送确定性输出
provider.push_script(vec![
    StreamEvent::TextDelta { text: "Hello!".into() },
    StreamEvent::MessageStop { stop_reason: None },
]);

let mut agent = AgentBuilder::new()
    .with_provider(provider.clone())
    .model_id("mock-1")
    .build()
    .await?;
```

### 1.5 运行时切换模型

```rust
let mut agent = AgentBuilder::new()
    .provider_config(ProviderConfig::deepseek(key))
    .model_id("deepseek-reasoner")
    .build()
    .await?;

// 运行时切换
agent.set_model("deepseek-v4-flash")?;
```

---

## 2. Agent 执行

### 2.1 三种运行模式

| 方法 | 返回 | 场景 |
|------|------|------|
| `run_once(msg)` | `Result<()>` | 纯副作用（忽略输出） |
| `run_once_capture(msg)` | `Result<TurnOutcome>` | 捕获最终结果 |
| `run_once_streaming(msg, tx)` | `Result<TurnOutcome>` | 通过 channel 获取实时事件 |

**示例：流式获取事件**

```rust
let (tx, mut rx) = tokio::sync::mpsc::channel::<AgentEvent>(64);
let outcome = agent.run_once_streaming("创建项目结构", &tx).await?;

loop {
    match rx.recv().await {
        Some(AgentEvent::TextDelta { text, .. }) => print!("{}", text),
        Some(AgentEvent::ToolCallStart { name, .. }) => println!("[tool] {}", name),
        Some(AgentEvent::Usage { input_tokens, output_tokens, .. }) => {
            // token 统计
        }
        Some(AgentEvent::TurnComplete { .. }) => break,
        Some(AgentEvent::Error { message, .. }) => eprintln!("error: {}", message),
        _ => {}
    }
}
```

### 2.2 AgentEvent 类型

| 事件 | 时机 | 关键字段 |
|------|------|---------|
| `TextDelta` | Provider 返回文本块 | `text`、`turn_id` |
| `ThinkingDelta` | 推理模型思考过程 | `text`、`turn_id` |
| `ToolCallStart` | 工具调用开始 | `call_id`、`name`、`input` |
| `ToolCallEnd` | 工具调用结束 | `call_id`、`name`、`output` |
| `Usage` | 每轮结束 | `input_tokens`、`output_tokens` |
| `Compacting` | 上下文压缩开始 | `trigger` |
| `Error` | 发生错误 | `message`、`fatal` |
| `TurnComplete` | 轮次完全结束 | `outcome` |

### 2.3 TurnOutcome 结果类型

```rust
pub enum TurnOutcome {
    Completed { text: String },
    RequiresUserDecision { request: PermissionRequest },
    CancelledByUser,
    BudgetExceeded { reason: String },
    Error { message: String },
    GracefulShutdown,
}
```

### 2.4 权限中断与恢复

LLM 调用工具触发权限检查 → 用户决策 → Agent 恢复执行：

```rust
let (tx, mut rx) = tokio::sync::mpsc::channel::<AgentEvent>(64);

// 第一轮：触发权限
let outcome = agent.run_once_streaming("删除日志文件", &tx).await?;
match outcome {
    TurnOutcome::RequiresUserDecision { request } => {
        println!("Agent 请求权限: {}", request.prompt);
        // 用户决策后恢复
        let decision = PermissionDecision::Allow;
        agent.record_permission_decision(request.id.clone(), decision.clone());
        let outcome2 = agent.resume_streaming(decision, &tx).await?;
    }
    _ => {}
}
```

---

## 3. 会话管理

### 3.1 SessionState（运行时模型）

`SessionState` 是领域层 Reducer 模型，通过 `apply(SessionEvent)` 驱动状态转移：

```rust
pub struct SessionState {
    pub id: String,
    pub parent_id: Option<String>,
    pub title: Option<String>,
    pub model: Option<String>,
    pub provider_key: Option<String>,
    pub status: SessionStatus,     // New / Active / Closed
    pub working_dir: Option<PathBuf>,
    pub messages: Vec<Message>,
    pub env_snapshots: Vec<EnvSnapshot>,
}
```

### 3.2 SessionSnapshot（持久化格式）

Agent 调用 `snapshot()` 时，将运行时状态（包括模型运行时、待审批权限、中断队列等）导出为完整快照：

```rust
let snapshot = agent.snapshot();
session_store.save_session(&snapshot)?;
```

### 3.3 SessionStore

两个内置实现：

- **InMemorySessionStore**：`HashMap` 存储，测试用
- **FileSessionStore**：按 `session_id` 分文件存储为 JSON，生产用

```rust
use fox_agent_sdk::FileSessionStore;

let store = Arc::new(FileSessionStore::new(PathBuf::from("./sessions")));
let mut agent = AgentBuilder::new()
    .provider_config(ProviderConfig::deepseek(key))
    .with_session_store(store)
    .build()
    .await?;
```

### 3.4 会话恢复

```rust
let mut agent = AgentBuilder::new()
    .provider_config(ProviderConfig::deepseek(key))
    .with_session_store(store.clone())
    .build()
    .await?;

let restored = Agent::load_from_store(
    model,
    harness,
    "session-1",
)?;

restored.run_once("继续刚才的工作").await?;
```

### 3.5 自动快照

`auto_snapshot: true`（默认开启）时，每次 `run_once` 调用自动持久化。

```rust
let config = FoxAgentSdkConfig {
    auto_snapshot: true,
    session_storage_dir: Some(PathBuf::from("./sessions")),
    ..Default::default()
};
```

---

## 4. 工具系统

### 4.1 Tool Trait

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> Value;       // JSON Schema
    async fn execute(
        &self,
        input: Value,
        ctx: ToolContext,
    ) -> Result<ToolOutput, ToolError>;
}
```

### 4.2 ToolOutput

```rust
pub struct ToolOutput {
    pub text: String,          // LLM 可读文本
    pub is_error: bool,        // 是否为错误
    pub json: Option<Value>,   // 结构化数据
}
```

### 4.3 ToolContext

每次工具调用注入完整的执行上下文：

```rust
pub struct ToolContext {
    pub session_id: String,
    pub message_id: String,
    pub tool_call_id: String,
    pub working_dir: Option<PathBuf>,
    pub execution_mode: ToolExecutionMode,  // Foreground / Background
    pub graceful_shutdown_requested: bool,
}
```

### 4.4 自定义工具示例

```rust
struct TimeTool;

#[async_trait]
impl Tool for TimeTool {
    fn name(&self) -> &str { "get_current_time" }
    fn description(&self) -> &str { "获取当前系统时间（ISO 8601 格式）" }
    fn parameters_schema(&self) -> Value {
        json!({"type":"object","properties":{},"additionalProperties":false})
    }

    async fn execute(
        &self,
        _input: Value,
        _ctx: ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let now = chrono::Utc::now().to_rfc3339();
        Ok(ToolOutput { text: now, is_error: false, json: None })
    }
}

let mut agent = AgentBuilder::new()
    .provider_config(ProviderConfig::deepseek(key))
    .with_tool(Arc::new(TimeTool))
    .build()
    .await?;
```

### 4.5 风险分级

每个工具注册时声明风险等级，权限系统据此做出决策：

| 风险等级 | 典型工具 | 默认行为 |
|---------|---------|---------|
| `Low` | `read`、`grep`、`glob` | Allow |
| `Medium` | `edit`、`todo` | Confirm |
| `High` | `write`、`bash` | Confirm |
| `Critical` | `websearch`、`webfetch` | Confirm |

### 4.6 工具执行保障

- **超时保护**：默认 60 秒（通过 `GovernanceGuard` 配置）
- **并发限制**：`GovernanceGuard` 中的 `Semaphore` 控制
- **优雅关闭**：`graceful_shutdown_requested` 在调用前检查

---

## 5. 权限与安全

### 5.1 SafetyConfig

```rust
pub struct SafetyConfig {
    pub default_policy: DefaultSafetyPolicy,    // Allow / Deny / Confirm
    pub tool_denylist: Option<Vec<String>>,     // 黑名单工具
    pub tool_allowlist: Option<Vec<String>>,    // 白名单工具
    pub custom_hook: Option<Arc<dyn PermissionHook>>,
    pub approval_timeout_secs: Option<u64>,     // 审批超时（默认 120 秒）
}
```

### 5.2 策略评估流程

```
自定义 hook (优先) → Denylist 检查 → Allowlist 检查 → Default Policy
```

### 5.3 ApprovalManager

三层缓存设计，减少重复审批：

| 层级 | 生命周期 | 场景 |
|------|---------|------|
| `ThisTurn` | 轮次结束清空 | 同轮内相同工具免重复审批 |
| `ThisSession` | 会话结束清空 | 用户确认一次，整个会话生效 |
| `ThisWorkspace` | 跨会话持久 | 信任的工具永久生效 |

```rust
let safety = SafetyConfig {
    default_policy: DefaultSafetyPolicy::Confirm,
    ..Default::default()
};

let approval = ApprovalManager::new("session-1", safety);

// 会话级别缓存 read 工具的审批
approval.cache_decision(
    "read",
    &PermissionResult::Allow,
    ApprovalScope::ThisSession,
).await;

// 检查缓存
match approval.check_cache("read").await {
    Some(result) => { /* 使用缓存结果 */ }
    None => { /* 需要用户决策 */ }
}
```

### 5.4 审计与溯源

每个 `PermissionRequest` 携带 `policy_source` 字段，记录决策逻辑来源（`"denylist"`、`"allowlist"`、`"default:confirm"` 等）。完整决策链可导出到 JSONL，用于安全审计和回溯分析，确保不可抵赖性。

```rust
approval.record_audit(&request, &result, turn_id).await;
approval.export_audit(&audit_path).await?;
```

---

## 6. 记忆系统

### 6.1 架构概览

Fox Agent SDK 内建记忆系统，支持跨会话学习和召回：

```
MemoryManager → 生命周期管理
MemoryGraph   → 图结构存储
Extractor     → LLM 驱动提取
EmbeddingProvider → 语义嵌入
ANN 索引      → 快速语义搜索
```

### 6.2 配置示例

```rust
let mem_config = MemoryConfig {
    enabled: true,
    storage_dir: Some(PathBuf::from("./memory")),
    extraction_interval_turns: 3,    // 每 3 轮触发提取
    max_recall_entries: 10,         // 召回条数上限
    embedding_model: "mistral-text-embed".to_string(),
    ..Default::default()
};
```

### 6.3 MemoryEntry 结构

```rust
pub struct MemoryEntry {
    pub id: String,
    pub session_id: Option<String>,
    pub content: String,
    pub category: MemoryCategory,       // Fact / Preference / Todo / QAPair
    pub scope: MemoryScope,            // Session / Global
    pub trust_level: TrustLevel,       // Low / Medium / High
    pub created_at: u64,
    pub tags: Vec<String>,
    pub embeddings: Option<Vec<f32>>,
}
```

### 6.4 召回模式

- **Relevant**：LLM 相关性校验 + 语义检索（精确但慢）
- **Recent**：最近记忆（快速但可能不精确）
- **Hybrid**：混合策略（平衡精度和速度）

### 6.5 记忆工作流

```
1. 用户: "我更喜欢中文回复"
2. MemoryExtractor 从消息中提取 → MemoryEntry { category: Preference, ... }
3. 下次会话: 当前消息触发 recall
4. 相关记忆注入 → system prompt dynamic 部分
5. Agent: "好的，我会用中文回复"
```

---

## 7. 规划系统

### 7.1 层次结构

| 结构 | 粒度 | 作用域 | 持久化键 |
|------|------|-------|---------|
| `GoalItem` | 目标 | Session / Global | `{session_id}:goal` / `:global_goals` |
| `VersionedPlan` | 计划 | Session | `{session_id}:plan` |
| `TodoItem` | 任务 | Session | `{session_id}:todo` |

### 7.2 PlanItem 结构

```rust
pub struct PlanItem {
    pub id: String,
    pub content: String,
    pub status: PlanStatus,     // Pending / InProgress / Done / Blocked / Cancelled
    pub depends_on: Vec<String>, // 依赖的任务 ID 列表
    pub assigned_to: Option<String>,
    pub priority: Priority,     // Low / Medium / High
    pub tags: Vec<String>,
    pub version: u64,
}
```

### 7.3 PlanningStore

与 `SessionStore` 类似的持久化接口：

```rust
pub trait PlanningStore: Send + Sync {
    fn save_todos(&self, session_id: &str, todos: &[TodoItem]) -> Result<(), String>;
    fn load_todos(&self, session_id: &str) -> Result<Vec<TodoItem>, String>;
    fn save_plan(&self, session_id: &str, plan: &VersionedPlan) -> Result<(), String>;
    fn load_plan(&self, session_id: &str) -> Result<VersionedPlan, String>;
    fn save_goals(&self, session_id: &str, goals: &[GoalItem], scope: GoalScope) -> Result<(), String>;
    fn load_goals(&self, session_id: &str) -> Result<Vec<GoalItem>, String>;
}
```

### 7.4 内置工具

Agent 通过以下工具管理规划：

- **goal**：设置和跟踪目标
- **plan**：制定和更新计划
- **todo**：管理任务项

规划状态在每轮构建 prompt 时注入 `dynamic_part`。

---

## 8. 上下文压缩

当会话消息超出 token 预算或轮次上限时，`CompactionManager` 自动执行压缩。

### 8.1 触发策略

| 策略 | 条件 | 说明 |
|------|------|------|
| `TokenBudget` | 总字符数超过阈值 | 最常见的触发方式 |
| `TurnCount` | 消息数超过 `max_turns_before_compaction` | 轮次控制 |
| `ContextLimitApproaching` | 上下文接近但未超预算 | 预防性压缩 |
| `Provider` | Provider 原生通知 | 如 Anthropic |
| `Manual` | 手动触发 | API 调用 |

### 8.2 压缩行为

1. 保留最近 `preserve_recent_messages` 条消息
2. 将更早的消息汇总为 `[Conversation summary]` 注记
3. 注入到 system 消息中
4. 记录 `CompactionEvent`（触发原因、删除/保留条数等）

### 8.3 配置

```rust
let compaction = CompactionConfig {
    token_budget: Some(60_000),               // 字符数阈值
    max_turns_before_compaction: 15,
    context_limit_threshold: 0.85,            // 85% 时触发
    preserve_recent_messages: 10,
    max_compaction_count: 5,
    ..Default::default()
};
```

---

## 9. 运行治理

### 9.1 BudgetConfig

```rust
pub struct BudgetConfig {
    pub token_budget: Option<u64>,         // Token 预算上限
    pub cost_budget_cents: Option<u64>,    // 费用预算（美分）
    pub max_consecutive_errors: u64,       // 连续错误上限
    pub provider_timeout_secs: u64,        // Provider 调用超时
    pub tool_timeout_secs: u64,            // 工具调用超时
    pub tool_concurrency_limit: u64,       // 工具并发数
}
```

### 9.2 Metrics 钩子

```rust
guard.add_metrics_hook(|metrics| {
    println!(
        "turns={} tokens_in={} tokens_out={} cost={}c err_rate={:.1}%",
        metrics.turns_completed,
        metrics.total_input_tokens,
        metrics.total_output_tokens,
        metrics.estimated_cost_cents,
        metrics.tool_error_rate() * 100.0,
    );
}).await;
```

### 9.3 自动停止条件

- 超预算：`TurnOutcome::BudgetExceeded`
- 连续错误过多：自动终止
- 正常完成：`TurnOutcome::Completed`

---

## 10. 事件录制与回放

### 10.1 EventRecorder

录制整轮事件到 JSONL：

```rust
let recorder = EventRecorder::new("session-1", 1);

// 在 agent 执行过程中，每次 agent event 到达时：
// recorder.record(&event);

recorder.export_to_file(PathBuf::from("events.jsonl")).await?;
```

### 10.2 ReplayRunner

黄金文件回放用于 CI 回归测试：

```rust
let events = EventRecorder::load_from_file(&PathBuf::from("golden.jsonl"))?;
let runner = ReplayRunner::new(events);

runner.run_with_assertions(vec![
    Check::TextContains("42"),
    Check::ToolCallPresent("calculator"),
    Check::NoErrors,
])?;
```

### 10.3 自动脱敏

导出时自动检测并脱敏 API key / JWT / PEM 私钥：

```
sk-abc123...  →  [API_KEY]
eyJhbGci...   →  [JWT]
```

---

## 11. MCP 集成

### 11.1 接入 MCP 服务器

```rust
use fox_agent_sdk::McpServerConfig;

let mcp_conf = McpServerConfig {
    name: "filesystem".to_string(),
    command: "npx".to_string(),
    args: vec!["-y".into(), "@modelcontextprotocol/server-filesystem".into(), "/tmp".into()],
    env: None,
    transport: McpTransport::Stdio,
};

let mut agent = AgentBuilder::new()
    .provider_config(ProviderConfig::deepseek(key))
    .with_mcp_server(mcp_conf)
    .build()
    .await?;
```

### 11.2 传输方式

- `McpTransport::Stdio`：通过子进程 stdin/stdout 通信
- `McpTransport::Sse { url }`：通过 HTTP SSE 连接

### 11.3 动态工具发现

`McpClient` 在启动时完成三步初始化：
1. `tools/list` → 发现所有可用工具
2. 为每个工具创建 `McpToolAdapter`（实现 `Tool` trait）
3. 将适配器注册到 harness 工具体系中

所有已加入权限检查、审计、超时控制等 SDK 级保障。

---

## 12. 域自适应 — 让 Agent 适配任意领域

Fox Agent SDK 是**通用 Agent 运行时**，同一个 Agent 二进制可以在 coding、量化交易、数据分析、运维、文档写作等截然不同的领域工作。域自适应通过三层递进机制实现。

### 12.1 工作原理

```
┌───────────────────────────────────────────────────────┐
│              域自适应分层                                │
│                                                         │
│ 第一层: AGENTS.md    (领域指引)                          │
│   项目/AGENTS.md        项目级约定                       │
│   ~/.fox-agent/AGENTS.md   个人全局偏好                  │
│   → 注入 static_part，支持前缀缓存                       │
│                                                         │
│ 第二层: Prompt Overlay  (覆盖指令)                       │
│   项目/.fox/prompt-overlay.md                           │
│   ~/.fox-agent/prompt-overlay.md                        │
│   → 以最高优先级追加到 static_part                       │
│                                                         │
│ 第三层: Planning Guidance  (system.md 内置)               │
│   system.md §Planning + §Domain Adaptation              │
│   → 告知 Agent 读取 AGENTS.md 并自适应                   │
└───────────────────────────────────────────────────────┘
```

### 12.2 分步演示：从 Coding Agent 到量化交易 Agent

**从编程项目开始**（默认情况）：

```
project/
├── AGENTS.md          ← "使用 Rust，遵循惯用模式。"
├── Cargo.toml
└── src/
```

Agent 读取 `AGENTS.md`，以 Rust 开发者身份工作。无需任何配置。

**切换到量化交易** — 只需替换 `AGENTS.md`：

```markdown
# AGENTS.md （量化交易项目）

你是一名量化交易策略分析师。
- 数据源：./data/ 目录下的 CSV 文件（OHLCV 日线数据）
- 回测引擎：使用 `backtrader` Python 库
- 绩效指标：夏普比率、最大回撤、胜率
- 绝对禁止在未得到用户明确确认的情况下执行实盘交易
- 将策略报告输出到 ./reports/ 目录，格式为 markdown
- 参考：策略参数在 ./config/strategy.yaml 中定义
```

```rust
let agent = AgentBuilder::new()
    .provider_config(ProviderConfig::deepseek(api_key))
    .working_dir("./trading-project")  // ← 指向交易项目
    .with_default_tools()
    .build()
    .await?;
```

就这样。同样的 `AgentBuilder` 代码，同样的工具——领域行为完全由 `AGENTS.md` 驱动。

### 12.3 最佳实践

| 实践 | 原因 |
|------|------|
| **AGENTS.md 聚焦领域规则** | 不要重复工具使用说明；system.md 已包含。聚焦领域规则、数据源、术语和约束。 |
| **用 Prompt Overlay 覆盖 system.md** | 如果 system.md 说"修改后自动提交"但你的领域不使用 git，创建 `.fox/prompt-overlay.md` 覆盖。 |
| **一个项目一个领域** | 不要让一个 `AGENTS.md` 涵盖多个领域。创建独立项目目录。 |
| **全局 AGENTS.md 存放个人偏好** | 将语言偏好、代码风格、工具链选择放在 `~/.fox-agent/AGENTS.md` 中，对所有项目生效。 |
| **规划层级与领域无关** | `goal`/`plan`/`todo` 在"发布功能"或"寻找 alpha 信号"两种场景下的工作方式完全一致。 |

### 12.4 领域示例

| 领域 | AGENTS.md 关键内容 | 典型技能 |
|------|-------------------|---------|
| **编程** | 语言、框架、测试规范、lint 规则 | `code-review`、`refactoring`、`api-design` |
| **量化交易** | 数据源、回测引擎、风险限制、执行规则 | `portfolio-optimization`、`market-microstructure` |
| **数据分析** | 工具（pandas、matplotlib）、数据位置、报告格式、引用规则 | `sql-analyst`、`statistical-modeling` |
| **SRE / 运维** | 集群端点、只读限制、告警阈值、runbook 位置 | `incident-response`、`capacity-planning` |
| **文档写作** | 风格指南、目标受众、输出格式、审核清单 | `api-docs`、`release-notes` |
| **科研** | 文献来源、实验方法、笔记规范 | `literature-review`、`experiment-design` |

### 12.5 Agent 如何读取 AGENTS.md

系统提示词会明确告知 Agent：

```
## 领域自适应

领域（编程、交易、科研、运维等）由你可用的工具、技能和项目上下文定义，
而非由你的身份定义。阅读项目指引（AGENTS.md、prompt overlay）理解
当前领域的约定。据此调整行为。
```

此内容属于 `static_part`，由 Provider 跨轮次缓存——Agent 在会话启动时读取一次，在整个会话期间持续持有领域知识。

### 12.6 Code Agent 应用的全局 AGENTS.md

当你基于 Fox Agent SDK 开发特定领域的应用（如 code agent）时，可以通过 `with_global_agents_md_path()` 指定应用自身的全局指引文件：

```rust
let mut agent = AgentBuilder::new()
    .provider_config(ProviderConfig::deepseek(key))
    .working_dir(user_project)                         // 用户项目根目录
    // 指定 code agent 应用自身的全局 AGENTS.md
    .with_global_agents_md_path(
        dirs::config_dir()
            .unwrap()
            .join("my-code-agent/AGENTS.md")
    )
    .with_default_tools()
    .build()
    .await?;
```

这样形成三层 AGENTS.md 体系：

```
[应用领域指引]  ← with_global_agents_md_path 指定的路径
[项目上下文]     ← <working_dir>/AGENTS.md 自动加载
[个人全局偏好]   ← 如果 global_agents_md_path 为 None，回退到 ~/.fox-agent/AGENTS.md
```

---

## 13. 故障排查

| 问题 | 可能原因 | 解决方案 |
|------|---------|---------|
| Agent 返回空文本 | 模型未连接 | 检查 API key 和 base URL |
| `BudgetExceeded` 错误 | Token/cost 限额触发 | 提高 `token_budget` 或 `cost_budget_cents` |
| 权限请求无限循环 | 默认策略为 `Confirm` 且无缓存 | 使用 `ApprovalManager` 缓存 |
| 工具调用挂起 | 工具超时 | 设置 `budget.tool_timeout_secs` |
| 记忆不持久化 | `storage_dir` 未设置 | 设置 `memory.storage_dir` |
| 编译错误：找不到 `enum` | 工具名未注册 | 调用 `.with_default_tools()` 或 `.with_tool(...)` |
