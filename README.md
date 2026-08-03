# Fox Agent SDK

**Fox Agent SDK** 是一个基于 Rust 的面向 AI 应用开发的生产级 Agent SDK，提供从快速原型到部署就绪治理的完整生命周期管理。

## 项目简介

本 SDK 践行 **Agent = Model + Harness** 的架构实践，实现了 `Agent`、`Model`、`Harness` 三大核心组件：

- **Agent**（智能体）—— 编排 Agent Loop（感知 → 决策 → 行动 → 观察），支持单轮/流式执行、事件录制与回放
- **Model**（模型）—— 通过统一 Provider 抽象接入 DeepSeek、OpenAI、Anthropic 等多家 LLM，可插拔、可 Mock
- **Harness**（工具箱）—— 封装 Agent 运行所需的基础能力：工具系统、记忆、安全审批、会话持久化、Swarm 多 Agent 协作

核心特性包括：`agent.toml` 声明式配置、跨会话长期记忆（语义召回 + 图结构级联检索）、细粒度权限审批、预算治理与可观测性、Claude Code 兼容的 Skills 技能系统、事件录制与回放、多 Agent 协调。基于 `tokio` 异步运行时，全链路非阻塞 I/O，`MockProvider` 支持确定性测试。

## 安装

```toml
[dependencies]
fox-agent-sdk = "0.1.0"
```

## 快速开始

```rust
use fox_agent_sdk::{AgentBuilder, AgentEvent, ProviderConfig, TurnOutcome};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("DEEPSEEK_API_KEY")?;

    let mut agent = AgentBuilder::new()
        .provider_config(ProviderConfig::deepseek(api_key))
        .model_id("deepseek-v4-flash")
        .with_default_tools()
        .build()
        .await?;

    let outcome = agent
        .run_once("What's the weather like in Tokyo?")
        .await?;

    match outcome {
        TurnOutcome::Completed { text } => println!("{}", text),
        TurnOutcome::RequiresUserDecision { request } => {
            println!("Permission needed: {}", request.prompt);
        }
        _ => {}
    }

    Ok(())
}
```

## 架构

```
fox-agent-sdk (门面)
├── fox-agent-core        # Provider、Model、Agent Loop、Event 类型、Config
├── fox-agent-providers   # DeepSeek、OpenAI、Anthropic、Mock
├── fox-agent-tools       # 内置工具 (bash、read、write、todo、plan、goal...)
└── fox-agent-swarm       # 多 Agent 协调器、监管器、重试
```

## 功能特性

### Builder API

几行代码即可初始化完整配置的 Agent，全部提供合理默认值：

```rust
AgentBuilder::new()
    .provider_config(ProviderConfig::deepseek(key))
    .model_id("deepseek-v4-flash")
    .working_dir(".")
    .with_default_tools()
    .with_safety_policy(SafetyConfig::default())
    // 非编程 Agent： .with_system_prompt("你是一个客服助手...")
    .build()
    .await?;
```

### agent.toml 配置驱动

所有组件（Provider、Model、Memory、Safety、MCP、Skills、Plugins、Compaction）均可通过
`agent.toml` 声明式配置，`AgentBuilder::sdk_config(cfg)` 一键注入。无需在代码中逐项设置。

```rust
let cfg = FoxAgentSdkConfig::load_from_file("agent.toml")?;
let mut agent = AgentBuilder::new()
    .sdk_config(cfg)
    .provider_config(ProviderConfig::deepseek(key))
    .build()
    .await?;
```

### 记忆系统

跨会话长期记忆，支持语义召回（embedding 向量）和图结构级联检索：

- **三级作用域** — Session（临时草稿）/ Project（项目知识库）/ Global（用户偏好），物理文件隔离
- **自动提取** — LLM 从对话中抽取候选记忆 → 去重 → 冲突检测 → 写入
- **语义召回** — Cascade 模式（Semantic + Keyword + BFS 图扩展），可选 HNSW ANN 加速
- **上下文注入** — 每轮开始前后台异步召回，按 category 分组 + 字符/条数预算控制后注入 `dynamic_part`
- **自动提升** — 跨多轮被反复强化的 Session 记忆自动提升到 Project/Global
- **治理** — 保留策略、大小限制、模型变化自动重嵌、审计日志、GC 清理

