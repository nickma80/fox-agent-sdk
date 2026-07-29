# Fox Agent SDK 基准测试与评估方案

> 目标：为 `fox-agent-sdk` 建立系统化的评估体系，覆盖性能基准、任务完成质量、Token 效率与健壮性四个维度，支持 CI 回归检测与研发期性能剖析。

---

## 1. 背景与现状

### 1.1 当前测试覆盖

| 层次 | 状态 | 说明 |
|------|------|------|
| 单元测试 | 良好 | 38 个 `#[cfg(test)]` 模块，642 次断言，覆盖所有 crate |
| 集成测试 | 有 | `MockProvider` 提供确定性 LLM mock，`foxtests` feature gate 管理 |
| 事件录制/回放 | 有 | `EventRecorder` + `ReplayRunner` + `GoldenTranscript` / `TranscriptCheck` |
| 性能基准 | 无 | 无 `benches/` 目录，无 criterion 依赖 |
| 延迟追踪 | 仪表化但未启用 | 生产代码中 `tracing` 已埋点，但无 subscriber 配置 |
| 模糊测试 | 无 | 无 proptest / fuzz 依赖 |

现有代码入口：

- `crates/fox-agent-sdk/src/event_recorder.rs` — 事件录制
- `crates/fox-agent-sdk/src/replay_runner.rs` — 事件回放与 TranscriptCheck 断言
- `crates/fox-agent-sdk/src/governance.rs` — GovernanceMetrics（仅做操作计数）
- `crates/fox-agent-core/src/event.rs` — TokenUsage 结构体
- `crates/fox-agent-providers/src/mock.rs` — MockProvider

### 1.2 当前问题

- **无性能基线**：任何代码改动都无法通过 CI 检测性能退化
- **延迟不可观测**：`tracing` 已埋点但无 subscriber，无法生成火焰图或 span 视图
- **回放基础设施未标准化**：`GoldenTranscript` 已有，但无规范化的测试用例集
- **Token 成本无感知**：`TokenUsage` 有数据但无汇总报告，研发期无法量化每次改动的 token 消耗变化
- **健壮性边界不明**：没有对抗性输入测试，不清楚框架对 LLM 畸形输出的抵抗能力

---

## 2. 设计逻辑

> 核心思想：**分层防御 + 因果解耦**——把模型的不确定性关在笼子里，让框架的确定性严丝合缝。

本方案不是单一测试方法的堆砌，而是一套**"测速 + 测准 + 测智 + 测稳"**的四维立体防御体系。其底层依据是**关注点分离（Separation of Concerns）**：将"框架稳定性"、"任务完成度"和"模型智能质量"解耦，分别用不同工具度量，互不干扰。

### 2.1 设计的核心依据——LLM Agent 的三个现实痛点

所有设计决策都源于对 **LLM Agent 非确定性（Non-determinism）** 的深刻认知：

| 痛点 | 描述 | 推导出的设计约束 |
|------|------|-----------------|
| **A. LLM 行为不可控** | 同一问题，GPT-4 和 Claude 的工具调用顺序可能完全不同；甚至同一模型两次回答也不同。 | 不能仅依赖"调用序列匹配"作为唯一正确性标准。 |
| **B. "过程"与"结果"无关** | 调用顺序变了，最终文件可能都是正确的。反之，顺序对了，编译也可能失败。 | 必须同时验证**执行过程**（框架路由）和**最终产物**（客观状态）。 |
| **C. 框架逻辑 vs 模型智能** | 需要区分"SDK 代码（路由、安全、序列化）有 Bug"，还是"模型选错了工具"。 | 框架层缺陷和模型层失误必须用**不同的测试机制**分别捕获。 |

**依据结论**：绝不能只用一把尺子（如仅用 Golden Transcript）来衡量所有东西。评估体系必须按**测试对象**分层，每一层使用最适合该对象的测量工具。

### 2.2 核心测试逻辑——四层金字塔模型

