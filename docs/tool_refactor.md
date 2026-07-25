# Tool 执行机制重构方案

## 1. 背景

当前 `fox-agent-sdk` 的工具执行链路，默认将 `ToolOutput.text` 直接回流为对话消息的一部分，再进入后续轮次的工作上下文。这个模型在工具输出较小时足够简单直接，但在以下场景中会快速失效：

- `read` 读取大文件；
- `grep` / `glob` / `search` 在代码库中大范围探索；
- `web_fetch` 抓取长网页正文；
- `mcp__filesystem__read_file` 这类返回大文本的 MCP 文件工具；
- `mcp__<server>__search_*`、`mcp__<server>__list_*` 这类批量枚举型 MCP 工具；
- `mcp__browser__*`、`mcp__fetch__*` 这类外部资源抓取型 MCP 工具；
- 多轮 `grep -> read -> grep -> read` 的探索型任务；
- 一个工具结果本身不大，但多个工具结果连续堆叠。

这些场景的共同问题不是“最终结论太大”，而是“中间过程太大”。主 Agent 真正需要的是结论、证据引用和下一步建议，而不是探索阶段产生的全部原始材料。如果把这些中间材料直接写入主消息流，就会产生以下后果：

- 主上下文被低价值、高体积的中间结果挤占；
- KV cache 命中率下降，系统提示词和高价值历史更容易被冲掉；
- 压缩频率被抬高，导致成本、延迟和不稳定性同时上升；
- 压缩后的摘要容易稀释动作意图，影响后续执行；
- 同一个问题在不同轮次被重复搜索，形成“搜索-压缩-遗忘-再搜索”的循环。

因此，这次重构的核心不是“如何更聪明地压缩工具结果”，而是“如何让大体积中间结果根本不进入主 Agent 工作上下文”。

## 2. 现状问题

### 2.1 当前链路

当前链路可以概括为：

1. 主 Agent 决定调用工具；
2. 工具执行并返回 `ToolOutput { text, json, is_error }`；
3. `text` 作为消息回写到 session working set；
4. 下一轮模型推理继续看到这些工具结果；
5. 当消息总量接近预算时，再由 compaction 机制做截断或摘要。

这种设计的问题在于：工具结果一旦进入消息流，后续所有上下文治理都变成“事后治理”。哪怕之后做了裁剪、摘要、归档，最糟糕的上下文膨胀其实已经发生。

### 2.2 结构性缺陷

现有方案的主要结构性缺陷如下：

- `Tool Result == Message`：把“工具输出”与“要给模型长期保留的信息”混为一谈。
- 主 Agent 亲自吸收噪声：探索型工作和决策型工作都在同一个上下文中进行。
- 压缩处于过后补救位置：无法阻止大文本在当前轮次瞬间撑爆上下文。
- 证据与结论未分层：一旦需要保留证据，往往只能把大段原文一起保留。
- 没有稳定的“结论返回协议”：不同工具、不同任务的结果呈现方式不一致。
- 本地工具与 MCP 工具治理不统一：虽然 MCP 工具被包装成普通 `Tool`，但实际还存在 server 类型、传输方式、能力边界、审批策略和连接状态等额外维度。

## 3. 重构目标

### 3.1 核心目标

1. 让大体积中间信息默认不进入主 Agent 工作上下文。
2. 将“探索”与“决策”拆到不同上下文中执行。
3. 为工具输出建立分层存储：摘要层、引用层、原文层。
4. 让压缩从主治理手段降级为最后兜底手段。
5. 保持现有 Agent Loop、SessionStore、Compaction、Hook 体系可渐进演进，不要求一次性推翻重写。

### 3.2 非目标

本方案暂不追求：

- 立刻实现完整的多 Agent 编排系统；
- 立刻引入分布式调度；
- 改变所有工具协议；
- 一次性替换现有 compaction 机制；
- 让每个工具都必须通过子 Agent 执行。

## 4. 设计原则

### 4.1 上游隔离优先

优先在工具结果进入消息流之前做治理，而不是等进入上下文后再压缩。

### 4.2 结论进入主上下文，原文留在外部

主 Agent 默认只看到结论、证据引用和建议动作；原始大文本留在子 Agent 私有上下文或 artifact store 中。

### 4.3 探索与决策分离

主 Agent 负责目标理解、计划更新、决策和最终输出；子 Agent 负责高噪声探索任务。

### 4.4 按需回读，不整包回灌

若主 Agent 需要复核证据，应按引用做定向回读，而不是把子 Agent 的完整中间过程重新注入主消息流。

### 4.5 渐进落地

优先复用现有 `Model::fork()`、session 双轨存储、hook、compaction 等基础设施，分阶段演进。

### 4.6 本地工具与 MCP 工具统一治理

