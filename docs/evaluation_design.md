# Fox Agent SDK 基准评估测试方案

> **目标**：为 `fox-agent-sdk`（框架层）建立统一的评估体系，覆盖**框架正确性、任务完成质量、Token 效率、性能基准**四个维度，支持 CI 回归检测与研发期性能剖析。
>
> **原则**：自研评估组件为主，外部 crates（`goldenscript`）为补充。不引入与 fox-agent-sdk 自有 Agent 抽象冲突的外部框架。

---

## 1. 整体架构

### 1.1 评估模型

```
┌─────────────────────────────────────────────────────────────────┐
│                     fox-agent-sdk (框架层)                      │
├─────────────────────────────────────────────────────────────────┤
│                        评估管线                                 │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────────┐ │
│  │ goldenscript │  │ 自研 TaskJudge│  │ 自研 BehaviorRules  │ │
│  │ (用例管理)    │  │(LLM-as-Judge)│  │ (确定性断言)         │ │
│  └──────────────┘  └──────────────┘  └──────────────────────┘ │
│  ┌──────────────┐  ┌──────────────┐                           │
│  │EventRecorder │  │ReplayRunner  │                           │
│  │ (事件录制)    │  │ (事件回放)    │                           │
│  └──────────────┘  └──────────────┘                           │
└─────────────────────────────────────────────────────────────────┘
```

### 1.2 各组件职责

| 组件 | 角色 | 核心用途 |
|-------|------|----------|
| **`goldenscript`**（外部 crate） | 用例管理 | 数据驱动的 Golden Master 测试，管理 `.gs` 用例文本，支持 `UPDATE_GOLDENFILES=1` 录金 |
| **`TaskJudge`**（自研，`eval/judge.rs`） | LLM 评分 | LLM-as-Judge 主观维度评分（方案合理性、错误恢复、冗余度） |
| **`BehaviorRuleEngine`**（自研，`eval/behavior_rules.rs`） | 确定性断言 | 规则引擎检查违规行为（重复工具调用、孤儿输出、Deny 后重试） |
| **`EventRecorder`**（自研，`event_recorder.rs`） | 事件录制 | Agent 执行流录制为结构化 JSONL |
| **`ReplayRunner`**（自研，`replay_runner.rs`） | 事件回放 | 从录制文件回放并比对差异 |
| **`criterion`**（外部 crate） | 性能基准 | 微基准测试（冷/热启动、工具延迟、并发吞吐） |
| **`proptest`**（外部 crate） | 对抗输入 | 随机输入健壮性测试 |

### 1.3 现有组件的处置

自研评估组件**保留并增强**，外部 crates 作为补充而非替代：

| 现有组件 | 处置 | 说明 |
|----------|------|------|
| `crates/fox-agent-sdk/src/eval/judge.rs`（自研 `TaskJudge`） | **保留** | 已有 LLM-as-Judge 实现，持续迭代，不引入外部替代 |
| `crates/fox-agent-sdk/src/eval/behavior_rules.rs`（自研规则引擎） | **保留** | 已有确定性断言能力，持续迭代 |
| `crates/fox-agent-sdk/src/event_recorder.rs` + `replay_runner.rs` | **保留** | 已有事件录制与回放能力，与 `goldenscript` 互补 |
| `benches/agent_bench.rs` + `tool_bench.rs`（criterion） | **保留** | 性能基准层继续使用 criterion |
| `tests/proptest.rs`（proptest） | **保留** | 健壮性对抗输入测试 |
| `tests/fixtures/transcripts/*.jsonl` | **保留** | 迁移至 `goldenscript` `.gs` 格式作为补充用例格式 |

---

## 2. 评估维度与指标

### 2.1 框架正确性（Framework Correctness）— 测 `fox-agent-sdk`

使用 `goldenscript` + 自研 `BehaviorRuleEngine` + `ReplayRunner` 组合，验证 SDK 自身的正确性。

| 测试类型 | 工具 | 验证内容 |
|----------|------|----------|
| Golden Transcript 回放 | `goldenscript` / `ReplayRunner` | 事件流序列一致性、工具调用路由正确性 |
| 行为规则检查 | `BehaviorRuleEngine` | 重复工具调用、孤儿输出、Deny 后重试等违规行为 |
| 端到端状态验证 | `BehaviorRuleEngine` + 物证断言 | 文件存在性、内容匹配、编译通过等客观产物验证 |