方案的测试逻辑是一个**四层金字塔**，自底向上从框架本身逐层覆盖到模型智能质量：

```
                         ┌─────────────────┐
                         │  4. 健壮性       │  ← 模糊测试：会不会崩溃？
                         │  (Robustness)    │
                         ├─────────────────┤
                         │  3. Token 效率   │  ← 成本量化：烧了多少钱？
                         │  (Efficiency)    │
                         ├─────────────────┤
                         │                  │
                         │  2. 质量回归     │  ← 多维验证：跑得对不对？
                         │  (Quality)       │     ├─ Golden Transcript（框架路由）
                         │                  │     ├─ TaskAssertions（客观产物）
                         │                  │     ├─ Behavior Rules（行为底线）
                         │                  │     └─ LLM-as-Judge（方案质量）
                         ├─────────────────┤
                         │  1. 性能基准     │  ← 微基准：框架本身快不快？
                         │  (Performance)   │
                         └─────────────────┘
```

#### 第一层：性能基准——测"框架本身快不快"

- **逻辑**：使用 `criterion` + `MockProvider`，**完全排除 LLM 网络延迟**，只测量纯 Rust 代码路径的开销（序列化、Governance 锁、工具路由）。
- **依据**：如果框架本身有性能回归（如引入了一个 O(n²) 的循环），必须通过纯 CPU 基准暴露，不能混在 API 延迟噪音里。
- **测试对象**：框架代码路径，与模型无关。

#### 第二层：质量回归——测"框架跑得对不对"

这是体系中最核心的一层，细分为 **4 种独立逻辑**，各自解决不同的问题：

| 测试子项 | 测试逻辑 | 本质依据 | 阻塞 CI |
|----------|----------|----------|---------|
| **Golden Transcript（序列回放）** | 录制过去的工具调用顺序，回放时强制要求顺序、参数、事件流完全一致。 | **测框架路由**：确保代码改动（如重构了 `ToolCall` 结构）没有破坏事件分发逻辑。是"回归测试"，不是"能力测试"。 | 是 |
| **TaskAssertions（端到端状态验证）** | 不关心调了什么工具，直接检查磁盘上的文件内容、目录结构、执行 `cargo build` 是否成功。 | **测客观结果**："不管黑猫白猫，抓到老鼠就是好猫"。解决 Golden Transcript 的"假阴性/假阳性"问题。 | 是 |
| **Behavior Rules（行为正确性规则）** | 注入通用规则引擎（如：不允许连续调用同一工具超过 N 次；Deny 后不应重试同一工具）。 | **测安全与理性底线**：不依赖录制数据，硬编码"优秀 Agent 的行为下限"，防止模型虽完成了任务但行为极其愚蠢（如死循环）。 | 是 |
| **LLM-as-Judge（质量评分）** | 用更强的模型给 Agent 的执行过程打分（合理性、冗余度、错误恢复）。 | **测隐性质量**：文件存在不代表方案优雅。用于评估模型升级或 Prompt 改动带来的"软性"影响。 | 否（趋势图） |

这四种逻辑互补关系如下：

- **Golden Transcript** 和 **TaskAssertions** 互补：前者验证"过程对不对"，后者验证"结果对不对"。
- **Behavior Rules** 作为通用安全网：无论模型怎么变，行为底线必须守住。
- **LLM-as-Judge** 作为趋势观察：回答"方案是不是变蠢了"这类主观问题。

#### 第三层：Token 效率——测"烧了多少钱"

- **逻辑**：聚合 `TokenUsage`，计算压缩率、缓存命中率、冗余工具调用比例。
- **依据**：在商业 SDK 中，成本是第一性原理。代码改动可能让 Agent 多调用一次工具，导致 Token 暴涨——这种"隐性成本回归"无法通过性能基准或质量回归捕获，必须有专门的成本追踪层。
- **测试对象**：每次 agent 执行的 Token 消耗模式。

