# Fox Agent SDK — Memory 设计 PRD（LLM Wiki 记忆系统）

> 版本：v3（2026-08-03 重构）
> 本文档合并了原 memory 重构设计稿（LLM wiki 方案，原 `memory_llm_wiki_design.md` 已删除），并将原 PRD 中的 embedding / ANN / 聚类相关设计移除，重构为 **无 embedding 的 LLM wiki 记忆系统**。
> 核心思想：记忆 = 带标题/摘要/别名/标签的条目 + 显式 `[[链接]]` + 自动维护的 `index`；检索靠 LLM 查询扩展 + 词汇召回 + LLM 重排 + 图链接扩散，语义理解全部复用主 Agent 的 `Provider`/`Model`。

---

## 1. 概述

Fox Agent SDK 的 Memory 模块是一个 **LLM wiki 式长期记忆系统**，为 Agent 提供跨会话、跨任务的知识积累与召回能力。它不依赖 embedding 向量化，而是将记忆组织成 LLM 可直接阅读的结构化文本（条目 + 索引 + 链接），用 LLM 自身的语义理解完成检索。

### 1.1 设计目标

- **LLM 语义召回**：语义匹配由主 Agent LLM 完成（查询扩展 + 重排），不引入 embedding 模型下载与推理
- **图结构记忆**：以 MemoryGraph 组织记忆节点、标签、关系边与 `[[链接]]`，支持级联（BFS）扩展召回
- **自动生命周期**：从对话转录到记忆写入、去重、冲突检测、后台 LLM 增强（enrich）全自动
- **治理与运维**：保留策略、大小限制、导入导出、索引重建、审计日志
- **域自适应兼容**：Memory 是领域无关的基础设施——coding 项目的代码习惯和量化项目的策略偏好共用同一套存储与召回体系。领域上下文通过 AGENTS.md 注入 system prompt（详见 Fox Agent SDK PRD §4.7.1 Domain Adaptation），Memory 层不感知具体领域

### 1.2 非目标

- 不依赖 embedding：无向量化模型、无向量字段、无 ANN/HNSW 索引、无聚类
- 不依赖外部 SaaS memory service（本地 JSON 文件）
- 不自建复杂知识图谱推理引擎
- 不构建专用 Memory 管理 UI
- **不做历史数据迁移**：删除 `embedding*` 字段后，`MemoryGraph` 仍按新结构读写；旧数据不迁移、不补全、不兼容处理

### 1.3 核心用户故事

1. 作为一个终端用户，当我用不同方式表达同一件事时，Agent 应能召回之前的相关记忆（靠 title/aliases + LLM 重排，而非向量相似度）。
2. 作为一个应用开发者，我可以让 Agent 自动从对话中学习用户偏好，无需手动操作。
3. 作为一个合规负责人，我可以删除、禁用、脱敏特定的记忆条目。
4. 作为一个测试工程师，Memory 的召回结果可回归验证。

---

## 2. 领域模型

### 2.1 核心领域概念

```mermaid
graph TD
    Conversation -->|auto_extract| IngestionPipeline
    IngestionPipeline -->|extract| ExtractedMemory
    ExtractedMemory -->|dedupe + contradiction| MemoryEntry
    MemoryEntry -->|enrich 后台 LLM| MemoryEntry
    MemoryEntry -->|persist| MemoryGraph
    MemoryGraph -->|构建| MemoryIndex
    MemoryIndex -->|index.md / pages/*.md| WikiExport
    Query -->|expand_query LLM| QueryExpansion
    QueryExpansion -->|lexical_prefilter 词汇预筛| PrefilterCandidates
    PrefilterCandidates -->|rerank LLM 可选| RerankedCandidates
    RerankedCandidates -->|cascade_retrieve BFS [[链接]]| RecallHits
    MemoryGraph -->|[[链接]]| CascadeExpansion
    RecallHits -->|merge + rank| FinalRecallHits
    FinalRecallHits -->|format| SystemPromptInjection
```

| 概念 | 类型 | 说明 |
|------|------|------|
| `MemoryEntry` | 实体 | 一条长期记忆（内容、类别、置信度、LLM 元数据 title/summary/aliases） |
| `MemoryGraph` | 聚合 | 记忆图（记忆节点 + 标签 + 边 + 元数据） |
| `MemoryIndex` | 值对象 | 紧凑目录（id/title/tags/aliases/summary），随写随更，可注入 prompt 或导出 markdown |
| `MemoryScope` | 值对象 | 作用域：Session（会话级，隔离）/ Project（项目级）/ Global（用户级）/ All |
| `RecallMode` | 值对象 | 召回策略：Recent / Keyword / Wiki |
| `WikiAssistant` | 领域服务 | LLM 语义接口：查询扩展 / 重排 / 增强 / 去重判断 |
| `QueryExpansion` | 值对象 | LLM 展开的检索术语集合（terms/aliases/entities/tags） |
| `EnrichedMemory` | 值对象 | LLM 增强结果（title/summary/tags/aliases/link_ids） |
| `RecallHit` | 值对象 | 单条召回结果（条目 + 分数 + 分项得分 + 来源） |
| `MemoryManager` | 领域服务 | 记忆系统的统一入口 |
| `MemoryInjection` | 值对象 | 注入 prompt 的计算结果 |
| `MemoryExtractor` | 领域服务 | 从对话转录中抽取候选记忆（LLM 驱动） |
| `MemoryRelevanceChecker` | 领域服务 | 验证候选记忆的相关性和冲突（LLM 驱动） |

### 2.2 MemoryEntry 结构

```
MemoryEntry
├── id: String                    # UUID v4
├── category: MemoryCategory      # Fact | Preference | Entity | Correction | Custom
├── content: String               # 记忆内容原文
├── tags: Vec<String>             # 标签列表
├── search_text: String           # 规范化搜索文本（自动生成，含 title/aliases）
├── created_at / updated_at       # 时间戳
├── access_count: u32             # 访问计数
├── source: Option<String>        # 来源（auto_extract / manual / promoted_from:session）
├── trust: TrustLevel             # High | Medium | Low
├── strength: u32                 # 强化次数
├── active: bool                  # 启用状态
├── superseded_by: Option<String> # 被取代的 ID
├── reinforcements: Vec<...>      # 强化面包屑
├── title: Option<String>         # LLM 生成的一行式标题（wiki 页面名）
├── summary: Option<String>       # LLM 生成的一句话摘要（用于 index 与注入）
├── aliases: Vec<String>          # LLM 生成的别名/同义词（提升词汇召回命中）
├── enriched: bool                # 是否已完成 LLM 增强
└── confidence: f32               # 置信度（0-1），支持时间衰减、访问加成
```

`search_text` 的归一化源为 `title + aliases + tags + content` 联合归一化（`refresh_search_text()`），纯文本召回质量直接受益。

### 2.3 MemoryGraph 结构