```toml
[memory]
enabled = true
auto_extract = true
auto_extract_scope = "Project"
injection_max_chars = 1500
injection_max_per_category = 3
```

### 会话管理

自动持久化与恢复：

- **auto_snapshot** — 每轮关键节点（用户消息、turn 完成、权限挂起）自动保存完整会话快照
- **异步写入** — snapshot 序列化 + 文件 I/O 在后台 `tokio::spawn` 中执行，不阻塞 Agent Loop
- **完整恢复** — 快照包含消息历史、模型运行时状态、挂起权限、中断队列，支持跨重启恢复
- **存储路径** — `{storage_dir}/sessions/{session_id}.json`（JSON pretty 格式）

```toml
auto_snapshot = true              # 默认开启
storage_dir = ".fox-agent-sdk"    # 所有持久化数据的根目录
```

### 多 Provider 支持

| Provider   | `provider_name` | 构造函数 |
|------------|-----------------|----------|
| DeepSeek   | `deepseek`      | `ProviderConfig::deepseek(key)` |
| OpenAI     | `openai`        | `ProviderConfig::new("openai", base_url, key)` |
| Anthropic  | `anthropic`     | `ProviderConfig::new("anthropic", base_url, key)` |
| Mock       | N/A             | `builder.with_provider(Arc::new(MockProvider::new("mock")))` |

### 工具系统

**内置工具**，通过 `with_default_tools()` 注册：

- `read` / `write` / `edit` — 文件操作
- `bash` — Shell 命令执行（沙箱约束）
- `grep` / `glob` — 代码搜索
- `todo` / `plan` / `goal` — 规划与任务跟踪
- `memory` — 跨会话学习
- `skill` — 按需加载领域专业知识（兼容 Claude Code 格式）

**自定义工具**，通过 `Tool` trait 实现：

```rust
struct MyTool;

#[async_trait]
impl Tool for MyTool {
    fn name(&self) -> &str { "my_tool" }
    fn description(&self) -> &str { "做一些有用的事情" }
    fn parameters_schema(&self) -> Value {
        json!({"type": "object", "properties": {...}})
    }
    async fn execute(
        &self,
        input: Value,
        ctx: ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        // ... 你的逻辑 ...
        Ok(ToolOutput {
            text: "done".into(),
            is_error: false,
            json: None,
        })
    }
}

// 注册
builder.with_tool(Arc::new(MyTool));
```

### Skills 技能系统（兼容 Claude Code）

按需加载的领域专业知识，采用 Claude Code 兼容的技能文件格式。技能**不会**预加载到系统提示词中，Agent 仅在需要时激活，避免 prompt 膨胀。

**技能文件格式**（`.md` 文件，放在 `.claude/skills/` 下）：

```markdown
---
name: pdf
description: PDF manipulation expert
allowed-tools: [read, write, bash]
model: claude-sonnet-4-20250514
---

You are a PDF expert. When asked about PDF files:

## Instructions
1. First read the file to understand its structure.
2. Plan the changes before writing.
```

Agent 通过内置 `skill` 工具发现并激活技能：

- `skill(action="list")` — 查看所有可用技能
- `skill(action="activate", name="pdf")` — 加载某个技能的专业知识
- `skill(action="deactivate")` — 卸载当前技能

```rust
// 使用 with_default_tools() 时自动加载技能：
AgentBuilder::new()
    .provider_config(ProviderConfig::deepseek(key))
    .model_id("deepseek-v4-flash")
    .working_dir(".")             // 扫描 .claude/skills/*.md
    .with_default_tools()         // 自动注册 SkillTool
    .build()
    .await?;
```

Claude Code 项目的技能文件可直接使用，无需转换。

### 权限与审批