#### 第四层：健壮性——测"会不会崩溃"

- **逻辑**：使用 `proptest` 随机生成畸形 JSON、10MB 超大输出、随机超时等对抗性输入。
- **依据**：真实世界充满"脏数据"。LLM 输出 `{"file": 123}` 而不是字符串路径时，SDK 必须能捕获错误并优雅降级，而不是整个 Agent 进程 `unwrap()` panic。
- **测试对象**：框架对异常输入的防御边界。

### 2.3 最终目的——达成生产级工程标准

这套方案的最终目的，是让 `fox-agent-sdk` 达到 **Production-Ready** 的工程标准：

1. **建立"安全网"**：任何开发者提交 PR，CI 会自动运行性能基准和质量回归。如果 PR 导致框架开销增加 30% 或事件路由错乱，**CI 直接红牌拦截**。
2. **区分"框架 Bug"与"模型蠢"**：当线上 Agent 表现不佳时，通过查看物证断言（TaskAssertions）和行为规则，能秒级定位——"文件创建成功了，说明框架没问题，是模型给的编译命令少了参数"。
3. **量化成本与速度**：让 Token 消耗和延迟像单元测试一样纳入版本管理，使得"优化 Token 消耗"成为一个可追踪、可考核的技术指标（而不是玄学）。
4. **对抗性防御**：通过模糊测试，确保 SDK 在 LLM 产生异常输出时能优雅降级，不会整个进程崩溃。

---

## 3. 评估维度与指标

### 3.1 性能基准（Performance）

| 指标 | 测量方式 | 目标 |
|------|----------|------|
| 端到端延迟（冷启动） | `run_once_streaming` 完整耗时 | P50 < 200ms（MockProvider），建立基线 |
| 端到端延迟（热启动） | 复用 session 的连续 turn | P50 < 100ms |
| 工具执行 P50/P95/P99 | 按工具名分组的耗时统计 | 识别高延迟工具 |
| 框架开销 | MockProvider 下纯框架路径耗时 | 排除 LLM 网络延迟 |
| 并发吞吐 | N 个 agent 并行执行下的 QPS | 评估 `GovernanceGuard.tool_slots()` 有效性 |

测量范围：排除 LLM API 网络调用（使用 `MockProvider` 替代真实 Provider），仅测量框架纯开销。

### 3.2 任务完成质量（Quality）

任务质量评估需要回答两个独立的问题：**Agent 做对了没有**（任务完成度），以及 **Agent 做得怎么样**（方案合理性）。GoldenTranscript 只能部分回答前者，回答不了后者。

#### 3.2.1 GoldenTranscript 的适用边界

GoldenTranscript 验证的是"框架是否按录制路径走"，而非"是否正确完成了任务"：

| 能验证 | 不能验证 |
|--------|----------|
| 工具调用序列与录制时一致 | 该序列是否是最优方案 |
| 框架没有丢失事件或 panic | Agent 是否产出了正确的文件内容 |
| 错误处理路径被正确触发 | 编译是否真的通过、测试是否真的运行 |
| 工具结果被正确路由（归档 / 直写） | 最终产物是否满足用户需求 |

两个核心误判风险：

- **假阳性**：LLM 模型升级后选择了更优的方案 → 回放因序列不匹配而报错
- **假阴性**：LLM 选择了看似正确但实质无效的工具调用序列 → 回放通过但任务实际未完成

因此 GoldenTranscript 适合做**框架层回归**（路由、事件、hook 是否正确），不适合做**任务完成质量评估**。后者需要以下三种补充手段。

#### 3.2.2 端到端状态验证（物证断言）

不验证 agent 说了什么、调了什么工具，而是直接验证**最终产物的客观状态**。Agent 用 `write` 还是 `edit` 创建文件不重要，重要的是文件内容正确、编译通过。