```
MemoryGraph
├── graph_version: u32           # GRAPH_VERSION
├── memories: HashMap<id, MemoryEntry>
├── tags: HashMap<tag_id, TagEntry>
├── edges: HashMap<source_id, Vec<Edge>>        # 出边（[[链接]]）
├── reverse_edges: HashMap<target_id, Vec<source_id>>  # 入边（反向链接，懒更新）
└── metadata: GraphMetadata
    ├── retrieval_count          # 召回计数
    └── link_discovery_count     # 链接发现计数
```

> **决策记录（2026-08-03）：JSON-first 权威存储**
>
> 1. `MemoryGraph` 并非 embedding 的产物——其 `edges`/`reverse_edges` 正是 `[[链接]]` 与反向链接，`tags` 对应 wiki 分类，`cascade_retrieve` 即链接扩散，可原样复用。
> 2. 单文件 JSON 整体序列化具备原子性，规避多文件 wiki「页面写了、索引没更」的分步不一致问题；存储缓存、GC、审计、export/import、promote 等治理能力全部现成，无需重写。
> 3. 权威存储取 JSON-first；`index.md` / `pages/*.md` 作为导出投影满足可读性诉求，不承担权威源职责。

### 2.4 EdgeKind 边类型

| 边类型 | 遍历权重 | 说明 |
|--------|---------|------|
| `HasTag` | 0.8 | 记忆 ↔ 标签 |
| `RelatesTo { weight }` | weight | 显式关系（`[[链接]]`） |
| `Supersedes` | 0.9 | 新记忆取代旧记忆 |
| `Contradicts` | 0.3 | 矛盾标记 |
| `DerivedFrom` | 0.7 | 派生来源 |

### 2.5 WikiAssistant 接口（`memory/wiki.rs`）

```rust
//! Wiki 式记忆的 LLM 语义接口：查询扩展 / 重排 / 增强 / 去重。

#[async_trait]
pub trait WikiAssistant: Send + Sync {
    /// 将用户查询展开为可检索的术语集合。
    async fn expand_query(&self, query: &str) -> Result<QueryExpansion, String>;

    /// 从候选条目中选出与查询最相关的子集（附理由）。
    async fn rerank(&self, query: &str, candidates: &[MemoryEntry])
        -> Result<Vec<RankedCandidate>, String>;

    /// 为原始条目生成 title/summary/tags/aliases/links。
    async fn enrich(
        &self,
        entry: &MemoryEntry,
        existing_titles: &[String],
    ) -> Result<EnrichedMemory, String>;

    /// 判断两条记忆是否表述同一件事（LLM 去重）。
    async fn are_same(&self, a: &str, b: &str) -> Result<bool, String>;
}

pub struct QueryExpansion {
    pub terms: Vec<String>,      // 规范化术语
    pub aliases: Vec<String>,    // 同义词/别名/中英变体
    pub entities: Vec<String>,   // 命名实体
    pub tags: Vec<String>,       // 建议标签
    pub natural_query: String,   // 自然语言改写（供重排/注入）
}

pub struct EnrichedMemory {
    pub title: String,
    pub summary: String,
    pub tags: Vec<String>,
    pub aliases: Vec<String>,
    /// 命中的既有条目 id 列表（按相关度排序），由 manager 建 [[链接]]。
    pub link_ids: Vec<String>,
}

pub struct RankedCandidate {
    pub id: String,
    pub score: f32,     // 0.0–1.0
    pub reason: String,
}
```

---

## 3. 存储架构

### 3.1 持久化层

```
{storage_dir}/
├── session_scoped/
│   └── {session_id}.json    # 会话级 MemoryGraph（会话隔离，不跨 session 共享）
├── projects/
│   └── {hash}.json         # 项目级 MemoryGraph
├── global.json              # 全局 MemoryGraph
└── memory.audit.jsonl       # 审计日志（JSONL）
```

每个 `{graph}.json` 旁可伴随 `{graph}.index.json`（`MemoryIndex`，与图同目录，随写随更）。

**作用域隔离模型**：

| 作用域 | 存储路径 | Key | 共享范围 |
|--------|---------|-----|---------|
| `Session` | `session_scoped/{session_id}.json` | session_id（已做路径安全净化）| 仅当前会话，会话间隔离 |
| `Project` | `projects/{hash}.json` | 工作目录哈希 | 同一项目目录的所有会话 |
| `Global` | `global.json` | 无 | 所有项目、所有会话 |

- **Session 作用域**用于任务态临时记忆、中间假设、草稿，避免污染跨会话召回。需通过 `with_session_id()` 绑定会话 ID
- **记忆提升**：Session 记忆可通过手动 `promote_memory()` 或自动提升（`auto_promote_enabled` + strength 阈值）沉淀到 Project/Global，避免有价值的知识随会话结束丢失。提升为单向：不能提升 INTO Session

- **存储格式**：MemoryGraph JSON（HashMap-based，清晰、可人工阅读）
- **缓存策略**：全局 LRU 缓存（`MemoryGraphCache`），减少重复 I/O
- **备份恢复**：写入前先写 `.tmp`，原子 rename。损坏文件自动回退 `.bak`
- **默认路径**：`$FOX_AGENT_DIR/memory/` → `$DATA_DIR/fox-agent/memory/` → `~/.fox-agent/memory/`

### 3.2 MemoryGraphCache（LRU）

```rust
// 缓存 key = 文件路径 abs
// 缓存条目 = MemoryGraph + last_access_time
// 最大容量：可配置，默认 128 条目
// 清理策略：LRU eviction
pub fn cache_graph(path: PathBuf, graph: &MemoryGraph) { ... }
pub fn cached_graph(path: &Path) -> Option<MemoryGraph> { ... }
pub fn invalidate_cache(path: &Path) { ... }
```

### 3.3 MemoryIndex 与 wiki 导出（新增 `memory/index.rs`）

```rust
/// 内存中的紧凑目录（随图缓存失效而重建）。
pub struct MemoryIndex {
    pub entries: Vec<IndexEntry>,   // {id, title, tags, aliases, summary}
    pub updated_at: DateTime<Utc>,
}

impl MemoryIndex {
    pub fn from_graph(graph: &MemoryGraph) -> Self;             // O(n) 构建
    pub fn lexical_score(&self, entry_id: &str, exp: &QueryExpansion) -> f32;
    pub fn to_prompt(&self, budget_chars: usize) -> Option<String>;   // llms.txt 风格
    pub fn to_markdown(&self) -> String;                              // 导出 index.md
    pub fn page_path(slug: &str) -> String;                          // pages/<slug>.md
}
```

- **存储**：`{graph}.index.json` 与图同目录；写入流程（remember/forget/update/enrich/import）后调用 `rebuild_index(scope)` 并复用现有 `invalidate_cache` 机制
- **注入**：SDK 在无具体查询的轮次（或作为兜底）注入 index 摘要，让 agent「知道有什么」，需要细节时再用 memory 工具取整条
- **导出**：可选把每条记忆渲染为 `pages/<slug>.md`（frontmatter: title/tags/aliases + 正文），配合 `index.md` 构成可 git 管理、可人工阅读的 wiki 目录