### 2.2 任务完成质量（Task Quality）— 测基于 SDK 构建的 Agent

| 评估方式 | 工具 | 说明 |
|----------|------|------|
| SWE-bench 基准 | 自研 SWE-bench 适配层 | 用 SDK 构建的 coding agent 在真实 GitHub Issue 上评估补丁生成能力 |
| 确定性物证断言 | `BehaviorRuleEngine` | 文件、编译、测试等客观状态验证 |
| LLM-as-Judge | `TaskJudge` | 方案合理性、错误恢复、冗余度等主观维度评分 |

### 2.3 Token 效率（Token Efficiency）

| 指标 | 数据来源 | 说明 |
|------|----------|------|
| 任务 Token 总量 | `swe-bench-adapter` Stream Parser | 从 Agent 执行流中提取 token 消耗 |
| 压实压缩率 | SDK 内置 `TokenReport` | 压实前后 context tokens 对比 |
| 冗余工具调用比例 | 自研轨迹分析 | 未产生有效结果的工具调用占比 |

### 2.4 性能基准（Performance）

保留现有 `criterion` 基准（`benches/agent_bench.rs`），结合超时机制测量框架纯开销。

| 指标 | 测量方式 | 目标 |
|------|----------|------|
| 端到端延迟（冷/热启动） | criterion + MockProvider | P50 < 200ms（冷）/ 100ms（热） |
| 工具执行 P50/P95/P99 | 按工具名分组 | 识别高延迟工具 |
| 框架开销 | 排除 LLM 网络延迟 | 纯 Rust 路径 |
| 并发吞吐 | N agent 并行 QPS | 评估 `tool_slots()` 有效性 |

---

## 3. 分阶段实施方案

### Phase 0: 基础设施搭建（0.5 天）

**目标**：建立评估框架骨架，确保 `goldenscript` 可正常集成并编写用例。

#### 3.1 添加依赖

```toml
# Cargo.toml（根 workspace）
[dev-dependencies]
goldenscript = "0.8"

[build-dependencies]
# 无需额外 build-dependency（goldenscript 通过 test_each_path 宏发现用例）
```

> 现有 dev-dependencies（criterion、proptest、tempfile）保留不动。

#### 3.2 创建首个 Goldenscript 用例

```text
# tests/fixtures/transcripts/001_create_file.gs
# 场景：Agent 创建文件
# 前置条件：工作目录为空

run_agent "创建一个名为 hello.txt 的文件，内容为 'Hello World'"
assert_file_exists "hello.txt"
assert_file_contains "hello.txt" "Hello World"
```

#### 3.3 Goldenscript Runner 骨架

```rust
// tests/golden_runner.rs
use goldenscript::{Command, Runner};

pub struct AgentRunner {
    // 预留：后续注入 FoxAgentSDK 实例
}

impl Runner for AgentRunner {
    fn run(&mut self, cmd: &Command) -> Result<String, String> {
        match cmd.name() {
            "run_agent" => {
                let _prompt = cmd.args().join(" ");
                // Phase 1 实现：调用 FoxAgentSDK.run_once(&prompt)
                Ok("ok".to_string())
            }
            "assert_file_exists" => {
                let path = cmd.args()[0];
                if std::fs::metadata(path).is_ok() {
                    Ok("✓".to_string())
                } else {
                    Err(format!("File {} does not exist", path))
                }
            }
            "assert_file_contains" => {
                let path = cmd.args()[0];
                let expected = cmd.args()[1];
                let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
                if content.contains(expected) {
                    Ok("✓".to_string())
                } else {
                    Err(format!("File {} does not contain '{}'", path, expected))
                }
            }
            _ => Err(format!("Unknown command: {}", cmd.name())),
        }
    }
}
```

**验收标准**：`cargo test` 能编译通过 goldenscript 相关代码，`UPDATE_GOLDENFILES=1 cargo test` 能生成黄金文件。

### Phase 1: 框架正确性测试 — `fox-agent-sdk`（2-3 天）

**目标**：用 `goldenscript` + `evals` 验证 SDK 框架层正确性。

#### 1.1 Goldenscript 测试用例

将 `tests/fixtures/transcripts/` 下的 JSONL 用例迁移为 `.gs` 用例文本：