```rust
/// 任务完成后的世界状态断言。
/// Agent 的输出正确与否由客观事实决定，不由调用路径决定。
#[derive(Debug, Clone)]
pub struct TaskAssertions {
    /// 预期已创建的文件（绝对路径或相对 working_dir）
    pub file_exists: Vec<PathBuf>,

    /// 文件内容预期包含的关键字符串（子串匹配即可）
    pub file_contains: Vec<(PathBuf, String)>,

    /// 文件内容预期不包含的字符串（例如不应有 TODO 残留）
    pub file_not_contains: Vec<(PathBuf, String)>,

    /// 预期已创建的目录
    pub dir_exists: Vec<PathBuf>,

    /// 命令断言（例如 cargo build 退出码应为 0）
    pub commands: Vec<CommandAssertion>,

    /// 最长允许耗时
    pub max_duration_secs: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct CommandAssertion {
    pub working_dir: PathBuf,
    pub command: String,
    pub expected_exit_code: i32,
    pub stdout_contains: Option<String>,
    pub stderr_not_contains: Option<String>,
}
```

示例：任务"创建 Rust 项目并编译通过"的断言：

```rust
TaskAssertions {
    file_exists: vec![
        "git-summary/Cargo.toml".into(),
        "git-summary/src/main.rs".into(),
    ],
    file_contains: vec![
        ("git-summary/Cargo.toml".into(), "[package]".into()),
        ("git-summary/Cargo.toml".into(), "git2".into()),
        ("git-summary/src/main.rs".into(), "fn main".into()),
    ],
    dir_exists: vec!["git-summary/src".into()],
    commands: vec![CommandAssertion {
        working_dir: "git-summary".into(),
        command: "cargo build".into(),
        expected_exit_code: 0,
        stdout_contains: Some("Compiling git-summary".into()),
        stderr_not_contains: Some("error".into()),
    }],
    max_duration_secs: Some(120),
}
```

这比序列匹配更鲁棒且更贴近实际使用场景。

#### 3.2.3 LLM-as-Judge（质量评分）

使用独立的评估模型对 agent 的完整执行过程打分，作为任务质量的主观维度补充。评估模型会看到：

- 用户原始任务描述
- Agent 的最终文本输出
- 工具调用摘要（名称 + 时间线）
- 任务断言通过/失败状态

评估维度（1-5 分）：

| 维度 | 评估内容 |
|------|----------|
| 任务完成度 | 是否解决了用户提出的问题？产物是否可用？ |
| 方案合理性 | 采取的步骤是否高效？有无不必要的工具调用？ |
| 错误恢复 | 遇到工具失败时是否采取了合适的补救措施？ |
| 冗余度 | 是否有重复操作或明显可合并的步骤？ |

这部分**不在 CI 中阻塞合并**，而是生成质量趋势图，用于发现框架改动对下游 agent 行为的隐性影响。

#### 3.2.4 行为正确性规则（框架层行为断言）

一些行为约束不依赖 golden data，适用于任意 LLM 输出。这些规则在 GoldenTranscript 回放后叠加检查：

| 行为约束 | 断言方式 |
|----------|----------|
| 不重复调用同一工具超过 N 次 | 统计同 turn 内同名 ToolCallStart 次数 |
| 遇到编译错误后必须重新编译 | 检查 bash(cargo build)[error] → read → edit → bash(cargo build) 序列 |
| 不应在完成信号后继续操作 | 检查多余的无意义 turn |
| Deny 后不应重试同一工具 | 连续同名 ToolCallStart 去重检查 |
| 长对话应触发压实 | 消息数超阈值后检查 Compaction 事件出现 |
| 工具输出不应全部外置（小输出直写） | 检查 `ToolResultRouting` 分布 |
| Subagent 委派后应有 artifact_read | subagent 调用后的后续 turn 检查 |

```rust
/// 行为正确性规则：不依赖 golden data 的通用断言。
#[derive(Debug, Clone)]
pub struct BehaviorRule {
    pub name: String,
    pub severity: RuleSeverity,      // Error | Warning
    /// 规则函数：输入事件流，返回违规列表
    pub check: fn(&[AgentEvent]) -> Vec<RuleViolation>,
}
```