---

## 4. Wiki 语义层

### 4.1 WikiAssistant 实现（复用 Provider）

`ProviderBackedWikiAssistant` 复用 `relevance.rs` 的 `call_provider` 模式（同一 `Provider` + `model_id`），不新增外部服务。LLM 语义能力通过 trait 注入（与现有 `MemoryExtractor` 一致）。

Prompt 设计要点：

- **expand_query**：输出 `TERMS: a, b, c` / `ALIASES: …` / `ENTITIES: …` / `TAGS: …` 行式格式，解析与 `ExtractedMemory` 一致（防御式：一行一字段）
- **rerank**：输入 `## Query` + `## Candidates`（编号列表，每条只给 id/title/summary），输出 `ID|SCORE|REASON` 行式格式
- **enrich**：输入原文 + 既有 title 列表（`existing_titles` 只取前 80 条截断，参考 extractor 做法）
- **are_same**：输出 `YES`/`NO`

### 4.2 写入管线

#### 4.2.1 同步快路径（API 不变）

`remember_project` / `remember_global` / `remember_session` / `remember` 保持同步：

```rust
pub fn remember(&self, entry: MemoryEntry, scope: MemoryScope) -> Result<String, String> {
    let mut entry = entry;
    entry.refresh_search_text();          // 含 title/aliases 归一化
    let graph = /* 按 scope 加载 */;
    let id = graph.add_memory(entry);
    /* 保存 + 审计 */
    self.invalidate_and_schedule_index(scope); // 使索引缓存失效（惰性重建）
    if self.cfg.enrich_on_write {
        self.spawn_enrich(id, scope);      // 后台异步 LLM 增强
    }
    Ok(id)
}
```

`prepare_entry_for_storage` 简化为 `refresh_search_text()` 纯文本处理。

#### 4.2.2 后台异步增强（`spawn_enrich`）

`MemoryManager` 新增 `wiki_assistant: Option<Arc<dyn WikiAssistant>>`（`#[cfg(test)]` 可注入 mock，生产由 SDK 装配）。enrich 流程：

```
spawn(async move {
    let entry = graph.get(id)?;                          // 读
    let existing_titles = index.all_titles();            // 复用 MemoryIndex
    let enriched = assistant.enrich(&entry, &titles).await?;
    graph.update(id, enriched);                          // 写 title/summary/tags/aliases/enriched=true
    if link_discovery_enabled {
        for link_id in enriched.link_ids {
            graph.link_memories(id, &link_id, 0.8);      // 建 RelatesTo 边（[[链接]]）
        }
    }
    save(graph); rebuild_index(scope);                    // 索引随写随更
});
```

- 竞态：enrich 重写时加 `RwLock` 或采用「读-改-写单文件」原子保存（现有 `write_json` 已是整体序列化，冲突窗口小，可接受；后续可加乐观版本号）
- enrich 失败：静默保留 `enriched=false`，条目仍可被词汇召回，下次 GC/`rebuild_index` 重试

### 4.3 去重（无 embedding）

```rust
fn duplicate_match(existing: &MemoryEntry, candidate: &MemoryEntry) -> bool {
    if existing.searchable_text() == candidate.searchable_text() { return true; }
    // 标题/别名/标签/内容 联合词重叠比例
    let overlap = title_alias_tag_overlap(existing, candidate);
    if overlap >= self.cfg.dedupe_min_overlap_ratio { return true; }
    // 可选：LLM 判断（异步，仅 wiki_enabled 且 assistant 存在时）
    false
}
```

`find_duplicate_for_ingestion` 保持同步词汇判断；`ingest_transcript` 中可选的 LLM `are_same` 放在 `verify_relevance` 同一 async 通道（不影响现有同步路径）。

---

## 5. 召回引擎

### 5.1 RecallMode 策略矩阵

| Mode | 种子来源 | 排序 | 需要 LLM |
|------|---------|------|---------|
| **Recent** | 全部 active 记忆 | `recency × 0.85 + trust × 0.15` | 否 |
| **Keyword** | `search_text` 关键词匹配 | `keyword × 0.65 + recency × 0.2 + trust × 0.15` | 否 |
| **Wiki** | 查询扩展 → 词汇预筛 → 图 BFS | 词汇分 + 重排分 + recency + trust | 可选（开关控制） |

`RecallMode` 枚举：`Recent` / `Keyword` / `Wiki`。

### 5.2 recall_wiki 流程

```
① 查询扩展（LLM，可选）
   expand_query(query) → QueryExpansion
   失败或未启用 → QueryExpansion::from_query(query) 纯词汇

② 词汇预筛：title(3.0) / aliases(2.0) / tags(1.5) / content(1.0) 加权
   lexical_prefilter(all, expansion, max_candidates) → top-N

③ LLM 重排（可选）
   rerank(query, candidates) → top-K（limit × 2）
   未启用 → 按词汇分直接截断

④ 图链接 BFS 扩散（复用 cascade_retrieve）
   seed_ids = chosen.map(id)
   cascade_retrieve(seed_ids, seed_scores, depth) → 合并去重取 max score
   → top_k(limit)
```

同步 `recall`（无 LLM）保持可用：`RecallMode::Keyword` 仍走纯文本，`RecallMode::Wiki` 在无 assistant 时退化为「词汇预筛 + 图扩散」。

### 5.3 RecallHit 返回值

```rust
pub struct RecallHit {
    pub entry: MemoryEntry,
    pub score: f32,                      // 综合得分
    pub score_breakdown: ScoreBreakdown, // 分项得分
    pub retrieval_source: RetrievalSource, // 来源标识
}

pub struct ScoreBreakdown {
    pub keyword_score: Option<f32>,      // 词汇匹配分
    pub recency_score: f32,
    pub graph_score: Option<f32>,        // 图扩散分
    pub trust_score: f32,
    pub final_score: f32,
}
```

---

## 6. Ingestion Pipeline（写入流水线）

### 6.1 完整流水线

```
Conversation Transcript
  │
  ▼
[1] MemoryExtractor::extract(transcript, existing_texts)
  │     → Vec<ExtractedMemory>              (LLM 抽取候选记忆)
  │
  ▼
[2] For each candidate (limited by auto_extract_max_items_per_turn):
  │
  ├── [2a] prepare_entry_for_storage  →  refresh_search_text（纯文本归一化）
  │
  ├── [2b] verify_relevance?
  │    └── MemoryRelevanceChecker::check_relevance → (bool, reason)
  │         ├── false → skipped_irrelevant++, continue
  │         └── true → 继续
  │
  ├── [2c] find_duplicate_for_ingestion
  │    ├── 候选匹配：同 category + 词重叠比例 ≥ dedupe_min_overlap_ratio
  │    │   重叠 = title/alias/tag/内容 联合词重叠 || 文本完全一致
  │    │   （可选：LLM are_same，异步通道）
  │    ├── 命中 → reinforce_memory(existing), skipped_duplicates++, continue
  │    └── 未命中 → 继续
  │
  ├── [2d] find_contradiction_for_ingestion
  │    ├── 候选：同 category + 文本不重叠
  │    └── MemoryRelevanceChecker::check_contradiction(new, existing)
  │         ├── 无冲突 → 继续写入
  │         └── 有冲突 → 写入后 apply_contradiction_policy
  │
  └── [2e] remember(candidate, scope)  →  写入 + 存盘（触发后台 enrich）
```