细粒度访问控制，支持缓存和审计：

```rust
let safety = SafetyConfig {
    default_policy: DefaultSafetyPolicy::Confirm, // 默认询问用户
    tool_denylist: Some(vec!["delete".into()]),    // 始终阻止
    tool_allowlist: Some(vec!["read".into()]),      // 始终允许
    ..Default::default()
};

// 审批缓存：在会话/工作区范围内跳过重复询问
let approval = ApprovalManager::new("session-1", safety);
approval.cache_decision(
    "read",
    &PermissionResult::Allow,
    ApprovalScope::ThisSession,
).await;
```

### 事件录制与回放

捕获每一轮执行用于调试、审计或 CI。录制时订阅 AgentEvent channel，
所有事件自动序列化到 JSONL：

```rust
let (tx, rx) = mpsc::channel::<AgentEvent>(64);
let recorder = Arc::new(EventRecorder::new("session-1", 1));

// 后台 task 消费事件并写入文件
let rec = recorder.clone();
let output = PathBuf::from("events.jsonl");
tokio::spawn(async move { rec.run(rx, Some(output)).await });

// 执行 Agent（tx 同时用于 Streaming 和 Recorder）
agent.run_once_streaming("你好", &tx).await?;
drop(tx); // 关闭 channel 后 run() 自动退出

// 回放
let loaded = EventRecorder::load_from_file(&PathBuf::from("events.jsonl")).unwrap();
```

事件导出中的密钥信息自动脱敏（`[REDACTED]`、`[API_KEY]`、`[JWT]`）。

### 治理与可观测性

运行时预算强制执行和指标采集：

```rust
let guard = GovernanceGuard::new(BudgetConfig {
    token_budget: Some(1_000_000),
    cost_budget_cents: Some(5000),
    tool_timeout_secs: 30,
    ..Default::default()
});

guard.add_metrics_hook(|metrics| {
    println!("Tokens: {}, Cost: {}c, Errors: {}%",
        metrics.total_tokens,
        metrics.estimated_cost_cents,
        metrics.tool_error_rate() * 100.0,
    );
}).await;
```

### Swarm 多 Agent 协作

通过监管器协调多个 Agent：

```rust
let coordinator = Arc::new(SwarmCoordinator::new());
let supervisor = SwarmSupervisor::with_defaults(coordinator.clone());

coordinator.spawn("worker-1", "researcher").await;
coordinator.spawn("worker-2", "coder").await;

coordinator.upsert_plan(vec![
    PlanItem { id: "t1".into(), content: "Research".into(), status: PlanStatus::Pending, ... },
    PlanItem { id: "t2".into(), content: "Implement".into(), status: PlanStatus::Pending, ... },
]);
```

监管器自动处理健康检查、超时、重试和任务重分配。

## 运行示例

```bash
# 使用 DeepSeek 的单 Agent 示例
DEEPSEEK_API_KEY="sk-xxx" cargo run --example simple_agent

# 通用 Agent（使用自定义系统提示词的客服机器人）
cargo run --example general_agent

# 权限审批流程
cargo run --example permission_flow

# 治理与预算控制
cargo run --example governance

# 事件录制与回放
cargo run --example event_replay

# Web 工具验证（websearch + webfetch）
cargo run --example web_tools
# 启用实时网络调用
WEB_TOOLS_LIVE=1 cargo run --example web_tools

# Swarm 多 Agent 演示
cargo run --example swarm_workflow

# 自定义工具注册
cargo run --example custom_tool
```

## 测试

### 单元测试

```bash
# 全部单元测试
cargo test

# 按 crate 运行
cargo test -p fox-agent-core
cargo test -p fox-agent-sdk
cargo test -p fox-agent-tools
```

### 按评估维度运行测试

评估体系按四层金字塔组织，详见 [docs/evaluation_design.md](docs/evaluation_design.md)。

#### 1. 性能基准（Performance）—— 测"框架本身快不快"