#### 3.2.5 质量评估矩阵总览

| 评估层 | 评估什么 | 阻塞 CI | 适用场景 |
|--------|----------|---------|----------|
| GoldenTranscript 回放 | 框架路由、事件、序列一致性 | 是 | 框架改动回归 |
| 端到端状态验证 | 客观产物正确性（文件、编译） | 是 | 行为功能回归 |
| LLM-as-Judge | 方案质量、效率、错误恢复 | 否（趋势图） | 框架对下游影响 |
| 行为正确性规则 | 通用行为约束 | 是 | 不限模型 |

### 3.3 Token 效率（Token Efficiency）

| 指标 | 测量方式 | 目标 |
|------|----------|------|
| 任务 Token 总量 | 单个任务完成的 `(input, output, total)` | 建立基线，每次改动后对比 |
| 压实压缩率 | `(压实前 context tokens - 压实后) / 压实前` | 评估 Turbo compaction 效果 |
| 冗余工具调用比例 | 未产生有效结果的工具调用 / 总调用 | 识别浪费的 tool round-trip |
| Cache 命中率 | `TokenUsage.cache_read_tokens / input_tokens` | 评估 prefix caching 收益 |

### 3.4 健壮性（Robustness）

| 指标 | 测量方式 | 目标 |
|------|----------|------|
| 畸形 JSON 处理 | LLM 返回非标准 JSON 格式的工具参数 | 框架不 panic，优雅降级 |
| 超大输出处理 | 工具返回 10MB+ 的文本 | `guard_tool_output` 截断生效 |
| 超时恢复 | 工具执行超时 | agent 能继续执行后续工具 |
| 并发工具错误传播 | 一个工具失败后批次内其余工具 | 正确 skip 且不阻塞 turn |
| 权限拒绝恢复 | `PermissionDecision::Deny` 后 | agent 能调整策略、不进入死循环 |

---

## 4. 工具选型

### 4.1 criterion — 微基准测试

```
cargo add --dev criterion
```

**用途**：测量纯框架路径的 CPU 耗时（端到端延迟、框架开销、工具执行耗时）。

**选型理由**：
- Rust 生态事实标准：内置统计分析（均值、标准差、线性回归）、HTML 报告、CI 回归检测
- 支持 `async` benchmark（通过 `to_async`）
- 比 `divan` 的报告更适合对外展示；`divan` 更适合库函数级的快速迭代

**替代方案排除**：
- `divan`：默认 alloc-aware，但对于 agent 级集成测试价值不大
- `iai`：基于指令计数的测量方式无法模拟 async runtime
- 手写 `std::time::Instant`：缺失统计分析和 CI 回归检测

### 4.2 tracing-chrome — 延迟剖析

```
cargo add --dev tracing-chrome tracing-subscriber
```

**用途**：输出 Chrome trace 格式的 span 火焰图，可在 `chrome://tracing` 中可视化 agent 执行全过程。

**选型理由**：
- 生产代码已使用 `tracing` 埋点（`#[tracing::instrument]`、`tracing::span`），无需额外侵入
- Chrome trace 是业界通用格式，任何团队成员均可打开分析
- 零编码成本：只需在 benchmark harness 中初始化 subscriber

### 4.3 GoldenTranscript + ReplayRunner — 质量回归

**已有基础设施**，需规范化用例。

**用途**：
- 录制真实 LLM 调用的事件流为 JSONL
- 回放时比对工具调用序列、验证输出关键内容
- 作为 CI 中的质量门禁

### 4.4 proptest — 模糊测试

```
cargo add --dev proptest
```

**用途**：生成任意规模的对抗性输入，验证框架边界行为。

**选型理由**：
- Rust 生态标准模糊测试框架
- 缩小策略（shrinking）可自动找到最简失败用例
- 比 `bolero` 更成熟稳定