### 6.2 ContradictionPolicy 冲突策略

| 策略 | 行为 |
|------|------|
| `Ignore` | 两者共存，不做处理 |
| `Supersede` | 新记忆取代旧记忆（旧 marked inactive + superseded_by） |
| `DowngradeConfidence` | 旧记忆置信度衰减 `contradiction_confidence_decay` |
| `MarkContradictionEdge` | 创建双向 `Contradicts` 边 |

### 6.3 记忆增强

```
reinforce(existing_id):
  - strength += 1
  - confidence = min(confidence + 0.05, 1.0)
  - 追加 Reinforcement 面包屑 (session_id, message_index, timestamp)
```

### 6.4 内存管理接口

```rust
// SDK 层 MemoryManager（fox-agent-sdk/src/memory.rs）
trigger_ingestion_for_turn(session_id, agent_model, memory_manager)
  ↓
CoreMemoryManager::ingest_transcript(transcript, extractor, checker)
```

触发条件：`auto_extract = true`，每轮结束后异步 `tokio::spawn`。

---

## 7. Memory Prompt 注入

### 7.1 注入管线

```
每个 Turn 开始前：

1. trigger_recall_for_next_turn()
   └── memory_manager.recall(query, limit, RecallMode::Wiki, All)
        └── wiki_enabled=false → RecallMode::Keyword

2. format_recall_hits_prompt(hits, max_chars, max_per_category)
   ├── 按 category 分组：corrections → facts → preferences → entities
   ├── 每组最多 max_per_category 条
   ├── 总字符数 ≤ max_chars
   └── 输出 Markdown 格式：## Category\n- content [source:trust]

3. 无具体查询的轮次（或兜底）：注入 MemoryIndex 摘要（to_prompt 按 index_budget_chars 裁剪）
   让 agent「知道有什么」，需要细节时再用 memory 工具取整条

4. 注入到 system prompt 的 dynamic_part
```

### 7.2 注入预算控制

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `max_candidates` | — | 检索候选项上限 |
| `max_results` | — | 最终返回结果上限 |
| `injection_max_chars` | — | 注入 prompt 的最大字符数 |
| `injection_max_per_category` | — | 每类最多注入条数 |
| `index_budget_chars` | — | MemoryIndex 注入字符预算 |

---

## 8. 治理与运维

### 8.1 CRUD API

| 操作 | 方法 | 说明 |
|------|------|------|
| 写入 | `remember(entry, scope)` | 同步快路径 + 后台 enrich（scope 支持 Session/Project/Global）|
| 提升 | `promote_memory(id, from, to)` | 将记忆从一个作用域提升到更长生命周期的作用域（如 Session→Project）|
| 召回 | `recall(query, limit, mode, scope)` | 3 种策略（Recent/Keyword/Wiki）|
| 搜索 | `search(text, scope)` | 精确关键词搜索 |
| 列表 | `list(scope)` | 按更新时间排序 |
| 删除 | `forget(id)` | 从图中移除 |
| 禁用/启用 | `disable_memory(id)` / `enable_memory(id)` | 标记 active 字段 |
| 脱敏 | `redact_memory(id, replacement)` | 替换内容 + 刷新 search_text |
| 统计 | `graph_stats()` | (memories, tags, edges) |
| 导出 | `export_to_path(scope, path)` | MemoryExportBundle JSON |
| 导入 | `import_from_path(path, merge)` | 替换或合并 |
| GC | `gc(max_age_hours)` | 清理过期 graph 文件 |
| 压缩 | `compact(scope, max_age_hours)` | 保留策略 + 大小限制 + GC |
| 增强 | `enrich(id)` | 对指定条目执行 LLM 增强（title/summary/aliases/links）|
| 重建索引 | `rebuild_index(scope)` | 重建 MemoryIndex + 补 enrich（原 reindex 语义转变）|
| 链接 | `link_memories(from, to, weight)` | 手工建 `RelatesTo` 边增强图扩散 |

### 8.2 保留策略

```
compact() 两步：

1. apply_retention_policy(graph)
   └── 删除 updated_at < Utc::now() - retention_days 的条目

2. apply_size_limit(graph)
   └── 超过 memory_size_limit → 删除低分条目（按 score + 时间排序）
```

### 8.3 标签与链接

```
tag_memory(id, tag_name):
  └── 创建/更新 TagEntry，添加 HasTag 边

link_memories(from_id, to_id, weight):
  └── 创建 RelatesTo 边
```

### 8.4 审计日志

```jsonl
{"timestamp":"2026-06-24T10:30:00Z","action":"forget","scope":"project","memory_id":"mem_xxx"}
{"timestamp":"2026-06-24T10:31:00Z","action":"redact","scope":"project","memory_id":"mem_xxx","details":{"before":"...","after":"[redacted]"}}
{"timestamp":"2026-06-24T10:32:00Z","action":"enrich","scope":"project","memory_id":"mem_xxx","details":{"updated":42}}
```

所有变异操作自动追加到 `memory.audit.jsonl`。

---

## 9. MemoryConfig 完整配置

```rust
pub struct MemoryConfig {
    pub enabled: bool,                          // 总开关
    pub storage_dir: Option<PathBuf>,           // 存储根目录

    // ── Wiki 模式 ──
    pub wiki_enabled: bool,                     // 启用 wiki 检索（查询扩展/重排/链接发现）
    pub enrich_on_write: bool,                  // 写入后是否后台 LLM 增强
    pub query_expansion_enabled: bool,          // 启用 LLM 查询扩展（关闭则退化为纯词汇召回）
    pub rerank_enabled: bool,                   // 启用 LLM 重排（关闭则按词汇分直接截断）
    pub rerank_candidate_multiplier: usize,     // 词汇预筛候选数 = max_results × 该倍数
    pub link_discovery_enabled: bool,           // 写入时是否 LLM 发现与既有条目的 [[链接]]
    pub index_budget_chars: usize,              // 注入的 MemoryIndex 字符预算
    pub dedupe_min_overlap_ratio: f32,          // 词汇去重重叠比例阈值

    // ── 检索与注入 ──
    pub max_candidates: usize,                  // 检索候选上限
    pub max_results: usize,                     // 召回结果上限
    pub injection_max_chars: usize,             // 注入字符上限
    pub injection_max_per_category: usize,      // 每类注入上限
    pub max_graph_depth: usize,                 // BFS 最大深度
    pub verify_relevance: bool,                 // LLM 相关性验证
    pub verify_model: Option<String>,           // 验证模型 ID

    // ── 自动抽取 ──
    pub auto_extract: bool,                     // 自动抽取
    pub auto_extract_scope: AutoExtractScope,   // 存储作用域（Session/Project/Global）
    pub auto_extract_message_window: usize,     // 抽取窗口
    pub auto_extract_max_items_per_turn: usize, // 每轮最大抽取数

    // ── 自动提升 ──
    pub auto_promote_enabled: bool,             // 启用 Session 记忆自动提升
    pub auto_promote_strength_threshold: u32,   // 提升阈值（strength ≥ 该值触发，默认 3）
    pub auto_promote_target: AutoExtractScope,  // 自动提升目标作用域（Project/Global）

    // ── 冲突与保留 ──
    pub contradiction_policy: ContradictionPolicy,
    pub contradiction_confidence_decay: f32,    // 冲突置信度衰减
    pub retention_days: Option<u64>,            // 保留天数
    pub memory_size_limit: Option<usize>,       // 最大条目数
}
```