```bash
# Agent 端到端延迟基准（MockProvider，排除 LLM 网络延迟）
cargo bench --bench agent_bench

# 工具执行耗时基准
cargo bench --bench tool_bench

# Chrome Trace 火焰图（在 chrome://tracing 中可视化）
$env:BENCH_TRACE_DIR = "./target/criterion"
cargo bench --bench agent_bench
# 打开 target/criterion/bench-trace.json
```

#### 2. 质量回归（Quality）—— 测"框架跑得对不对"

```bash
# Goldenscript Golden Master 测试（.gs 用例回放）
cargo test --test golden_transcripts

# Goldenscript 录制/更新黄金文件（二选一）
#   Bash / Linux:
UPDATE_GOLDENFILES=1 cargo test --test golden_transcripts
#   Windows PowerShell:
#   $env:UPDATE_GOLDENFILES = "1"
#   cargo test --test golden_transcripts
#   Remove-Item Env:\UPDATE_GOLDENFILES   # 清除变量（回到验证模式）

# 深度场景测试（MockProvider 驱动完整评估管线）
cargo test --test scenario_tests -- --nocapture

# 行为规则检查（重复工具调用、孤儿输出、Deny 后重试等）
cargo test --test behavior_rules

# 自定义任务 + 物证断言
cargo test --test custom_tasks

# LLM-as-Judge 质量评分（prompt 构建 + 解析 + 加权平均）
cargo test --test quality_judge
```

#### 3. Token 效率（Efficiency）—— 测"烧了多少钱"

```bash
# Token 消耗追踪：输入/输出 token、缓存命中率、压实统计
cargo test --test token_tracking -- --nocapture
```

#### 4. 健壮性（Robustness）—— 测"会不会崩溃"

```bash
# 模糊测试：畸形 JSON、超大输出、随机超时等对抗性输入
cargo test --test proptest
```

#### SWE-bench 基准（Phase 2，需真实 LLM + feature gate）

```bash
# SWE-bench 数据加载器（无需真实 LLM）
cargo test --test swe_bench_loader

# SWE-bench 评估流程（数据结构验证，无需真实 LLM）
cargo test --test swe_bench_eval

# SWE-bench 批量评估（需 swe_bench feature + 真实 LLM）
cargo test --features swe_bench --test swe_bench_batch -- --ignored
```

## API 概览

| 模块 | 关键类型 | 用途 |
|------|---------|------|
| `AgentBuilder` | `builder::AgentBuilder` | 一行代码构建 Agent |
| `Agent` | `agent::Agent` | 运行单轮或流式 Agent |
| `Harness` | `harness::Harness` | 工具/安全/记忆/压缩容器 |
| `GovernanceGuard` | `governance::GovernanceGuard` | 预算、指标、成本跟踪 |
| `EventRecorder` | `event_recorder::EventRecorder` | JSONL 导出与回放 |
| `ApprovalManager` | `approval_manager::ApprovalManager` | 三层缓存、超时自动拒绝、审计 |
| `ReplayRunner` | `replay_runner::ReplayRunner` | 黄金转录验证 |
| `SwarmSupervisor` | `swarm::SwarmSupervisor` | 健康检查、重试、重分配、报告 |
| `MemoryManager` | `memory::MemoryManager` | 三级作用域记忆、语义召回、注入管线 |
| `mask_secrets` | `scrub::mask_secrets` | 事件/日志导出的密钥脱敏 |

## 非功能性特性

- **异步优先** — 所有 I/O 和 LLM 调用均为非阻塞，基于 `tokio`
- **可测试** — `MockProvider` 提供确定性单元/集成测试
- **可观测** — 结构化事件、指标钩子、预算强制执行
- **安全** — 权限工作流（denylist/allowlist）、密钥脱敏
- **可回放** — 黄金转录回放，用于 CI 回归测试

## 环境要求

- Rust 2024 版本（1.85+）
- `tokio` 异步运行时
- 真实 Provider 需要 `DEEPSEEK_API_KEY` / `OPENAI_API_KEY` / `ANTHROPIC_API_KEY`

## 许可

MIT License