```text
# tests/fixtures/transcripts/001_create_file.gs
# 场景：Agent 创建文件
# 前置条件：工作目录为空

run_agent "创建一个名为 hello.txt 的文件，内容为 'Hello World'"
assert_file_exists "hello.txt"
assert_file_contains "hello.txt" "Hello World"
```

#### 1.2 Goldenscript Runner

```rust
// tests/golden_runner.rs
use goldenscript::{Command, Runner};

pub struct AgentRunner {
    sdk: FoxAgentSDK,
}

impl Runner for AgentRunner {
    fn run(&mut self, cmd: &Command) -> Result<String, String> {
        match cmd.name() {
            "run_agent" => {
                let prompt = cmd.args().join(" ");
                let result = self.sdk.run_once(&prompt)?;
                Ok(format!("Success: {:?}", result))
            }
            "assert_file_exists" => {
                let path = cmd.args()[0];
                if std::fs::metadata(path).is_ok() {
                    Ok("✓".to_string())
                } else {
                    Err(format!("File {} does not exist", path))
                }
            }
            "assert_file_contains" => {
                let path = cmd.args()[0];
                let content = cmd.args()[1];
                let file_content = std::fs::read_to_string(path).unwrap();
                if file_content.contains(content) {
                    Ok("✓".to_string())
                } else {
                    Err(format!("File {} does not contain '{}'", path, content))
                }
            }
            _ => Err(format!("Unknown command: {}", cmd.name())),
        }
    }
}
```

#### 1.3 集成 Goldenscript 测试

```rust
// tests/golden_transcripts.rs
use goldenscript::test_each_path;

test_each_path! {
    in "tests/fixtures/transcripts" as path => {
        #[test]
        fn test_golden_transcript() {
            let mut runner = AgentRunner::new();
            goldenscript::run_path(&path, &mut runner).unwrap();
        }
    }
}
```

更新黄金文件：`UPDATE_GOLDENFILES=1 cargo test --test golden_transcripts`

#### 1.4 行为规则检查（自研 `BehaviorRuleEngine`）

既有行为规则引擎（`crates/fox-agent-sdk/src/eval/behavior_rules.rs`）已有违规检测能力，直接在 Goldenscript Runner 中集成调用：

```rust
// tests/golden_runner.rs（扩展示例）
impl Runner for AgentRunner {
    fn run(&mut self, cmd: &Command) -> Result<String, String> {
        match cmd.name() {
            // ... 已有命令 ...
            "assert_no_excessive_tool_calls" => {
                let max = cmd.args()[0].parse::<usize>().unwrap_or(3);
                let violations = self.behavior_engine
                    .check_no_excessive_calls(&self.last_trajectory, max);
                if violations.is_empty() {
                    Ok("✓".to_string())
                } else {
                    Err(format!("Excessive tool calls: {:?}", violations))
                }
            }
            _ => Err(format!("Unknown command: {}", cmd.name())),
        }
    }
}
```

### Phase 2: 任务完成质量测试 — 基于 SDK 构建的 Agent（3-5 天）

**目标**：在真实软件工程任务上评估基于 SDK 构建的 Agent 的完成质量。

#### 2.1 SWE-bench 数据加载

自研 SWE-bench 数据加载器，加载 JSON/JSONL 格式的 SWE-bench Lite 数据集：

```rust
// tests/swe_bench_loader.rs
pub struct SweBenchLoader {
    dataset_path: PathBuf,
}

impl SweBenchLoader {
    pub fn load_lite(&self) -> anyhow::Result<Vec<SweBenchTask>> {
        // 解析 SWE-bench Lite JSONL 数据集
        // 返回任务列表（实例 ID、仓库、Issue 描述、难度分级）
        todo!()
    }
}
```

#### 2.2 SWE-bench 评估流程

```rust
// tests/swe_bench_eval.rs
#[tokio::test]
#[cfg(feature = "swe_bench")]
async fn test_swe_bench_instance() -> anyhow::Result<()> {
    let loader = SweBenchLoader::new("data/swe-bench-lite.jsonl")?;
    let instance = loader.load_instance("django__django-12345")?;
    let prompt = instance.generate_prompt()?;

    // 用 SDK 构建 coding agent 并运行
    let agent = AgentBuilder::new()
        .with_provider(provider)
        .with_tool(Arc::new(BashTool))
        .with_tool(Arc::new(ReadTool))
        .with_tool(Arc::new(EditTool))
        .build()
        .await?;
    let patch = agent.run_once(&prompt).await?;

    let result = evaluate_patch_with_test_suite(&instance, &patch).await?;
    assert!(result.passed);
    Ok(())
}
```