**已删除的配置项**（原 embedding/ANN/聚类相关）：`embedding_enabled` / `embedding_model_path` / `embedding_model_id` / `embedding_hf_endpoint` / `embedding_hf_token` / `embedding_cache_dir` / `auto_download_embedding_model` / `ann_enabled` / `ann_min_vectors` / `ann_candidate_multiplier` / `cluster_similarity_threshold` / `cluster_min_members` / `rebuild_on_model_change`；`dedupe_similarity_threshold`（余弦阈值）改为 `dedupe_min_overlap_ratio`（词重叠比例）。

---

## 10. 测试体系

### 10.1 测试覆盖

| 测试 | 覆盖内容 |
|------|----------|
| `test_remember_and_recall` | 写入 → keyword 召回 |
| `test_search_and_forget` | 搜索 → 删除 → 确认不可召回 |
| `test_tag_and_link` | 标签、链接、关联查询 |
| `test_graph_stats` | 统计计数 |
| `test_recall_detailed_exposes_source_and_breakdown` | RecallHit 结构完整性 |
| `test_cascade_recall_surfaces_graph_hits` | Wiki 模式图扩散 |
| `test_wiki_recall_matches_alias_without_literal_overlap` | 无字面重叠但别名命中 |
| `test_enrich_populates_title_summary_aliases_links` | 后台 enrich（mock assistant）|
| `test_export_and_import_roundtrip` | 导出导入完整性 |
| `test_import_bundle_merge` | 导入合并 |
| `test_import_bundle_replace` | 导入替换 |
| `test_disable_enable_and_redact_memory` | 启用/禁用/脱敏 + 审计 |
| `test_compact_applies_retention_and_size_limit` | 保留 + 大小限制 |
| `test_regression_dataset_covers_keyword_and_wiki_modes` | 双模式回归 |
| `session_memory_is_isolated_from_project` | Session 作用域隔离 |
| `manual_promote_moves_session_memory_to_project` | 手动提升 Session→Project + 溯源 |
| `promote_into_session_is_rejected` | 拒绝反向提升 INTO Session |
| `auto_promote_triggers_at_strength_threshold` | 强化达阈值自动提升 |
| `test_ingest_transcript_reinforces_duplicates` | 重复强化 |
| `test_ingest_transcript_marks_contradictions` | 冲突标记 |
| `test_ingest_transcript_skips_irrelevant_candidates` | 不相关过滤 |
| `test_index_serialization_and_prompt_budget` | index.json 序列化 + 注入字符预算 |

### 10.2 测试基础设施

- `new_test()`：独立临时目录的 MemoryManager
- `StaticWikiAssistant`：确定性注入查询扩展 / 重排 / enrich 结果（仿 `StaticExtractor`）
- `StaticExtractor` / `StaticChecker`：确定性注入/验证

---

## 11. 子模块清单

| 文件 | 内容 |
|------|------|
| `mod.rs` | MemoryManager（主入口 + CRUD + ingest + recall + 治理）|
| `types.rs` | MemoryEntry, TrustLevel, MemoryCategory, MemoryScope, RecallMode, Reinforcement |
| `graph.rs` | MemoryGraph, Edge, EdgeKind, TagEntry, GraphMetadata, BFS cascade |
| `wiki.rs` | WikiAssistant trait + ProviderBackedWikiAssistant + QueryExpansion/EnrichedMemory/RankedCandidate |
| `index.rs` | MemoryIndex 构建/打分/to_prompt/to_markdown |
| `ranking.rs` | top_k_by_score (BinaryHeap) |
| `relevance.rs` | MemoryExtractor, MemoryRelevanceChecker, ExtractedMemory |
| `prompt.rs` | 记忆格式化（prompt 注入 + 展示）|
| `storage.rs` | JSON 读写 + 备份恢复 + LRU 缓存 + GC |

### 11.1 SDK 层对接（fox-agent-sdk/src/memory.rs）

| 组件 | 功能 |
|------|------|
| `MemoryInjection` / `MemoryInjectionState` | Agent turn 间的注入状态机 |
| `trigger_recall_for_next_turn()` | 背景异步 wiki recall（`wiki_enabled` 时用 `RecallMode::Wiki`）|
| `MemoryInjectionEvent` | 驱动状态转换的事件 |
| `model_for_memory_tasks()` | 为记忆任务 fork 模型实例（供 WikiAssistant 复用）|
| `run_memory_prompt()` | 运行 LLM 抽取/验证/查询扩展/重排 |
| `auto_extract` | 每轮后异步触发 ingestion pipeline |
| `wiki_assistant` 装配 | 构造 `ProviderBackedWikiAssistant` 注入 `MemoryManager` |

---

## 12. 边界与约束

### 12.1 性能

- LLM 调用（扩展/重排/enrich）全部异步，不阻塞主 Agent Loop；词汇预筛先行收敛候选，控制 LLM 调用次数
- JSON 文件使用 LRU 内存缓存，避免重复解析
- `MemoryIndex` 内存构建 O(n)，随图缓存失效惰性重建

### 12.2 降级策略

| 场景 | 行为 |
|------|------|
| wiki_enabled 未启用 | Wiki → fallback Keyword（纯词汇召回）|
| expand_query 失败 | 退化为 `QueryExpansion::from_query(query)` 纯词汇 |
| rerank 失败/未启用 | 按词汇分直接截断 |
| enrich 失败 | 条目保留 `enriched=false`，仍可被词汇召回，下次 rebuild_index 重试 |
| LLM 不可用（无 assistant） | Wiki → 词汇预筛 + 图扩散，无语义增强 |
| JSON 文件损坏 | 自动回退 `.bak` 备份 |
| 备份也损坏 | 返回空 MemoryGraph（优雅降级，不 panic） |

### 12.3 安全性

- 所有变异操作记录审计日志
- `redact_memory` 支持内容替换 + 刷新 search_text
- `disable_memory` 标记 inactive 但不删除
- `forget` 永久删除（不可逆）
- 无敏感信息主动进入记忆的检查机制（依赖上层脱敏）

### 12.4 域自适应（Domain Adaptation）

