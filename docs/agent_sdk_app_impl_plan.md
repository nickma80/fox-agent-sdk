# Fox Agent SDK 应用开发优化实施计划

## 1. 文档目标

本文档基于 [agent_sdk_app_optimization_prd.md](file:///d:/ws/ai/fox-agent-sdk/docs/agent_sdk_app_optimization_prd.md)，输出一份可执行、可排期、可验收的实施计划，用于指导 `fox-agent-sdk` 从“可复用 Agent 内核”升级为“适合业务应用接入的 Agent SDK”。

本文档聚焦：

- 实施顺序
- 模块拆分
- 代码落点
- 任务分解
- 里程碑安排
- 测试与验收策略

不重复 PRD 的需求定义细节，重点说明“怎么做”。

## 2. 实施原则

### 2.1 总体原则

- 先补基础抽象，再做上层装配与产品化增强
- 先保证单 Agent 状态闭环，再推进 Swarm 产品化
- 先提供默认实现，再开放可替换扩展点
- 优先增量接入，减少对现有 `Harness / Agent / ToolExecutor` 的破坏性改造
- 每个里程碑都必须同时交付：
  - 代码实现
  - 示例
  - 定向测试
  - 编译验证

### 2.2 设计原则

- `fox-agent-core` 负责领域模型、抽象接口和基础数据结构
- `fox-agent-sdk` 负责运行时装配、状态接线和对应用层暴露的高阶能力
- `fox-agent-tools` 负责工具层接入与规划类工具落地
- `fox-agent-swarm` 负责同进程 Swarm 的产品化增强
- `examples` 负责最小可运行接入范式

## 3. 实施顺序

建议采用以下顺序推进：

1. 状态闭环
2. 应用装配收敛
3. 事件与权限治理
4. 运行治理与观测
5. Swarm 产品化增强
6. 示例、回放与测试模板

原因：

- `SessionStore / PlanningStore` 是 Builder、Replay、SwarmSupervisor 的共同前置
- `EventEnvelope` 是 EventRecorder、审批审计、Metrics、回放能力的统一基础
- Swarm 的失败恢复和任务汇总依赖单 Agent 的状态模型与事件模型稳定

## 4. 代码落点规划

### 4.1 `crates/fox-agent-core`

负责以下内容：

- `SessionStore` trait
- `PlanningStore` trait
- `SessionSnapshot / PlanningStateSnapshot`
- `EventEnvelope`
- `EventRecorder` 的基础模型
- `PermissionRequest / ApprovalDecision` 扩展模型
- 运行治理配置模型

建议新增目录：

- `src/session/`
- `src/planning/`
- `src/telemetry/`

### 4.2 `crates/fox-agent-sdk`

负责以下内容：

- `AgentBuilder`
- `SwarmRuntimeBuilder`
- session 自动快照接线
- 从 snapshot 恢复 Agent
- EventRecorder 接线
- 权限审批缓存与超时流程接线
- metrics hook 接线

建议新增目录：

- `src/builder/`
- `src/session/`
- `src/events/`
- `src/metrics/`

### 4.3 `crates/fox-agent-tools`

负责以下内容：

- `todo / plan / goal` 工具改造为基于 `PlanningStore`
- 规划状态读取、写入、merge、checkpoint 操作

### 4.4 `crates/fox-agent-swarm`

负责以下内容：

- `SwarmSupervisor`
- worker 生命周期状态机
- 失败恢复
- 任务重分配
- 汇总报告

### 4.5 `examples`

负责以下内容：

- 单 Agent CLI
- AskUser 审批流
- 文件沙箱代理
- Swarm 执行器

## 5. 里程碑计划

## 5.1 M1：状态闭环

### 目标

补齐会话与规划数据的持久化闭环，为 Builder、Replay、Swarm 提供稳定底座。

### 范围

- `SessionStore`
- `InMemorySessionStore`
- `FileSessionStore`
- `PlanningStore`
- `InMemoryPlanningStore`
- `FilePlanningStore`
- turn 结束自动快照
- 从 snapshot 恢复 Agent 基础状态

### 核心任务

1. 定义 `SessionSnapshot`
2. 定义 `PlanningStateSnapshot`
3. 抽象 `SessionStore`
4. 抽象 `PlanningStore`
5. 将 `todo / plan / goal` 改造为通过 store 读写
6. 在 turn 完成、权限中断、恢复执行后统一触发快照
7. 增加 session 恢复入口

### 关键数据结构建议

`SessionSnapshot` 至少包含：

- `session_id`
- `working_dir`
- `messages`
- `model runtime state`
- `pending permission`
- `interrupt state`
- `metadata`
- `updated_at`

`PlanningStateSnapshot` 至少包含：

- `session_id`
- `scope`
- `todos`
- `plan`
- `goals`
- `version`
- `updated_at`
- `source`

### 验收标准

- 应用重启后可恢复会话消息历史
- `todo / plan / goal` 可从文件存储恢复
- 每轮执行完成后自动生成最新快照

### 预计工期

- `6-8d`

## 5.2 M2：应用装配收敛

### 目标

降低业务应用接入复杂度，把 SDK 从“散装接线”收敛到标准化 Builder。

### 范围

- `AgentBuilder`
- `SwarmRuntimeBuilder`
- 默认装配策略
- 默认 store / tools / safety / sandbox 接入

### 核心任务

1. 设计 `AgentBuilder`
2. 支持以下配置项：
   - provider
   - model_id
   - sdk config
   - session store
   - planning store
   - sandbox
   - safety policy
   - default tools
   - custom tools
3. 提供 `build_agent()`
4. 提供 `build_swarm_runtime()`
5. 提供合理默认值与默认能力集

### Builder API 建议

建议至少包含：

- `with_provider_config(...)`
- `with_model_id(...)`
- `with_working_dir(...)`
- `with_sdk_config(...)`
- `with_session_store(...)`
- `with_planning_store(...)`
- `with_default_tools()`
- `with_tool(...)`
- `with_sandbox(...)`
- `with_safety_policy(...)`
- `build_agent()`

### 验收标准

- 应用仅提供 provider config 与 working_dir 即可初始化 Agent
- 注册自定义工具无需直接操作 `Harness`
- 单 Agent 最小示例初始化代码控制在 30 行以内

### 预计工期

- `4-5d`

## 5.3 M3：事件、权限与运行治理

### 目标

让 SDK 具备应用级事件治理、审批工作流与运行治理能力。

### 范围

- `EventEnvelope`
- `EventRecorder`
- 权限审批增强
- 审批缓存
- 审批超时
- metrics hook
- 预算治理

### 核心任务

1. 定义 `EventEnvelope`
2. 为事件补齐标准字段：
   - `event_id`
   - `session_id`
   - `turn_id`
   - `timestamp`
   - `trace_id`
   - `parent_event_id`
   - `source`
3. 实现 `EventRecorder`
4. 支持 `jsonl` 导出与事件回放基础能力
5. 扩展 `PermissionRequest`
6. 实现审批缓存：
   - 仅本次
   - 本会话
   - 当前工作区
7. 实现超时自动拒绝
8. 实现审批审计记录
9. 增加 metrics hook
10. 增加预算治理配置：
   - provider timeout
   - provider retry
   - tool timeout
   - tool concurrency limit
   - token budget
   - cost budget

### 验收标准

- 事件可按顺序导出和回放
- 审批支持缓存和超时拒绝
- 应用能拿到 latency / usage / estimated cost 指标
- 超出预算时返回结构化错误

### 预计工期

- `7-9d`

## 5.4 M4：Swarm 产品化与开发者体验

### 目标

让同进程 Swarm 从原语能力升级为最小可用产品能力，并补齐 examples 与 replay 模板。

### 范围

- `SwarmSupervisor`
- worker 生命周期
- 失败恢复
- 任务重分配
- 汇总报告
- replay runner
- examples
- 测试模板

### 核心任务

1. 定义 worker 生命周期：
   - `ready`
   - `running`
   - `blocked`
   - `completed`
   - `failed`
   - `timed_out`
2. 实现 supervisor 视角任务收敛
3. 实现失败重试与转派
4. 实现 worker health check
5. 实现汇总报告
6. 基于 `EventRecorder` 实现 replay runner
7. 补齐官方 examples
8. 补齐 golden transcript replay 测试模板

### 验收标准

- worker 失败后可按策略重试或转派
- 所有 worker 完成后可获得汇总报告
- examples 覆盖单 Agent、权限流、Swarm 三类核心场景

### 预计工期

- `7-8d`

## 6. 任务拆解

| 编号 | 任务 | 模块 | 说明 | 预估 |
|---|---|---|---|---|
| T1 | 设计 `SessionStore` trait 与 snapshot 模型 | 会话 | 冻结会话快照结构 | 2d |
| T2 | 实现 `InMemorySessionStore / FileSessionStore` | 会话 | 包括自动快照接线 | 3d |
| T3 | 设计 `PlanningStore` trait | 规划 | 抽象 todo/plan/goal 存储 | 2d |
| T4 | 改造 `todo / plan / goal` 工具 | 规划 | 接入 planning store | 3d |
| T5 | 实现 `AgentBuilder` | 装配 | 默认装配与简化初始化 | 2d |
| T6 | 实现 `SwarmRuntimeBuilder` | 装配 | 为后续 supervisor 铺路 | 2d |
| T7 | 定义 `EventEnvelope` | 事件 | 统一事件外部模型 | 2d |
| T8 | 实现 `EventRecorder` | 事件 | 导出、回放基础能力 | 3d |
| T9 | 扩展权限审批模型 | 权限 | 审批缓存、超时、审计 | 4d |
| T10 | 实现 metrics hook 与预算治理 | 治理 | latency、usage、cost | 3d |
| T11 | 实现 `SwarmSupervisor` 最小版本 | Swarm | worker 生命周期与重试 | 4d |
| T12 | 补 replay 与 examples | DX | 示例与测试模板 | 3d |

## 7. 依赖关系

### 7.1 核心依赖

- `SessionStore` 是 `Replay / EventRecorder / SwarmSupervisor / Builder` 的前置
- `PlanningStore` 是规划工具持久化与 session 恢复的前置
- `EventEnvelope` 是 `EventRecorder / 审批审计 / telemetry` 的前置
- `Permission workflow` 依赖 `SessionStore + EventEnvelope`
- `SwarmSupervisor` 依赖 `Builder + EventEnvelope + Session 恢复`

### 7.2 推荐顺序

1. `SessionStore + PlanningStore`
2. `AgentBuilder`
3. `EventEnvelope + EventRecorder`
4. `Permission workflow`
5. `Metrics / budget governance`
6. `SwarmSupervisor`
7. `Replay + Examples`

## 8. 迭代建议

### Iteration 1

- `SessionStore`
- `PlanningStore`
- 文件持久化
- 自动快照

### Iteration 2

- `AgentBuilder`
- `SwarmRuntimeBuilder`
- 默认装配策略

### Iteration 3

- `EventEnvelope`
- `EventRecorder`
- `Permission workflow`

### Iteration 4

- `Metrics / cost / runtime governance`
- `SwarmSupervisor`
- `Replay`
- `Examples`

## 9. 测试计划

### 9.1 单元测试

覆盖以下能力：

- `SessionStore`
- `PlanningStore`
- `EventEnvelope`
- `EventRecorder`
- 审批缓存
- 审批超时
- metrics hook
- supervisor 状态机

### 9.2 集成测试

覆盖以下流程：

- session save / load / snapshot / restore
- turn 自动快照
- `todo / plan / goal` 恢复
- 审批缓存命中
- 审批超时自动拒绝
- event replay
- swarm 失败重试或重分配

### 9.3 Golden 测试

建议引入固定样本：

- transcript replay
- mock provider 输出
- tool stub 输出
- event sequence baseline

### 9.4 编译与示例验证

每个里程碑至少执行：

- 定向 `cargo test`
- `cargo check -p fox-agent-core -p fox-agent-tools -p fox-agent-sdk -p fox-agent-swarm`
- example smoke test

## 10. 风险与控制

### 10.1 风险一：状态模型反复返工

风险：

- 如果 `SessionSnapshot / PlanningStateSnapshot / EventEnvelope` 定义过晚，后续 Builder、Replay、Swarm 都会重复改造

控制策略：

- 优先冻结三类核心模型
- 在 M1 阶段完成模型评审

### 10.2 风险二：权限工作流反向污染 UI

风险：

- 如果把 UI 审批逻辑耦合进 SDK，会导致接入方难以替换

控制策略：

- SDK 只提供审批模型、缓存、超时和审计
- UI 层保持在应用侧

### 10.3 风险三：Swarm 复杂度膨胀

风险：

- 过早引入跨进程或分布式能力会偏离当前目标

控制策略：

- 当前阶段只做同进程 supervisor
- 不扩展到分布式协议

## 11. 推荐排期

| 周次 | 目标 | 交付 |
|---|---|---|
| 第 1 周 | 状态模型与 store 抽象 | `SessionStore / PlanningStore / Snapshot` |
| 第 2 周 | 文件存储与恢复闭环 | `FileSessionStore / FilePlanningStore / auto snapshot` |
| 第 3 周 | Builder 收敛 | `AgentBuilder / SwarmRuntimeBuilder / 默认装配` |
| 第 4 周 | 事件与权限治理 | `EventEnvelope / EventRecorder / Permission workflow` |
| 第 5 周 | 运行治理与 Swarm | `metrics hook / budget / SwarmSupervisor / Replay / Examples` |

## 12. 首批落地建议

建议直接从以下三项开始：

1. `SessionStore + SessionSnapshot`
2. `PlanningStore + todo/plan/goal 改造`
3. `AgentBuilder`

原因：

- 这三项能最快降低接入成本
- 也是后续事件回放、审批缓存、Swarm 产品化的共用基础

## 13. 建议交付物

- `docs/agent_sdk_app_impl_plan.md`
- `examples/simple_agent_builder.rs`
- `examples/ask_user_approval_flow.rs`
- `examples/sandbox_agent.rs`
- `examples/swarm_runtime.rs`
- `tests/fixtures/` 下的 transcript / replay / event 基线样本