无论是内置工具还是 MCP 工具，都应进入同一套执行治理模型：统一做路由决策、权限计算、结果外置、审计记录和上下文隔离。

但 MCP 工具还需要补充一层 server 级管理：

- 识别工具所属的 MCP server；
- 为 server 声明 capability profile；
- 结合传输方式区分 `stdio` 与 `sse` 风险；
- 在 tool 级与 server 级同时做审批和结果路由。

## 5. 总体方案

### 5.1 核心思想

将工具执行分为两类：

- **低噪声执行**：输出短小、直接服务于当前推理，可由主 Agent 直接调用，结果以简短消息回流。
- **高噪声执行**：会产生大量中间信息，不直接进入主上下文，而是委派给独立子 Agent，在隔离上下文中完成探索，再回传结构化摘要。

因此，新的设计不是“每个工具都由子 Agent 执行”，而是“高噪声任务由子 Agent 承接，主 Agent 只消费摘要结果”。

这里的“工具”同时包含：

- SDK 内置工具；
- 自定义本地工具；
- 通过 `mcp__<server>__<tool>` 形式暴露的 MCP 工具。

### 5.2 重构后的职责分层

#### 主 Agent

主 Agent 保留以下信息：

- 用户目标与约束；
- 当前任务计划和状态栏；
- 已确认的关键事实和设计决策；
- 子 Agent 回传的结论摘要；
- 当前轮需要执行的少量低噪声工具结果。

主 Agent 不再默认持有以下信息：

- 大文件全文；
- 大范围搜索的命中列表；
- 大网页正文；
- 子 Agent 内部多轮探索过程；
- 未被决策消费的大段中间材料。

#### 子 Agent

子 Agent 是一次性、任务型、上下文隔离的探索执行体，适合承接以下工作：

- 读取大量文件；
- 在代码库中大范围搜索；
- 跨多个候选文件交叉定位；
- 多网页抓取和比对；
- 从大量原始结果中筛选证据和关键结论。

子 Agent 在自己的上下文中完成完整探索，只把一个小而稳定的结果包返回给主 Agent。

#### Artifact Store

Artifact Store 用于保存大体积原始结果，例如：

- 原始文件片段；
- 完整搜索结果；
- 网页正文快照；
- 子 Agent 的中间候选集合；
- 已结构化但不适合直接进入消息流的大对象。

Artifact Store 的存在，使“保留证据”不再等于“把原文塞进主上下文”。

## 6. 新的执行模型

### 6.1 执行模式分类

建议将工具执行显式区分为三种模式：

1. **Inline**
   - 由主 Agent 直接调用；
   - 输出较小；
   - 结果可安全写入当前消息流。

2. **Isolated**
   - 由子 Agent 在隔离上下文中执行；
   - 输出可能很大；
   - 默认只回传结构化摘要。

3. **Externalized**
   - 工具或子 Agent 的输出直接写入 artifact store；
   - 主 Agent 只拿到 artifact 引用和预览摘要；
   - 需要时再定向读取。

这三个模式可以组合使用。一个高噪声任务通常表现为：`Isolated + Externalized`。

对于 MCP 工具，执行模式不只由 tool 名称决定，还应叠加：

- MCP server 的 profile；
- 该 server 的传输类型；
- 返回结果体积预估；
- 是否涉及外部网络或远程副作用。

### 6.2 新链路

重构后的标准链路如下：

1. 主 Agent 识别当前任务是否属于高噪声探索；
2. 若是低噪声任务，直接 inline 执行；
3. 若是高噪声任务，创建 `SubagentTask`；
4. 子 Agent 使用独立 `Model::fork()` 和独立 working context 执行探索；
5. 子 Agent 将大体积原始结果写入 artifact store；
6. 子 Agent 产出 `SubagentSummary`；
7. 主 Agent 仅接收 `SubagentSummary` 并继续决策；
8. 若主 Agent 需要复核，再按 `evidence_refs` 做定向读取。

### 6.3 MCP Tool 的接入与管理

MCP 工具不能只被视为“名字更长的普通工具”。在新的执行模型中，MCP 需要被纳入统一工具平面，但同时保留 server 级管理能力。

#### MCP 需要新增的管理维度

- **server 维度**：同一个 MCP server 下的多个工具共享传输通道、审批策略和能力边界。
- **profile 维度**：不同 server 的本质能力不同，例如 `filesystem`、`external_api`、`browser`、`shell`、`read_only`。
- **transport 维度**：`stdio` 与 `sse` 的稳定性、启动成本、风险画像不同。
- **naming 维度**：Provider 侧仍需使用 `mcp__<server>__<tool>` 兼容命名，但 SDK 内部应保留原始 MCP 标识，避免丢失 server/tool 语义。
- **lifecycle 维度**：MCP server 还涉及连接、断连、重试、工具发现、descriptor 缓存等生命周期问题。