Memory 层是**完全领域无关**的基础设施。无论 Agent 在 coding、量化交易、数据分析还是运维场景下运行，Memory 都使用同一套存储、召回、去重、冲突检测逻辑。领域的语义差异由 system prompt 中的 AGENTS.md 注入处理（详见 Fox Agent SDK PRD §4.7.1），Memory 返回的结果被注入到 `dynamic_part`，与领域指令共同作用于 Agent 决策。

---

## 13. 典型用例

### 用户偏好记忆

```
会话 1: 用户说"我喜欢简洁的 Rust 代码"
  → auto_extract → MemoryEntry(category=Preference, "喜欢简洁 Rust", trust=High)
  → 后台 enrich → title="简洁 Rust 代码偏好", aliases=["优雅代码","可读性优先"]

会话 2: 用户问"帮我写个排序函数"
  → wiki recall: expand_query("帮我写个排序函数") → terms=[排序,函数,rust...]
  → 词汇预筛 + 重排命中"简洁 Rust 代码偏好"（经 aliases/title 关联）
  → 注入到 system prompt
  → Agent 生成短小精悍的代码
```

### 知识纠正

```
会话 1: Agent 错误使用 serde_json::from_reader
  → auto_extract → MemoryEntry(category=Correction, "用 serde_json::from_str", trust=High)

会话 2: 类似场景
  → wiki recall 命中纠正
  → Agent 不再犯同样的错误
```

### 跨项目用户偏好

```
项目 A: 用户说"用 tabs 而不是 spaces"
  → remember_global() → 全局 memory

项目 B（不同目录）:
  → list(MemoryScope::Global) → 能读到 tabs 偏好
  → 注入 prompt
```

### 会话隔离与记忆提升

```
会话 A: "排查断连原因"（诊断任务）
  → remember(scope=Session) → session_scoped/{A}.json（任务态临时记忆）
  → 探索中记录的中间假设不会污染其他会话

会话 B: "重构支付模块"（并行任务）
  → recall(scope=Session) → 只召回会话 B 自己的记忆
  → 看不到会话 A 的诊断假设，避免任务干扰

有价值内容沉淀:
  → 手动: promote_memory(id, Session, Project)  → 显式提升到项目级
  → 自动: auto_promote_enabled=true 时，会话记忆被反复强化
          （strength ≥ auto_promote_strength_threshold）自动提升
  → 提升后记忆带 source="promoted_from:session"，可审计
```

### 运维清理

```rust
// 删除过期记忆（保留 90 天，最多存 1000 条）
let stats = memory_manager.compact(scope, 90 * 24).unwrap();
// stats.project_removed = 3, stats.global_removed = 1

// 导入之前导出的记忆
let stats = memory_manager.import_from_path(path, true).unwrap();

// 重建索引并补齐未增强条目的 LLM 元数据
let updated = memory_manager.rebuild_index(MemoryScope::All).unwrap();
```

---

## 14. 重构范围与实施路线图

### 14.1 重构范围（从原实现移除的内容）

**文件删除**
- `crates/fox-agent-core/src/memory/embedding.rs`
- `crates/fox-agent-core/src/memory/ann.rs`

**依赖删除**
- `crates/fox-agent-core/Cargo.toml`：`mistralrs`（如存在 feature 门控一并移除）

**方法/类型删除（core）**
- `MemoryManager`：`embedding_provider` 字段、`semantic_enabled()`、`recall_semantic`、`recall_semantic_with_ann`、`reembed_graph`、`refresh_graph_embedding_metadata`、`ensure_scope_embeddings_current`、`maybe_rebuild_graph_for_model_change`、`reembed`、`rebuild_ann`、`refresh_clusters`
- `graph.rs`：`ClusterEntry`、`refresh_clusters`、`clear_clusters`、`TempCluster`、`cosine_similarity`；`GraphMetadata` 的 embedding 相关字段
- `types.rs`：`embedding/embedding_model/embedding_version` 字段、`with_embedding`、`set_embedding_metadata`、`RecallMode::Semantic`
- `mod.rs`：`RetrievalSource::Semantic/SemanticAnn`、`semantic_duplicate_like`（embedding 分支）

**工具/SDK/Py 调用面**
- `fox-agent-tools/src/memory.rs`：删除 action `reembed` / `refresh_clusters` / `rebuild_ann`；`recall` mode 枚举去掉 `semantic`、增加 `wiki`；`stats` 输出去掉 clusters；新增 action `rebuild_index` / `enrich`
- `fox-agent-sdk/src/memory.rs`：`semantic_enabled()` → `wiki_enabled()`；`trigger_recall_for_next_turn` 在 `wiki_enabled` 时用 `RecallMode::Wiki`；装配 `wiki_assistant`
- `fox-agent-py/src/memory.rs`：移除 `embedding_enabled` 参数、`SEMANTIC` 常量（改为 `wiki`）

**可复用的非 embedding 组件**（保留）：`keyword_match_score` / `memory_matches_search` / `memory_score` / `effective_confidence`（时间衰减）；`has_text_overlap` / `find_duplicate_in_graph`；`MemoryGraph`（tags/edges/reverse_edges/BFS `cascade_retrieve`）；`NarrativeRecord` 与 `remember_narrative` / `build_narrative_prompt`；`MemoryExtractor` / `MemoryRelevanceChecker`；存储层（JSON 图 + 缓存 + GC + 审计）。

### 14.2 实施路线图

> **当前进度（2026-08-03）**：Phase 1（数据模型与配置纯删改）、Phase 2（WikiAssistant + MemoryIndex 新模块）、Phase 3（写入管线）、Phase 4（检索管线）、Phase 5（索引持久化 / 批量补增强 / wiki 导出）已完成并全绿；Phase 6-7 待执行。`RecallMode::Wiki` 已接入 `recall_wiki`（同步词汇预筛+图扩散），LLM 查询扩展/重排经 `recall_wiki_async` 使用。

**Phase 0 — 调用面预清理（已完成）**
- [x] `RecallMode` 删除 `Cascade`、新增 `Wiki`（core/tools/sdk/py 全部调用点重构为 `wiki`）
- [x] `recall_detailed` 的 `Wiki` 分支暂映射到 `recall_cascade()`（过渡期，Phase 4 换成 `recall_wiki`）
- [x] 文档合并：memory_prd.md 重构为 LLM wiki 方案，删除 design doc，同步 prd.md / application-developer-guide.md / README / RELEASE / CLAUDE / whitepaper / tools README