---

## 5. 实现计划

### Phase 1：基础性能基线（P0，1-2 天）

**目标**：在 CI 中可运行的基准测试，检测框架纯开销的回归。

**任务**：

1. 添加 `criterion`、`tracing-chrome`、`tracing-subscriber` 到 workspace 或根 `Cargo.toml` 的 `[dev-dependencies]`
2. 创建 `benches/` 目录，编写第一个基准文件 `benches/agent_bench.rs`
3. 基准用例：
   - `run_once_streaming_cold`：新建 agent + 单次 turn（MockProvider，无工具调用场景）
   - `run_once_streaming_with_tools`：单次 turn + 2-3 个 mock 工具调用
   - `tool_execution_bash`：bash 工具执行简单命令（`echo hello`）
   - `tool_execution_read`：read 工具读取中等大小文件
4. 配置 CI（`.github/workflows/` 或等效）：`cargo bench --bench agent_bench`
5. 在全局测试 helper 中初始化 `tracing_chrome::ChromeLayer`，按 `BENCH_TRACE_DIR` 环境变量输出

**产出物**：
- `benches/agent_bench.rs`
- CI 基准回归检查
- Chrome trace 文件（不在 CI 中存储，仅开发者本地使用）

### Phase 2：质量基准（P1，3-5 天）

**目标**：建立多层次质量评估体系，覆盖框架回归、物证断言、质量评分与行为规则。

#### Phase 2a：GoldenTranscript + 端到端状态验证（2-3 天）

**任务**：

1. 设计评估用例清单（15-20 个场景），每个用例包含：
   - `GoldenTranscript` JSONL（框架回归用）
   - `TaskAssertions`（端到端状态验证用）
   - 场景覆盖：
     - **文件操作**：多文件创建 + 编辑 + 验证
     - **代码搜索**：grep + read 组合的全库搜索
     - **bash 操作**：编译、测试运行、git 操作
     - **错误处理**：工具失败、权限拒绝、超时
     - **压实场景**：长对话触发压实后的行为正确性
2. 规范化录制流程：通过 `examples/record_transcript.rs` 或等效脚本录制真实 LLM 交互
3. 扩展 `TranscriptCheck`：
   ```rust
   pub struct TranscriptCheck {
       pub expected_tool_calls: Vec<ExpectedTool>,
       pub output_must_contain: Option<String>,
       pub output_must_not_contain: Option<String>,
       pub max_turns: Option<usize>,
       pub max_errors: usize,
       /// 新增：事件重放完毕后执行的端到端状态断言
       pub task_assertions: Option<TaskAssertions>,
   }

   pub struct ExpectedTool {
       pub name: String,
       pub max_calls: usize,
   }
   ```
4. 编写 `tests/golden_transcripts.rs`，将 JSONL 文件与 `TranscriptCheck` + `TaskAssertions` 配对
5. `TaskAssertions` 执行器：回放后在临时目录中运行 `file_exists`、`file_contains`、`CommandAssertion`
6. CI 集成：`cargo test --features foxtests golden_transcripts`

**产出物**：
- `tests/fixtures/transcripts/*.jsonl`（15-20 个 golden 文件）
- `tests/golden_transcripts.rs`
- `crates/fox-agent-core/src/task_assertions.rs`（TaskAssertions 类型定义 + 执行器）

#### Phase 2b：LLM-as-Judge + 行为正确性规则（1-2 天）

**任务**：

1. 实现 LLM-as-Judge 评分器 `TaskJudge`：
   - 构造评估 prompt（包含用户任务、agent 输出、工具调用时间线、物证断言结果）
   - 调用评估模型获取 1-5 分评分（4 个维度：完成度、合理性、错误恢复、冗余度）
   - 输出评分报告 JSON