#### 2.3 批量 SWE-bench 评估

```rust
// tests/swe_bench_batch.rs
#[cfg(feature = "swe_bench")]
#[tokio::test]
async fn run_swe_bench_batch() -> anyhow::Result<()> {
    let loader = SweBenchLoader::new("data/swe-bench-lite.jsonl")?;
    let instances = loader.load_all()?;
    let mut report = BatchReport::new();

    for instance in &instances {
        let patch = agent.run_once(&instance.generate_prompt()?).await?;
        let result = evaluate_patch_with_test_suite(&instance, &patch).await?;
        report.add_result(instance.id.clone(), result);
    }

    println!("通过率: {:.2}%", report.pass_rate() * 100.0);
    Ok(())
}
```

#### 2.4 自定义任务评估（物证断言）

利用自研 `BehaviorRuleEngine` 做确定性物证断言：

```rust
// tests/custom_tasks.rs
#[tokio::test]
async fn test_create_rust_project() -> anyhow::Result<()> {
    let agent = AgentBuilder::new().with_provider(provider).build().await?;
    let ctx = agent.run_once("创建一个名为 git-summary 的 Rust 项目，依赖 git2 crate，编写一个能打印最新 commit 信息的程序").await?;

    // 物证断言
    let cwd = ctx.working_dir();
    assert!(cwd.join("git-summary/Cargo.toml").exists(), "Cargo.toml should exist");
    assert!(cwd.join("git-summary/src/main.rs").exists(), "main.rs should exist");

    let output = std::process::Command::new("cargo")
        .arg("build")
        .current_dir(cwd.join("git-summary"))
        .output()?;
    assert!(output.status.success(), "cargo build should succeed");

    Ok(())
}
```

#### 2.5 LLM-as-Judge（自研 `TaskJudge`）

利用自研 `TaskJudge` 做主观维度评分：

```rust
// tests/quality_judge.rs
#[tokio::test]
async fn test_code_quality() -> anyhow::Result<()> {
    let agent = AgentBuilder::new().with_provider(provider).build().await?;
    let result = agent.run_once("实现一个 LRU Cache").await?;

    let judge = TaskJudge::new(judge_provider);
    let score = judge.evaluate(
        "请评估以下代码的质量（1-5分）：\n维度：正确性、可读性、性能",
        &result.final_reply.unwrap()
    ).await?;

    assert!(score >= 3.0, "Quality score should be at least 3");
    Ok(())
}
```

### Phase 3: Token 效率追踪（0.5-1 天）

利用自研 Stream Parser 提取 token 消耗，聚合现有 `TokenUsage`：

```rust
// tests/token_tracking.rs
#[tokio::test]
async fn track_token_efficiency() -> anyhow::Result<()> {
    let mut analyzer = StreamAnalyzer::new();

    let (tx, mut rx) = tokio::sync::mpsc::channel(100);
    let agent = AgentBuilder::new().with_provider(provider).build().await?;
    tokio::spawn(async move {
        agent.run_once_streaming("solve task", &tx).await
    });

    while let Some(chunk) = rx.recv().await {
        analyzer.parse_chunk(&chunk)?;
    }

    let metrics = analyzer.metrics();
    println!("Input tokens: {}", metrics.input_tokens);
    println!("Output tokens: {}", metrics.output_tokens);
    println!("Tool calls: {}", metrics.tool_calls);
    Ok(())
}
```

### Phase 4: CI 集成（0.5 天）

```yaml
# .github/workflows/evaluation.yml
name: Evaluation

on:
  push:
    branches: [main]
  pull_request:

jobs:
  framework-tests:          # 框架正确性（必须通过）
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo test --test golden_transcripts
      - run: cargo test --test behavior_rules

  swe-bench:                # SWE-bench（耗时较长，仅 main）
    runs-on: ubuntu-latest
    if: github.event_name == 'push' && github.ref == 'refs/heads/main'
    steps:
      - uses: actions/checkout@v4
      - run: cargo test --features swe_bench --test swe_bench_batch -- --ignored
      - uses: actions/upload-artifact@v4
        with:
          name: swe-bench-results
          path: target/swe-bench-results.json

  benchmark:                # 性能基准（criterion）
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: cargo bench --bench agent_bench
      - uses: benchmark-action/github-action-benchmark@v1
        with:
          tool: 'cargo'
          output-file-path: target/criterion/
          alert-threshold: '130%'
```