**Phase 1 — 数据模型与配置（纯删改，编译期可过）**
- [x] `types.rs`：删除 `embedding/embedding_model/embedding_version` 字段、`with_embedding`/`set_embedding_metadata`；新增 `title/summary/aliases/enriched`；`refresh_search_text` 纳入 title/aliases；`RecallMode` 删 `Semantic`
- [x] `config.rs`：删除 embedding/ann/cluster 字段；新增 wiki 系列字段（`wiki_enabled`/`enrich_on_write`/`query_expansion_enabled`/`rerank_enabled`/`rerank_candidate_multiplier`/`link_discovery_enabled`/`index_budget_chars`/`dedupe_min_overlap_ratio`）；`dedupe_similarity_threshold` → `dedupe_min_overlap_ratio`；`Default` 同步
- [x] `graph.rs`：删除 `ClusterEntry`/`refresh_clusters`/`clear_clusters`/`TempCluster`/`cosine_similarity`；`GraphMetadata` 清理 embedding 字段（保留 `retrieval_count`/`link_discovery_count`）
- [x] `mod.rs`：删除 `embedding_provider` 字段、`semantic_enabled()`、`recall_semantic`/`recall_semantic_with_ann`、`reembed`/`rebuild_ann`/`refresh_clusters`/`reembed_graph`、`RetrievalSource::Semantic/SemanticAnn`、`with_embedding_provider`/`with_ann_settings`、`semantic_duplicate_like`（embedding 分支）、`cosine_similarity` 等
- [x] 删除 `embedding.rs`/`ann.rs`；`Cargo.toml` 移除 `mistralrs`（并移除仅被 ANN 使用的 `vectorlite`）
- [x] 编译期清理所有引用（含测试与 tools/sdk/py 调用面）：embedding/ann/cluster 测试已**删除或改写**（未采用 `#[ignore]` 暂挂，因相关 API 已不存在）
- [x] 验收：`cargo build && cargo test` 通过（core 88 / tools 57 / sdk 135 / mcp 6 全绿），`grep -r "embedding\|ann\|cluster\|mistralrs" crates/*/src` 无命中

> **Phase 1 实现注记**：
> - 去重语义按 §4.3 落地为**词重叠比例**：新增 `text_overlap_ratio`（Jaccard，>2 字符词集），`duplicate_match` = 文本完全一致 || 重叠比例 ≥ `dedupe_min_overlap_ratio`（默认 0.6）；`has_text_overlap` 改为比例 > 0 的薄封装，供矛盾检测预筛复用。
> - `prompt.rs` 的 `explain_hit` 移除 semantic 分支与 `semantic_score` 展示。
> - `graph_stats()` 返回值由四元组改为三元组 `(memories, tags, edges)`。

**Phase 2 — WikiAssistant（新模块 + 实现 + mock）**
- [x] 新建 `memory/wiki.rs`：`QueryExpansion` / `EnrichedMemory` / `RankedCandidate` / `WikiAssistant` trait（§2.5）
- [x] `ProviderBackedWikiAssistant`（复用 `relevance.rs` 的 provider 调用模式，§4.1 prompt 设计）
- [x] 新建 `memory/index.rs`：`MemoryIndex` 构建/`lexical_score`/`to_prompt`/`to_markdown`（§3.3）
- [x] 单测：prompt 解析、词汇加权打分、index 构建与序列化
- [x] 验收：`cargo build && cargo test` 通过（core 110 全绿，wiki 11 + index 11）

> **Phase 2 实现注记**：
> - 新模块经 lib.rs `pub use` 公开（`fox_agent_core::wiki` / `fox_agent_core::index`），后续 Phase 3/6 由 `MemoryManager`/tools/py 引用。
> - `parse_rerank_output` 对**分数无法解析**的行整行跳过（防御式解析）；按 id 去重保留最高分并降序。
> - `QueryExpansion::from_query` 为纯词法回退（`normalize_search_text` 分词，>1 字符），`NATURAL` 缺失时回退原 query。
> - `lexical_score` 归一化为 [0,1]：每个词项贡献其最佳命中字段权重（title 3.0 / aliases 2.0 / tags 1.5 / summary 1.0），除以 `3.0 × n`。
> - `MemoryIndex` 为图的**有损投影**（id/title/summary/tags/aliases，不含正文），`to_prompt` 按 `index_budget_chars` 裁剪并标注 `... (N more entries)`。

**Phase 3 — 写入管线**
- [x] `remember*` 同步快路径改造（`refresh_search_text` 含 title/aliases + `after_write` → `spawn_enrich` 后台增强，§4.2）
- [x] 去重改造：`title_alias_tag_overlap`（§4.3 Jaccard）+ 可选 `are_same` LLM 二次判重
- [x] `ingest_transcript` 接入新去重与 LLM 判重（`find_llm_duplicate_for_ingestion`）
- [x] 索引惰性重建：`rebuild_index(scope)`（写入后不显式维护，读取时按图重建）
- [x] 单测：惰性索引重建、后台 enrich 元数据应用与幂等、链接发现、LLM 判重命中/未命中、`ingest_transcript` 改述判重端到端
- [x] 验收：`cargo build && cargo test` 通过（core memory 55 全绿，全库除 py 外全绿）

> **Phase 3 实现注记**：
> - `wiki_assistant: Option<Arc<dyn WikiAssistant>>` 由 `with_wiki_assistant` 注入；未注入时全链路保持同步、纯词汇（完全向后兼容）。
> - `spawn_enrich` 为 `tokio::spawn` 后台任务，`run_enrich` 幂等（`entry.enriched=true` 跳过），失败仅 `tracing::warn` 不阻塞写路径。
> - `run_enrich` 应用 title/summary/tags/aliases 并去重追加；`link_discovery_enabled` 时对 `link_ids` 建 `RelatesTo(0.8)` 边（跳过自链）。
> - LLM 判重**每候选最多一次 `are_same`**：仅对同类别、词重叠比例最大且 >0 的候选调用；`best_overlap ≤ 0` 直接跳过，避免无谓 LLM 调用。
> - `duplicate_match` = 搜索文本完全一致 || 词重叠比例（>2 字符词集 Jaccard）≥ `dedupe_min_overlap_ratio`；`has_text_overlap` 仍用于矛盾检测入口（比例 > 0）。
> - `rebuild_index` 按作用域加载 Session/Project/Global 图合并，按 id 去重、过滤 `!active`；仅投影 id/title/summary/tags/aliases。索引持久化与 `enriched=false` 批量补增强属 Phase 5。

**Phase 4 — 检索管线**
- [x] `recall_wiki` 同步路径（查询扩展词汇回退 → 加权词汇预筛 → 图 BFS 扩散，§5.2）
- [x] `recall_wiki_async`（LLM 查询扩展 + 可选 LLM 重排 + 图扩散；无 assistant / 调用失败自动退化）
- [x] `recall_detailed` 的 `Wiki` 分支切换为 `recall_wiki`（原过渡映射 `recall_cascade` 已删除，BFS 抽取为 `expand_cascade` 共用）
- [x] 词汇预筛加权：title 3.0 / aliases 2.0 / tags 1.5 / summary 1.0 / content 1.0（`lexical_prefilter_score`，content 兜底保证未 enrich 条目可召回）
- [x] 集成测试（mock assistant）：同步预筛+图扩散、LLM 扩展驱动 title/alias/content 权重排序、重排改写种子序、无 assistant 回退
- [x] 验收：`cargo build && cargo test` 通过（core memory 59 全绿，全库除 py 外全绿）

