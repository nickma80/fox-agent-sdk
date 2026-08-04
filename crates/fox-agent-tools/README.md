# fox-agent-tools

本 crate 是 Fox Agent SDK 的官方工具集合，从 [babycode (jcode)](https://github.com/1jehuang/babycode) 的 `src/tool/` 移植而来。

## 已迁移的工具

| 工具名 | 对应 babycode 源文件 | 说明 |
|--------|---------------------|------|
| `read` | `src/tool/read.rs` | 读取文件，支持行范围（offset/limit、start/end）、二进制检测、图片读取（PNG/JPEG/GIF/WebP/BMP 含 base64 视觉数据） |
| `write` | `src/tool/write.rs` | 写入文件，自动创建父目录，生成 compact diff（`1- old` / `1+ new` 格式） |
| `edit` | `src/tool/edit.rs` | 精确字符串替换，展示上下文和 diff，支持模糊匹配（trim、空白符归一化） |
| `grep` | `src/tool/grep.rs` | 正则搜索文件，基于 `ignore` crate 的 gitignore 感知并行遍历，排除二进制文件 |
| `glob` | `src/tool/glob.rs` | Glob 模式文件搜索，gitignore 感知，按修改时间倒序排列 |
| `ls` | `src/tool/ls.rs` | 递归目录列表，默认忽略 `node_modules/.git/target` 等，最大深度 5 |
| `bash` | `src/tool/bash.rs` | Shell 命令执行，支持前台/后台模式、超时、工作目录、后台任务追踪 |
| `webfetch` | `src/tool/webfetch.rs` | URL 抓取，HTML → 文本/Markdown 转换（基于 regex），流式下载、大小限制 |
| `websearch` | `src/tool/websearch.rs` | 网页搜索，支持 DuckDuckGo（HTML 解析）和 Bing API（需 `FOX_BING_API_KEY` 环境变量） |
| `lsp` | `src/tool/lsp.rs` | LSP 操作占位（未集成实际 LSP） |
| `invalid` | `src/tool/invalid.rs` | 报告无效/未知工具调用 |
| `agentgrep` | `src/tool/agentgrep.rs` | 高级代码搜索，支持 grep/find/outline/trace 四种模式，依赖外部 `agentgrep` crate |
| `todo` | `src/tool/todo.rs` | 会话本地待办事项管理 |
| `plan` | `src/tool/plan.rs` | 会话本地共享计划 |
| `goal` | `src/tool/goal.rs` | 目标和里程碑追踪 |
| `context` | `src/tool/` | 规划上下文渲染器（`render_planning_context`） |

### 适配改动

- 使用 `fox_agent_core::Tool` trait（而非 babycode 的 `jcode_tool_core::Tool`）
- 错误类型使用 `ToolError::Message { message }`（而非 `anyhow::Error`）
- 移除所有 babycode 特有依赖：`Bus`、`TUI`、`Session`、`ContentBlock`、`storage`、`harness`、`background`
- 工具名统一使用 `read`/`write`/`edit`/`grep`/`glob`/`ls`/`bash`/`webfetch`/`websearch`（而非 `read_file`/`write_file`/`run_shell`）

---

## 尚未迁移的工具

以下工具存在于 babycode 的 `src/tool/` 中，尚未移植到 fox-agent-tools。

### 🔴 困难 — 依赖 babycode 核心基础设施

| 工具 | babycode 源文件 | 行数 | 未移植原因 |
|------|----------------|------|-----------|
| `bg` | `src/tool/bg.rs` | 878 | 强依赖 `crate::background::BackgroundTaskManager`（基于文件的后台任务持久化、进程组管理、状态文件轮询）。需先在 fox-agent 中实现等价的跨会话后台任务管理器。 |
| `communicate` (swarm) | `src/tool/communicate.rs` | — | 子代理通信/集群协作工具，依赖 `crate::agent::Agent`、`Session`、`provider::Provider`、`protocol::HistoryMessage`。与 fox-agent-swarm crate 功能重叠。 |
| `task` (subagent) | `src/tool/task.rs` | — | 子代理任务委派，需要 Agent/Session/Provider 基础设施。 |
| `batch` | `src/tool/batch.rs` | — | 批量工具执行，需要 Registry 并发控制，依赖 `BatchSubcallProgress` 等类型。 |
| `mcp` | `src/tool/mcp.rs` | — | MCP 服务器管理工具，依赖 `crate::mcp::McpManager`。 |
| `memory` | `src/tool/memory.rs` | — | 记忆管理，依赖 babycode 的 memory 系统和 Session 存储。 |

### 🟡 中等 — 需剥离 babycode 依赖但工作量较大

| 工具 | babycode 源文件 | 行数 | 未移植原因 |
|------|----------------|------|-----------|
| `multiedit` | `src/tool/multiedit.rs` | 283 | 单一文件多次编辑，逻辑与 `edit` 重复，可通过多次调用 `edit` 替代。 |
| `patch` | `src/tool/patch.rs` | 319 | 标准 unified diff 应用。功能被 `apply_patch` 覆盖。 |
| `apply_patch` | `src/tool/apply_patch.rs` | 650 | Codex 风格 patch 应用（`*** Begin Patch` / `*** End Patch`）。较复杂但独立于 babycode 基础设施。 |
| `codesearch` | `src/tool/codesearch.rs` | — | 语义代码搜索，可能依赖 tree-sitter。功能与 `agentgrep` 部分重叠。 |
| `conversation_search` | `src/tool/conversation_search.rs` | — | 对话历史搜索，依赖 Session 存储。 |
| `session_search` | `src/tool/session_search.rs` | — | 会话搜索，依赖 Session 存储系统。 |
| `side_panel` | `src/tool/side_panel.rs` | — | TUI 侧面板工具，依赖 `jcode-side-panel-types`。 |
| `browser` | `src/tool/browser.rs` | — | 浏览器桥接工具，依赖浏览器自动化基础设施。 |
| `selfdev` | `src/tool/selfdev/` | — | 自开发工具集（构建队列、重载、启动），依赖 `jcode-selfdev-types`。 |

### 🟢 简单 — 可独立移植且工作量小

| 工具 | babycode 源文件 | 行数 | 说明 |
|------|----------------|------|------|
| `skill` | `src/tool/skill.rs` | ~100 | Skill 管理（list/show/install/uninstall）。需要 SkillRegistry。 |
| `open` | `src/tool/open.rs` | ~80 | 在浏览器中打开 URL。使用 `open` crate。 |
| `debug_socket` | `src/tool/debug_socket.rs` | ~150 | 调试 socket 工具，用于直接 socket 访问调试。 |
| `gmail` | `src/tool/gmail.rs` | — | Gmail 集成（搜索、发送邮件）。依赖 `reqwest` + OAuth。 |
| `ambient` | `src/tool/ambient.rs` | — | 环境模式工具（`end_ambient_cycle`、`schedule_ambient`、`request_permission`、`send_message`）。 |

### 已移除（无需迁移）

| 文件 | 原因 |
|------|------|
| `invalid.rs` | 已移植（作为简化版） |
| `tests.rs` (babycode) | 测试逻辑已重写为 fox-agent-tools 风格 |

---

## 迁移优先级建议

1. **P0 — apply_patch**: 最常用的代码编辑工具之一，独立于 babycode 基础设施
2. **P1 — multiedit**: 多次编辑同一文件的便捷接口
3. **P1 — patch**: 标准 unified diff 支持（与 apply_patch 互补）
4. **P2 — open**: 简单的浏览器打开功能，仅依赖 `open` crate
5. **P2 — skill**: Skill 管理，需配合 `fox-agent-core::Skill` trait
6. **P3 — bg**: 需要先在 fox-agent 中实现后台任务管理器
7. **P3 — batch**: 需要并发执行框架

---

## 依赖

### 外部 crates

| crate | 用途 |
|-------|------|
| `agentgrep (git)` | 高级代码搜索引擎（grep/find/outline/trace） |
| `reqwest` | HTTP 客户端（webfetch, websearch） |
| `regex` | 正则表达式（grep, webfetch HTML 转换, websearch 解析） |
| `ignore` | gitignore 感知的文件遍历（grep, glob） |
| `glob` | Glob 模式匹配（glob, ls） |
| `similar` | 文本 diff 生成（write, edit） |
| `base64` | 图片 base64 编码（read 工具） |
| `urlencoding` | URL 编码（websearch） |
| `chrono` | 时间戳处理 |
| `futures` | 异步流处理（webfetch） |

### workspace crates

| crate | 用途 |
|-------|------|
| `fox-agent-core` | Tool trait、ToolContext、ToolOutput 等核心类型 |

---

## 注册所有工具

```rust
use fox_agent_tools::default_tool_executor;

let executor = default_tool_executor().await;
let defs = executor.tool_definitions().await;
// defs 包含所有 16 个已注册工具的 ToolDefinition
```

---

## 记忆系统迁移计划 (Memory)

### babycode 记忆系统架构

```
┌──────────────────────────────────────────────────────────────────┐
│                          MemoryTool                               │
│  (remember / recall / search / list / forget / tag / link)       │
└──────────────────────────┬───────────────────────────────────────┘
                           │
┌──────────────────────────▼───────────────────────────────────────┐
│                         MemoryManager                             │
│  · 双作用域: Project (按工作目录哈希) / Global (全局)              │
│  · 存储格式: MemoryGraph v2 (HashMap-based JSON)                  │
│  · 重复检测: 词重叠比例 + 可选 LLM 判断                            │
│  · 索引: search_text + MemoryIndex (title/tags/aliases/summary)   │
│  · 三层管线: Search(wiki) → Verify(sidecar) → Inject(prompt)      │
└────┬───────────────────────┬────────────────────────────────────┘
     │                       │
     ▼                       ▼
┌──────────────┐    ┌──────────────────┐
│  MemoryGraph  │    │  Sidecar          │
│  · tags 节点   │    │  (外部 LLM 模型)  │
│  · [[链接]]    │    │                  │
│  · 6种边类型   │    │  · 相关性验证     │
│  · BFS 级联    │    │  · 矛盾检测       │
│    cascade     │    │  · 记忆提取       │
└──────────────┘    └──────────────────┘
```

### 存储机制 (babycode)

| 维度 | 实现 |
|------|------|
| **路径** | `~/.jcode/memory/projects/<hash>.json` / `~/.jcode/memory/global.json` |
| **格式** | `MemoryGraph` (JSON, v2) + `MemoryStore` (旧 flat, **不迁移**) |
| **ID 格式** | `mem_<uuid>` |
| **缓存** | LRU 内存缓存，减少 JSON 反序列化 |
| **GC** | 按 max_age_hours 清理，可保留 N 条 |

### 图结构 (MemoryGraph)

```
存储器 ──HasTag────▶ 标签节点
  │
  ├──RelatesTo─────▶ 相关记忆 (weight: 0.0-1.0)
  ├──Supersedes────▶ 新版本 (旧标记 inactive)
  ├──Contradicts───▶ 矛盾 (双向边)
  └──DerivedFrom───▶ 来源
```

BFS `cascade_retrieve` 通过标签传播和边权重衰减发现间接相关记忆。

### 关键类型

| 类型 | 说明 |
|------|------|
| `MemoryEntry` | 单条记忆: id, category, content, tags, search_text, title/summary/aliases (LLM), confidence, trust... |
| `MemoryCategory` | Fact / Preference / Entity / Correction / Custom |
| `TrustLevel` | High (用户明确) / Medium (观察) / Low (推断) |
| `MemoryScope` | Session / Project / Global / All |
| `MemoryGraph` | 有向图: memories, tags, edges, reverse_edges |
| `EdgeKind` | HasTag / RelatesTo / Supersedes / Contradicts / DerivedFrom |

---

### 迁移设计

#### 决策说明

1. **存储路径 → 配置化**：`FoxAgentSdkConfig.storage_dir` 统一管理，memory 存于 `{storage_dir}/memory/`
2. **语义能力 → Provider**：查询扩展/重排/enrich 通过 `fox_agent_core::Provider` trait 调用主 agent 的模型（无需独立 embedding 模型）
3. **Sidecar (Haiku) → Provider 模型**：不使用 babycode 的独立 Haiku 调用，改为通过 `fox_agent_core::Provider` trait 调用主 agent 的模型做相关性验证和记忆提取
4. **跳过 MemoryStore**：只迁移 `MemoryGraph (v2)`，不做 `from_legacy_store` 迁移

#### 配置文件扩展 (`fox-agent-core/src/config.rs`)

```rust
#[derive(Debug, Clone)]
pub struct MemoryConfig {
    pub enabled: bool,
    /// 记忆存储根目录。None = 默认 ~/.fox-agent/memory/
    pub storage_dir: Option<PathBuf>,
    /// 启用 wiki 检索（查询扩展/重排/enrich）
    pub wiki_enabled: bool,
    /// 最大候选记忆数 (wiki/keyword 检索)
    pub max_candidates: usize,        // default: 30
    /// 最终返回的最大结果数
    pub max_results: usize,            // default: 10
    /// 图 BFS 最大深度
    pub max_graph_depth: usize,        // default: 2
    /// 启用 Provider 模型验证相关性 (替代 babycode 的 Sidecar)
    pub verify_relevance: bool,        // default: false
    /// 相关性验证用的 model_id (缺省使用主 agent 的 model)
    pub verify_model: Option<String>,
    /// 自动从对话中提取记忆 (需 Provider 模型)
    pub auto_extract: bool,            // default: false
}
```

#### 相关性验证 (替代 Sidecar)

babycode 的 `Sidecar::check_relevance()` 改为通过 `Provider::complete()` 调用:

```rust
/// 通过 Provider 检查记忆相关性
pub async fn check_relevance(
    provider: &dyn Provider,
    model_id: &str,
    memory_content: &str,
    current_context: &str,
) -> Result<(bool, String), ToolError> {
    let system = "You are a memory relevance checker...";
    let prompt = format!("## Stored Memory\n{memory_content}\n\n## Current Context\n{current_context}");
    let msg = Message::user(&prompt);
    let mut stream = provider.complete(model_id, &[msg], &[], system, "", None).await?;
    // 解析 RELEVANT: yes/no + REASON:
    Ok((is_relevant, reason))
}
```

对应的 babycode 功能移植：

| babycode Sidecar 方法 | 替代方案 |
|----------------------|---------|
| `check_relevance()` | `MemoryRelevanceChecker` trait → 通过 `Provider` 调用 |
| `check_contradiction()` | 同上，prompt 改为矛盾检测 |
| `extract_memories()` | `MemoryExtractor` trait → 通过 `Provider` 调用 |
| `extract_memories_with_existing()` | 同上，附加已有记忆列表去重 |

```rust
/// Trait for verifying memory relevance using an LLM
#[async_trait]
pub trait MemoryRelevanceChecker: Send + Sync {
    async fn check_relevance(&self, memory: &str, context: &str) -> Result<(bool, String)>;
    async fn check_contradiction(&self, new: &str, existing: &str) -> Result<bool>;
}

/// Trait for extracting memories from conversation transcripts
#[async_trait]
pub trait MemoryExtractor: Send + Sync {
    async fn extract(&self, transcript: &str, existing: &[String]) -> Result<Vec<ExtractedMemory>>;
}
```

默认实现 `ProviderRelevanceChecker` 接受 `Arc<dyn Provider>` + `model_id`，通过 `Provider::complete()` 调用。

---

### Phase 1: 核心类型 + 配置 (fox-agent-core)

**文件**: `crates/fox-agent-core/src/memory/`

```
fox-agent-core/src/memory/
├── mod.rs           # MemoryManager (无 Sidecar 依赖的纯逻辑)
├── types.rs         # MemoryEntry, MemoryCategory, TrustLevel, MemoryScope, MemoryStore (仅保留 MemoryGraph 兼容)
├── graph.rs         # MemoryGraph v2, Edge, EdgeKind, TagEntry (无 from_legacy_store)
├── storage.rs       # 文件持久化 (JSON), 读/写/缓存, GC 清理
├── ranking.rs       # top_k_by_score, top_k_by_ord (纯函数)
├── prompt.rs        # format_entries_for_prompt, format_relevant_prompt
├── relevance.rs     # MemoryRelevanceChecker trait + ProviderRelevanceChecker impl
└── extract.rs       # MemoryExtractor trait + ProviderExtractor impl
```

**需要新增的 workspace 依赖**: `tracing` (已有), `uuid` (已有), `serde_json` (已有), `chrono` (已有 / 需要 feature `serde`)

**预计代码量**: ~2500 行 (类型 400 + graph 500 + storage 300 + manager 600 + ranking 100 + prompt 200 + relevance 200 + extract 200)

#### 核心调整 vs babycode

| 维度 | babycode | fox-agent-core |
|------|----------|---------------|
| 存储 API | `jcode_storage::read_json` (自动备份恢复) | `read_json_with_backup()` 自实现 |
| ID 生成 | `jcode_core::id::new_id("mem")` | `uuid::Uuid::new_v4()` |
| 语义召回 | babycode 向量嵌入 | LLM wiki（查询扩展 + 词汇预筛 + 重排），复用 Provider |
| 语义模型 | 独立 embedding 模型 | 复用主 Agent `Provider`（`model_for_memory_tasks`） |
| logging | `crate::harness::logging` | `tracing` crate |
| Sidecar | 独立 Haiku 模型 | `Provider` trait 注入 |
| 事件 | `MemoryEventKind` + 全局变量 | `MemoryStateEvent` enum 扩展 + `tracing::event` |
| Pending | 全局 `PENDING_MEMORY` HashMap | 通过 `MemoryManager::pending()` 方法访问 |
| 旧格式 | `MemoryStore` + `LegacyNotesFile` | **不迁移，仅支持 MemoryGraph v2** |
| 缓存 | `cache_graph` LRU | 保留 LRU 缓存 |

---

### Phase 2: MemoryManager (fox-agent-core)

```rust
pub struct MemoryManager {
    project_dir: Option<PathBuf>,
    storage_dir: PathBuf,
    graph_cache: Arc<std::sync::Mutex<LruCache<PathBuf, MemoryGraph>>>,
    test_mode: bool,
}

impl MemoryManager {
    pub fn new(config: &MemoryConfig) -> Self;
    pub fn with_project_dir(self, dir: PathBuf) -> Self;

    // 核心 CRUD
    pub fn remember(&self, entry: MemoryEntry, scope: MemoryScope) -> Result<String>;
    pub fn promote_memory(&self, id: &str, from: MemoryScope, to: MemoryScope) -> Result<String>;
    pub fn recall(&self, query: &str, limit: usize, mode: RecallMode) -> Result<Vec<(MemoryEntry, f32)>>;
    pub fn search(&self, text: &str, scope: MemoryScope) -> Result<Vec<MemoryEntry>>;
    pub fn list(&self, scope: MemoryScope) -> Result<Vec<MemoryEntry>>;
    pub fn forget(&self, id: &str) -> Result<bool>;

    // 图操作
    pub fn tag_memory(&self, id: &str, tag: &str) -> Result<()>;
    pub fn link_memories(&self, from: &str, to: &str, weight: f32) -> Result<()>;
    pub fn get_related(&self, id: &str, depth: usize) -> Result<Vec<MemoryEntry>>;
    pub fn cascade_retrieve(&self, seed_ids: &[String], depth: usize, limit: usize) -> Result<Vec<(String, f32)>>;
    pub fn graph_stats(&self) -> Result<(usize, usize, usize, usize)>;

    // 持久化
    pub fn save(&self) -> Result<()>;
    pub fn gc(&self, max_age_hours: u64, max_entries_per_scope: usize) -> Result<GCResult>;
}
```

#### 存储路径规范

```
{storage_dir}/
├── global.json                # 全局记忆图 (MemoryGraph v2)
└── projects/
    └── <sha256_prefix>.json   # 项目记忆图
```

- `storage_dir` 默认 `~/.fox-agent/memory/`
- 项目路径使用 SHA256 哈希的短前缀 (前 16 位) 作为文件名
- Graph 加载时尝试缓存命中，miss 后读 JSON

#### LLM 不可用的回退

当 `wiki_enabled` 未启用或 LLM 调用失败时，`recall(mode: "wiki")` 回退到「词汇预筛 + 图扩散」：

```rust
pub enum RecallMode {
    Recent,     // 按 updated_at 倒序
    Keyword,    // 纯文本 search_text 包含
    Wiki,       // LLM wiki：查询扩展 + 词汇召回 + 图扩散
}
```

---

### Phase 3: MemoryGraph (fox-agent-core)

从 `jcode-memory-types/src/graph.rs` 移植，仅保留 v2 版本 (graph_version = 2)：

- ✅ `MemoryGraph` — HashMap 存储的图结构
- ✅ `EdgeKind` — HasTag / RelatesTo / Supersedes / Contradicts / DerivedFrom
- ✅ `TagEntry` — 标签节点
- ✅ `cascade_retrieve()` — BFS 级联检索 (带权重衰减)
- ❌ `from_legacy_store()` — 不迁移
- ❌ `GRAPH_VERSION` 版本检测逻辑 — 始终为 v2

```rust
pub struct MemoryGraph {
    pub graph_version: u32,         // 固定 2
    pub memories: HashMap<String, MemoryEntry>,
    pub tags: HashMap<String, TagEntry>,
    pub edges: HashMap<String, Vec<Edge>>,
    pub reverse_edges: HashMap<String, Vec<String>>,
    pub metadata: GraphMetadata,
}
```

---

### Phase 4: MemoryTool (fox-agent-tools)

```rust
// crates/fox-agent-tools/src/memory.rs
pub struct MemoryTool {
    manager: MemoryManager,
    relevance_checker: Option<Arc<dyn MemoryRelevanceChecker>>,
    extractor: Option<Arc<dyn MemoryExtractor>>,
}
```

支持 action:
- `remember` — 存储 (content, category, scope=[session|project|global], tags)
- `recall` — 检索 (query, mode=[recent|keyword|wiki], limit, scope)
- `search` — 文本搜索 (query, scope)
- `list` — 列出所有 (scope)
- `forget` — 删除 (id)
- `promote` — 提升作用域 (id, scope=源作用域, to_scope=[project|global])，如 Session→Project
- `tag` — 添加标签 (id, tags)
- `link` — 链接记忆 (from_id, to_id, weight)
- `related` — 图遍历 (id, depth)
- `stats` — 图统计信息
- `reindex` — 重建图内搜索字段索引 (scope)
- `rebuild_index` — 重建 MemoryIndex 并持久化 `{graph}.index.json` (scope)
- `enrich` — 批量补增强 `enriched=false` 条目 (scope, limit=0 不限；需装配 wiki assistant)
- `export` — 导出 wiki (index.md / pages/*.md)
- `import` — 导入 (id, content, ...)
- `compact` — 图压缩 (older_than_hours)

---

### Phase 5: Harness 自动记忆管线

记忆不应只靠工具调用，还应在后台自动检索和注入。在 `fox-agent-sdk` 的 `Harness` 中集成：

```
每次 turn 完成后:
  1. MemoryManager.recall() 从当前对话上下文检索相关记忆
  2. 可选: verify_relevance() 用 Provider 模型验证
  3. 将验证通过的记忆格式化为 # Memory 段
  4. 注入下一个 turn 的 system_dynamic 中

每 N 个 turn (可配):
  5. MemoryManager.extract() 用 Provider 模型从对话提取新记忆
  6. 存储到项目图
```

这需要 `fox-agent-core::Harness` trait 增加 memory hooks：

```rust
#[async_trait]
pub trait Harness: Send + Sync {
    // ... existing methods ...

    /// 获取记忆管理器 (None = 记忆功能未启用)
    fn memory_manager(&self) -> Option<&MemoryManager>;

    /// 在当前 turn 完成后调用，用于后台记忆提取和检索
    async fn on_turn_complete(&self, messages: &[Message]) -> Result<()> {
        // 默认空实现，由具体 Harness 覆盖
        Ok(())
    }
}
```

---

### 实现总览

| Phase | 内容 | crate | 估算 |
|-------|------|-------|------|
| **1** | 扩展 config + 核心类型 (MemoryEntry, MemoryCategory, MemoryGraph v2) | `fox-agent-core` | ~800 行 |
| **2** | 文件存储 + LRU 缓存 + GC | `fox-agent-core` | ~500 行 |
| **3** | MemoryManager (CRUD + 图操作) | `fox-agent-core` | ~800 行 |
| **4** | 相关性验证 + 记忆提取 trait + Provider 实现 | `fox-agent-core` | ~400 行 |
| **5** | MemoryTool (8种 action) | `fox-agent-tools` | ~400 行 |
| **6** | Harness 自动管线集成 | `fox-agent-sdk` | ~300 行 |
| **7** | 测试 | 各处 | ~500 行 |
| **总计** | | | **~3700 行** |

---

## 提示词构建系统迁移计划 (Prompt)

### 现状对比

| 维度 | babycode (`src/harness/prompt.rs`) | fox-agent-sdk (`prompt_builder.rs`) |
|------|-----------------------------------|------------------------------------|
| **模板** | ~50 行 `system_prompt.md` + 4 个 self-dev 文件 | 无，硬编码 `"You are Fox Agent SDK runtime."` |
| **PromptBuilder** | 完整实现: 版本号/git hash、会话上下文、AGENTS.md、overlay、skills、memory、selfdev | 只有 `build_split()` 方法 |
| **SplitPrompt** | `static_part` (缓存) + `dynamic_part` (每轮变化) | 同上，但缺少 static/dynamic 分界策略 |
| **Session Context** | 日期/时间/时区、OS/架构、硬件(CPU/GPU/内存)、git 分支/状态、工作目录 | **无** |
| **AGENTS.md** | 自动加载 `./AGENTS.md` + `~/.AGENTS.md` | **无** |
| **Prompt overlay** | 自动加载 `.jcode/prompt-overlay.md` + `~/.jcode/prompt-overlay.md` | **无** |
| **Skills** | 可用 skill 列表展示 | **无** |
| **Self-dev** | 4 个模板文件，按产品上下文(TUI/Desktop)切换 | **无** |
| **ContextInfo** | 详尽的分段字符数统计、token 估算、telemetry 分类 | **无** |
| **硬件检测** | 读取 `/sys/...` DMI、`/proc/cpuinfo`、`/proc/meminfo`、`lspci` | **无** |
| **Git 信息** | `git branch --show-current`, `git status --porcelain` | **无** |

### 提示词组装流程对比

```
babycode:
  system_prompt.md (embed)
  + selfdev hint/mode (embed)
  + AGENTS.md (project + global)
  + prompt-overlay.md (project + global)
  + skills section
  + memory section (from MemoryManager)
  + active skill prompt
  ─────────────────────────────────
  → SplitPrompt { static, dynamic }

fox-agent (当前):
  "You are Fox Agent SDK runtime."
  + planning context (from fox-agent-tools)
  + memory injection (from MemoryInjectionState → dynamic)
  ─────────────────────────────────
  → SplitPrompt { static, dynamic }
```

### Prompt 分段策略

| 段 | babycode 归属 | 说明 | 缓存 |
|---|--------------|------|------|
| 系统模板 | static | `system_prompt.md` 身份/规则 | ✅ |
| 硬件/OS/时间 | static | 跨 session 基本不变 | ✅ |
| Git 信息 | static | 不常变 | ✅ |
| AGENTS.md | static | 项目指令文件 | ✅ |
| Prompt overlay | static | 用户额外指令 | ✅ |
| Skills 列表 | static | 可用 skill 清单 | ✅ |
| Self-dev 模式 | static | 仅 self-dev session | ✅ |
| 规划上下文 (todo/plan/goal) | dynamic | 每轮可能变 | ❌ |
| 记忆注入 | dynamic | 每轮相关记忆不同 | ❌ |
| 活跃 Skill | dynamic | 当前激活的 skill | ❌ |

### 迁移计划

#### Phase 1: 系统模板 + PromptBuilder 核心 (`fox-agent-core`)

**文件**: `crates/fox-agent-core/src/prompt/`

```
fox-agent-core/src/prompt/
├── mod.rs          # PromptBuilder, SplitPrompt, ContextInfo
├── templates/
│   └── system.md   # 嵌入的系统 prompt 模板
```

**`PromptBuilder` 核心结构**:
```rust
#[derive(Clone)]
pub struct PromptBuilder {
    pub version: String,
    pub git_hash: String,
    pub system_template: String,
}
```

**`SplitPrompt` 扩展**:
```rust
#[derive(Debug, Clone)]
pub struct SplitPrompt {
    pub static_part: String,
    pub dynamic_part: String,
}
// 增加方法:
impl SplitPrompt {
    pub fn chars(&self) -> usize;
    pub fn estimated_tokens(&self) -> usize;
}
```

**`ContextInfo`** — 新增 (从 babycode 移植):
```rust
#[derive(Debug, Clone, Default)]
pub struct ContextInfo {
    pub system_prompt_chars: usize,
    pub session_context_chars: usize,
    pub has_project_agents_md: bool,
    pub project_agents_md_chars: usize,
    pub has_global_agents_md: bool,
    pub global_agents_md_chars: usize,
    pub skills_chars: usize,
    pub memory_chars: usize,
    pub prompt_overlay_chars: usize,
    pub tool_defs_chars: usize,
    pub total_chars: usize,
    // ...
}
impl ContextInfo {
    pub fn estimated_tokens(&self) -> usize;
    pub fn breakdown(&self) -> Vec<(&'static str, usize, &'static str)>;
}
```

#### Phase 2: Session Context 构建 (`fox-agent-core::prompt`)

```rust
impl PromptBuilder {
    pub fn build_session_context(&self, working_dir: Option<&Path>) -> String {
        // Date/Time/Timezone, OS/Arch, version, working dir
        // Optional: hardware (CPU/GPU/Memory), git info
    }
}
```

硬件检测设计为 trait，允许不同平台实现:
```rust
#[async_trait]
pub trait SystemInfoProvider: Send + Sync {
    async fn cpu_model(&self) -> Option<String>;
    async fn memory_total(&self) -> Option<String>;
    async fn gpu_info(&self) -> Option<String>;
}

// 默认 Linux 实现 (读 /proc, /sys, lspci)
pub struct LinuxSystemInfo;
pub struct NoopSystemInfo; // 非 Linux 回退
```

#### Phase 3: AGENTS.md 和 Prompt Overlay (`fox-agent-core::prompt`)

- `build_agents_md(working_dir)` — 加载 `./AGENTS.md` + `~/.AGENTS.md`
- `build_prompt_overlay(working_dir)` — 加载 `.fox/prompt-overlay.md` + `~/.fox/prompt-overlay.md`
- 路径通过 config 可配置 (`FoxAgentSdkConfig.prompt.agents_md_paths`)

```rust
impl PromptBuilder {
    pub fn load_agents_md(&self, working_dir: Option<&Path>) -> Vec<(String, ContextInfo)>;
    pub fn load_prompt_overlay(&self, working_dir: Option<&Path>) -> (Option<String>, usize);
}
```

#### Phase 4: Skills 和 Memory 集成

Skills 和 Memory 已有基础，只需在 `build_split` 中拼接:

```
PromptBuilder::build_split():
  static = system_template
         + session_context
         + agents_md (if any)
         + prompt_overlay (if any)
         + skills_list (if any)

  dynamic = planning_context
          + memory_injection
          + active_skill_prompt
```

#### Phase 5: FoxAgentSdkConfig 增加 prompt 配置

```rust
#[derive(Debug, Clone)]
pub struct PromptConfig {
    pub template_path: Option<PathBuf>,  // 自定义 system prompt 模板
    pub enable_session_context: bool,    // 默认 true
    pub enable_hardware_detection: bool, // 默认 true (Linux-only)
    pub enable_git_info: bool,           // 默认 true
    pub enable_agents_md: bool,          // 默认 true
    pub enable_prompt_overlay: bool,     // 默认 true
    pub agents_md_paths: Vec<PathBuf>,   // 额外 AGENTS.md 路径
}
```

#### Phase 6: SDK Harness 集成

更新 `fox-agent-sdk::Harness` 的 `build_system_prompt_split()`:

```rust
// 当前 (简化版)
pub async fn build_system_prompt_split(&self) -> SplitPrompt {
    let dynamic = self.build_dynamic_parts().await;
    self.prompt_builder.build_split(&self.session_state.id, dynamic)
}

// 迁移后 (完整版)
pub async fn build_system_prompt_split(&self) -> (SplitPrompt, ContextInfo) {
    let static_prompt = self.prompt_builder.build_static(&self.working_dir);
    let dynamic_prompt = self.build_dynamic_parts().await;
    let info = ContextInfo::new(&static_prompt, &dynamic_prompt);
    (SplitPrompt { static_part: static_prompt, dynamic_part: dynamic_prompt }, info)
}
```

`dynamic_parts` 包含:
1. Planning context (todo/plan/goal from fox-agent-tools)
2. Memory injection (from MemoryInjectionState)
3. Active skill prompt

#### Phase 7: Telemetry (可选)

通过 `ContextInfo.breakdown()` 提供 prompt 分段的字符数统计，可用于:
- Provider 调用前估算 token 预算
- Compaction 触发决策
- 调试视图显示 prompt 构成

### 实现顺序

| Phase | 内容 | crate | 估算 |
|-------|------|-------|------|
| **1** | SplitPrompt + ContextInfo + PromptBuilder 核心 | `fox-agent-core` | ~300 行 |
| **2** | Session context (时间/OS/硬件/git) | `fox-agent-core` | ~200 行 |
| **3** | AGENTS.md + prompt overlay 加载 | `fox-agent-core` | ~100 行 |
| **4** | Skills + Memory 集成到 build_split | `fox-agent-core` | ~100 行 |
| **5** | PromptConfig | `fox-agent-core` | ~50 行 |
| **6** | SDK PromptBuilder 改用核心 PromptBuilder | `fox-agent-sdk` | ~150 行 |
| **7** | 系统 prompt 模板 (system.md) | `fox-agent-core` | ~50 行 |
| **8** | 测试 | 各处 | ~300 行 |
| **总计** | | | **~1250 行** |

---

## Agent Turn Loop 迁移计划 (Agent)

### 现状对比

| 维度 | babycode (`src/agent/turn_loops.rs` + `turn_execution.rs`) | fox-agent-sdk (`src/agent.rs`) |
|------|----------------------------------------------------------|-------------------------------|
| **总行数** | ~970 + ~500 = ~1470 行 | ~500 行 |
| **核心循环** | `fn run_turn()` — 单次循环内含 API 调用、工具执行、重试逻辑 | `fn run_turn_streaming()` — 循环内调用 API、执行工具 |
| **Max 循环次数** | `MAX_TOOL_LOOP_ITERATIONS = 100` | 无上限 (loop) |
| **Context limit 重试** | `MAX_CONTEXT_LIMIT_RETRIES = 5`，自动 compaction 后重试 | **无** — 一次性失败 |
| **Incomplete continuation** | `MAX_INCOMPLETE_CONTINUATION_ATTEMPTS = 3`，检测截断后追加 "continue" | **无** |
| **Degenerate response** | 检测 0 token 输出，自动重试 | **无** |
| **Tool 执行** | 流结束后顺序执行，每个 tool 单独 push message | 流结束后批量执行，先 push assistant message 再逐个执行 |
| **Permission** | 无权限系统 (安全性在 harness 层) | `PermissionResult::Allow/Deny/AskUser`，Three-state permission |
| **Stream 处理** | ~30 种事件类型 (ThinkingDelta, ToolUseStart, ToolInputDelta, NativeToolCall, Compaction, SessionId...) | 4 种事件类型 (TextDelta, ThinkingDelta, ToolUse, Usage, MessageStop) |
| **SDK bridge tools** | `NativeToolCall` + `ToolResult` 事件 | **无** |
| **Generated images** | `GeneratedImage` 事件 → 构建 visual context 重新注入 | **无** |
| **Token usage** | 细粒度跟踪 (input/output/cache_read/cache_write) | 通过 `StreamEvent::Usage` |
| **Compaction** | 自动检测 context limit → compaction → retry (最多 5 次) | 每次 turn 前执行 (不 retry) |
| **Memory** | `MemoryInjection` + `SessionEvent`，注入为 `<system-reminder>` message | `MemoryInjectionState` + `MemoryStateEvent` |
| **Soft interrupts** | `inject_soft_interrupts()` | `take_pending_interrupts()` |
| **Graceful shutdown** | `graceful_shutdown_signal` 传递到 ToolContext | `is_graceful_shutdown_requested()` 检查 |
| **Telemetry / TUI** | `Bus::global().publish(ToolEvent)` + `SubagentStatus` | `AgentEvent` channel + `tracing` |

### babycode Turn Loop 流程图

```
run_turn() loop:
  1. repair_missing_tool_outputs
  2. messages_for_provider (maybe compact)
  3. build_memory_prompt → inject as <system-reminder> message
  4. build_system_prompt_split
  5. API complete (streaming, 30+ event types)
  6. Stream processing: text, thinking, tool, native, compaction, image, usage
  7. filter_truncated_tool_calls
  8. For each tool call:
     a. Validate tool
     b. Check if SDK already executed (ToolResult event)
     c. If native tool and SDK error → execute locally
     d. Else use SDK result → push message
     e. Or execute locally → push message
  9. inject_soft_interrupts
  10. Check retry conditions: context_limit → compact+loop, incomplete → "continue"+loop
  11. No tool calls → return text
```

### fox-agent Turn Loop (当前)

```
run_turn_streaming() loop:
  1. TurnStart event
  2. graceful_shutdown check
  3. maybe_compact_messages
  4. inject_interrupts
  5. memory_injection → build prompt
  6. API complete (streaming, 5 event types)
  7. Stream processing: text, thinking, usage, tool_use, message_stop
  8. No tools → return Completed { text }
  9. Push assistant message (含所有 tool calls)
  10. For each tool call:
      a. Permission check (Allow/Deny/AskUser)
      b. Allow → execute → push result
      c. Deny → push error result
      d. AskUser → save pending → return RequiresUserDecision
  11. Loop back to 1
```

### 需移植的关键特性

| 特性 | babycode | fox-agent | 优先级 | 说明 |
|------|----------|-----------|--------|------|
| **Context limit 自动重试** | `try_auto_compact_after_context_limit()` + retry (5次) | ❌ | P0 | Provider 返回 overlimit 时自动 compaction + retry |
| **Incomplete continuation** | `maybe_continue_incomplete_response()` / `maybe_continue_degenerate_response()` | ❌ | P0 | 模型输出被 max_tokens 截断时自动请求续写 |
| **截断 tool call 过滤** | `filter_truncated_tool_calls()` | ❌ | P0 | 丢弃因截断产生的 null/空 input 的 tool call |
| **Thinking 事件全面处理** | ThinkingStart/Delta/End/Done + reasoning_content 存储 | ThinkingDelta 仅 | P1 | 完整 thinking 管线：计时、显示、存储 |
| **Token usage 细分** | input/output/cache_read/cache_write 四字段 | 仅 input+output+total | P1 | 缓存命中统计 |
| **Tool loop 上限** | MAX_TOOL_LOOP_ITERATIONS = 100 | 无上限 | P1 | 防止无限循环 |
| **Compaction 事件传递到 Provider** | StreamEvent::Compaction (provider 侧 compaction) | ❌ | P2 | Provider 返回 compaction 事件时通知模型 |
| **Tool result 含 duration** | `add_message_with_duration()` | ❌ | P2 | 模型能看到工具执行耗时 |
| **Generated images** | `GeneratedImage` → visual context → reinject | ❌ | P3 | 图像生成工具的输出作为视觉上下文重新注入 |
| **Native tool calls (SDK bridge)** | `NativeToolCall` + `NativeToolResult` | ❌ | P3 | Claude Code CLI 集成支持 |

### 迁移设计

#### P0: Context limit 自动重试

```rust
// agent.rs 常量
const MAX_CONTEXT_LIMIT_RETRIES: u32 = 5;
const CTRL_LIMIT_KEYWORDS: &[&str] = &[
    "context_length_exceeded",
    "max_context_length",
    "too many tokens",
    "maximum context length",
];

impl Agent {
    fn try_auto_compact_after_context_limit(&self, error: &str) -> bool {
        CTRL_LIMIT_KEYWORDS.iter().any(|kw| error.contains(kw))
            && self.harness.compaction_manager.try_read()
                .map(|cm| cm.can_compact())
                .unwrap_or(false)
    }
}
```

在 `run_turn_streaming` 中：

```rust
let mut context_limit_retries = 0u32;
loop {
    // ... existing ...
    match self.model.complete(...).await {
        Ok(stream) => stream,
        Err(e) => {
            if self.try_auto_compact_after_context_limit(&e.to_string())
                && context_limit_retries < MAX_CONTEXT_LIMIT_RETRIES
            {
                context_limit_retries += 1;
                self.harness.maybe_compact_messages().await;
                continue;
            }
            return Err(AgentError::Provider(e));
        }
    };
    context_limit_retries = 0;
    // ... stream processing ...
}
```

#### P0: Incomplete continuation

```rust
impl Agent {
    fn maybe_continue_incomplete(&self, stop_reason: Option<&str>,
        attempts: &mut u32) -> Result<bool, AgentError>
    {
        if *attempts >= MAX_INCOMPLETE_CONTINUATION_ATTEMPTS { return Ok(false); }
        let should_continue = matches!(stop_reason,
            Some("max_tokens" | "length" | "tool_use") // tool_use without tools = truncated
        );
        if should_continue {
            *attempts += 1;
            self.harness.session_state.messages.push(
                Message::user("Please continue.")
            );
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn maybe_continue_degenerate(&self, text: &str,
        attempts: &mut u32) -> Result<bool, AgentError>
    {
        if *attempts >= MAX_INCOMPLETE_CONTINUATION_ATTEMPTS { return Ok(false); }
        if text.trim().is_empty() {
            *attempts += 1;
            self.harness.session_state.messages.push(
                Message::user("Your response was empty. Please try again.")
            );
            Ok(true)
        } else {
            Ok(false)
        }
    }
}
```

#### P0: 截断 tool call 过滤

```rust
impl Agent {
    fn filter_truncated_tool_calls(&self, stop_reason: Option<&str>,
        calls: &mut Vec<PendingToolCall>, msg_id: Option<&str>)
    {
        if !matches!(stop_reason, Some("max_tokens" | "length" | "tool_use")) {
            return;
        }
        let before = calls.len();
        calls.retain(|tc| {
            // Discard tool calls with null/empty input (truncated mid-generation)
            !tc.input.is_null() && tc.input != serde_json::Value::Object(Default::default())
        });
        let removed = before - calls.len();
        if removed > 0 {
            warn!(removed, "Filtered truncated tool calls");
        }
    }
}
```

#### P1: TokenUsage 扩展

```rust
// fox-agent-core/src/provider.rs
pub struct TokenUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub total_tokens: u32,
    pub cache_read_input_tokens: Option<u32>,
    pub cache_creation_input_tokens: Option<u32>,
}
```

### 实现顺序

| Phase | 内容 | crate | 估算 |
|-------|------|-------|------|
| **1** | StreamEvent 扩展 (MessageStop stop_reason) | `fox-agent-core` | ~30 行 |
| **2** | TokenUsage 扩展 (cache_read, cache_creation) | `fox-agent-core` | ~30 行 |
| **3** | Context limit 自动检测 + compaction retry (P0) | `fox-agent-sdk` | ~150 行 |
| **4** | Incomplete continuation + degenerate response (P0) | `fox-agent-sdk` | ~100 行 |
| **5** | 截断 tool call 过滤 (P0) | `fox-agent-sdk` | ~80 行 |
| **6** | Tool result duration 跟踪 (P2) | `fox-agent-sdk` | ~50 行 |
| **7** | Tool loop 上限 MAX_TOOL_LOOP_ITERATIONS (P1) | `fox-agent-sdk` | ~30 行 |
| **8** | 测试 | 各处 | ~400 行 |
| **总计** | | | **~870 行** |
