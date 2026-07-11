# Fox Agent SDK v0.1.0 Release Notes

Fox Agent SDK 首个公开发布版本。面向 AI 应用开发的生产级 Agent SDK，提供从快速原型到部署就绪治理的完整生命周期管理。

## 核心模块

| Crate | 职责 |
|-------|------|
| `fox-agent-sdk` | 门面层：Agent Builder、事件流、会话管理、compaction、governance |
| `fox-agent-core` | 核心抽象：Provider/Mock traits、AgentEvent 类型、Config、Memory 管线 |
| `fox-agent-providers` | LLM 适配：DeepSeek、OpenAI、Anthropic、Mock |
| `fox-agent-tools` | 内置工具集：bash、read、write、edit、grep、glob、plan/goal/todo |
| `fox-agent-mcp` | MCP 协议集成：SSE transport |
| `fox-agent-swarm` | 多 Agent 编排：coordinator、supervisor、retry |

## 主要特性

### Agent 运行时

- Builder API — 几行代码初始化完整配置的 Agent
- 多 Provider 支持（DeepSeek、OpenAI、Anthropic），可扩展
- 流式响应 + Thinking/Reasoning 分离
- 会话持久化与恢复
- Turn 循环自动继续（incomplete continuation、degenerate response 检测）

### 工具系统

- 内置工具：`bash`、`read`、`write`、`edit`、`grep`、`glob`
- 规划工具：`goal`（长期目标 + 里程碑）、`plan`（依赖分解）、`todo`（即时任务）
- Memory 管线：语义嵌入搜索 + 关键词回退；三层作用域隔离（Session / Project / Global）+ 记忆提升（手动 `promote` / 达阈值自动提升）
- Skill 系统：兼容 Claude Code 技能文件（`.claude-plugin`），按需加载
- Plugin/Hook 系统：支持 Claude Code marketplace 插件的安装与发现

### 治理与安全

- Token / 成本预算强制执行
- 权限审批工作流（denylist / allowlist / 审批缓存）
- 文件操作安全沙箱
- 密钥脱敏（事件导出）

### MCP 集成

- SSE transport 支持
- 外部工具发现与调用

### Swarm 多 Agent

- 多 Agent 协调器
- 健康检查、超时重试、任务重分配

### Planner & Project Awareness

- 三层规划系统（goal / plan / todo）
- Project 指令自动发现（`AGENTS.md`、`.fox-code/rules.md`）
- 分层 prompt 注入（规划 + 记忆上下文）

## 基础设施

- **配置管理**：TOML 配置文件 + 环境变量 + 全局代理
- **Memory 存储**：本地文件 + 向量索引（vectorlite）
- **Compaction**：上下文压缩，防止 token 超限
- **事件系统**：结构化 AgentEvent，支持录制与回放
- **错误处理**：两阶段 Provider 重试（快速退避 + 慢速等待网络恢复）
- **安全字符串截断**：UTF-8 字符边界保护

## 示例程序

| 示例 | 说明 |
|------|------|
| `simple_agent` | 单 Agent 基础用法 |
| `multi_provider` | 多 Provider 切换 |
| `permission_flow` | 权限审批流程 |
| `swarm_workflow` | 多 Agent 协调 |
| `custom_tool` | 自定义工具注册 |
| `general_agent` | 通用非编程 Agent |
| `mcp_integration` | MCP 工具集成 |
| `langchain_users` | LangChain 用户迁移 |
| `planning_demo` | 规划系统演示 |

## 已知限制

- Memory 嵌入模型需在首次使用时自动下载（~600MB），下载期间语义搜索回退到关键词匹配
- MCP 仅支持 SSE transport，尚未实现 stdio transport
- Swarm 监管器尚未支持跨进程 Agent 分发
- 仅支持 Rust 语言绑定，暂无 Python/Node.js SDK

## 环境要求

- Rust 2024 edition（1.85+）
- Tokio 异步运行时