> **Phase 4 实现注记**：
> - `recall_wiki` 为同步私有方法（`recall_detailed` 的 `Wiki` 分支使用）；`recall_wiki_async` 为公开异步方法，供 Phase 6 调用面注入使用。
> - 预筛候选数 = `limit × rerank_candidate_multiplier`；种子数 = `limit × 2`（§5.2 ③）；图扩散深度/广度沿用 `max_graph_depth` 与 `limit × 3`。
> - 重排输出以 1-based 候选序号引用预筛结果；序号无法映射时整行跳过，全部不可用则回退词法序种子。
> - 种子得分 = 词汇分 0.4 + 重排分 0.4 + recency 0.15 + trust 0.05（无重排时词汇 0.7 + recency 0.2 + trust 0.1）；`keyword_score` 始终保留词汇分供可解释性。
> - `apply_cascade_results` 对种子按图分重算 `final_score`（单调变换，不破坏重排次序）；图邻居命中标记 `CascadeGraph`。
> - `recall_cascade` 私有方法已删除（无调用方），其 BFS 逻辑收敛为 `expand_cascade` 与 wiki 共用。

**Phase 5 — 索引与 wiki 导出**
- [x] 索引持久化 `{graph}.index.json`（`persist_index`，按图本地投影；`load_index` 保持惰性重建）
- [x] `enriched=false` 批量补增强（`backfill_enrich`，幂等，失败跳过）
- [x] `index.md` / `pages/<slug>.md` 导出（`export_wiki`，§3.3 frontmatter + 正文，slug 唯一化）
- [x] index 注入 prompt 路径（core 侧 `index_to_prompt`，SDK 侧装配见 Phase 6）
- [x] 单测：持久化 round-trip、导出 slug 唯一性与链接精确性、批量补增强幂等、预算注入
- [x] 验收：`cargo build && cargo test` 通过（core memory 63 全绿，全库除 py 外全绿）

> **Phase 5 实现注记**：
> - 写路径保持惰性：`remember*`/`forget` 等不主动重建/写盘索引（避免每次写入 I/O），`load_index` 总是从当前图重建，杜绝陈旧快照；`persist_index` 供导出/快照场景显式落盘。
> - `persist_index` 为每个覆盖作用域写**本图局部**投影（`{graph}.index.json`），返回合并投影；文件路径由 graph 路径 `with_extension("index.json")` 派生。
> - `backfill_enrich` 仅在装配 assistant 时生效（否则返回 0）；`limit==0` 表示不限制；逐条容错（失败仅 warn）。
> - `export_wiki` 的 slug 分配**确定性排序**（`created_at` + id），重复标题追加 `-2`/`-3`…；index.md 链接指向实际 page 文件（映射 slug），不再用 `to_markdown` 的独立 slugify，避免链接漂移。
> - 页面 frontmatter：title/tags/aliases（引号转义），正文为原始 content。
> - `WikiExportStats { index_path, pages_dir, pages_written, memories }` 新增导出统计。

**Phase 6 — 工具/SDK/Py 改造**
- [x] `fox-agent-tools/src/memory.rs`：删除 action `reembed`/`refresh_clusters`/`rebuild_ann`（工具 schema L119/129/144 同步）；mode 枚举去 `semantic`；`stats` 输出去掉 `semantic_score`（**已于 Phase 1 编译期清理中一并完成**）
- [x] `fox-agent-sdk/src/memory.rs`：`semantic_enabled()` → `wiki_enabled()`（**已于 Phase 1 完成**）
- [x] `fox-agent-py/src/memory.rs`：移除 `embedding_enabled` 参数、`SEMANTIC` 常量（改为 `wiki`）（**已于 Phase 1 完成**）
- [x] `fox-agent-tools/src/memory.rs`：新增 action `rebuild_index` / `enrich`（`rebuild_index`→`persist_index` 重建并落盘 `{graph}.index.json`；`enrich`→`backfill_enrich`，`limit==0` 不限，未装配 assistant 时 no-op 返回 0）
- [x] `fox-agent-sdk/src/memory.rs`：装配 `wiki_assistant`

> **Phase 6 实现注记**：
> - `Model` trait 新增 `provider()` 访问器（默认 `None`），`DefaultModel` 返回 `Some(provider)`；SDK `MemoryManager::with_wiki_assistant(model)` 据此构建 `ProviderBackedWikiAssistant`（未暴露 provider 时 no-op）。
> - `Harness::attach_wiki_assistant(&mut self, model)` 在 `AgentBuilder::build` 中于 `Harness::new`/`with_permission_hook` 之后调用，接线 memory_manager 的 assistant。
> - `trigger_recall_for_next_turn`：`wiki_enabled` 时优先 `recall_wiki_async`（LLM 扩展 + 重排），失败回退同步 `recall_detailed(Wiki)`；否则 Keyword。**无具体查询或查询未命中时注入 MemoryIndex 摘要**（§7.1 step 3 的 SDK 侧装配，`inject_index_digest` 按 `index_budget_chars` 裁剪，空索引 no-op）。
> - builder 在注册默认工具后，用 harness 的 `memory_manager.core()` 覆盖注册 `MemoryTool`（`register_tool` 按名覆盖），使 `enrich`/`rebuild_index` 与注入管线共用同一存储和 wiki assistant。
> - `fox-agent-py`：新增 `rebuild_index(scope)` 同步方法、`enrich(scope, limit=0)` 异步方法（`future_into_py`）；本机无 Python 环境未编译验证（遵循既有同步/异步绑定模式）。

**Phase 7 — 测试收尾**
- [x] 删除/重写 Phase 1 中 `#[ignore]` 的 embedding 测试；新增 §10.1 wiki 全链路测试（全库无 `#[ignore]` embedding 测试残留；新增 `test_wiki_recall_matches_alias_without_literal_overlap`、`test_enrich_populates_title_summary_aliases_links`）
- [x] `cargo clippy -- -D warnings && cargo fmt --check && cargo test` 全绿（含 test-target 10 处 `field_reassign_with_default` 修复）
- [x] 更新 `docs/evaluation_design.md` 中 memory 相关段落（如提及 embedding 之处）（检查无 embedding 提及，无需修改）

---

## 15. 验收标准

1. `cargo build && cargo clippy -- -D warnings && cargo fmt --check && cargo test` 全绿，且 **代码库中不存在 `embedding`/`ann`/`cluster`/`mistralrs` 引用**（除文档外）。
2. 无 embedding 配置下，`recall` 语义命中：写入「用户偏好 python 命名 snake_case」，用「python 命名习惯」查询能召回（靠 title/aliases + 重排）。
3. `index.md` / `pages/` 可导出且人可读；`rebuild_index` 可补齐 `enriched=false` 条目的增强。
4. 现有同步 `remember/recall/keyword/search` API 签名不变，工具/SDK/Py 三层编译通过。
5. 全库不依赖外部模型下载与本地模型缓存，满足 AGENTS.md「不依赖外部服务/配置」原则。