#### MCP 在新链路中的位置

建议将 MCP 工具执行链路明确为：

1. 解析工具名，识别 `server` 与 `tool`；
2. 读取 `McpServerProfile` 与 tool descriptor snapshot；
3. 计算风险与结果路由策略；
4. 判断是否需要委派给子 Agent；
5. 执行 MCP 调用；
6. 将原始结果写入 artifact store 或直接摘要化；
7. 向主 Agent 返回 `SubagentSummary` 或短摘要消息。

也就是说，MCP 需要接入的是“统一执行模型”，而不是绕过它。

## 7. 关键数据结构

以下数据结构是设计建议，名称可在实现时微调，但职责应保持稳定。

### 7.1 SubagentTask

```rust
pub struct SubagentTask {
    pub task_id: String,
    pub objective: String,
    pub task_type: SubagentTaskType,
    pub input_scope: InputScope,
    pub expected_output: ExpectedOutput,
    pub budget: SubagentBudget,
}
```

作用：

- 描述主 Agent 委派给子 Agent 的任务；
- 限定探索范围，避免子 Agent 无边界膨胀；
- 声明期待的返回结果形态。

### 7.2 SubagentSummary

```rust
pub struct SubagentSummary {
    pub task_id: String,
    pub objective: String,
    pub findings: Vec<String>,
    pub evidence_refs: Vec<EvidenceRef>,
    pub recommendations: Vec<String>,
    pub uncertainties: Vec<String>,
    pub next_queries: Vec<String>,
}
```

作用：

- 作为主 Agent 默认消费的唯一回传物；
- 控制体积，保证可预测性；
- 将“结论”和“证据定位”同时保留。

### 7.3 EvidenceRef

```rust
pub struct EvidenceRef {
    pub source_type: EvidenceSourceType,
    pub locator: String,
    pub preview: Option<String>,
}
```

`locator` 可以是：

- 文件路径 + 行号区间；
- artifact id；
- URL；
- session 内部对象引用。

### 7.4 ArtifactRecord

```rust
pub struct ArtifactRecord {
    pub artifact_id: String,
    pub session_id: String,
    pub producer: ArtifactProducer,
    pub artifact_type: ArtifactType,
    pub size_bytes: u64,
    pub content_hash: String,
    pub class: ArtifactClass,
    pub ref_count: u32,
    pub last_access_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub metadata: serde_json::Value,
    pub storage_path: PathBuf,
}
```

作用：

- 保存大对象元信息；
- 提供按需回读能力；
- 支持未来做 TTL、清理、索引和审计。

### 7.4a Artifact Store 存储治理

`artifact store` 不能被设计成无限增长的永久归档层，否则它只是在解决“上下文膨胀”的同时制造“磁盘膨胀”。因此，它应被定义为**有上限、可过期、可清理、按引用提升保留级别的受控缓存层**。

#### 生命周期分级

建议按保留价值将 artifact 分为三类：

1. **ephemeral**
   - 临时探索结果；
   - 默认短 TTL；
   - 优先被淘汰。

2. **referenced**
   - 已被 `evidence_refs`、`SubagentSummary` 或主 Agent 结论引用；
   - 延长 TTL；
   - 优先保留。

3. **pinned**
   - 用户显式要求保留，或系统认为属于关键交付证据；
   - 不参与常规自动淘汰；
   - 仍受全局硬上限约束。

#### 配额模型

为避免存储空间失控，建议同时设置四级配额：

- **单条 artifact 上限**：限制单次保存的最大字节数；
- **单 session 上限**：限制一个会话能占用的总空间；
- **单 workspace / project 上限**：限制当前项目累计占用；
- **全局 store 上限**：限制 `.fox-agent-sdk` 下 artifact 总容量。

一旦超出任一层级配额，系统应立即触发回收，而不是继续无条件写入。

#### 清理策略

建议组合使用以下策略，而不是依赖单一机制：

- **TTL 过期删除**：最基本、最可预测的回收方式；
- **LRU**：优先淘汰最近最少访问的对象；
- **引用优先**：未被引用的 artifact 比已引用对象更早删除；
- **内容去重**：按 `content_hash` 去重，避免同一内容重复落盘；
- **压缩存储**：对大文本对象进行 `gzip` 或 `zstd` 压缩；
- **摘要替代原文**：空间紧张时只保留摘要与定位信息，删除全文。

#### 写入策略

并不是所有大输出都应该进入 artifact store。建议在写入前先经过一次价值判断：

