# RFC: 压缩期指令与未完成动作保护（B + C 方案）

> **状态**: ⚠️ 已废弃（superseded by Narrative Memory system, 2026-07-08）
> **替代方案**: `NarrativeRecord` 结构化叙事记忆 + 简化 Compaction
> **理由**: B+C 三条独立机制（pin user messages + pending actions + first message）被统一的叙事结构替代——一次 LLM 调用同时完成 summary + narrative extraction，存入 MemoryGraph 跨 session 复用。
> **影响范围**: B+C 代码已从 `compaction.rs` 移除（~240 行），`CompactionConfig` 中 `pin_first_user_message`/`pin_recent_user_messages`/`preserve_pending_actions` 字段已移除。
>
> ---
>
> **以下为历史文档，保留供参考。**
>
> **原始背景问题**: 长会话压缩后，模型丢失"写入 `docs/plan.md`"这类**输出动作指令**，
> 把本应落盘的产物当作纯文本回复输出。
>
> **原始实现落点**: [compaction.rs](file:///d:/ws/ai/fox-agent-sdk/crates/fox-agent-sdk/src/compaction.rs)
> `do_compact` + 新增 helpers；[config.rs](file:///d:/ws/ai/fox-agent-sdk/crates/fox-agent-core/src/config.rs)
> `CompactionConfig` 新增 3 个开关。

---

## 1. 问题链路

```
超大文档 (prd 88K + tech_design 92K)
  → Agent 分片 read，每片触发 Context guard truncated (budget=80000)
  → 反复截断 + 反复重试，token 预算快速耗尽
  → turn 162: 上下文压缩触发，57 条消息 → 11 条
  → 压缩丢失了原始指令中的"写入 docs/plan.md"
  → turn 163+: 模型只记得语义目标（"生成开发计划"），丢了输出动作（"调用 write 落盘"）
  → 模型自然地把计划当作文本回复输出，不再调用 write 工具
```

根因有两层：

1. **压缩按"条数"保留，把用户指令整条冲掉了（直接根因）**。
   [`do_compact`](file:///d:/ws/ai/fox-agent-sdk/crates/fox-agent-sdk/src/compaction.rs) 按
   `preserve_recent_messages` 保留**最近 N 条消息，不区分角色**。一个用户输入之后，
   agent 常产生**几十条** assistant/tool 消息（尤其分片 read 大文档 + 反复重试的场景），
   于是"最近 40 条"可能**一条 user 消息都不含**——用户的原始指令落入被 drain 区段，
   被摘要成一句语义描述，动作指令（"写入 `docs/plan.md`"）随之丢失。
2. **摘要是"语义压缩"，动作意图易被稀释**。即使指令进入摘要，LLM/机械摘要也可能弱化
   "调用 write 落盘"这一动作，模型只记住"生成开发计划"的语义目标。
3. **上游诱因**：`read` 超大文档反复触发 `Context guard truncated`，在单文件上烧掉数十
   turn，把预算打空、抬高压缩触发频率。

---

## 2. 现状（改造前的基线）

- 压缩触发已分层（见 `CompactionMode`）：
  - `PreSend`：发送模型前的溢出安全网，仅在**严格超预算**时压。
  - `Proactive`：turn 完成后 / context-limit 报错后的预防式收敛。
- 双轨存储：`SessionState.full_messages` 永不压缩，仅 `messages`（工作上下文）被压缩。
  → **完整历史用于 restore/展示，压缩只影响发给模型的工作集。**
- 压缩产物：单条 `System: "Conversation summary:\n..."`，重复压缩时**替换**而非堆叠。

> 注意：`full_messages` 未丢，但**发给模型的 `messages` 里，动作指令已被摘要**。
> 本 RFC 解决的是"工作上下文里动作意图丢失"，不是磁盘历史丢失。

---

## 3. 目标

1. **B（按角色 Pin user 消息）— 首要修复**：压缩时保留**首条 + 最近 K 条** `user` 消息原文。
   因为"按条数保留"不保证任何 user 消息存活（见 §1 根因 1），B 才是本问题的**直接解**。
2. **C（未完成动作提取）— 长尾保险**：从被压区段提取"未完成动作 + 产出物路径"，
   以结构化、置顶、可完成的 TODO 块注入，覆盖"指令在很早、B 窗口之外"的场景。
3. **上游（read 单次上限）**：降低预算被单文件打空的概率，从而降低压缩触发频率。

**责任分层**：B 直接保住指令原文（覆盖绝大多数场景）；C 兜住长尾（指令太早、或跨多条隐含表达）。
两者互补，代价都低。

> **实现选型说明**：B 采用"**summary 生成后重新注入 pinned user 消息**"，而非改造
> drain 算法去"中间保留"。理由——现有 drain 依赖"连续前缀 + 孤儿保护"假设，中间保留会
> 破坏该假设并使孤儿保护复杂化。重注入方案完全绕开这一风险，且注入的消息随 `messages`
> 持久化、restore 后仍在。

---

## 4. 方案 B：按角色 Pin user 消息（重注入实现）

### 4.1 规则

`do_compact` 照常 drain `..split_at` 前缀（含孤儿保护，**算法不变**）。在 summary 生成之后，
从**被 drain 的区段**里挑选以下 user 消息，**原样重新注入**回工作 `messages`：

- **首条** `user` 消息（`pin_first_user_message`，通常含全局目标 / 约束 / 产出要求）。
- **最近 K 条** `user` 消息（`pin_recent_user_messages`，K 默认 4；当前任务指令）。

去重后按原始时间序注入，位置在摘要 System 块**之后**，使阅读顺序为
`[被丢弃内容的摘要] → [关键用户指令] → [保留的近期消息]`。

> 为什么"最近 K 条 user"不等于"最近 N 条 any"：一次用户输入后可能跟随几十条
> assistant/tool 消息，`preserve_recent_messages`（默认 40）的窗口可能一条 user 都不含。
> 按角色挑选才能保证指令存活。

### 4.2 为什么不 Pin 所有 user 消息（否决方案 A）

user 消息不一定小——本次根因之一恰是用户粘了大段日志。Pin 所有 user 消息会让"一次性大日志"
永久占住窗口，更浪费预算。故按"首条 + 最近 K 条"挑选，长尾交给 C。

### 4.3 配置（已实现）

```rust
// CompactionConfig（默认值）
pub pin_first_user_message: bool,      // 默认 true
pub pin_recent_user_messages: usize,   // K，默认 4
pub preserve_pending_actions: bool,    // C 开关，默认 true
```

### 4.4 为什么重注入而非"中间保留"

现有 `do_compact` 的孤儿保护依赖"drain 连续前缀"假设。若改成"保留中间某条 user 消息"，
drain 就不再连续，孤儿保护与切分逻辑都要重写。**重注入**方案把 pinned 消息当作"摘要后追加"，
drain 算法零改动，也不会产生新的孤儿（user 消息不参与 tool_call 配对）。

### 4.5 跨压缩不堆叠

pinned user 消息是普通 `User` 消息，会随下一次压缩正常进入 drain→重选→重注入，
**始终是单副本**，不会累积。

---

## 5. 方案 C：未完成动作提取（长尾保险）

> **实现取舍**：C 采用**无状态、每次压缩重新计算**的设计，而非持久化的 `PendingAction`
> 状态机。每次压缩都从被 drain 区段现场扫描一遍，并现场检测完成信号——因此天然不会
> "误提取被永久钉住"（下次压缩重算即可纠正），也无需跨进程持久化。**当前实现为规则层**，
> LLM 语义抽取列为可选后续（见 §5.5）。

### 5.1 动作扫描（规则层）

对被 drain 区段中的 `user` / `assistant` 自然语言内容扫描"**动作动词 + 产出物路径**"共现：

- **动词字典**（中英双语）：`write / create / save / edit / generate / output / produce / export /
  写入 / 写到 / 保存 / 生成 / 落盘 / 输出到 / 创建 / 编辑 / 产出 / 导出`。
- **路径提取**（无依赖启发式）：token 仅含路径安全字符，且含扩展名（`.` 后 1–8 位、
  首字符为字母），因此 `docs/plan.md` 命中，而 `3.14` / `1.2.3` 被拒。
- 命中即产出一个 `{ verb, target }`。

### 5.2 安全阀（回应评审的误提取隐患）

1. **否定语气过滤**：动词命中位置**前 16 字符**窗口内若含否定标记
   （`don't / do not / no need / without / 不要 / 暂不 / 无需 / 别` ...），该动作**跳过**。
   例："do not write anything to secret.txt" → 不提取 `secret.txt`。
2. **数量上限** `MAX_PENDING_ACTIONS = 8` + 按规范化 target 去重：噪声扫描也无法让上下文无界增长。
3. **artifact 自跳过**：扫描时跳过压缩自身产生的 `Conversation summary` / `[Pending Actions]`
   块，避免递归自我强化。

### 5.3 完成检测（无状态、现场判定）

关联 `write / edit / create` 的 `ToolUse{ id, file_path }` 与**成功**（`is_error == false`）的
`ToolResult{ call_id }`，得到"已完成 target 集合"。检测范围覆盖**被 drain 区段 + 存活尾部**
（成功的 write 可能落在任一侧）。

- 某动作的 target 若命中已完成集合 → **不注入**。
- 路径匹配：规范化（trim、`\`→`/`、去 `./`）后**相等**，或**互为路径后缀**（相对 vs 绝对）。

> **已知局限（诚实标注）**：完成检测仅凭"路径匹配 + write 成功"，无法判断"写了但内容
> 不完整/不正确"。当前采取**保守策略**——宁可多留一个已完成项为 pending（下次匹配上即消失），
> 也不误删真正未完成的动作。

### 5.4 注入格式（醒目、置顶、不堆叠）

生成一条独立 `System` 消息，插入到工作 `messages` **最顶部**（在摘要块之前），
让模型压缩后第一眼可见：

```
[Pending Actions — MUST NOT DROP]
- action: write
  target: docs/plan.md
  status: pending
```

**不堆叠**：每次压缩前先 `retain` 掉尾部残留的旧 `[Pending Actions]` 块，再用当次重算结果
重新注入——因此任何时刻**至多一个** pending 块。它是普通 System 消息，随 `messages`
持久化、restore 后仍在。

### 5.5 可选后续：LLM 语义抽取

规则层保证召回底线。未来可复用 `make_summarizer` 的模型通道，让**摘要调用同时**返回
结构化 pending actions（一次 LLM 调用返回 summary + JSON），与规则层取并集，提升语义准确度。
当前未实现，避免每次压缩额外增加一次 LLM 调用。

---

## 6. 上游改造：read 单次上限（未实现，纠正定位）

> **评审纠正**：原设想"给 read 加分页游标（若尚未支持）"与代码不符。核实结论：
> 1. [read.rs](file:///d:/ws/ai/fox-agent-sdk/crates/fox-agent-tools/src/read.rs) **早已支持**
>    `offset` / `limit`（`DEFAULT_LIMIT=5000` 行）并返回下一页游标 + 续读提示。
> 2. `Context guard truncated` **并非 read 工具发出**，而是
>    [`guard_tool_output`](file:///d:/ws/ai/fox-agent-sdk/crates/fox-agent-sdk/src/agent.rs)
>    在**任意工具返回后**按 `SINGLE_OUTPUT_MAX_FRACTION=0.30` /
>    `CONTEXT_GUARD_THRESHOLD=0.85 × token_budget` 截断。

**真正的杠杆**（供后续，本次未做）：
- 下调 read `DEFAULT_LIMIT`（5000 行偏大），并在续读提示中更强地引导小步翻页。
- 或/并调整 `guard_tool_output` 阈值。

该项与 B/C 正交，属"降低压缩发生频率"的上游治理，可独立推进。

---

## 7. 组合与优先级（实现状态）

| 层次 | 作用 | 性质 | 状态 |
|---|---|---|---|
| **B** 按角色 Pin user | 保留首条 + 最近 K 条指令原文 | **直接根因修复** | ✅ 已实现 |
| **C** 未完成动作提取 | 动作意图注入置顶 TODO | 长尾保险（规则层） | ✅ 已实现 |
| read 单次上限 | 降低压缩触发频率 | 上游治理 | ⬜ 未实现 |

**B + C 互补**：B 直接保住指令原文（覆盖绝大多数场景）；C 兜住长尾（指令太早、B 窗口之外）。

---

## 8. 验证（已落地单测）

已实现单测（[compaction.rs](file:///d:/ws/ai/fox-agent-sdk/crates/fox-agent-sdk/src/compaction.rs) tests）：
- `pinned_user_instruction_survives_compaction` — B：指令被 tool 突发挤出窗口后仍原样存活。
- `pending_action_injected_when_write_not_done` — C：未完成 write 生成含 target 的 pending 块。
- `pending_action_not_injected_when_write_completed` — C：write 成功后不再注入。
- `negated_action_is_not_extracted` — C：否定语气（"do not write ..."）不提取。
- `pending_block_does_not_stack_across_compactions` — C：跨多次压缩至多一个 pending 块。

**结果**：`fox-agent-core` 62 + `fox-agent-sdk` 57 全绿。

端到端回归（依赖真实模型，人工验证）：复现超大 prd + tech_design → 要求写入 `docs/plan.md`，
压缩后 agent 应**仍调用 `write`** 落盘，而非纯文本输出。

---

## 9. 决策记录（已定）

1. **C 采用无状态、每次重算**，不引入持久化 `PendingAction`（天然规避"误提取永久钉住"），
   因此不涉及"挂 SessionState 还是 CompactionManager"——pending 块作为普通 System 消息随
   `messages` 持久化即可。
2. **K（`pin_recent_user_messages`）默认 4**；`pin_first_user_message` 默认 true。
3. **C 当前仅规则层默认开启**（`preserve_pending_actions=true`）；LLM 语义抽取列为可选后续（§5.5）。
4. **read 单次上限未实现**，定位已纠正（§6），留作后续上游治理。
