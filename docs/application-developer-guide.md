# Fox Agent SDK 应用开发指南

面向基于 Fox Agent SDK 构建 AI Agent 应用的开发者的完整指南。涵盖从
Agent 生命周期管理到高级配置的全部内容。

---

## 目录

1. [Agent 构建](#1-agent-构建)
2. [Agent 执行](#2-agent-执行)
3. [会话管理](#3-会话管理)
4. [工具系统](#4-工具系统)（含工具结果路由）
5. [权限与安全](#5-权限与安全)
6. [记忆系统](#6-记忆系统)
7. [规划系统](#7-规划系统)
8. [上下文压缩](#8-上下文压缩)
9. [运行治理](#9-运行治理)
10. [事件录制与回放](#10-事件录制与回放)
11. [MCP 集成](#11-mcp-集成)
12. [域自适应 — 让 Agent 适配任意领域](#12-域自适应--让-agent-适配任意领域)
13. [Claude Code 兼容：Skills / Hooks / Plugins](#13-claude-code-兼容skills--hooks--plugins)
14. [进度事件](#14-进度事件)
15. [故障排查](#15-故障排查)

---

## 1. Agent 构建

### 1.1 最小示例

```rust
// 最小示例：从 agent.toml 加载全部配置
use fox_agent_sdk::{AgentBuilder, AgentEvent, StreamEvent};
use std::path::PathBuf;

#[tokio::main]
async fn main() {
    // 加载项目配置（agent.toml + AGENTS.md）
    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cfg = FoxAgentSdkConfig::load_from_file(project_root.join("agent.toml"))
        .unwrap_or_else(|_| FoxAgentSdkConfig::default());

    // 构建 Agent — 所有配置 (safety, MCP, skills, budget ...) 自动生效
    let mut agent = AgentBuilder::new()
        .working_dir(&project_root)
        .sdk_config(cfg)
        .with_global_agents_md_path(project_root.join("AGENTS.md"))
        // 只需要显式覆盖 Provider（未放在 agent.toml 中时）
        .provider_config(ProviderConfig::deepseek("sk-your-api-key"))
        .model_id("deepseek-v4-flash")
        .build()
        .await
        .expect("build agent");

    // 执行一次对话
    let outcome = agent
        .run_once_streaming("用中文回答：介绍一下 Rust 编程语言", event_tx)
        .await
        .unwrap();
    println!("Done: {:?}", outcome);
}
```

### 1.2 Builder 配置选项

| 方法 | 参数类型 | 说明 |
|------|----------|------|
| `new()` | — | 创建 Builder |
| `sdk_config(cfg)` | `FoxAgentSdkConfig` | 注入从 `agent.toml` / 代码构建的配置（safety、MCP、budget、memory …） |
| `provider_config(cfg)` | `ProviderConfig` | 使用代码配置 LLM Provider（不放在 agent.toml 时） |
| `with_provider(provider)` | `Arc<dyn Provider>` | 直接注入已构建的 Provider 实例（用于测试/Mock） |
| `model_id(id)` | `&str` | 指定模型 ID |
| `working_dir(path)` | `impl Into<PathBuf>` | 设定项目根目录（建议与 agent.toml 所在目录一致） |
| `with_global_agents_md_path(path)` | `impl Into<PathBuf>` | 指定 `AGENTS.md` 文件路径 |
| `with_default_tools()` | — | 注册所有内置工具 |
| `with_tool(tool)` | `Arc<dyn Tool>` | 注册一个自定义工具 |
| `with_system_prompt(prompt)` | `&str` | 设置系统提示词 |
| `system_prompt_builder()` | — | 获取 SystemPromptBuilder 用于多级 prompt 组合 |
| `with_planning_store(store)` | `Arc<dyn PlanningStore>` | 注入规划上下文存储 |
| `with_safety_policy(safety)` | `SafetyConfig` | 运行时覆盖安全规则（allowlist、denylist、MCP auto-approve …） |
| `with_permission_hook(hook)` | `fn(&str, &Value) -> PermissionResult` | 注入自定义权限钩子 |
| `with_storage_dir(path)` | `impl Into<PathBuf>` | 设置 SDK 内部存储目录（覆盖 agent.toml 中的 storage_dir） |

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
    .model_id("deepseek-v4-flash")
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
| `run_once(msg)` | `Result<(), AgentError>` | 纯副作用（忽略输出） |
| `run_once_capture(msg)` | `Result<TurnOutcome, AgentError>` | 捕获最终结果 |
| `run_once_streaming(msg, tx)` | `Result<TurnOutcome, AgentError>` | 通过 channel 获取实时事件 |

**示例：流式获取事件**

```rust
use tokio::sync::mpsc;
let (tx, mut rx) = mpsc::channel::<AgentEvent>(64);
let outcome = agent.run_once_streaming("创建项目结构", &tx).await?;

// 注意：rx 会在 turn 结束后自动关闭，所以 rx.recv() 最终返回 None。
while let Some(ev) = rx.recv().await {
    match ev {
        AgentEvent::ModelTextDelta { text } => print!("{}", text),
        AgentEvent::ToolCallStart { name, .. } => println!("[tool] {}", name),
        AgentEvent::ModelUsage { usage } => {
            println!("tokens: in={} out={}", usage.input_tokens, usage.output_tokens);
        }
        AgentEvent::TurnEnd { outcome, .. } => {
            println!("turn finished: {:?}", outcome);
        }
        AgentEvent::Error { error } => eprintln!("error: {}", error),
        _ => {}
    }
}
```

### 2.2 AgentEvent 类型

| 事件 | 时机 | 关键字段 |
|------|------|---------|
| `TurnStart` | 轮次开始 | `turn_id: u64` |
| `TurnEnd` | 轮次结束 | `turn_id: u64`、`outcome: TurnOutcome` |
| `ModelMessageStart` | 模型开始生成 | `message_id: String` |
| `ModelTextDelta` | Provider 返回文本块 | `text: String` |
| `ModelThinkingDelta` | 推理模型思考过程 | `text: String` |
| `ModelMessageEnd` | 模型生成完毕 | `message_id: String` |
| `WaitingForModel` | 模型流暂停（心跳） | `elapsed_secs: u64` |
| `ModelUsage` | Token 用量统计 | `usage: TokenUsage` |
| `ToolCallStart` | 工具调用开始 | `call_id: String`、`name: String`、`input: Value` |
| `ToolInputDelta` | 工具参数流式生成 | `index: usize`、`call_id: Option<String>`、`tool_name: Option<String>`、`delta: String` |
| `ToolCallEnd` | 工具调用结束 | `call_id: String`、`output: ToolOutput` |
| `ToolExecutionProgress` | 工具执行心跳（≥5s） | `call_id: String`、`tool_name: String`、`elapsed_secs: u64` |
| `PlanProgress` | Plan 进度更新 | `completed: usize`、`total: usize`、`current_item: Option<String>` |
| `PermissionRequest` | 需要用户授权 | `request_id: String`、`tool_name: String`、`prompt: String`、`risk_level: String`、`policy_source: String`、`tool_summary: String` |
| `Compaction` | 上下文压缩 | `event: CompactionEvent` |
| `MemoryStateChanged` | 记忆状态变更 | `event: MemoryStateEvent` |
| `MemoryInjected` | 记忆已注入 context | `count: u32`、`memory_ids: Vec<String>` |
| `SoftInterruptInjected` | 软中断注入 | `interrupt: InjectedInterrupt` |
| `Error` | 发生错误 | `error: AgentError` |
| `McpServerConnected` | MCP 服务器连接 | `server_name: String` |
| `McpServerDisconnected` | MCP 服务器断开 | `server_name: String`、`error: Option<String>` |

### 2.3 TurnOutcome 结果类型

```rust
pub enum TurnOutcome {
    Completed { text: String },
    RequiresUserDecision { request: PermissionRequest },
    Cancelled,
    Failed { error: AgentError },
}
```

### 2.4 权限中断与恢复

LLM 调用工具触发权限检查 → 用户决策 → Agent 恢复执行。`PermissionDecision` 只有 `Allow` 和 `Deny { reason }` 两种变体。

```rust
use tokio::sync::mpsc;
let (tx, mut rx) = mpsc::channel::<AgentEvent>(64);

// 循环处理：权限可能触发多次
let mut is_first = true;
let mut decision: Option<PermissionDecision> = None;
loop {
    let outcome = if is_first {
        is_first = false;
        agent.run_once_streaming("删除日志文件", &tx).await?
    } else {
        agent.resume_streaming(decision.take().unwrap(), &tx).await?
    };

    match outcome {
        TurnOutcome::Completed { text } => {
            println!("执行完成: {}", text);
            break;
        }
        TurnOutcome::RequiresUserDecision { request } => {
            println!("Agent 请求权限: {}", request.prompt);
            // 收集用户决策，下一轮迭代走 resume_streaming
            decision = Some(PermissionDecision::Allow);
        }
        TurnOutcome::Cancelled => {
            println!("已取消");
            break;
        }
        TurnOutcome::Failed { error } => {
            eprintln!("执行失败: {}", error);
            break;
        }
    }
}
```

> **注意**：`resume_streaming` 只处理从上一次 `RequiresUserDecision` 暂停的那个 tool call 开始恢复。如果 assistant 同时返回了多个 tool call 且都需要权限，每次 `RequiresUserDecision` 只暂停一个，需要多次 `resume_streaming`。

---

## 3. 会话管理

### 3.1 三层模型：运行时 → 快照 → 持久化

```
SessionState (运行时) ──snapshot()──▶ SessionSnapshot (可序列化)
                                           │
                                     SessionStore trait
                                      ┌────┴────┐
                              FileSessionStore  InMemorySessionStore
                              ({storage_dir}/sessions/)   (HashMap, 测试用)
```

| 概念 | 定位 | 包含内容 |
|------|------|---------|
| `SessionState` | 内存中的运行时状态 | 消息历史 `Vec<Message>`、会话状态 `SessionStatus`、标题/模型、env 快照 |
| `SessionSnapshot` | 可序列化的完整快照 | 继承 `SessionState` 全部字段 **+** 模型运行时状态、挂起的权限请求、挂起的工具调用队列、中断状态、turn 计数器 |
| `SessionStore` | trait，持久化接口 | `save_session()` / `load_session()` / `delete_session()` / `list_sessions()` |

### 3.2 SessionState（运行时模型）

`SessionState` 是领域层 Reducer 模型，通过 `apply(SessionEvent)` 驱动状态转移：

```rust
pub struct SessionState {
    pub id: String,
    pub parent_id: Option<String>,
    pub title: Option<String>,
    pub model: Option<String>,
    pub provider_key: Option<String>,
    pub status: SessionStatus,          // New / Active / Closed
    pub working_dir: Option<PathBuf>,
    pub messages: Vec<Message>,         // 当前工作上下文（可能已被压缩）
    pub full_messages: Vec<Message>,    // 完整未压缩的对话记录
    pub env_snapshots: Vec<EnvSnapshot>,
    pub created_at: u64,                // Unix 秒
}
```

### 3.3 SessionSnapshot（持久化格式）

Agent 调用 `snapshot()` 时，将运行时状态（包括模型运行时、待审批权限、中断队列等）导出为完整快照。`SessionSnapshot` **包含了恢复 Agent 运行所需的全部信息**，不仅包括对话历史。

```rust
pub struct SessionSnapshot {
    // ── 继承自 SessionState ──
    pub session_id: String,
    pub parent_id: Option<String>,
    pub title: Option<String>,
    pub model: Option<String>,
    pub provider_key: Option<String>,
    pub status: SessionStatus,
    pub working_dir: Option<PathBuf>,
    pub messages: Vec<Message>,
    pub full_messages: Vec<Message>,
    pub env_snapshots: Vec<EnvSnapshot>,

    // ── 附加的运行时状态（SessionState 中没有的） ──
    pub model_runtime_state: ModelRuntimeState,         // 模型上下文窗口状态
    pub pending_permission: Option<PermissionRequest>,  // 挂起的权限请求
    pub pending_tool_calls: Vec<PendingToolCallSnapshot>, // 待执行的工具调用
    pub interrupt_state: InterruptSnapshot,             // 中断管理
    pub next_turn_id: u64,                              // 下一个 turn ID

    pub metadata: Option<serde_json::Value>,
    pub updated_at: u64,
    pub created_at: u64,
}
```

### 3.4 默认存储位置与 storage_dir 的关系

**`FoxAgentSdkConfig.storage_dir`** 是所有持久化数据的根目录：

```
{storage_dir}/                          ← 默认 ".fox-agent-sdk"
├── sessions/         ← FileSessionStore 的 root_dir
│   └── {session_id}.json
├── planning/         ← 规划数据
│   ├── goals.json
│   └── plans.json
├── memory/           ← 长期记忆系统
│   ├── session_scoped/   ← Session 作用域记忆（临时）
│   ├── projects/         ← Project 作用域记忆
│   ├── global.json       ← Global 作用域记忆
│   └── memory.audit.jsonl
└── artifacts/        ← Artifact Store（大文件外置存储）
```

**存储位置解析规则**（`resolve_storage_root()`）：

1. 若 `storage_dir` 是绝对路径 → 直接使用
2. 若 `storage_dir` 是相对路径 → 相对于 `AgentBuilder::working_dir()` 拼接
3. **默认值**：`storage_dir = ".fox-agent-sdk"`，`working_dir` 为 Agent 传入的项目根目录
   ⇒ 最终默认路径：**`{项目根目录}/.fox-agent-sdk/sessions/`**

### 3.5 SessionStore

trait 定义：

```rust
pub trait SessionStore: Send + Sync {
    fn save_session(&self, snapshot: &SessionSnapshot) -> Result<(), String>;
    fn load_session(&self, session_id: &str) -> Result<Option<SessionSnapshot>, String>;
    fn delete_session(&self, session_id: &str) -> Result<(), String>;
    fn list_sessions(&self) -> Result<Vec<String>, String>;
}
```

两个内置实现：

| 实现 | 存储后端 | 用途 |
|------|---------|------|
| `FileSessionStore` | `{root_dir}/{session_id}.json`（JSON pretty 格式） | **默认**，生产环境 |
| `InMemorySessionStore` | `HashMap<String, SessionSnapshot>` | 测试，`AgentBuilder::new()` 无 work_dir 时自动降级 |

AgentBuilder 构建时**自动创建** `FileSessionStore`，路径为 `{storage_root}/sessions/`。若需要自定义（如切换为 `InMemorySessionStore`），通过 `with_session_store()` 覆盖：

```rust
let store = Arc::new(InMemorySessionStore::default());
let mut agent = AgentBuilder::new()
    .sdk_config(cfg)
    .provider_config(ProviderConfig::deepseek(key))
    .with_session_store(store)
    .build()
    .await?;
```

### 3.6 自动快照（auto_snapshot）

`auto_snapshot` 默认为 **`true`**。Agent Loop 在关键生命周期节点自动保存完整会话快照：

| 触发点 | 时机 | trigger 标签 |
|--------|------|-------------|
| 用户消息到达 | 每轮开始时 | `user_message` |
| turn 完成 | 工具循环结束 | `turn_completed` |
| 等待用户权限 | 需要审批时 | `awaiting_permission` |
| turn 取消 | 被中断时 | `turn_cancelled` |

**快照以异步后台方式写入磁盘**（`tokio::spawn`），JSON 序列化 + 文件 I/O 不阻塞主 Agent Loop。即使写入失败也只记录 warning 日志，不影响 Agent 继续执行。

在 `agent.toml` 中控制：
```toml
auto_snapshot = true   # 默认开启
```

### 3.7 会话恢复

通过 `SessionStore::load_session()` 读取 `SessionSnapshot`，再用 `Agent::from_session_snapshot()` 重建 Agent：

```rust
// 从 store 加载快照
if let Some(snapshot) = harness.session_store.load_session("session-1")? {
    let model = load_model(&cfg)?;  // 重新连接 Provider
    let restored = Agent::from_session_snapshot(model, harness, snapshot);
    restored.run_once("继续刚才的工作").await?;
} else {
    eprintln!("session not found");
}
```

也可以在构建 Agent 时通过 `session_id()` 指定要恢复的会话：
```rust
let mut agent = AgentBuilder::new()
    .sdk_config(cfg)
    .provider_config(ProviderConfig::deepseek(key))
    .session_id("session-1")   // 若存在则自动恢复
    .build()
    .await?;
```

相对路径会基于 `working_dir` 解析。

```rust
// 显式指定绝对路径
let config = FoxAgentSdkConfig {
    auto_snapshot: true,
    storage_dir: dirs::data_dir().unwrap().join("fox-code"),
    ..Default::default()
};

// 通过 Builder 指定相对路径 → working_dir/.fox-code/
let mut agent = AgentBuilder::new()
    .working_dir("./my-project")
    .provider_config(ProviderConfig::deepseek(key))
    .with_storage_dir(".fox-code")
    .build()
    .await?;
```

### 3.8 会话并发与隔离

多个 Agent 实例（对应不同 session）可以同时运行，互不干扰。隔离性来自两个层面：

**实例隔离**：每个 Agent 实例持有独立的 `SessionState` 和 `SessionSnapshot`，通过 `Harness` 中的 `Arc<RwLock<SessionState>>` 保护。不同 Agent 之间不共享任何可变状态，一个 session 的消息历史、权限请求、工具调用队列完全独立。

```rust
// 两个 session 并发运行，互不干扰
let agent1 = AgentBuilder::new()
    .session_id("session-1")
    .provider_config(ProviderConfig::deepseek(key.clone()))
    .build().await?;

let agent2 = AgentBuilder::new()
    .session_id("session-2")
    .provider_config(ProviderConfig::deepseek(key))
    .build().await?;

let (tx1, _rx1) = mpsc::channel(64);
let (tx2, _rx2) = mpsc::channel(64);

// 并发执行
let (r1, r2) = tokio::join!(
    agent1.run_once_streaming("分析用户数据", &tx1),
    agent2.run_once_streaming("生成周报", &tx2),
);
```

**存储隔离**：每个 session 以独立的 `{session_id}.json` 文件存储，不同 session 的持久化数据天然分离。`auto_snapshot` 通过 `tokio::spawn` 后台异步写入，不会阻塞其他 session 的运行。

> **注意**：如需更高的并发隔离（如完全独立的 working_dir），可通过 `AgentBuilder::working_dir()` 为不同 Agent 指定不同的项目根目录。

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

### 4.7 工具结果路由与子 Agent 隔离

当工具输出非常大时（如全库搜索结果、大文件读取、网页抓取），默认的"工具结果直写消息流"模式会导致主 Agent 上下文被低价值噪声挤占。Fox Agent SDK 通过**统一路由引擎**自动判断如何处理每个工具结果。

#### 三种路由模式

| 模式 | 描述 | 触发条件 |
|------|------|----------|
| `Inline` | 结果直接写入消息流 | 小输出，低上下文压力 |
| `Externalize` | 完整结果存入 artifact store，消息中仅保留摘要和引用 | 大输出或高风险 MCP 工具 |
| `DelegateToSubagent` | 委托子 Agent 在隔离上下文中执行探索 | 高噪声探索型任务 |

#### 配置

```toml
[artifact_store]
enabled = true
max_artifact_bytes = 1_048_576
max_session_bytes = 33_554_432
ephemeral_ttl_hours = 24

[routing_policy]
enabled = true
local_externalize_threshold_chars = 8_000
local_delegate_threshold_chars = 24_000
context_pressure_threshold = 0.70
delegate_candidate_tools = ["grep", "glob", "web_fetch", "web_search", "ls", "read"]
```

#### `artifact_read` 工具

主 Agent 可以通过内置的 `artifact_read` 工具按需分页回读已外置的 artifact，避免重新执行大工具。

```
参数: artifact_id, offset_chars, limit_chars
```

#### 子 Agent 委派

当预设的高噪声工具产生大输出时，routing engine 会自动将其委派给子 Agent：

```rust
// 子 Agent 在隔离上下文中执行，只回传结构化摘要
SubagentSummary {
    task_id: "...",
    objective: "...",
    findings: [...],
    evidence_refs: [...],
    recommendations: [...],
    uncertainties: [...],
    next_queries: [...],
}
```

也可以手动通过 `subagent` 工具显式委派探索任务。

#### 治理指标

`GovernanceMetrics` 提供跨会话的聚合指标，帮助监控存储压力和路由决策质量：

```
Routing: N total — inline X%, summarize Y%, externalize Z%, delegate W%
Artifacts: N writes (M bytes), R reads, GC: D deleted (F bytes freed)
Sub-agents: N tasks — S success, T timeout, E error
Compaction: C triggers, MCP: M calls
```

可通过 `harness.governance_metrics.snapshot().format_summary()` 获取报告。

#### 审计事件

以下事件在 tool→routing→artifact→subagent→summary 链路中全程追踪：

| 事件 | 触发时机 |
|------|----------|
| `RoutingDecision` | 路由引擎对每个工具结果作出决策时 |
| `ArtifactStored` | 工具结果外置存储时（含 server/transport 等 MCP 元数据） |
| `ArtifactRead` | `artifact_read` 回读时 |
| `ArtifactGc` | GC 回收过期对象时 |
| `SubagentTaskStarted` | 子 Agent 任务派发时 |
| `SubagentTaskCompleted` | 子 Agent 完成任务时（含 findings_count、evidence_count、elapsed_secs） |

---

## 5. 权限与安全

> **完整可运行示例**：[`examples/safety_demo.rs`](../examples/safety_demo.rs) — 演示安全策略配置、权限中断处理、审计导出的端到端流程。

### 5.1 SafetyConfig

```rust
pub struct SafetyConfig {
    /// default_policy — 未匹配到其他规则时的默认策略。
    pub default_policy: DefaultSafetyPolicy,    // Allow / Deny / Confirm

    /// tool_denylist — 工具黑名单。匹配的工具始终要求用户确认。
    /// 支持 `*` 通配符：`"mcp__akshare__*"`、`"mcp__*"`、`"stock_*"`。
    /// 优先级高于 allowlist 和 mcp_auto_approve。
    pub tool_denylist: Option<Vec<String>>,

    /// tool_allowlist — 工具白名单。若设置，仅匹配的工具自动允许。
    /// 支持 `*` 通配符。未匹配到的工具会被 Deny。
    /// `None` 表示白名单模式关闭。
    pub tool_allowlist: Option<Vec<String>>,

    /// productive_tool_confirm — 启用时 write/edit/非只读 bash 始终确认。
    pub productive_tool_confirm: bool,

    /// mcp_auto_approve_servers — 自动批准指定 MCP server 的所有工具。
    /// 等价于在 allowlist 中添加 `"mcp__<server>__*"`。
    /// 当 AgentBuilder 设置了 `auto_approve: true` 的 McpServerConfig 时自动填充。
    pub mcp_auto_approve_servers: Option<Vec<String>>,

    /// approval_cache — 审批缓存配置。
    pub approval_cache: ApprovalCacheConfig,
}
```

### 5.1a Productive Tool Confirm（产出工具确认）

当启用时 write/edit/bash（非只读）始终需要用户确认，不分析用户消息内容。

**只读 bash 命令自动放行**：`ls`、`grep`、`cat`、`find`、`git log`、`cargo check` 等。

**配置**：
```toml
[safety]
productive_tool_confirm = true   # 默认开启
```

#### 设计意图：为什么需要独立的确认层

`productive_tool_confirm` 解决的是 allowlist/denylist/default_policy 这套规则体系覆盖不到的盲区——**"能不能用"和"该不该问"是两个正交维度**。

| 机制 | 解决的问题 | 典型问题 |
|------|-----------|---------|
| allowlist / denylist | **能不能用**（访问控制） | 哪些工具对 Agent 开放 |
| productive_tool_confirm | **该不该问**（确认策略） | 允许用的工具中，哪些每次都要确认 |

**具体场景**：用户说"帮我看看这段代码为什么有 bug"，Agent 的 LLM 可能自行决定调用 `write` 去修改文件。如果 `write` 在 allowlist 中且 `default_policy=Allow`，这个写操作会静默执行——用户没有授权修改的意图，但 Agent 越权了。

**为什么不能靠 allowlist/denylist 替代**：

- 把 `write` 放进 denylist → 用户说"修复这个 bug"时，每次也要弹确认，体验差
- `default_policy=Confirm` → 连 `read`、`grep` 等只读工具也会弹确认，干扰太多

`productive_tool_confirm` 做的是**精准分类**：只对"产出类工具"（write / edit / 非只读 bash）强制确认，其他工具照常放行。这样 allowlist 可以宽松配置（开放大量只读工具），而产出类操作始终在用户视线内。

**为什么是后处理层而非另一个规则**：因为它的逻辑是"无论什么路径拿到的 Allow 结果，产出类工具一律提权"。如果把这个逻辑嵌入 allowlist 内部、mcp_auto_approve 内部、default_policy 内部，就需要在三处重复同样的判断——后处理层是最干净的实现方式。

### 5.2 策略评估流程

> **`productive_tool_confirm` 不是独立步骤，而是附着在 Allow 结果上的后处理层。** 当主决策层返回 Allow 时，若 `productive_tool_confirm=true` 且工具为 write/edit/非只读 bash，则最终结果被提升为 AskUser。

```
1. custom_hook（自定义权限钩子 — 最高优先级，跳过所有规则）
   ↓ (未注册则继续)
2. Denylist（通配符匹配） → 命中 → AskUser（直接短路）
   ↓ (未命中)
3. Allowlist（通配符匹配）
   ├── 命中 → Allow ──→ productive_tool_confirm 后处理 → 最终 Allow 或 AskUser
   └── 已设置但未命中 → Deny
   ↓ (allowlist 为 None 时继续)
4. MCP auto-approve servers
   ├── 命中 → Allow ──→ productive_tool_confirm 后处理 → 最终 Allow 或 AskUser
   └── 未命中 ↓
5. MCP Profile Gating（基于 server 种类/传输方式） → 命中 → AskUser
   ↓ (未命中或非 MCP 工具)
6. Default Policy
   ├── Deny  → 直接返回 Deny
   ├── Confirm → 直接返回 AskUser
   └── Allow → productive_tool_confirm 后处理 → 最终 Allow 或 AskUser
```

**总结优先级**：`custom_hook > denylist > allowlist > mcp_auto_approve > default_policy`，`productive_tool_confirm` 作为后处理层，覆盖第 3/4/6 步的 Allow 结果。

#### 通配符规则示例

```toml
[safety]
# 允许 akshare MCP server 的所有工具
tool_allowlist = ["read", "grep", "mcp__akshare__*"]

# 黑名单优先级最高 —  即使 MCP auto-approve 了 akshare，此工具仍需确认
tool_denylist = ["mcp__akshare__delete_data"]

# 批量自动批准多个 MCP server
mcp_auto_approve_servers = ["akshare", "filesystem"]
```

### 5.3 自定义权限钩子

```rust
let mut agent = AgentBuilder::new()
    .sdk_config(cfg)
    .provider_config(ProviderConfig::deepseek(key))
    .with_permission_hook(|tool_name: &str, _input: &Value| {
        // 自定义逻辑：文件系统操作必须用户确认
        if tool_name.starts_with("mcp__filesystem__") {
            return PermissionResult::AskUser { ... };
        }
        PermissionResult::Allow
    })
    .build()
    .await?;
```

### 5.4 ApprovalManager

三层缓存设计，减少重复审批：

| 层级 | 生命周期 | 场景 |
|------|---------|------|
| `ThisTurn` | 轮次结束清空 | 同轮内相同工具免重复审批 |
| `ThisSession` | 会话结束清空 | 用户确认一次，整个会话生效 |
| `ThisWorkspace` | 跨会话持久 | 信任的工具永久生效 |

### 5.5 审计与溯源

每个 `PermissionRequest` 携带 `policy_source` 字段，记录决策逻辑来源（`"denylist"`、`"allowlist"`、`"default:confirm"`、`"productive-tool-confirm"` 等）。SDK 提供两种审计方式：

#### 推荐：`with_audit_handler()` 自动回调

通过 `AgentBuilder::with_audit_handler()` 注册回调，Agent 在每次用户做出权限决策后**自动调用**，无需在每个 `RequiresUserDecision` 分支中手写 `record_audit`：

```rust
use fox_agent_sdk::{AgentBuilder, ApprovalManager, PermissionDecision, PermissionResult};

let approval = ApprovalManager::new("session-1", SafetyConfig::default());
let audit_path = std::path::PathBuf::from("./audit/permissions.jsonl");

let agent = AgentBuilder::new()
    .provider_config(ProviderConfig::deepseek(key))
    .with_default_tools()
    .with_safety_policy(safety)
    // 审计回调 — SDK 自动在每次权限决策时调用
    .with_audit_handler(move |req, dec, turn| {
        let result = match dec {
            PermissionDecision::Allow => PermissionResult::Allow,
            PermissionDecision::Deny { reason } =>
                PermissionResult::Deny { reason: reason.clone() },
        };
        // record_audit 自动触发，无需在 RequireUserDecision 分支中手写
        approval.record_audit(req, &result, *turn).await;
    })
    .build()
    .await?;

// 应用层只需处理业务逻辑，审计已由回调自动覆盖
let mut is_first = true;
let mut decision: Option<PermissionDecision> = None;
loop {
    let outcome = if is_first {
        is_first = false;
        agent.run_once_streaming("帮我优化代码", &tx).await?
    } else {
        agent.resume_streaming(decision.take().unwrap(), &tx).await?
    };

    match outcome {
        TurnOutcome::RequiresUserDecision { request } => {
            println!("需要确认: {} ({})", request.tool_name, request.policy_source);
            decision = Some(PermissionDecision::Allow); // 审计自动记录
        }
        TurnOutcome::Completed { text } => break,
        _ => break,
    }
}
```

回调接收三个参数：
- `&PermissionRequest` — 原始权限请求（含 `policy_source`、`tool_name`、`risk_level` 等）
- `&PermissionDecision` — 用户的最终决策（Allow / Deny）
- `u64` — 决策发生时的 turn ID

#### 手动方式（备选）

如果不需要自动回调，也可以手动调用 `ApprovalManager::record_audit()`：

```rust
let approval = ApprovalManager::new("session-1", SafetyConfig::default());

match outcome {
    TurnOutcome::RequiresUserDecision { request } => {
        let decision = PermissionDecision::Allow;
        let result = PermissionResult::Allow;
        approval.record_audit(&request, &result, turn_id).await;
    }
    // ...
}

// 导出审计日志
approval.export_audit(&audit_path).await?;
```

#### 导出的 JSONL 示例

```jsonl
{"timestamp":1719900000,"session_id":"session-abc123","turn_id":1,"tool_name":"write","input":{"file_path":"/project/main.rs","content":"// optimized code..."},"decision":{"AskUser":{"request":{"request_id":"req-001","tool_name":"write","prompt":"`write` is a modification tool and requires your confirmation","risk_level":"High","policy_source":"productive-tool-confirm","tool_summary":"write"}}},"request_id":"req-001","latency_ms":12}
```

每条记录包含：`timestamp`、`session_id`、`turn_id`、`tool_name`、`input`、`decision`（含 `policy_source`）、`latency_ms`。

---

## 6. 记忆系统

Fox Agent SDK 内建完整的语义长期记忆系统，支持跨会话知识积累与召回。

### 6.1 三级作用域模型

记忆按生命周期分为三个隔离的作用域：

| 作用域 | 存储路径 | 共享范围 | 用途 |
|--------|---------|-----|------|
| `Session` | `memory/session_scoped/{session_id}.json` | 仅当前会话 | 中间假设、任务草稿、临时记忆 |
| `Project` | `memory/projects/{hash}.json` | 同工作目录的所有会话 | 项目规范、代码习惯、架构决策 |
| `Global` | `memory/global.json` | 所有项目、所有会话 | 用户偏好、全局约定 |

**Session → Project 提升**：跨多轮被反复强化的 Session 记忆可自动（`auto_promote_enabled`，strength 达阈值）或手动（`promote_memory`）提升到 Project/Global。提升为单向：不能降级回 Session。

### 6.2 架构概览

```
MemoryManager → 统一入口（CRUD、召回、治理）
MemoryGraph   → 图结构存储（节点 + 标签 + 聚类 + 边）
Extractor     → LLM 驱动从对话中抽取候选记忆
RelevanceChecker → LLM 校验记忆相关性与冲突
EmbeddingProvider → 语义向量化（Rust 本地推理）
ANN 索引      → HNSW 快速语义搜索（vectorlite）
```

### 6.3 配置示例

```toml
[memory]
enabled = true                       # 启用记忆系统
embedding_enabled = true             # 启用语义召回
embedding_model_id = "Qwen/Qwen3-Embedding-0.6B"
# embedding_model_path = "/local/path/to/model"

# 召回与注入预算
max_candidates = 30
max_results = 10
injection_max_chars = 1500           # 注入 prompt 的最大字符数
injection_max_per_category = 3       # 每类最多注入条数
max_graph_depth = 2                  # 图 BFS 扩展深度

# 自动提取
auto_extract = true                  # 从对话中自动抽取记忆
auto_extract_scope = "Project"       # "Project" 或 "Global"
auto_extract_message_window = 6      # 转录窗口（最近 N 条消息）
auto_extract_max_items_per_turn = 4  # 每轮最多提取条数

# 自动提升
auto_promote_enabled = false         # 开启后 strength ≥ 3 自动提升到 Project
auto_promote_strength_threshold = 3
auto_promote_target = "Project"

# 去重与聚类
dedupe_similarity_threshold = 0.92
cluster_similarity_threshold = 0.9
cluster_min_members = 2

# 冲突处理
contradiction_policy = "MarkContradictionEdge"
# Ignore | Supersede | DowngradeConfidence | MarkContradictionEdge

# 治理
# retention_days = 90
# memory_size_limit = 10000
rebuild_on_model_change = false     # embedding 模型变化时自动重嵌
```

```rust
// 代码中编程配置
let mem_config = MemoryConfig {
    enabled: true,
    auto_extract: true,
    auto_extract_scope: MemoryScope::Project,
    injection_max_chars: 1500,
    injection_max_per_category: 3,
    ..Default::default()
};
// 记忆存储于 {storage_dir}/memory/
// 由 FoxAgentSdkConfig.storage_dir 控制根目录
```

### 6.4 MemoryEntry 结构

```rust
pub struct MemoryEntry {
    pub id: String,                     // UUID v4
    pub category: MemoryCategory,       // Fact | Preference | Entity | Correction | Narrative | Custom
    pub content: String,                // 记忆原文
    pub tags: Vec<String>,              // 标签列表
    pub search_text: String,            // 规范化搜索文本
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub access_count: u32,              // 访问计数
    pub source: Option<String>,         // auto_extract | manual | promoted_from:session
    pub trust: TrustLevel,              // High | Medium | Low
    pub strength: u32,                  // 强化次数
    pub active: bool,                   // 启用/禁用状态
    pub superseded_by: Option<String>,  // 被取代的 ID
    pub reinforcements: Vec<Reinforcement>,  // 强化面包屑（session_id, message_index, timestamp）
    pub embedding: Option<Vec<f32>>,    // 语义向量
    pub embedding_model: Option<String>, // 模型标识
    pub embedding_version: Option<String>, // 模型版本
    pub confidence: f32,                // 置信度（0-1），支持时间衰减和访问加成
}
```

### 6.5 MemoryCategory 类别

| 类别 | 含义 | 实例 |
|------|------|------|
| `Fact` | 客观事实 | "项目使用 PostgreSQL 作为数据库" |
| `Preference` | 用户偏好 | "用户偏好中文回复，代码注释用英文" |
| `Correction` | 纠错信息 | "上次关于 API 端点的结论有误，正确端点是 /api/v2" |
| `Entity` | 实体/概念 | "AuthService 负责所有认证与授权逻辑" |
| `Narrative` | 叙事记录 | 会话的结构化叙事（compaction 自动生成），见 §8.2 |
| `Custom(name)` | 自定义类别 | 领域特定的记忆分类 |

### 6.6 Narrative 叙事记忆

`MemoryCategory::Narrative` 存储**会话的叙事结构**：用户要求了什么、Agent 做了什么、结果是什么。

每条 Narrative 由 compaction 自动生成：

```json
{
  "user_intent": "分析 Sprint 3 完成度",
  "actions_taken": ["read docs/plan.md", "grep Sprint 3"],
  "findings": ["Sprint 3.1 已完成", "Sprint 3.4 有编译错误"],
  "files_modified": [],
  "decisions": ["需要先修复编译错误再继续"],
  "pending_work": ["Sprint 3.4 修复", "Sprint 3.5 集成测试"]
}
```

Narrative 在后续 turn 的 prompt 中注入为 `## Session History` section。

### 6.7 召回模式

| 模式 | 种子来源 | 排序权重 | 需要 embedding |
|------|---------|---------|---------------|
| **Recent** | 全部 active 记忆 | `recency × 0.85 + trust × 0.15` | 否 |
| **Keyword** | `search_text` 关键词匹配 | `keyword × 0.65 + recency × 0.2 + trust × 0.15` | 否 |
| **Semantic** | 全量 cosine 相似度 | `cosine × 0.7 + recency × 0.15 + trust × 0.15` | 是 |
| **Cascade** | Semantic/Keyword top-k(×2) → BFS 图扩展 | seed score + graph score + recency + trust | 阶梯式 |

当 embedding 不可用时自动回退到 Keyword 模式。

### 6.8 ANN 索引（可选）

启用 ANN 后，Semantic 召回通过 HNSW 索引加速（引擎：`vectorlite`）：

```toml
ann_enabled = true
ann_min_vectors = 256               # 达到此向量数才启用 ANN
ann_candidate_multiplier = 8        # ANN 候选数 = limit × 此值
```

索引文件存储在 `{graph_path}.ann.bin`，首次 semantic recall 时惰性构建。数据变化后自动失效并重建。

### 6.9 记忆注入 Context 的完整流程

```
每轮 turn 开始前：

1. trigger_memory_for_next_turn()               [后台 tokio::spawn]
   └── 提取最新 user message 文本作为 query
   └── recall(MemoryScope::All, Cascade)
        ├── 返回 RecallHit 列表（按综合得分排序）
        └── select_recall_hits_for_injection()
             ├── per-category ≤ max_per_category 条（默认 3）
             └── 总字符数 ≤ injection_max_chars（默认 1500）

2. format_recall_hits_prompt()
   └── 按 category 分组：## Corrections → ## Facts → ## Preferences → ## Entities
   └── 固定顺序确保纠错信息最优先

3. take_memory_injection_for_prompt()
   └── 消费 pending injection，写入 dynamic_part 的 "4. Memory injection" 节

4. system prompt = static_part + dynamic_part
   └── 发送给 LLM
```

### 6.10 Ingestion Pipeline（写入流水线）

```
对话转录（最近 N 条消息）
  │
  ▼
[1] MemoryExtractor::extract()           ← LLM 抽取候选记忆
  │
  ▼
[2] For each candidate:
  ├── embed 生成语义向量
  ├── verify_relevance（LLM 校验）
  ├── find_duplicate → reinforce（strength+1, confidence+0.05）
  ├── find_contradiction → apply_contradiction_policy
  └── remember → 写入 graph + 持久化
```

### 6.11 冲突处理策略

| 策略 | 行为 |
|------|------|
| `Ignore` | 新旧共存，不做处理 |
| `Supersede` | 新记忆取代旧（旧标记 inactive + superseded_by） |
| `DowngradeConfidence` | 旧记忆置信度衰减 `contradiction_confidence_decay` |
| `MarkContradictionEdge` | 创建双向 `Contradicts` 边 |

### 6.12 存储布局

```
{storage_dir}/
├── sessions/                          ← SessionSnapshot（会话快照）
│   └── {session_id}.json
└── memory/                            ← 记忆系统根目录
    ├── session_scoped/                ← Session 记忆（临时，会话结束后无意义）
    │   └── {session_id}.json
    ├── projects/                      ← Project 记忆（跨会话共享）
    │   └── {project_hash}.json
    │   └── {project_hash}.ann.bin     ← HNSW 索引
    ├── global.json                    ← Global 记忆
    ├── global.ann.bin                 ← HNSW 索引
    └── memory.audit.jsonl             ← 审计日志
```

### 6.13 治理与运维

| 操作 | 方法 | 说明 |
|------|------|------|
| 写入 | `remember(entry, scope)` | 自动 embed + 持久化 |
| 召回 | `recall(query, limit, mode, scope)` | 4 种策略 |
| 搜索 | `search(text, scope)` | 精确关键词搜索 |
| 删除 | `forget(id)` | 从图中移除 |
| 禁用 | `disable_memory(id)` / `enable_memory(id)` | 标记 active 字段 |
| 脱敏 | `redact_memory(id, replacement)` | 替换内容 + 重新 embed |
| 提升 | `promote_memory(id, Session → Project)` | 单向，不可降级 |
| 导出 | `export_to_path(scope, path)` | JSON bundle |
| 导入 | `import_from_path(path, merge)` | 合并或替换 |
| GC | `gc(max_age_hours)` | 清理过期文件（含 session_scoped） |
| 压缩 | `compact(scope)` | 保留策略 + 大小限制 |
| 重嵌 | `reembed(scope)` | 模型变化后重建所有 embedding |

SDK 层 API：

```rust
// 写入
let mem_manager = agent.harness().memory_manager();
mem_manager.add_memory("用户偏好中文回复").await;

// 也可以直接操作 core MemoryManager：
let core = mem_manager.core();
core.remember(entry, MemoryScope::Project)?;
core.recall(Some("中文偏好"), 5, RecallMode::Cascade, MemoryScope::All)?;
```

### 6.14 记忆工作流示例

```
1. 第 1 轮: 用户说 "我更喜欢中文回复"
   → auto_extract 从转录中提取 → MemoryEntry { category: Preference, content: "用户偏好中文回复" }
   → 写入 Project scope（配置了 auto_extract_scope = "Project"）

2. 第 2 轮: 用户说 "帮我重构支付模块"
   → 每轮开始前 trigger_recall → 以 "帮我重构支付模块" 为 query → 召回
   → 匹配到 "用户偏好中文回复" → 注入 dynamic_part
   → Agent 用中文回复

3. 第 3+ 轮: 类似偏好被反复 reinforce → strength 累积
   → 若 auto_promote_enabled = true，strength ≥ 3 时自动提升到 Project
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

### 8.2 压缩行为：叙事结构提取

压缩时，SDK 将被丢弃的消息转化为**结构化的叙事记录**（NarrativeRecord），存储在 MemoryGraph 中：

```
被丢弃的消息
  → LLM 提取 JSON 叙事记录
  → 存入 MemoryGraph（Session scope）
  → 注入 "## Session History" 到后续 turn 的 dynamic prompt
```

每条 `NarrativeRecord` 包含：
- `user_intent` — 用户要求了什么
- `actions_taken` — Agent 调用了哪些工具
- `findings` — 关键发现/结果
- `files_modified` — 修改/创建的文件
- `decisions` — 做出的决策
- `pending_work` — 未完成的工作

叙事记录跨 session 持久化——restore 时自动可见，无需加载全部历史消息。

### 8.2a 智能截断（Tool Output Guard）

当单个工具输出过大或累计上下文接近预算时，SDK 自动截断工具输出，采用 **head+tail** 策略：
- 保留文件**前 25%**（结构信息：imports、类型定义）
- 保留文件**后 60%**（核心逻辑：最近的业务代码）
- 被省略的部分标注精确的行号范围，Agent 可按需定向读取

截断会同时触发 **Context 压力反馈**（Soft Interrupt），提醒 Agent 停止读取大文件、
改用 grep 等定向搜索。压力等级：
- **MODERATE**（>50%）：温和提醒
- **HIGH**（>70%）：强调警告
- **CRITICAL**（>90%）：紧急，标记为 urgent

截断后的输出示例：
```
[文件头部：imports + struct 定义]
─── Lines 26-472 omitted (447 lines) ───
[Use offset=26 limit=300 to read this section]

[文件尾部：核心业务逻辑]
[OUTPUT TRUNCATED: 45k → 24k chars | Context 72% full (57600/80000)]
```

### 8.3 配置

```rust
let compaction = CompactionConfig {
    token_budget: 3_200_000,              // 字符数阈值（≈800K tokens）
    max_turns_before_compaction: 500,     // 轮次后备触发
    context_limit_threshold: 0.90,        // 90% 时预压缩
    preserve_recent_messages: 80,         // 保留最近消息数
    max_compaction_count: 10,             // 压缩次数上限
    min_compaction_gap_turns: 20,         // 两次压缩最小间隔
    llm_summary_enabled: true,            // LLM 结构化叙事提取
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

- 超预算：`AgentError::BudgetExceeded { message }`（turn 返回 `Err`）
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
use fox_agent_sdk::{McpServerConfig, McpTransportMode};

// stdio 模式 — 通过子进程 stdin/stdout 通信
let mcp_conf = McpServerConfig {
    name: "akshare".to_string(),
    transport: McpTransportMode::Stdio {
        command: "uvx".into(),
        args: vec!["akshare-mcp".into()],
        env: None,
        cwd: None,
        startup_grace_ms: Some(10_000),  // uvx 首次需安装包，适当延长
    },
    auto_approve: true,   // 该 server 的所有工具自动批准
    request_timeout_ms: Some(60_000),
    ..Default::default()
};

let mut agent = AgentBuilder::new()
    .sdk_config(cfg)
    .provider_config(ProviderConfig::deepseek(key))
    .with_mcp_server(mcp_conf)
    .build()
    .await?;

// MCP 工具名称格式：mcp__<server>__<tool>
// 示例：
//   mcp__akshare__get_news_data
//   mcp__akshare__get_hist_data
```

### 11.2 传输方式

| 模式 | 适用场景 | 关键参数 |
|------|---------|---------|
| `McpTransportMode::Stdio` | 本地 MCP server（子进程） | `command`、`args`、`startup_grace_ms` |
| `McpTransportMode::Sse` | 远程 MCP server | `url`、`custom_headers`、`timeout_secs` |

`Stdio` 模式的 `startup_grace_ms` 控制子进程启动后等待多久才开始健康检查。对于 `uvx` / `npx` 等首次需安装包的工具，建议设置为 `10_000`（10 秒）或更长。

### 11.3 自动批准（auto_approve）

`McpServerConfig::auto_approve = true` 会将 server 名注入 `SafetyConfig::mcp_auto_approve_servers`，该 server 的所有工具在权限检查时自动 `Allow`（遵守 denylist 最高优先级）。

在 `agent.toml` 中可直接配置：
```toml
[safety]
mcp_auto_approve_servers = ["akshare", "filesystem"]
```

### 11.4 动态工具发现

`McpClient` 在连接时完成三步初始化：
1. `initialize` → MCP 握手
2. `notifications/initialized` → 通知服务器就绪
3. `tools/list` → 发现所有工具并注册为 `McpTool`

每个 MCP 工具的 `name` 会自动转换为 Provider 兼容格式（`mcp://server/tool` → `mcp__server__tool`），确保通过 DeepSeek / OpenAI 等 API 的 `^[a-zA-Z0-9_-]+$` 校验。

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

## 13. Claude Code 兼容：Skills / Hooks / Plugins

Fox Agent SDK 全面兼容 Claude Code 的三种扩展机制，让你可以直接复用 Claude Code
生态中的 Skill、Hook 和 Plugin。

### 13.1 Skills — 可插拔专家指令

Skill 是包含 **YAML frontmatter + Markdown 正文** 的 `.md` 文件，定义某个特定
领域的专家行为。Agent 在需要时可以按名激活 Skill，将其 prompt 注入上下文。

**Skill 文件格式**（与 Claude Code 完全兼容）：

```markdown
---
name: code-review
description: Review code for bugs, performance, and style issues
version: 1.0
allowed-tools: [read, grep, glob, edit]
model: claude-sonnet-4-20250514
args:
  - name: style-guide
    description: Path to style guide (e.g. google, airbnb)
    required: false
disable-model-invocation: false
---

You are a senior code reviewer. Follow these rules:

1. Check for correctness first, then style
2. Reference {{WORKING_DIR}}/.style-guides/{{ARGS.style-guide}} when available
3. Read template files from {{SKILL_DIR}}/templates/ for formatting conventions
4. Always explain the *why* behind each suggestion

## Process

1. Run `grep` for common anti-patterns
2. `read` critical files in full
3. Summarize findings with severity: 🔴 critical / 🟡 warning / 🔵 info
```

**YAML 字段说明**：

| 字段 | 必填 | 说明 |
|------|------|------|
| `name` | 是 | Skill 唯一标识符 |
| `description` | 否 | 描述 Skill 用途（缺省 = name） |
| `allowed-tools` | 否 | 激活后允许使用的工具列表 |
| `model` | 否 | 推荐模型（如 `claude-sonnet-4-20250514`） |
| `version` | 否 | 语义化版本号 |
| `args` | 否 | 参数定义列表，每个参数含 `name`、`description`、`required` |
| `disable-model-invocation` | 否 | 是否禁止模型自动调用此 Skill（默认 false） |

**模板变量**（在 prompt 正文中使用）：

| 变量 | 解析结果 |
|------|---------|
| `{{SKILL_DIR}}` | Skill 文件所在目录的绝对路径 |
| `{{WORKING_DIR}}` | Agent 工作目录绝对路径 |
| `{{ARGS.<name>}}` | 调用时传入的参数值 |

**Skill 加载源与优先级**：

SDK 从多个位置自动发现和加载 Skill（高优先级覆盖低优先级）：

| 优先级 | 来源 | 路径 | 枚举值 |
|--------|------|------|--------|
| 1（最高） | 项目 | `<working_dir>/.claude/skills/` | `SkillSource::Project` |
| 2 | 自定义 | 配置的 `additional_directories` | `SkillSource::Additional` |
| 3 | 全局 | `{storage_dir}/skills/` | `SkillSource::Global` |
| 4 | 插件 | 已安装的 Plugin skills 目录 | `SkillSource::Plugin` |

**TOML 配置**：

```toml
[skills]
enabled = true               # 启用 Skill 系统
load_global = true           # 加载全局 Skill（{storage_dir}/skills/）
reload_strategy = "Manual"   # "Manual" | "Auto"（Auto 未实现）
# additional_directories = ["/path/to/custom/skills"]
```

---

### 13.2 Hooks — 生命周期拦截器

Hook 是在 Agent 生命周期的特定节点上自动执行的脚本，可以拦截、修改或增强 Agent
行为。

**支持的 12 个生命周期事件**：

| 事件 | 触发时机 | 说明 |
|------|---------|------|
| `SessionStart` | 会话开始时 | 初始化环境、加载配置 |
| `UserPromptSubmit` | 用户提交 prompt 后 | 预处理用户输入 |
| `PreToolUse` | 工具调用前 | 阻止或修改工具输入 |
| `PostToolUse` | 工具调用后 | 审查、格式化工具输出 |
| `Notification` | 需要通知用户时 | 单向通知，不改变流程 |
| `Stop` | Agent 停止时 | 异常/预算耗尽 |
| `SubagentStop` | 子 Agent 完成时 | 子任务结果处理 |
| `PreCompact` | 上下文压缩前 | 注入关键上下文，防止丢失 |
| `PermissionPrompt` | 权限弹窗时 | 自定义权限决策 |
| `PreFileWrite` | 文件写入前 | 备份、格式检查 |
| `PostFileWrite` | 文件写入后 | Lint、format、git add |
| `PreCompact` | 上下文压缩前 | 注入关键上下文，防止丢失 |

**目前已集成到 Agent Loop 的事件**：

| 事件 | 集成状态 | 行为 |
|------|---------|------|
| `PreToolUse` | 已集成 | hook 可返回 `Block { reason }` 阻止执行，或 `Allow { modified_input }` 修改输入 |
| `PostToolUse` | 已集成 | hook 可返回 `Block { reason }` 阻止结果返回给 LLM |
| `PreCompact` | 已集成 | hook 可返回 `InjectContext { context }` 注入额外上下文 |

其余事件框架已就绪，等待后续版本接入 Agent Loop。

**Hook 配置文件**（JSON 格式）：

Hook 从以下路径自动加载：
1. `<working_dir>/.claude/hooks/`（项目级）
2. `{storage_dir}/hooks/`（全局级）
3. `additional_directories` 中配置的自定义路径

每个目录下的 JSON 文件定义一组 hook：

```json
{
    "hooks": [
        {
            "event": "pre-tool-use",
            "command": "python3",
            "args": ["/path/to/security-check.py"],
            "matcher": "bash"
        },
        {
            "event": "post-tool-use",
            "command": "bash",
            "args": ["-c", "echo 'Tool completed' >> /tmp/audit.log"],
            "matcher": null
        },
        {
            "event": "pre-compact",
            "command": "node",
            "args": ["/path/to/save-context.js"]
        }
    ]
}
```

**Hook 字段说明**：

| 字段 | 说明 |
|------|------|
| `event` | 生命周期事件名（kebab-case，如 `"pre-tool-use"`） |
| `command` | 执行的命令 |
| `args` | 命令参数列表 |
| `matcher` | 可选。匹配特定工具名称（如 `"bash"`、`"write"`），`null` 表示匹配所有 |

**Hook 的 stdin 输入 / stdout 输出协议**：

脚本通过 stdin 接收 JSON 格式的上下文信息：

```json
{
    "session_id": "abc123",
    "event": "pre-tool-use",
    "working_dir": "/home/user/my-project",
    "tool_name": "bash",
    "tool_input": { "command": "rm -rf /" }
}
```

脚本通过 stdout 返回 JSON 格式的决策：

```json
// 允许执行（可修改输入）
{ "decision": "allow", "modified_input": { "command": "rm -rf ./tmp" } }

// 阻止执行
{ "decision": "block", "reason": "危险操作被拦截" }

// 注入上下文（仅在 PreCompact 等事件中有意义）
{ "decision": "inject_context", "context": "请保留此关键信息: ..." }
```

**TOML 配置**：

```toml
[hooks]
enabled = true               # 启用 Hook 系统
timeout_secs = 30            # 单个 hook 执行超时（秒）
max_concurrent = 5           # 每个事件最多并行执行的 hook 数
load_global = true           # 加载全局 Hook（{storage_dir}/hooks/）
# additional_directories = ["/path/to/custom/hooks"]
```

---

### 13.3 Plugins — 打包分发 Skill + Hook

Plugin 是将 Skill、Hook 和相关资源打包在一起的分发机制，可以从 Git 仓库、
Marketplace 安装。

**`plugin.json` 格式**（插件根目录）：

```json
{
    "name": "code-review",
    "version": "1.0.0",
    "description": "自动化代码审查插件",
    "author": "your-team",
    "repository": "https://github.com/your-org/code-review-plugin",
    "license": "MIT",
    "min_sdk_version": "0.1.0",
    "entry": {
        "skills": ["skills/"]
    },
    "dependencies": {}
}
```

**`entry.skills`**：指定插件内包含 Skill 文件的子目录。SDK 会递归扫描这些目录，
加载其中所有 `.md` 文件。

**插件安装方式**：

1. **从 GitHub 安装**（通过 Marketplace）：

SDK 预配置了 Claude Code 的两个默认 marketplace：

| Marketplace | GitHub 仓库 | 说明 |
|-------------|------------|------|
| `claude-plugins-official` | `anthropics/claude-plugins-official` | Anthropic 官方维护，审核严格 |
| `claude-plugins-community` | `anthropics/claude-plugins-community` | 社区贡献，通过自动校验 |

配置方式：

```toml
[[plugins.marketplaces]]
name = "claude-plugins-official"
source = "GitHub"
owner = "anthropics"
repo = "claude-plugins-official"
branch = "main"
auto_update_hours = 12

[[plugins.marketplaces]]
name = "claude-plugins-community"
source = "GitHub"
owner = "anthropics"
repo = "claude-plugins-community"
branch = "main"
auto_update_hours = 24
```

SDK 会在 `build()` 时自动刷新 marketplace 索引并缓存到
`{storage_dir}/plugins/marketplaces/`。

2. **从本地路径安装**：

```rust
let mut plugin_mgr = PluginManager::new(
    storage_dir.join("plugins"),
    vec![],
);
plugin_mgr.install_from_path(&PathBuf::from("/path/to/my-plugin")).await?;
```

3. **自动预安装**（通过 TOML 配置）：

```toml
[plugins]
enabled = true
preinstall = ["code-review", "security-audit"]
```

**插件目录结构示例**：

```
{storage_dir}/plugins/code-review/
├── plugin.json          # 插件元数据
└── skills/
    └── code-review.md   # Skill 文件
```

**TOML 完整配置**：

```toml
[plugins]
enabled = true                # 启用插件系统
auto_update_hours = 12        # 自动检查更新间隔（0 = 禁用）
preinstall = ["code-review"]  # 启动时自动安装的插件名

# Claude Code 官方 marketplace
[[plugins.marketplaces]]
name = "claude-plugins-official"
source = "GitHub"
owner = "anthropics"
repo = "claude-plugins-official"
branch = "main"
auto_update_hours = 12

# Claude Code 社区 marketplace
[[plugins.marketplaces]]
name = "claude-plugins-community"
source = "GitHub"
owner = "anthropics"
repo = "claude-plugins-community"
branch = "main"
auto_update_hours = 24
```

---

### 13.4 一站式 TOML 配置示例

将以上三种扩展机制整合到 `agent.toml`：

```toml
# Skills
[skills]
enabled = true
load_global = true
reload_strategy = "Manual"

# Hooks
[hooks]
enabled = true
timeout_secs = 30
max_concurrent = 5
load_global = true

# Plugins
[plugins]
enabled = true
auto_update_hours = 0
preinstall = ["code-review"]

[[plugins.marketplaces]]
name = "claude-plugins-official"
source = "GitHub"
owner = "anthropics"
repo = "claude-plugins-official"
branch = "main"
auto_update_hours = 12

[[plugins.marketplaces]]
name = "claude-plugins-community"
source = "GitHub"
owner = "anthropics"
repo = "claude-plugins-community"
branch = "main"
auto_update_hours = 24
```

启动时 Builder 自动加载以上所有配置，无需额外代码：

```rust
let cfg = FoxAgentSdkConfig::load_from_file("agent.toml")?;
let mut agent = AgentBuilder::new()
    .sdk_config(cfg)
    .provider_config(ProviderConfig::deepseek(key))
    .build()
    .await?;
// Skills、Hooks、Plugins 已自动加载并生效
```

**加载日志示例**（`RUST_LOG=info`）：

```
INFO Loaded 3 project skills from .claude/skills/
INFO Loaded 5 global skills from {storage_dir}/skills/
INFO Loaded 2 hooks from .claude/hooks/
INFO Loaded installed plugins (count=1)
INFO Loaded 2 plugin skills
```

### 13.5 目录速查

| 扩展 | 项目级路径 | 全局级路径 |
|------|-----------|-----------|
| Skills | `<working_dir>/.claude/skills/` | `{storage_dir}/skills/` |
| Hooks | `<working_dir>/.claude/hooks/` | `{storage_dir}/hooks/` |
| Plugins | — | `{storage_dir}/plugins/` |

---

## 14. 进度事件

SDK 提供丰富的事件流，应用层（如 TUI）可通过 `AgentEvent` channel 订阅实时进度。

### 14.1 ToolExecutionProgress — 工具执行心跳

当工具执行超过 **5 秒**后，SDK 每 **3 秒**发送一次 `ToolExecutionProgress` 事件。
TUI 可据此显示"正在执行 bash... (12s)"。

```rust
loop {
    match rx.recv().await {
        Some(AgentEvent::ToolExecutionProgress { call_id, tool_name, elapsed_secs }) => {
            // 更新 TUI 状态栏
            status_bar.set(format!("⏳ {tool_name} (running {elapsed_secs}s)"));
        }
        Some(AgentEvent::ToolCallEnd { call_id, .. }) => {
            status_bar.clear();
        }
        _ => {}
    }
}
```

### 14.2 bash stdout 流式输出

`ToolContext` 包含可选 `progress_tx` 通道，bash 工具会将 stdout/stderr 逐行通过该通道发送。
TUI 可在工具执行区域实时滚动显示最新输出。

```rust
// 在 ToolContext 中注入 progress channel
let (progress_tx, mut progress_rx) = tokio::sync::mpsc::channel::<ToolProgressEvent>(64);
let ctx = ToolContext {
    // ... 其他字段 ...
    progress_tx: Some(progress_tx),
};

// TUI 侧消费流式输出
tokio::spawn(async move {
    while let Some(ev) = progress_rx.recv().await {
        match ev {
            ToolProgressEvent::StdoutLine { line, stream } => {
                // 追加到终端输出区域
                terminal_output.push(line);
            }
            ToolProgressEvent::Progress { message, current, total } => {
                // 自定义进度（如文件处理进度）
            }
        }
    }
});
```

### 14.3 PlanProgress — 整体任务进度

当 Agent 通过 `plan` 工具更新计划状态时，SDK 自动发送 `PlanProgress` 事件。
TUI 可渲染步骤进度条。

```rust
Some(AgentEvent::PlanProgress { completed, total, current_item }) => {
    let pct = if total > 0 { (completed * 100 / total) as u32 } else { 0 };
    progress_bar.set(pct, format!("Step {completed}/{total}"));
    if let Some(item) = current_item {
        status_bar.set(format!("Current: {item}"));
    }
}
```

TUI 渲染效果示例：
```
╭─ Plan Progress ──────────────────────────╮
│ [████████████░░░░░░░░░░░░] 3/7 steps (42%)│
│ Current: Implementing cache layer         │
╰──────────────────────────────────────────╯
```

### 14.4 WaitingForModel — 模型等待心跳

当模型响应缓慢时，SDK 每 **8 秒**发送一次 `WaitingForModel` 事件。
比之前的 30 秒间隔大幅缩短，TUI 可据此显示"等待模型中... (8s)"而非静态 spinner。

```rust
Some(AgentEvent::WaitingForModel { elapsed_secs }) => {
    status_bar.set(format!("Waiting for model... ({}s)", elapsed_secs));
}
```

### 14.5 Intent Anchor — 意图锚点

从 Agent 收到的首条用户消息自动作为 Intent Anchor 注入到每轮 dynamic prompt 中，
确保 Agent 不会在多轮对话中"忘记"原始任务。同时驱动 Intent Guard（见 §5.1a）。

应用层无需额外操作——Intent Anchor 由 Harness 自动管理。可通过 Harness 访问：

```rust
let anchor = agent.harness().latest_user_message_text().await;
if let Some(msg) = anchor {
    println!("Current task: {msg}");
}
```

### 14.6 Drift Detection — 意图漂移检测

当 Agent 连续 5 轮以上没有收到新的用户消息时（即 Agent 在自行探索），SDK 每 3 轮
自动注入一条"焦点提醒"软中断，让 Agent 重新审视当前工作是否仍在原始任务范围内。

应用层可通过 `SoftInterruptInjected` 事件感知：
```rust
Some(AgentEvent::SoftInterruptInjected { interrupt }) => {
    if interrupt.urgent {
        notification.show("⚠ Agent may be drifting off-task");
    }
}
```

---

## 15. 故障排查

| 问题 | 可能原因 | 解决方案 |
|------|---------|---------|
| Agent 返回空文本 | 模型未连接 | 检查 API key 和 base URL |
| `BudgetExceeded` 错误 | Token/cost 限额触发 | 提高 `token_budget` 或 `cost_budget_cents` |
| 权限请求无限循环 | 默认策略为 `Confirm` 且无缓存 | 使用 `ApprovalManager` 缓存 |
| 工具调用挂起 | 工具超时 | 设置 `budget.tool_timeout_secs` |
| 记忆不持久化 | `storage_dir` 未设置 | 设置 `FoxAgentSdkConfig.storage_dir` |
| 编译错误：找不到 `enum` | 工具名未注册 | 调用 `.with_default_tools()` 或 `.with_tool(...)` |
| Agent 擅自修改代码（偏离意图） | Intent Guard 未启用或策略过宽 | 确保 `enable_intent_guard: true`；使用"分析"类动词限范围 |
| 长会话后 Agent 忘记原始任务 | Drift Detection 间隔过长 | 调整 `DRIFT_DETECTION_THRESHOLD` / `DRIFT_DETECTION_INTERVAL` |
| TUI 显示卡死无进度 | 工具执行无心跳事件 | 确保订阅 `ToolExecutionProgress` 事件；检查 bash 是否使用流式模式 |
| 大文件读取后 Context 被占满 | read 默认上限仍偏大 | 降低 `DEFAULT_LIMIT`；调整 compaction `token_budget` |
| 工具输出被截断后信息不完整 | 智能截断保留了头部 | 使用 `offset` + `limit` 定向读取被省略的中间段落 |