- 纯噪声、一次性候选列表、没有后续复用价值的结果，直接丢弃；
- 可能被二次回读的证据对象，写入 artifact store；
- 体积过大但价值有限的对象，只保留摘要层与引用层，不保存完整原文；
- 对重复出现的同一文件片段、同一 URL 正文、同一 MCP 返回内容，优先复用已有 artifact。

#### 回收时机

GC 不应只在磁盘打满后被动触发，建议至少在以下时机运行：

- artifact 写入完成后做配额检查；
- session 结束时做一次轻量清理；
- Agent 启动时清理过期对象；
- 周期性后台任务清理长时间未访问的 artifact；
- 全局空间接近硬上限时执行强制降级回收。

#### MCP 场景下的特殊要求

MCP 的 `filesystem`、`browser`、`fetch`、`search`、`list` 类工具尤其容易产生大量 artifact，因此建议增加额外约束：

- 搜索结果优先保存结构化命中，不保存全量原始列表；
- 网页正文优先保存提炼摘要，全文按需保留；
- `sse` 远程 server 的长文本结果默认更短 TTL；
- 远程资源保存时附带 `server_name`、`tool_name`、`transport_mode` 和请求元数据，便于回收与审计；
- 未被主 Agent 或 summary 引用的 MCP artifact，优先淘汰。

#### 推荐原则

一句话概括：

> artifact store 的目标是“支持按需回读”，不是“永久保存所有中间结果”。

#### `agent.toml` 配置草案

建议为 artifact store 增加独立配置段，风格与现有 `[compaction]`、`[safety]` 保持一致：

```toml
# ── Artifact Store ───────────────────────────────────────────

[artifact_store]
enabled = true
base_dir = ".fox-agent-sdk/artifacts"

# 单条 artifact 的最大大小；超过后只保留摘要和引用
max_artifact_bytes = 1_048_576          # 1 MiB

# 单个 session 可占用的最大空间
max_session_bytes = 33_554_432          # 32 MiB

# 单个 workspace / project 可占用的最大空间
max_project_bytes = 268_435_456         # 256 MiB

# 全局 artifact store 的硬上限
max_global_bytes = 1_073_741_824        # 1 GiB

# 默认 TTL（小时）
ephemeral_ttl_hours = 24
referenced_ttl_hours = 168              # 7 days
pinned_ttl_hours = 0                    # 0 = no ttl, still subject to hard limit

# 结果写入策略
compress_large_text = true
compression = "zstd"                    # "zstd" | "gzip" | "none"
deduplicate_by_content_hash = true

# 回收策略
gc_on_startup = true
gc_on_session_end = true
gc_after_write = true
gc_high_watermark = 0.85
gc_low_watermark = 0.70

# 淘汰优先级
eviction_policy = "ttl_lru_unref_first" # ttl_lru_unref_first | ttl_only | lru

# 极端情况下是否允许只保留摘要，不保留原文
allow_summary_only_fallback = true

# MCP 结果的额外约束
mcp_remote_ttl_hours = 12
mcp_search_store_full_payload = false
mcp_browser_store_full_html = false
```

#### 配置项说明

- `enabled`
  - 是否启用 artifact store。
  - 若关闭，则系统只能保留摘要层和引用层，无法回读原始大对象。

- `base_dir`
  - artifact 的根目录。
  - 建议固定在项目根目录下的 `.fox-agent-sdk/artifacts`，避免依赖外部环境变量。

- `max_artifact_bytes`
  - 单条 artifact 的大小上限。
  - 超限后不应硬写原文，而应降级为“摘要 + 引用”。

- `max_session_bytes`
  - 单个 session 的空间预算。
  - 防止长会话持续探索把磁盘吃满。

- `max_project_bytes`
  - 当前项目累计 artifact 上限。
  - 防止某个项目长期堆积无用中间结果。

- `max_global_bytes`
  - 全局硬上限。
  - 到达后必须触发强制回收或拒绝继续写入。

- `ephemeral_ttl_hours` / `referenced_ttl_hours` / `pinned_ttl_hours`
  - 对应三种 artifact 生命周期等级。
  - `pinned_ttl_hours = 0` 表示不按 TTL 自动删除，但并不意味着无限制豁免硬上限。

- `compress_large_text`
  - 是否压缩大文本对象。
  - 对源码片段、网页正文、MCP 文本结果通常收益较高。

- `deduplicate_by_content_hash`
  - 是否按内容 hash 去重。
  - 对重复读取同一文件片段、重复抓取同一 URL 特别重要。

- `gc_high_watermark` / `gc_low_watermark`
  - 高低水位线。
  - 例如达到 85% 触发回收，回收到 70% 停止，避免频繁抖动。

- `eviction_policy`
  - 推荐默认使用 `ttl_lru_unref_first`。
  - 即优先删除已过期、最近最少访问且未被引用的 artifact。

