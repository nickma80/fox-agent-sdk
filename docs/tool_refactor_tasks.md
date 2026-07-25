# Tool 执行机制重构计划与任务

## 1. 目标

基于 [tool_refactor.md](file:///d:/ws/ai/fox-agent-sdk/docs/tool_refactor.md) 的方案，本计划将重构工作拆解为可逐步落地的阶段、任务与验收标准，目标是：

- 让高噪声工具结果不再默认进入主 Agent 工作上下文；
- 建立统一的结果路由机制，覆盖本地工具与 MCP 工具；
- 引入 artifact store 作为受控缓存层，而不是永久归档层；
- 在后续阶段引入子 Agent 隔离探索；
- 保留 compaction 作为兜底，而不是主治理手段。

## 2. 总体策略

### 2.1 实施原则

- 先做结果路由，再做子 Agent 隔离，最后做策略自动化。
- 先打通最小闭环，再扩展覆盖更多工具和 MCP server。
- 先保证主流程稳定，再提升摘要质量和自动决策能力。
- 配置必须落在标准配置文件中，不依赖环境变量和外部服务。

### 2.2 分阶段路线

1. **Phase 0：设计收口与基础准备**
2. **Phase 1：结果路由与 artifact store 最小闭环**
3. **Phase 2：MCP 纳入统一治理**
4. **Phase 3：子 Agent 隔离探索**
5. **Phase 4：策略自动化与治理闭环**

## 3. 模块映射

本次重构主要涉及以下模块：

- `crates/fox-agent-core/src/config.rs`
- `crates/fox-agent-sdk/src/agent.rs`
- `crates/fox-agent-sdk/src/harness.rs`
- `crates/fox-agent-sdk/src/builder.rs`
- `crates/fox-agent-sdk/src/mcp.rs`
- `crates/fox-agent-sdk/src/safety.rs`
- `crates/fox-agent-sdk/src/compaction.rs`
- `agent.toml`
- `docs/tool_refactor.md`

如果后续单独拆出 artifact store 模块，建议新增：

- `crates/fox-agent-sdk/src/artifact_store.rs`
- `crates/fox-agent-sdk/src/tool_routing.rs`
- `crates/fox-agent-sdk/src/subagent.rs`

## 4. Phase 0：设计收口与基础准备

### 4.1 阶段目标

在不改运行时行为的前提下，先把配置、数据结构和埋点接口准备好，避免后续实现阶段边改边返工。

### 4.2 任务清单

1. 新增 `ArtifactStoreConfig`、`ArtifactCompression`、`ArtifactEvictionPolicy` 配置结构。
2. 将 `artifact_store` 挂到 `FoxAgentSdkConfig` 顶层。
3. 在 `agent.toml` 增加对应配置模板与注释。
4. 定义 `ToolResultRouting`、`ArtifactWriteDecision`、`ArtifactRetentionClass` 等核心枚举。
5. 明确 `ArtifactRecord`、`McpServerProfile`、`McpToolDescriptorSnapshot` 的最终字段。
6. 为关键事件定义审计字段，至少包括：
   - `tool_name`
   - `routing_decision`
   - `artifact_id`
   - `server_name`
   - `subagent_task_id`
7. 为现有 `ToolContext` 或内部执行上下文补充路由所需元信息。

### 4.3 验收标准

- 配置结构可以从 `agent.toml` 正确反序列化。
- 默认值完整、无需依赖外部环境变量。
- 文档中的配置项与 Rust 结构保持一致。
- 现有行为不变，编译通过。

### 4.4 风险提示

- 若此阶段字段定义不稳定，后续 Phase 1-4 会频繁返工。

## 5. Phase 1：结果路由与 artifact store 最小闭环

### 5.1 阶段目标

先解决“工具结果默认直写主消息流”的核心问题，不引入完整子 Agent，只建立结果路由和外置存储的最小闭环。

### 5.2 任务清单

1. 实现 `ArtifactStore` 最小版本，支持：
   - 写入 artifact
   - 读取 artifact
   - 删除 artifact
   - 按 session 枚举 artifact
2. 为 artifact 元数据增加：
   - `size_bytes`
   - `content_hash`
   - `class`
   - `ref_count`
   - `last_access_at`
   - `expires_at`
3. 实现最小 GC：
   - 启动时清理过期对象
   - 写入后触发配额检查
   - session 结束时轻量清理
4. 在工具执行链路中引入 `ToolResultRouting` 决策。
5. 为本地高噪声工具配置默认路由：
   - `read`
   - `grep`
   - `glob`
   - `web_fetch`
6. 当结果超过阈值时：
   - 原文写入 artifact store
   - 主消息流只保留摘要和引用
7. 加入 `summary-only fallback`，避免磁盘紧张或超限时阻塞主流程。
8. 记录结果路由与 artifact 写入事件到审计链路。

### 5.3 代码任务建议

- `config.rs`
  - 增加 `ArtifactStoreConfig`
- `builder.rs`
  - 将 `artifact_store` 配置注入运行时
- `agent.rs`
  - 在工具执行后引入 routing 与 artifact 外置逻辑
- 新增 `artifact_store.rs`
  - 实现最小本地文件存储与元数据管理
- 可新增 `tool_routing.rs`
  - 封装路由判定逻辑，避免逻辑散落在 `agent.rs`

### 5.4 验收标准

- 大型 `read` / `web_fetch` 结果不再完整回灌主消息流。
- 主消息中可以看到摘要和 artifact 引用。
- artifact store 能自动清理过期数据。
- 配额超限时系统会降级为摘要-only，而不是直接失败。
- 现有 compaction 机制仍可正常工作。

### 5.5 建议测试

- 单元测试：
  - 配置默认值
  - 路由决策
  - artifact 去重
  - TTL 回收
- 集成测试：
  - 超大 `read` 结果只回摘要
  - session 结束触发清理
  - 空间超限触发 `summary-only fallback`

## 6. Phase 2：MCP 纳入统一治理

### 6.1 阶段目标

让 MCP 工具不再只是“包装成普通 Tool”，而是进入统一执行模型，同时保留 server 级管理能力。

### 6.2 任务清单

1. 为 `McpServerConfig` 增加或关联 `McpServerProfile`。
2. 建立 `Server Registry`，维护 `server_name -> profile` 映射。
3. 建立 `Descriptor Cache`，缓存 `tools/list` 返回的 descriptor snapshot。
4. 在 MCP 工具执行前加入 routing 适配层：
   - 识别 `server` / `tool`
   - 读取 profile
   - 结合 descriptor 判断是否 `Inline` / `Externalize` / `DelegateToSubagent`
5. 为高噪声 MCP server 配置默认策略：
   - `filesystem`
   - `browser`
   - `external_api`
6. 对远程 `sse` MCP 结果启用更严格的 TTL 和摘要优先策略。
7. 将 MCP 结果的 `server_name`、`tool_name`、`transport` 写入 artifact 元数据和审计日志。
8. 对未声明 profile 的 MCP server 默认按高风险、低信任处理。

### 6.3 代码任务建议

- `mcp.rs`
  - 增加 profile、descriptor cache、routing adapter
- `builder.rs`
  - 注入 MCP profile 和默认策略
- `agent.rs`
  - 将 MCP 执行接入统一 routing 流程
- `safety.rs`
  - 让审批可以感知 server profile 与 routing 结果

### 6.4 验收标准

- 高噪声 MCP 工具不会再直接把大结果灌入主消息流。
- 不同 MCP server 可获得稳定、可解释的默认执行策略。
- 未声明 profile 的 server 会被更保守地处理。
- 审计记录中可以追踪 MCP 工具的 server、tool、transport 和 artifact 关联。

### 6.5 建议测试

- 单元测试：
  - `McpServerProfile` 默认策略
  - descriptor 驱动的 routing 决策
- 集成测试：
  - `filesystem` MCP 大文本读文件被外置
  - `browser` MCP 不保存完整 HTML
  - 未配置 profile 的 MCP 工具默认保守处理

## 7. Phase 3：子 Agent 隔离探索

### 7.1 阶段目标

将高噪声探索任务整体从主 Agent 剥离，由子 Agent 在隔离上下文中执行，只返回结构化摘要。

### 7.2 任务清单

1. 定义 `SubagentTask`、`SubagentSummary`、`EvidenceRef` 的最终接口。
2. 基于 `Model::fork()` 构建子 Agent 运行时。
3. 建立主 Agent -> 子 Agent 的任务派发接口。
4. 实现 `SubagentStop` 的结果规范化逻辑。
5. 为以下任务默认启用子 Agent：
   - 多轮 `grep -> read`
   - 大范围代码搜索
   - 文档库遍历
   - MCP `filesystem` / `browser` / `external_api` 的高噪声探索
6. 让子 Agent 输出固定结构的 `SubagentSummary`：
   - `objective`
   - `findings`
   - `evidence_refs`
   - `recommendations`
   - `uncertainties`
   - `next_queries`
7. 防止子 Agent 完整过程回灌主消息流。
8. 将子 Agent 产生的原始结果写入 artifact store，并建立与主任务的关联。

### 7.3 代码任务建议

- 新增 `subagent.rs`
  - 子 Agent 生命周期与任务执行
- `agent.rs`
  - 委派逻辑与结果接收
- `harness.rs`
  - 主上下文与子上下文隔离
- `hooks`
  - 正式接入 `SubagentStop`

### 7.4 验收标准

- 主 Agent 在高噪声探索任务中只收到 `SubagentSummary`，不收到完整原始过程。
- 子 Agent 能独立消费 MCP descriptor snapshot 和 artifact 引用。
- 主上下文体积在探索型任务中显著下降。
- 主 Agent 仍可按 `evidence_refs` 做定向回读。

### 7.5 建议测试

- 集成测试：
  - 大范围搜索任务走子 Agent
  - 子 Agent 只回摘要，不回全量过程
  - 主 Agent 根据 `evidence_refs` 成功回读原文

## 8. Phase 4：策略自动化与治理闭环

### 8.1 阶段目标

让系统自动判断何时外置、何时走子 Agent、何时只保留摘要，并建立完整的治理指标和回放能力。

### 8.2 任务清单

1. 引入统一的 routing policy engine。
2. routing 输入至少覆盖：
   - 当前上下文压力
   - 工具类型
   - 结果体积预估
   - MCP server profile
   - descriptor 特征
   - 近期连续探索行为
3. 打通 routing 与：
   - 审批系统
   - artifact store
   - MCP lifecycle
   - subagent dispatch
4. 建立治理指标：
   - artifact 写入量
   - artifact 回读率
   - 摘要命中率
   - 子 Agent 命中率
   - 压缩触发率
   - MCP server 健康度
5. 为回放系统补充：
   - routing 决策事件
   - artifact 生命周期事件
   - subagent task 事件
6. 增加配置校验与调试输出，方便开发阶段观察策略行为。

### 8.3 验收标准

- 系统能自动根据任务类型和上下文压力切换执行模式。
- 压缩触发率显著下降。
- 高噪声探索任务的稳定性提升。
- 审计与回放可以串起 tool -> routing -> artifact -> subagent -> final summary 的完整链路。

## 9. 横切任务

以下任务应贯穿所有阶段：

### 9.1 配置与兼容性

- 保持 `agent.toml` 向后兼容。
- 新配置项提供合理默认值。
- 不依赖环境变量。

### 9.2 Windows 兼容性

- 路径与目录管理兼容 Windows。
- 本地 artifact 存储、清理和锁文件逻辑在 Windows 下可靠。
- 涉及 MCP stdio 的行为不破坏现有 `CREATE_NO_WINDOW` 约束。

### 9.3 审计与可观测性

- 每个阶段都补齐事件日志。
- 关键对象具备稳定 ID：
  - `artifact_id`
  - `subagent_task_id`
  - `server_name`
  - `tool_name`

### 9.4 文档同步

- 每个阶段完成后同步更新：
  - `docs/tool_refactor.md`
  - `docs/tool_refactor_tasks.md`
  - 如有必要，更新 `docs/application-developer-guide.md`

## 10. 里程碑定义

### M1：结果不再默认回灌

达成条件：

- 本地大结果工具支持 artifact 外置；
- 主消息流只保留摘要和引用；
- 有最小 GC。

### M2：MCP 进入统一治理

达成条件：

- MCP server profile 可配置；
- descriptor cache 可用；
- MCP 大结果支持外置和保守路由。

### M3：探索正式隔离

达成条件：

- 子 Agent 可运行；
- 高噪声探索任务可以委派；
- 主 Agent 默认只消费 `SubagentSummary`。

### M4：策略自动化闭环

达成条件：

- routing policy 自动决策；
- 指标、审计、回放全部打通；
- compaction 明显退居兜底。

## 11. 建议执行顺序

推荐按以下顺序推进：

1. `config.rs` 与 `agent.toml` 配置打底
2. `artifact_store.rs` 最小闭环
3. 本地工具 routing
4. MCP profile + descriptor cache
5. MCP routing
6. 子 Agent runtime
7. 自动化 routing policy
8. 指标、回放和治理收尾

## 12. 建议拆分方式

如果要进一步拆成开发任务单，建议按以下粒度拆 issue：

1. `feat(config): 增加 ArtifactStoreConfig`
2. `feat(runtime): 增加 ArtifactStore 最小实现`
3. `feat(runtime): 引入 ToolResultRouting 与摘要外置`
4. `feat(mcp): 增加 McpServerProfile 与 descriptor cache`
5. `feat(mcp): MCP 工具接入统一 routing`
6. `feat(subagent): 增加 SubagentTask / SubagentSummary`
7. `feat(runtime): 主 Agent 委派高噪声探索到子 Agent`
8. `feat(policy): 增加 routing policy engine`
9. `feat(observability): 增加 artifact/subagent/routing 审计事件`
10. `test(integration): 覆盖本地工具、MCP、子 Agent 三条主链路`

## 13. 一句话总结

重构的推进顺序应当是：

> 先把大结果从主消息流中拿出去，再把高噪声探索从主 Agent 中拿出去，最后让系统自动决定什么时候这么做。