2. 实现行为正确性规则引擎 `BehaviorRuleEngine`：
   - 注册 5-8 条 `BehaviorRule`
   - 接收 `Vec<AgentEvent>`，执行所有规则，返回 `Vec<RuleViolation>`
3. 将 LLM-as-Judge 和规则引擎集成到 `tests/quality_eval.rs`
4. LLM-as-Judge 结果**不阻塞 CI**，输出到 `target/eval-results/{timestamp}.json` 供趋势分析
5. 行为正确性规则在 CI 中作为**额外断言**，`severity: Error` 的违规阻塞合并

**产出物**：
- `crates/fox-agent-sdk/src/eval/judge.rs`（LLM-as-Judge）
- `crates/fox-agent-sdk/src/eval/behavior_rules.rs`（行为规则）
- `tests/quality_eval.rs`

### Phase 3：Token 效率追踪（P1，0.5-1 天）

**目标**：每次 benchmark 输出 Token 消耗报告。

**任务**：

1. 在 `TokenUsage` 旁边添加聚合器 `TokenReport`：
   ```rust
   #[derive(Default)]
   pub struct TokenReport {
       pub total_input: u64,
       pub total_output: u64,
       pub cache_read: u64,
       pub cache_write: u64,
       pub tool_calls: u64,
       pub compactions: u64,
   }
   ```
2. 扩展 `EventRecorder`，在录制期间自动聚合 `AgentEvent::ModelUsage` 和 `AgentEvent::Compaction { .. }`
3. 每个 golden transcript 回放后打印或存储 `TokenReport`
4. CI 中以表格形式展示 Token 变化（与上次基线对比）

**产出物**：
- `crates/fox-agent-core/src/report.rs`（TokenReport）
- `EventRecorder` 扩展
- CI 中 Token 消耗对比表

### Phase 4：健壮性测试（P2，1-2 天）

**目标**：通过模糊测试覆盖框架边界。

**任务**：

1. 添加 `proptest` 依赖
2. 编写模糊测试用例：
   - `tool_output_fuzz`：向 `harness.push_message()` 注入任意字符串，验证不 panic
   - `json_input_fuzz`：随机生成 JSON 结构作为工具参数，验证反序列化不 panic
   - `compact_fuzz`：随机消息序列的压实处理
   - `safety_input_fuzz`：随机构造的 tool name 和 input 对 `SafetySystem::check()` 不 panic
3. 如果发现 bug，记录 issue 并修复
4. CI 集成：`cargo test --features foxtests proptest`（或独立 job）

**产出物**：
- `tests/proptest/` 目录下的模糊测试文件
- 发现的 bug 修复

---

## 6. 目录结构规划

```
fox-agent-sdk/
├── benches/                          # Phase 1
│   ├── agent_bench.rs                # 端到端延迟 + 框架开销
│   ├── tool_bench.rs                 # 单个工具执行耗时
│   └── harness.rs                    # 共享 benchmark helper
├── tests/
│   ├── fixtures/
│   │   └── transcripts/              # Phase 2a
│   │       ├── 001_create_project.jsonl
│   │       ├── 002_multi_file_edit.jsonl
│   │       ├── 003_codebase_search.jsonl
│   │       ├── ...
│   │       └── CHECKLIST.md          # 用例清单与录制说明
│   ├── golden_transcripts.rs         # Phase 2a
│   ├── quality_eval.rs               # Phase 2b: Judge + 行为规则
│   └── proptest/                     # Phase 4
│       ├── tool_output_fuzz.rs
│       ├── json_input_fuzz.rs
│       └── compact_fuzz.rs
├── crates/
│   ├── fox-agent-core/src/
│   │   ├── task_assertions.rs        # Phase 2a: TaskAssertions 类型 + 执行器
│   │   └── report.rs                 # Phase 3: TokenReport
│   └── fox-agent-sdk/src/
│       └── eval/                     # Phase 2b
│           ├── judge.rs              # LLM-as-Judge 评分器
│           └── behavior_rules.rs     # 行为正确性规则引擎
└── docs/
    └── evaluation_design.md          # 本文档
```