- `allow_summary_only_fallback`
  - 当空间或预算过紧时，允许只保留摘要层和证据定位，直接放弃原文落盘。

- `mcp_remote_ttl_hours`
  - 针对远程 MCP server 返回结果的默认 TTL。
  - 远程内容通常复用价值更低、稳定性更差，建议更短。

- `mcp_search_store_full_payload`
  - 是否保存 MCP 搜索工具的全量原始结果。
  - 推荐默认 `false`，只保留命中摘要和关键证据。

- `mcp_browser_store_full_html`
  - 是否保存 MCP browser 工具返回的完整 HTML。
  - 推荐默认 `false`，优先保存正文提炼结果或结构化提取结果。

#### 默认值建议

对当前项目，更稳妥的默认策略是：

- 默认开启 artifact store，但限制较严；
- 默认开启压缩、去重和自动 GC；
- 默认不保存 MCP 搜索的全量 payload；
- 默认不保存 browser 的完整 HTML；
- 默认允许 `summary-only fallback`，优先保证主流程稳定，而不是强保留原文。

#### Rust 配置结构草案

在 Rust 侧，建议将 artifact store 作为 `FoxAgentSdkConfig` 的一级配置模块，风格与现有 `MemoryConfig`、`CompactionConfig`、`SafetyConfig` 保持一致。

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ArtifactStoreConfig {
    /// 是否启用 artifact store。
    pub enabled: bool,
    /// 相对工作目录或项目根目录的存储路径。
    pub base_dir: PathBuf,

    /// 单条 artifact 的最大大小。
    pub max_artifact_bytes: u64,
    /// 单个 session 的总空间上限。
    pub max_session_bytes: u64,
    /// 单个 workspace / project 的总空间上限。
    pub max_project_bytes: u64,
    /// 全局 artifact store 的硬上限。
    pub max_global_bytes: u64,

    /// 不同保留级别的默认 TTL（小时）。
    pub ephemeral_ttl_hours: u64,
    pub referenced_ttl_hours: u64,
    /// 0 表示不按 TTL 自动删除，但仍受硬上限约束。
    pub pinned_ttl_hours: u64,

    /// 大文本是否启用压缩。
    pub compress_large_text: bool,
    /// 压缩算法。
    pub compression: ArtifactCompression,
    /// 是否按内容哈希做去重。
    pub deduplicate_by_content_hash: bool,

    /// 是否在启动时执行 GC。
    pub gc_on_startup: bool,
    /// 是否在 session 结束时执行 GC。
    pub gc_on_session_end: bool,
    /// 是否在每次写入后做配额检查与 GC。
    pub gc_after_write: bool,
    /// 触发 GC 的高水位线。
    pub gc_high_watermark: f64,
    /// GC 目标低水位线。
    pub gc_low_watermark: f64,
    /// 淘汰策略。
    pub eviction_policy: ArtifactEvictionPolicy,

    /// 空间紧张时是否允许只保留摘要与引用。
    pub allow_summary_only_fallback: bool,

    /// 远程 MCP 结果的默认 TTL。
    pub mcp_remote_ttl_hours: u64,
    /// 是否保存 MCP 搜索工具的全量 payload。
    pub mcp_search_store_full_payload: bool,
    /// 是否保存 MCP browser 工具的完整 HTML。
    pub mcp_browser_store_full_html: bool,
}
```

配套枚举建议如下：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArtifactCompression {
    None,
    Gzip,
    Zstd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArtifactEvictionPolicy {
    TtlOnly,
    Lru,
    TtlLruUnrefFirst,
}
```

默认实现建议如下：

```rust
impl Default for ArtifactStoreConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            base_dir: PathBuf::from(".fox-agent-sdk/artifacts"),
            max_artifact_bytes: 1_048_576,
            max_session_bytes: 33_554_432,
            max_project_bytes: 268_435_456,
            max_global_bytes: 1_073_741_824,
            ephemeral_ttl_hours: 24,
            referenced_ttl_hours: 168,
            pinned_ttl_hours: 0,
            compress_large_text: true,
            compression: ArtifactCompression::Zstd,
            deduplicate_by_content_hash: true,
            gc_on_startup: true,
            gc_on_session_end: true,
            gc_after_write: true,
            gc_high_watermark: 0.85,
            gc_low_watermark: 0.70,
            eviction_policy: ArtifactEvictionPolicy::TtlLruUnrefFirst,
            allow_summary_only_fallback: true,
            mcp_remote_ttl_hours: 12,
            mcp_search_store_full_payload: false,
            mcp_browser_store_full_html: false,
        }
    }
}
```

#### 与 `FoxAgentSdkConfig` 的集成方式

建议在 SDK 顶层配置中增加：