---

## 4. 目录结构规划

```
fox-agent-sdk/
├── Cargo.toml
├── benches/                          # 已有（保留）
│   ├── agent_bench.rs
│   ├── tool_bench.rs
│   └── harness.rs
├── tests/
│   ├── golden_runner.rs              # Phase 0-1: goldenscript Runner 实现
│   ├── golden_transcripts.rs         # Phase 1: goldenscript 测试入口（test_each_path! + mock_scripts）
│   ├── scenario_tests.rs             # Phase 1-2: 深度场景测试（15 个 MockProvider 驱动的评估管线测试）
│   ├── behavior_rules.rs             # Phase 1: 行为规则测试（调用自研 BehaviorRuleEngine）
│   ├── custom_tasks.rs               # Phase 2: 自定义任务 + 物证断言
│   ├── swe_bench_loader.rs           # Phase 2: SWE-bench 数据加载器
│   ├── swe_bench_eval.rs             # Phase 2: SWE-bench 评估流程
│   ├── swe_bench_batch.rs            # Phase 2: 批量 SWE-bench
│   ├── quality_judge.rs              # Phase 2: LLM-as-Judge 评测
│   ├── token_tracking.rs             # Phase 3: Token 效率追踪
│   ├── proptest.rs                   # 已有（保留）
│   └── fixtures/
│       └── transcripts/              # Phase 1: goldenscript 用例（.gs 格式）
│           ├── 001_smoke.gs
│           ├── 002_create_file.gs
│           ├── 003_multi_step.gs
│           └── ...
└── crates/
    └── fox-agent-sdk/src/eval/       # 自研评估组件（保留并增强）
        ├── judge.rs                  #   TaskJudge：LLM-as-Judge 评分
        ├── behavior_rules.rs         #   BehaviorRuleEngine：确定性断言
        └── mod.rs
```

---

## 5. 测试用例清单

### 5.1 Goldenscript 框架测试（Phase 1）

| ID | 场景 | 验证点 |
|----|------|--------|
| 001 | 单文件创建 | 文件存在、内容正确 |
| 002 | 多文件编辑 | 编辑顺序、内容正确 |
| 003 | 文件读取 | 读取内容匹配 |
| 004 | Bash 命令执行 | 命令输出正确 |
| 005 | 工具调用序列 | 事件流顺序一致 |
| 006 | 错误诊断与修复 | 错误→修复→验证循环 |
| 007 | 权限拒绝恢复 | Deny 后不循环 |
| 008 | 压实后行为一致 | 压实不丢失关键上下文 |

### 5.2 SWE-bench 任务（Phase 2）

使用 `ruvllm` 加载 SWE-bench Lite 的 300+ Python 任务，按 `swe-bench-adapter` 难度分级：

| 难度 | 数量（预估） | 用途 |
|------|-------------|------|
| Easy | ~50 | 快速验证、CI 门禁 |
| Medium | ~100 | 日常回归测试 |
| Hard | ~100 | 周度性能评估 |
| Expert | ~50 | 版本发布前评估 |

### 5.3 自定义质量任务（Phase 2）

| ID | 场景 | 评估方式 |
|----|------|----------|
| Q001 | 创建 Rust 项目并编译 | 物证断言 (predicate) |
| Q002 | 代码搜索与重构 | 物证断言 + LLM-as-Judge |
| Q003 | Git 操作与日志分析 | 物证断言 |
| Q004 | 错误诊断与修复 | 行为规则检查 |
| Q005 | 长对话压实 | TokenReport 验证 |

---

## 6. 预期产出

| Phase | 产出物 | 验收标准 |
|-------|--------|----------|
| Phase 0 | goldenscript 依赖配置、Runner 骨架、首个 `.gs` 用例 | `UPDATE_GOLDENFILES=1 cargo test` 能生成黄金文件 |
| Phase 1 | 10+ goldenscript 用例、行为规则测试 | `cargo test` 全部通过 |
| Phase 2 | SWE-bench 评估脚本、自定义任务 | 能运行 SWE-bench Lite 并输出报告 |
| Phase 3 | Token 效率报告 | 每次评估输出 token 消耗统计 |
| Phase 4 | CI 配置文件 | PR 自动运行评估，性能回归报警 |

---

## 7. 风险与注意事项