---

## 7. CI 集成

```yaml
# .github/workflows/evaluation.yml（概念示例）
jobs:
  benchmark:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo bench --bench agent_bench -- --output-format bencher | tee output.txt
      - uses: benchmark-action/github-action-benchmark@v1
        with:
          tool: 'cargo'
          output-file-path: output.txt
          github-token: ${{ secrets.GITHUB_TOKEN }}
          auto-push: true
          alert-threshold: '130%'

  golden-transcripts:
    runs-on: ubuntu-latest
    steps:
      - run: cargo test --features foxtests golden_transcripts

  quality-eval:
    runs-on: ubuntu-latest
    steps:
      - run: cargo test --features foxtests quality_eval
      # LLM-as-Judge 结果不阻塞，仅归档供趋势分析
      - uses: actions/upload-artifact@v4
        if: always()
        with:
          name: eval-results
          path: target/eval-results/

  behavior-rules:
    runs-on: ubuntu-latest
    steps:
      - run: cargo test --features foxtests behavior_rules

  proptest:
    runs-on: ubuntu-latest
    steps:
      - run: cargo test --features foxtests proptest
```

---

## 8. 附录：Golden Transcript 用例清单（建议）

| ID | 场景 | 难度 | 预期工具调用 | 关键验证点 |
|----|------|------|-------------|-----------|
| 001 | 创建 Rust 项目并编译 | 简单 | bash(cargo new), bash(cargo build) | 工具序列、`cargo build succeeded` |
| 002 | 多文件编辑 | 中等 | read, edit(多次), bash(cargo check) | 编辑顺序、操作后内容正确 |
| 003 | 全库搜索 | 中等 | grep, read(多次) | 搜索结果完整性 |
| 004 | Git log 分析 | 中等 | bash(git log), grep | 工具结果正确传递 |
| 005 | 错误诊断与修复 | 困难 | bash(cargo build), read, edit, bash(cargo build) | 错误→修复→验证循环 |
| 006 | 权限被拒绝恢复 | 困难 | bash → Deny → agent 改用替代方案 | Deny 后不循环 |
| 007 | 工具超时处理 | 中等 | bash(sleep 120) → timeout | 超时后继续执行其他工具 |
| 008 | 压实后行为一致 | 困难 | 超长对话触发压实 → 后续正确 | 压实不丢失关键上下文 |
| 009 | MCP 工具调用 | 中等 | mcp__* 工具 | MCP 工具路由正确 |
| 010 | 子 Agent 委派 | 困难 | subagent, artifact_read | 子 Agent 产物正确回读 |
| 011 | 多 turn 对话 | 中等 | 3-5 个连续 turn | Session 上下文连贯 |
| 012 | 大文件读取 | 中等 | read(10MB+) → guard_tool_output | 截断生效 |
| 013 | 并发工具执行 | 中等 | 同一轮多个 tool call | 互不干扰 |
| 014 | plan + todo 状态一致性 | 中等 | plan, todo, 后续工具 | 计划与实际执行一致 |
| 015 | 空仓库克隆与初始化 | 简单 | bash(git clone), ls, read | 环境初始化 |

---

## 9. 风险与注意事项

- **Golden Transcript 依赖真实 LLM API**：录制需要可用的 API key 环境；回放使用 `MockProvider` 不依赖网络
- **criterion 基准的噪声**：CI 环境（GitHub Actions shared runner）的 CPU 抖动可能影响基准精度。建议使用 `--sample-size` 和 `--noise-threshold` 参数调整
- **Golden Transcript 维护成本**：LLM 行为随模型版本变化，Golden 文件需定期更新。建议按模型 ID 组织目录
- **不依赖外部服务的约束遵守**：所有基准测试的 CI 运行均使用 `MockProvider`，不发起真实 API 调用；Golden Transcript 录制是开发者的离线操作