```rust
pub struct FoxAgentSdkConfig {
    // ...
    pub memory: MemoryConfig,
    pub compaction: CompactionConfig,
    pub safety: SafetyConfig,
    pub artifact_store: ArtifactStoreConfig,
    pub mcp: McpConfig,
    // ...
}
```

这样可以带来几个直接收益：

- 配置加载路径与现有 `agent.toml` 机制一致；
- 不需要引入额外环境变量或外部配置中心；
- artifact store、compaction、MCP 管理可以在同一配置对象上联合决策；
- 后续可以在 builder 和 runtime 中按需覆写，但默认仍来自标准配置文件。

#### 运行时配合建议

仅有配置结构还不够，运行时最好再补两个衍生对象：

1. **ArtifactRetentionClass**
   - `Ephemeral`
   - `Referenced`
   - `Pinned`

2. **ArtifactWriteDecision**
   - `Drop`
   - `SummaryOnly`
   - `StoreFull`
   - `ReuseExisting { artifact_id }`

其中 `ArtifactWriteDecision` 是真正把“配置”转成“执行动作”的桥梁，适合由 tool routing 或 subagent summary 阶段统一产出。

#### 设计注意点

- `base_dir` 应支持相对路径解析到工作目录或项目根目录，避免依赖环境变量。
- `gc_high_watermark` 必须大于 `gc_low_watermark`，否则配置校验应失败。
- `pinned_ttl_hours = 0` 只表示不按 TTL 自动删除，不代表可以突破 `max_global_bytes` 这类硬限制。
- 远程 MCP 结果相关开关应只影响默认行为，具体 server 仍可被 `McpServerProfile` 覆盖。

### 7.5 McpServerProfile

```rust
pub struct McpServerProfile {
    pub server_name: String,
    pub kind: McpServerKind,
    pub transport: McpTransportKind,
    pub auto_approve: bool,
    pub allowed_tools: Vec<String>,
    pub preferred_routing: ToolResultRouting,
    pub capability_tags: Vec<String>,
}
```

作用：

- 为 MCP server 提供统一的治理入口；
- 决定默认审批策略和默认结果路由；
- 将 server 级能力边界显式化，而不是只按 tool 名字符串匹配。

### 7.6 McpToolDescriptorSnapshot

```rust
pub struct McpToolDescriptorSnapshot {
    pub server_name: String,
    pub tool_name: String,
    pub original_name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub output_hint: Option<String>,
}
```

作用：

- 保存 MCP 工具发现阶段的结构化描述；
- 为路由策略、审批解释、子 Agent 提示词生成提供依据；
- 避免每轮都把完整 descriptor 注入主上下文。

## 8. 主 Agent 与子 Agent 的边界

### 8.1 主 Agent 负责什么

- 理解用户意图；
- 维护计划、目标、状态栏；
- 决定是否发起子任务；
- 消费子任务摘要并做下一步决策；
- 触发编辑、测试、交付等执行动作；
- 面向用户输出最终答案。

### 8.2 子 Agent 负责什么

- 接收明确的探索目标；
- 在隔离上下文中运行多轮搜索/阅读/筛选；
- 吸收高噪声中间结果；
- 将原始结果外置保存；
- 输出结构化结论摘要，而非完整过程日志。

### 8.3 为什么不能让子 Agent 完整回传全过程

如果子 Agent 把自己的搜索过程、候选列表、大段读取结果完整传回主 Agent，那么上下文膨胀问题只是被“横向转移”而非被解决。真正需要稳定的是返回协议，而不是“换一个执行者但仍回传全部细节”。

## 9. 何时使用子 Agent

### 9.1 按任务类型触发

以下任务应优先进入子 Agent：

- 大范围代码搜索；
- 多文件批量读取；
- 文档库或网页库遍历；
- 大量候选结果筛选；
- 带明显探索链路的分析任务。
- `mcp__filesystem__*` 这类高体积文件读取或目录遍历；
- `mcp__browser__*`、`mcp__fetch__*` 这类网页抓取与页面探索；
- 任意返回列表、搜索命中、网页正文或批量记录的 MCP 工具。

### 9.2 按体积预估触发

若工具输出预计超过阈值，则不应先执行再截断，而应直接升级为隔离执行。

示例阈值可以包括：

- 单次结果预计字符数；
- 候选文件数量；
- 网页正文长度；
- 连续 read/grep 次数；
- 当前上下文压力比例。

### 9.3 按行为模式触发

当主 Agent 已进入以下模式时，应自动倾向切到子 Agent：

- 连续两次以上 `grep -> read`；
- 刚发生上下文压力提醒；
- 当前轮已出现一次大输出截断；
- 模型正在“探索”而非“决策”。
- 连续调用同一 MCP server 的搜索、列表、抓取类工具；
- MCP 工具返回体积或条目数量明显超过 inline 阈值。