1. **Goldenscript 版本兼容性**：`goldenscript` 0.8 为较新版本，Phase 0 需验证 API 可用性，必要时按实际 API 调整文档代码。
2. **SWE-bench 数据泄露**：部分实例的镜像仓库可能包含未来修复提交，需确保评估时从 Git 历史剥离这些提交。
3. **Goldenscript 维护成本**：LLM 行为随模型版本变化，黄金文件需定期更新。建议按模型版本组织目录。
4. **评估耗时**：SWE-bench 完整评估可能需数小时。CI 仅运行 Easy 子集（~50 个任务），完整评估在 nightly 或 release 前运行。
5. **沙箱环境**：SWE-bench 补丁评估需沙箱执行测试，确保 CI runner 有足够资源（Docker、磁盘）。
6. **不依赖外部服务约束**：框架正确性测试使用 `MockProvider` 与本地模型，不发起真实 API 调用；SWE-bench 评估仅在显式 feature / 独立 job 中启用。

---

## 8. 已知限制（Phase 1-3 实现后）

以下是 Phase 1-3 实现过程中发现的已知限制，供后续迭代参考。

### 8.1 `no_retry_after_deny` 规则 ~已修复~

**修复前**：`check_retry_after_deny` 中 `ToolCallEnd` 分支为空，`last_denied_tool` 从未被设置，规则永不触发。

**修复方案**：引入 `call_id → name` HashMap，在 `ToolCallStart` 时记录映射，在 `ToolCallEnd` 时通过 `call_id` 查找工具名并设置 `last_denied_tool`。

```rust
// crates/fox-agent-sdk/src/eval/behavior_rules.rs
fn check_retry_after_deny(events: &[AgentEvent]) -> Vec<RuleViolation> {
    let mut tool_names: HashMap<String, String> = HashMap::new();
    // ...
    AgentEvent::ToolCallStart { call_id, name, .. } => {
        tool_names.insert(call_id.clone(), name.clone()); // 记录映射
    }
    AgentEvent::ToolCallEnd { call_id, output, .. } if output.is_error => {
        if output.text.contains("denied") || ... {
            if let Some(name) = tool_names.get(call_id) {
                last_denied_tool = Some(name.clone()); // 通过 call_id 查找
            }
        }
    }
}
```

### 8.2 Goldenscript `run_agent` 强依赖 MockProvider — **已修复**

**修复方案**：在 [golden_integration.rs](file:///d:/ws/ai/fox-agent-sdk/tests/golden_integration.rs) 中为每个 `.gs` 用例注册对应预录脚本。

**关键发现 - Agent 循环多次调用 `complete()`**：Agent 的 `run_turn_streaming` 在每次收到 ToolUse 后执行工具，然后再次调用 `provider.complete()` 获取后续响应。因此 N 次工具调用需要 N+1 个 mock 脚本（按 FIFO 队列逐一消费）。例如：
- `002_create_file`（1 次 write 调用）：2 个脚本（tool_use → 最终响应）
- `003_multi_step`（2 次工具调用：read + write）：3 个脚本（read → write → 最终响应）

**当前状态**：`golden_integration.rs` 中的 `mock_scripts()` 为所有 3 个 `.gs` 用例注册了对应的预录脚本，7 个测试全部通过（`cargo test --test golden_integration`）。

### 8.3 SWE-bench 和 TaskJudge 依赖真实 LLM

**现象**：`swe_bench_batch` 和 `task_judge_e2e` 测试需要真实 LLM provider，暂无法在纯 MockProvider 环境运行。

**应对**：通过 Cargo features 隔离：

```toml
[features]
swe_bench = []       # 启用 SWE-bench 批量评估
quality_judge = []   # 启用 TaskJudge 端到端测试
```

- `#[cfg(feature = "swe_bench")]` — `swe_bench_batch` 中的批量评估测试
- `#[cfg(feature = "quality_judge")]` — `quality_judge` 中的 `task_judge_e2e` 测试
- 两个 feature 下的测试均标记 `#[ignore]`，需显式 `-- --ignored` 执行

**影响**：
- `cargo test` 默认跳过这两个测试（不影响 CI 门禁）
- 需要额外配置 `cargo test --features swe_bench -- --ignored` 才能启用

**建议**：Phase 2 后续可为 TaskJudge 集成一个轻量本地 Judge 模型（如 Ollama + llama3.2:1b），减少对云端 LLM 的依赖。
