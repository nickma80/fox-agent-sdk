# 上下文管理与分层压缩重构方案

## 1. 重构目标

优化上下文的结构和质量，在保护 KV Cache 前缀一致性的前提下，通过分层压缩机制和 Agent Status Bar 降低上下文膨胀、提升 Agent 任务跟踪能力。

核心目标：
1. **KV Cache 友好**：静态在前，动态在后，缓存锚点明确
2. **Agent Status Bar**：用 prompt 末尾的结构化状态块替代软中断，不污染消息历史
3. **分层压缩**：五层渐进式压缩，从低成本策略到高成本兜底，逐层触发
4. **熔断保护**：避免压缩死循环持续烧 token

---

## 2. 现状审查

### 2.1 上下文结构（KV Cache 友好性）

**已有**：[`SplitPrompt`](file:///d:/ws/ai/fox-agent-sdk/crates/fox-agent-core/src/prompt/mod.rs#L18-L23) 定义了 `static_part` / `dynamic_part` 分离，[prompt_builder.rs](file:///d:/ws/ai/fox-agent-sdk/crates/fox-agent-sdk/src/prompt_builder.rs#L70-L148) 正确分类。

当前动态部分内部顺序：
```
intent_anchor → planning_context → narrative_history → memory_injection → active_skill
```

**差距 1**：`SplitPrompt` 是架构层的标签，实际 `model.stream_raw_messages()` 提交的 `Vec<Message>` 中静态和动态合并为一条 `SystemMessage`。KV cache 能否生效取决于 Provider 实现，SDK 端没有缓存锚点声明。

**差距 2**：动态部分内部排序缺乏声明——哪些必须放最前面（prefix 匹配关键）、哪些放后面无所谓，没有优先级约定。

### 2.2 Agent Status Bar（任务跟踪）

**已有**：
- [软中断 `queue_soft_interrupt()`](file:///d:/ws/ai/fox-agent-sdk/crates/fox-agent-sdk/src/harness.rs) 定期向消息流注入 `"Interrupt: Remember your task..."`
- [Drift Detection](file:///d:/ws/ai/fox-agent-sdk/crates/fox-agent-sdk/src/agent.rs#L36-L39) — `consecutive_auto_turns` 计数器，超阈值后注入提醒
- [Intent Anchor](file:///d:/ws/ai/fox-agent-sdk/crates/fox-agent-sdk/src/prompt_builder.rs#L74-L86) — 最新用户消息在 dynamic section 首部展示

**差距 3**：**软中断严重污染对话历史**：

| 问题 | 影响 |
|------|------|
| 中断作为 `Role::User` 消息注入 | Tool 循环被打断，agent 需响应中断而非继续执行 |
| 每条中断是完整消息 | token 膨胀，尤其是高频注入场景 |
| 与真实用户消息混在一起 | compaction 时难以区分，摘要质量下降 |

### 2.3 分层压缩机制

**已有**：单层 `CompactionManager::do_compact()` — 将所有旧消息 squash 成一条 `"Conversation summary:"`。

| 层级 | 描述 | 现状 |
|------|------|------|
| **L1** 工具结果预算控制 | 大输出外置到 artifact store | **已实现**（routing engine + artifact store） |
| **L2** 噪声直接删除 | 低价值内容（搜索结果中未使用的行）移除 | **未实现** |
| **L3** API 层微压缩 | 通过 `prefill` / `context_edit` 移除工具结果 | **未实现** |
| **L4** 归档式摘要 | 逐轮结构化摘要（如 git log，每轮保留独立记录） | **部分实现**（有 LLM 摘要，但每次 squash 成一条） |
| **L5** 全量压缩 + 熔断 | LLM 驱动压缩 + 连续失败熔断器 | **部分实现**（有 `max_compaction_count`，但无熔断器） |

**差距 4 — L2 噪声删除缺失**：`guard_tool_output()` 会 truncate 超长输出，但短结果中低信噪比内容（如 `grep` 返回 200 行但只需其中 3 行）不会自动净化。

**差距 5 — L3 API 层微压缩缺失**：当 `context_pressure > 0.9` 时，SDK 不做服务端消息裁剪。

**差距 6 — L4 归档式摘要缺失**：当前 compaction 把所有旧消息 squash 成一条系统消息。缺少 per-turn 结构化记录（如 git log 那样保留每轮摘要而非 git squash 合并）。

**差距 7 — L5 熔断器缺失**：`max_compaction_count` 只限制总次数，但在"压缩→立即又超预算→再压缩"的死循环中没有熔断逻辑。

---

## 3. 重构方案

```
Phase A: Agent Status Bar（替代软中断）
Phase B: KV Cache 锚点声明 + 动态部分排序
Phase C: L2 噪声删除
Phase D: L4 归档式摘要（per-turn structured records）
Phase E: L5 熔断器 + L3 API 微压缩
```

### 3.1 Phase A：Agent Status Bar（优先级最高）

**目标**：用 Status Bar 替代 `queue_soft_interrupt` 和 Drift Detection 的大部分逻辑。

**核心思想**：Agent Status Bar 是注入到 prompt 动态部分**末尾**的一个固定格式块。它始终在 agent 视野中，但不需要"打断"agent 的正常执行流程。Agent 看到 status bar 就自然了解当前进度和限制。

#### 数据结构

```rust
/// Agent runtime status displayed at the end of context.
/// Updated every turn; placed in the dynamic section of the prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStatus {
    /// The current task objective (from latest user message)
    pub current_objective: String,
    /// Plan with completion status
    pub plan_steps: Vec<PlanStepStatus>,
    /// Number of turns executed so far
    pub turn: u64,
    /// Number of tool calls made so far
    pub tools_called: u64,
    /// Number of compactions performed
    pub compactions: u64,
    /// Consecutive turns without new user input
    pub consecutive_auto_turns: u32,
    /// Threshold for task-drift warning
    pub auto_turn_limit: u32,
    /// Time elapsed since session start (seconds)
    pub elapsed_secs: u64,
    /// Token usage breakdown
    pub token_usage: Option<TokenUsageStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStepStatus {
    pub description: String,
    pub status: StepStatus,   // Pending | InProgress | Done | Skipped
    pub tool_calls: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StepStatus {
    Pending,
    InProgress,
    Done,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsageStatus {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}
```

#### 渲染格式

```markdown
<!-- AGENT_STATUS_BAR -->
# Task Status

## Current Objective
Perform code review on PR #42 — check for security issues and performance regressions.

## Plan Progress
- [~] 1/3 Read and understand changed files (3 of 5 files read)
- [ ] 2/3 Analyze diff for security issues and performance regressions
- [ ] 3/3 Write structured review comments

## Runtime
| Metric        | Value    |
|---------------|----------|
| Turn          | 7        |
| Tools Called  | 12       |
| Auto-Turns    | 3/5      |
| Elapsed       | 2m 34s   |

<!-- /AGENT_STATUS_BAR -->
```

**当 auto-turns 接近临界值时**自动升级警告：

```markdown
⚠️ WARNING: 4/5 consecutive auto-turns. If you've completed the current task,
call todo_write to mark it done and wait for the user.
```

#### 与现有机制的替换

| 现有机制 | 替代方案 |
|----------|----------|
| `queue_soft_interrupt("Remember your task...")` | Status bar 中 `Current Objective` 始终可见 |
| Drift Detection 注入 "Interrupt" 消息 | Drift Detection 只更新 `consecutive_auto_turns` 计数字段；status bar 显示 `⚠️ WARNING` |
| `Update Tasks` 软中断 | `PlanProgress` 从 `GoalCheckpoint` 和 `todo_write` 工具结果中自动提取，同步到 status bar |
| `GoalCheckpoint` 中断注入 | `GoalCheckpoint` 保留，但只更新 status bar 的 `plan_steps`，不再注入消息 |

#### 改动范围

1. 新增 `AgentStatus` 到 `fox_agent_core::status` 模块
2. `Agent` 新增 `status: Arc<RwLock<AgentStatus>>` 字段
3. 每次 turn 结束时自动调用 `self.status.write().await.increment_turn()` 等更新
4. `PromptBuilder::build_split()` 在 `dynamic_part` 末尾追加 `render_agent_status()`
5. 删除 `agent.rs` 中的 `queue_soft_interrupt()` 调用和 Drift Detection 的消息注入
6. Drift Detection 改为只更新 `consecutive_auto_turns`，警告由 status bar 渲染
7. 测试更新：所有依赖软中断消息的测试改为验证 status bar 内容

**收益**：

| 维度 | 改善 |
|------|------|
| 消息污染 | 零污染 — status bar 是 prompt 的一部分，不是消息 |
| Token 开销 | 固定 ~200-400 chars，不随 turn 数增长 |
| Agent 执行 | 不打断 tool 循环 — agent 自然感知状态 |
| KV Cache | status bar 在动态末尾，不影响静态 prefix 缓存 |

---

### 3.2 Phase B：KV Cache 锚点声明

**目标**：明确标注 KV cache 边界，确保静态部分的 prefix 一致性。

#### SplitPrompt 增强

```rust
pub struct SplitPrompt {
    /// Static content suitable for provider prompt caching (template, AGENTS.md, skills).
    /// Begins with a cache-break anchor comment.
    pub static_part: String,
    /// Dynamic content that changes every turn (memory, planning, status bar).
    pub dynamic_part: String,
    /// Line number of the cache-anchor boundary in the assembled prompt.
    /// Used by providers to identify which prefix to cache.
    pub cache_anchor_line: Option<usize>,
}
```

#### 组装顺序约定

```
┌──────────────────────────────────────────────┐
│ STATIC (KV Cacheable)                        │
├──────────────────────────────────────────────┤
│ 1. SYSTEM_TEMPLATE       — 角色定义          │
│ 2. SESSION_CONTEXT       — 时间/CWD/OS       │
│ 3. AGENTS.md             — 项目规则          │
│ 4. MCP_RESOURCES         — 连接的 MCP 资源    │
│ 5. SKILLS_LIST           — 可用技能          │
│ 6. PROMPT_OVERLAY        — 附加提示          │
├──────────────────────────────────────────────┤  ← Cache Anchor
│ DYNAMIC (Per-Turn)                           │
├──────────────────────────────────────────────┤
│ 7. INTENT_ANCHOR         — 当前任务（低频变） │
│ 8. NARRATIVE_SUMMARY     — 归档历史（低频变） │
│ 9. PLANNING_CONTEXT      — 计划状态（中频变） │
│ 10. MEMORY_INJECTION     — 记忆注入（中频变） │
│ 11. ACTIVE_SKILL         — 激活技能（低频变） │
│ 12. STATUS_BAR           — 状态栏（高频变）   │
└──────────────────────────────────────────────┘
```

**排序原则**：低频变动 → 中频变动 → 高频变动。`INTENT_ANCHOR` 放在动态首部是因为它很少变（只有用户发新消息时才改），后续的所有动态片段都能受益于它的 prefix 缓存。

---

### 3.3 Phase C：L2 噪声删除

**目标**：自动识别并移除低价值工具结果，对噪声做摘要是对 token 的浪费。

#### 策略

```
工具输出 < 1000 chars     → 不做处理
工具输出 1000-8000 chars  → 检查引用率
  - Agent 仅引用 < 20% 的行 → 移除未引用行，替换为 [... N lines omitted, use artifact_read for full content]
  - Agent 引用 ≥ 20% 的行  → 不做处理
工具输出 > 8000 chars     → 已由 L1 routing engine 处理（externalize）
```

#### 覆盖的工具

- `grep` / `glob` — 搜索结果中大量未引用行
- `read` — 大文件读取中未引用的代码段
- `web_search` — 搜索结果中未访问的 URL
- `web_fetch` — 爬取内容中未引用的 HTML 文本

#### 引用率计算

```rust
/// Heuristic: extract quoted/referenced snippets from subsequent agent messages,
/// count how many lines of the tool output are referenced.
fn noise_ratio(
    tool_output: &str,
    subsequent_messages: &[Message],
) -> (usize, usize) {
    let output_lines: Vec<&str> = tool_output.lines().collect();
    let mut referenced = vec![false; output_lines.len()];

    for msg in subsequent_messages {
        if msg.role != Role::Assistant { continue; }
        let text = msg.text_content();
        for (i, line) in output_lines.iter().enumerate() {
            // Simple substring match (could be enhanced with fuzzy matching)
            if text.contains(line) {
                referenced[i] = true;
            }
        }
    }

    let ref_count = referenced.iter().filter(|&&r| r).count();
    (ref_count, output_lines.len())
}
```

#### 实现位置

在 `compaction.rs` 增加 `CompactionStep::NoiseRemoval`，在 L4 归档摘要之前执行。也作为一个独立的 `clean_tool_results()` 方法供 tool loop 在每个工具结果注入后调用。

#### 配置

```toml
[context_management]
l2_noise_removal_enabled = true
l2_noise_reference_threshold = 0.20    # 引用率低于此阈值则移除
l2_noise_min_output_chars = 1000       # 短于此长度的输出不检查
```

---

### 3.4 Phase D：L4 归档式摘要

**目标**：将 squash 式压缩改为 git log 式的结构化归档。

**核心区别**：

```
当前（squash）:
  "Conversation summary: The user asked to fix bug #42.
   Agent read files, ran grep, wrote fix..."

重构后（git log）:
  ## Compaction: Turns 1-3
  Intent: fix bug #42
  Actions: read src/main.rs, grep "panic", edit src/main.rs
  Findings: root cause was null pointer in handle_request()
  Decisions: Added null check + unit test

  ## Compaction: Turns 4-7
  Intent: add test coverage
  Actions: write tests/unit_test.rs, cargo test
  Findings: 3 tests pass, 1 edge case fails
  Decisions: Skipped edge case, noted in TODO
```

每个 `TurnNarrative` 独立保留，不合并。新压缩时旧 narrative 不修改，只追加新条目。

#### 数据结构

```rust
/// A per-turn-range structured narrative — like a git commit for the conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnNarrative {
    /// Turn range this narrative covers
    pub turn_range: (u64, u64),
    /// What the user asked for
    pub user_intent: String,
    /// List of actions taken (tool name + brief description)
    pub actions_taken: Vec<String>,
    /// Key discoveries or results
    pub findings: Vec<String>,
    /// Files that were created or modified
    pub files_modified: Vec<String>,
    /// Decisions or conclusions reached
    pub decisions: Vec<String>,
    /// Unfinished work
    pub pending_work: Vec<String>,
    /// Token usage for this range
    pub token_usage: Option<(u64, u64)>,
    /// Compaction timestamp
    pub compacted_at: u64,
}
```

#### 消息格式

`TurnNarrative` 以专用 `ContentBlock` 注入消息历史，而非替换旧消息：

```rust
// Insert as: Role::System, ContentBlock::CompactionNarrative { ... }
// Old compaction narratives are NOT removed — they stack like git log.
// New narratives are appended after existing ones.
```

#### LLM 摘要 Prompt 更新

当前提示词已要求输出结构化 JSON（`user_intent`、`actions_taken`、`findings` 等），只需调整格式为 per-turn 分组：

```
For each distinct sub-task in the conversation:
- Identify the turn range (start_turn, end_turn)
- Extract user_intent, actions_taken, findings, files_modified, decisions, pending_work
Output a JSON array of narrative objects, one per sub-task group.
```

#### 配置

```toml
[context_management]
l4_archival_enabled = true
l4_max_narratives = 20     # 最多保留的 narrative 条目数
l4_per_turn_budget_chars = 500   # 每条 narrative 的字符预算
```

---

### 3.5 Phase E：L5 全量压缩熔断器 + L3 API 微压缩

#### L5 熔断器

**目标**：避免 compaction 死循环 — 压缩后立即又超预算，再次触发压缩，反复烧 token。

```rust
/// Circuit breaker for compaction loops.
///
/// When `consecutive_failures` reaches `max_consecutive_failures`,
/// the breaker opens and compaction is disabled for `cooldown_turns`.
#[derive(Debug, Clone)]
pub struct CompactionCircuitBreaker {
    pub consecutive_failures: u32,
    pub max_consecutive_failures: u32,
    pub cooldown_turns: u32,
    pub last_failure_turn: u64,
    pub state: CircuitState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CircuitState {
    /// Normal operation — compaction allowed
    Closed,
    /// Breaker tripped — compaction disabled
    Open,
    /// Testing if compaction can resume
    HalfOpen,
}
```

**状态转换**：

```
Closed ──(连续失败 N 次)──▶ Open ──(冷却 M 轮)──▶ HalfOpen
HalfOpen ──(成功)─────────▶ Closed
HalfOpen ──(失败)─────────▶ Open
```

```rust
impl CompactionCircuitBreaker {
    /// Returns true if compaction is allowed.
    pub fn allow_compaction(&mut self, current_turn: u64) -> bool {
        match self.state {
            CircuitState::Closed => true,
            CircuitState::Open => {
                if current_turn - self.last_failure_turn > self.cooldown_turns as u64 {
                    self.state = CircuitState::HalfOpen;
                    true
                } else {
                    false
                }
            }
            CircuitState::HalfOpen => true, // always allow the test attempt
        }
    }

    /// Report compaction result.
    /// "Success" means the context size actually decreased.
    pub fn report(&mut self, success: bool, current_turn: u64) {
        if success {
            self.state = CircuitState::Closed;
            self.consecutive_failures = 0;
        } else {
            self.last_failure_turn = current_turn;
            match self.state {
                CircuitState::Closed | CircuitState::HalfOpen => {
                    self.consecutive_failures += 1;
                    if self.consecutive_failures >= self.max_consecutive_failures {
                        self.state = CircuitState::Open;
                    }
                }
                CircuitState::Open => {} // already open, nothing to do
            }
        }
    }
}
```

#### L3 API 层微压缩

**目标**：当 `context_pressure > 0.9` 时，利用服务端 API 从对话前缀中移除指定工具结果。

**前提**：
- 仅 `context_pressure > 0.9` 时触发 — 这是最紧迫的状态
- Fires regardless of prefix-cache hit rate — context overflow justifies the cache miss cost

**实现**：

```rust
/// Provider trait extension for API-level context editing.
#[async_trait]
pub trait ContextEditor: Provider {
    /// Remove specified message ranges from the conversation history prefix.
    /// Returns the updated (possibly shorter) message list.
    async fn context_edit(
        &self,
        messages: &[Message],
        remove_indices: &[usize],
    ) -> Result<Vec<Message>, ProviderError>;
}
```

**移除策略**：
1. 选择已确认不被后续对话引用的工具结果
2. 移除工具结果时连带移除触发该结果的 `tool_use` 消息
3. 移除点之后的缓存会失效 — 接受这次代价，因为上下文已接近溢出

```rust
fn select_messages_for_removal(messages: &[Message]) -> Vec<usize> {
    // Heuristic: tool results that are > threshold and were referenced
    // less than N times in subsequent messages are candidates for removal.
    let mut candidates = Vec::new();
    // ... selection logic ...
    candidates
}
```

**配置**：

```toml
[context_management]
l3_api_compression_enabled = true
l3_api_compression_threshold = 0.90  # context_pressure threshold
l3_api_max_removals = 5              # max messages to remove per operation
l5_circuit_breaker_max_failures = 3
l5_circuit_breaker_cooldown_turns = 10
```

---

## 4. 配置汇总

```toml
[context_management]
# ── L1: Tool Result Budget Control (已实现) ──
# 参见 [artifact_store] 和 [routing_policy] 配置段

# ── L2: Noise Removal ──
l2_noise_removal_enabled = true
l2_noise_reference_threshold = 0.20
l2_noise_min_output_chars = 1000

# ── L3: API-Level Context Edit ──
l3_api_compression_enabled = true
l3_api_compression_threshold = 0.90
l3_api_max_removals = 5

# ── L4: Archival Summaries ──
l4_archival_enabled = true
l4_max_narratives = 20
l4_per_turn_budget_chars = 500

# ── L5: Full Compaction + Circuit Breaker ──
l5_llm_summary_enabled = true       # replaces compaction.llm_summary_enabled
l5_circuit_breaker_max_failures = 3
l5_circuit_breaker_cooldown_turns = 10

# ── Agent Status Bar ──
status_bar_enabled = true
status_bar_warn_auto_turns = true   # show ⚠️ when approaching limit
```

---

## 5. 实施路线

| Phase | 工作项 | 优先级 | 依赖 | 收益 |
|-------|--------|--------|------|------|
| **A** | Agent Status Bar | 最高 | 无 | 替代软中断，减少 30-50% 消息膨胀 |
| **B** | KV Cache 锚点 | 高 | 无 | 提高 Provider 缓存命中率 |
| **C** | L2 噪声删除 | 中 | A（需跟踪引用） | 减少 10-20% 工具消息 token |
| **D** | L4 归档式摘要 | 中 | A（status bar 提供 turn 信息） | 改善压缩后信息保留率 |
| **E** | L5 熔断 + L3 微压缩 | 中 | D（依赖 L4 作为前置策略） | 避免死循环 + 最后手段裁剪 |

建议从 **Phase A** 开始——收益最直接、风险最低。Phase A 完成后，B 可以并行推进，C/D/E 依次跟进。

---

## 6. 触发流程图

```
每轮 Turn 结束
    │
    ├── L1: routing engine 检查工具输出大小
    │     └─ 超阈值 → Externalize / DelegateToSubagent / SummarizeOnly
    │
    ├── L2: 检查工具输出引用率
    │     └─ 引用率 < 阈值 → 移除未引用行（替换为省略标记）
    │
    ├── 检查 context_pressure
    │     ├─ > 0.85 → L4 触发编译摘要
    │     ├─ > 0.90 → L3 API 层移除
    │     └─ > 0.95 → L5 全量压缩（若熔断器允许）
    │
    ├── 更新 AgentStatus 计数器
    │     └─ 渲染到 prompt 动态部分末尾
    │
    └── 进入下一轮
```

---

## 7. 验收标准

### Phase A
- [ ] `AgentStatus` 结构定义并注册到 `Agent`
- [ ] Status bar 渲染在 prompt 动态部分末尾
- [ ] 每次 turn 自动更新 `consecutive_auto_turns`、`tools_called`、`turn`
- [ ] `⚠️ WARNING` 在 auto-turns 接近限制时自动显示
- [ ] `queue_soft_interrupt` 调用点全部移除
- [ ] Drift Detection 消息注入逻辑移除
- [ ] 所有现有测试通过（验证 status bar 而非中断消息）
- [ ] 新增测试：验证 status bar 更新和渲染正确

### Phase B
- [ ] `SplitPrompt.cache_anchor_line` 正确计算
- [ ] 动态部分按频率排序（低频→高频）
- [ ] Provider 接口不变，向后兼容

### Phase C
- [ ] `noise_ratio()` 正确计算引用率
- [ ] 未引用行被 `[... N lines omitted ...]` 替换
- [ ] 仅在输出 1000-8000 char 范围内触发
- [ ] 被移除的内容可通过 `artifact_read` 恢复

### Phase D
- [ ] `TurnNarrative` 独立条目存储（不 squash）
- [ ] 新 compaction 追加而非覆盖旧 narrative
- [ ] LLM 摘要 prompt 适配 per-turn 格式
- [ ] `l4_max_narratives` 限制生效

### Phase E
- [ ] `CompactionCircuitBreaker` 三态转换正确
- [ ] 连续 3 次压缩失败后熔断
- [ ] 冷却期后 HalfOpen 测试
- [ ] `ContextEditor::context_edit()` trait 定义
- [ ] 移除策略选择置信度高的候选消息