### 9.4 MCP 场景下的特殊触发规则

MCP 工具除了按体积和行为触发外，还应补充以下规则：

- **按 server kind 触发**：`filesystem`、`browser`、`external_api` 类型默认更倾向 `Isolated` 或 `Externalized`。
- **按 transport 触发**：`sse` 远程 server 返回长文本时，优先走摘要化和 artifact 化，避免主上下文持有远程噪声。
- **按副作用触发**：涉及远程写入、删除、提交等动作的 MCP 工具，不应由子 Agent 在未审批情况下隐式执行。
- **按 descriptor 触发**：当工具描述中含有 `list`、`search`、`read`、`fetch`、`crawl` 等高噪声特征时，默认提高隔离优先级。

## 10. Tool Result 的分层存储

建议将工具结果拆成三层，而不是用单一 `text` 承担所有职责。

### 10.1 层次定义

1. **摘要层**
   - 提供给主 Agent；
   - 体积严格受控；
   - 保留关键事实、结论和建议。

2. **引用层**
   - 提供证据定位；
   - 不直接携带大段原文；
   - 支持主 Agent 按需回读。

3. **原文层**
   - 保存完整原始结果；
   - 默认不进入主消息流；
   - 存储在 artifact store 或子 Agent 私有工作区。

对于 MCP 工具，原文层还应补充来源元数据，例如：

- `server_name`；
- `original_tool_name`；
- `transport_mode`；
- 调用时间、超时配置和 request id；
- 远程资源定位信息。

### 10.2 设计收益

- 保留证据而不污染主上下文；
- 允许结果复核；
- 降低重复搜索概率；
- 为审计、回放和恢复提供基础。

## 11. 与 Compaction 的新关系

### 11.1 压缩不再是第一道防线

重构后，系统的治理优先级应调整为：

0. 子 Agent 上下文隔离；
1. 工具结果预算控制；
2. 噪声直接删除；
3. API 层微压缩；
4. 归档式摘要；
5. 全量压缩。

这意味着压缩仍然重要，但它处理的是“隔离后仍残留的问题”，而不是承担主战场。

### 11.2 为什么压缩仍然需要保留

即使引入子 Agent，仍然会有以下内容进入主上下文：

- 用户消息；
- 主 Agent 的计划与决策；
- 小型工具结果；
- 子 Agent 的摘要结果；
- 系统状态栏和动态注入内容。

这些内容在长会话中仍可能累积，因此 compaction 仍然是必要的安全网。但它的对象应更多是“高价值信息的长期整理”，而不是“海量工具文本的被动清运”。

## 12. 与现有模块的映射关系

### 12.1 Model 层

可以复用现有 `Model::fork()` 能力，为子 Agent 提供独立的运行时状态和上下文窗口。

### 12.2 Session 层

建议继续保留双轨思路，但将“完整历史”再细分：

- 主 Agent 消息历史；
- 子 Agent 子任务摘要历史；
- artifact 元数据历史；
- 可选的子 Agent 内部执行记录。

需要强调的是，主 Agent 的 `messages` 不应自动吸收子 Agent 的完整原始过程。

### 12.3 Hook 层

可以利用现有或未来的以下 hook 点：

- `PreToolUse`：识别是否需要切换到隔离执行；
- `PostToolUse`：阻止大结果直接回灌主消息流；
- `SubagentStop`：对子 Agent 结果进行规范化处理；
- `PreCompact`：在兜底压缩前补充关键上下文。

### 12.4 Tool 层

建议为工具增加执行元信息，例如：

```rust
pub enum ToolResultRouting {
    Inline,
    SummarizeOnly,
    Externalize,
    DelegateToSubagent,
}
```

即便短期内不改变 `ToolOutput` 结构，也可以先通过路由策略决定结果进入哪条通道。

### 12.5 MCP 层

MCP 需要在现有模块映射中被明确成独立治理层，而不是仅在注册时“包成一个 Tool”。

建议增加以下能力：

- **Server Registry**：维护 `server_name -> McpServerProfile` 映射。
- **Descriptor Cache**：缓存 `tools/list` 返回的工具描述，供路由和审批使用。
- **Lifecycle Manager**：管理 server 连接、断连、重试和健康状态。
- **Routing Adapter**：将 MCP tool call 转换为统一的 `ToolResultRouting` 决策输入。
- **Audit Binding**：将 `task_id`、`artifact_id`、`server_name`、`tool_name` 绑定到同一审计链路。

这样 MCP 工具才能真正接入“统一执行机制”，而不是只共享一个表面的 `Tool` trait。

## 13. 实施路径

### Phase 1：结果路由与外置存储

目标：先解决“工具结果默认直写消息流”的问题，不立即引入复杂子 Agent 编排。

建议工作：

- 为工具执行增加结果路由策略；
- 对大输出结果先落 artifact store；
- 消息中仅保留预览摘要和 artifact 引用；
- 保持主 Agent 单体运行不变。
- 为 MCP 增加 `McpServerProfile` 和 descriptor cache；
- 为 MCP 工具建立 server 级默认路由策略。

预期收益：

- 立刻降低单次大工具输出对主上下文的冲击；
- 改造成本最低；
- 不依赖完整子 Agent 生命周期。

### Phase 2：引入探索型子 Agent

目标：将高噪声探索任务整体委派给隔离上下文。

建议工作：

- 引入 `SubagentTask` / `SubagentSummary`；
- 基于 `Model::fork()` 构建子 Agent 运行时；
- 为 `read` / `grep` / `glob` / `web_fetch` 等高噪声工具提供默认委派策略；
- 将 `SubagentStop` 正式接入主流程。
- 为高噪声 MCP server 提供默认委派策略，如 `filesystem` / `browser` / `external_api`。
- 让子 Agent 能消费 MCP descriptor snapshot，而不是把完整 descriptor 注入主上下文。

预期收益：

- 主上下文与探索噪声彻底解耦；
- 搜索类任务的稳定性明显提升；
- 压缩触发频率进一步下降。

### Phase 3：策略自动化与治理闭环

目标：让系统自动判断何时切换执行模式。

建议工作：

- 建立基于上下文压力、输出体积、工具类型的 routing policy；
- 记录子 Agent 命中率、摘要质量、回读频率等指标；
- 引入 artifact TTL、冷热分层和清理机制；
- 将结果路由接入审计与回放体系。
- 加入 MCP server 健康度、调用体积、摘要命中率、审批命中率等专属指标。
- 打通 MCP 工具的连接状态与 routing policy，避免对不稳定 server 做高频 inline 调用。

预期收益：

- 减少 prompt 层人工提示；
- 提升系统自适应能力；
- 为更复杂的多 Agent 协作打基础。

## 14. 风险与权衡

### 14.1 摘要失真风险

子 Agent 只回传摘要，可能导致主 Agent 丢失关键细节。

缓解方式：

- 固定 `SubagentSummary` 结构；
- 强制包含 `evidence_refs`；
- 支持主 Agent 定向回读。

### 14.2 实现复杂度上升

引入子 Agent、artifact store 和路由策略后，系统复杂度会提高。

缓解方式：

- 按阶段落地；
- Phase 1 先做结果外置，Phase 2 再做子 Agent；
- 保持接口稳定，避免一次性大改。

### 14.3 调试难度上升

结果不再全部出现在主消息流中，问题定位可能更分散。

缓解方式：

- 为 artifact 和 subagent task 建立统一 ID；
- 将主 Agent、子 Agent、artifact 之间的关联写入审计记录；
- 在回放工具中支持按 task_id 聚合查看。

### 14.4 MCP 管理复杂度上升

MCP 接入后，除了结果路由，还会引入 server 级配置漂移、descriptor 变化、连接稳定性和跨 server 策略差异。

缓解方式：

- 强制引入 `McpServerProfile`；
- 对 descriptor 做快照和缓存；
- 在审计中记录 `server_name`、`tool_name`、`transport` 和 profile 版本；
- 未声明 profile 的 server 默认按高风险、低信任处理。

## 15. 成功标准

若该方案有效，系统应表现出以下变化：

- 主 Agent 上下文平均体积显著下降；
- `read` / `grep` / `web_fetch` 类任务导致的压缩触发率下降；
- 高噪声 MCP 工具不会再把大文本直接灌入主上下文；
- 不同 MCP server 的工具可以按 profile 获得稳定、可解释的默认执行策略；
- 同类复杂探索任务的完成率提升；
- 上下文压力软中断频率下降；
- 用户得到的最终答案更稳定，重复搜索和遗忘现象减少。

## 16. 结论

这次重构的本质，不是给现有工具输出链路再叠一层更复杂的压缩，而是改变“哪些信息有资格进入主上下文”这一根本决策。

新的原则应当明确：

- 主 Agent 负责决策，不负责吞噬海量探索噪声；
- 子 Agent 负责探索，并在隔离上下文中吸收大体积中间信息；
- 原始结果进入 artifact store，而不是直接进入消息流；
- 主 Agent 默认只消费结构化摘要和证据引用；
- MCP 工具必须纳入统一的路由、审批和 server 级治理模型；
- compaction 继续保留，但退居兜底。

一句话概括：

> 不要把海量工具输出压缩后再塞回主上下文，而要让它们从一开始就停留在主上下文之外。
