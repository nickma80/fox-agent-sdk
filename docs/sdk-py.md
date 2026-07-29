# Fox Agent SDK — Python 语言绑定方案

> 目标：将 `fox-agent-sdk` 作为底层能力暴露给 Python 开发者，提供 Pythonic 的 API 体验，同时保持与 Rust SDK 同等的能力和性能。

---

## 1. 项目概述

### 1.1 背景

`fox-agent-sdk` 是一个基于 Rust 的高性能 AI Agent 开发框架，提供了完整的 LLM Agent 基础设施：多 Provider 支持、工具系统、记忆管理、MCP 集成、Skills/Hooks/Plugins 生态等。目前仅 Rust 开发者可通过 `AgentBuilder` API 使用。

Python 是 AI/ML 生态的主流语言，大量开发者（尤其是 LangChain、AutoGPT、CrewAI 等框架的用户）希望在 Python 环境中获得 `fox-agent-sdk` 的能力——高性能工具执行、结构化事件流、子 Agent 上下文隔离等。

### 1.2 目标

| 目标 | 描述 |
|------|------|
| **全能力暴露** | Python SDK 覆盖 Rust SDK 的核心能力（Agent、工具、记忆、MCP、事件流） |
| **Pythonic API** | 使用 async/await、context manager、async generator 等 Python 惯用模式 |
| **零拷贝事件流** | 事件直接在 Rust 层产生、Python 层消费，无需序列化中转 |
| **pip 可安装** | 通过 `pip install fox-agent-sdk` 一行安装，预编译 wheel |
| **自定义工具** | Python 开发者可用纯 Python 编写自定义 Tool |

### 1.3 范围

**Phase 1（MVP）**：Agent 创建与运行、事件流、内置工具、Provider 配置
**Phase 2**：自定义工具、MCP 集成、记忆系统、Session 持久化
**Phase 3**：Skills/Hooks/Plugins、Swarm 多 Agent、评估体系

### 1.4 非目标

- 不在 Python 层重新实现 Agent 循环逻辑（性能敏感的调度、流处理、Token 计数均在 Rust 侧完成）
- 不提供 Python 版本的 Provider 实现（所有 LLM 调用通过 Rust Provider 发起）

---

## 2. 技术选型

### 2.1 核心方案：PyO3 + maturin

| 维度 | 选型 | 理由 |
|------|------|------|
| **FFI 框架** | [PyO3](https://pyo3.rs/) | Rust 生态事实标准，支持 async、class、enum 直接映射 |
| **构建工具** | [maturin](https://www.maturin.rs/) | 一键 `maturin build` 生成 wheel，支持 CI 自动发布 |
| **类型桩** | PyO3 自动生成 `.pyi` + 手写补充 | IDE 自动补全和类型检查 |
| **异步桥接** | `pyo3-asyncio` + `tokio` runtime | 将 tokio runtime 挂到 Python asyncio event loop 上 |

### 2.2 替代方案及排除

| 方案 | 排除理由 |
|------|---------|
| gRPC / HTTP 子进程 | 引入网络延迟和序列化开销；部署复杂（需管理子进程生命周期） |
| 纯 Python 重写 | 维护两套代码，性能损失，无法跟上 Rust 侧迭代 |
| PyO3 手写 `#[no_mangle]` | 开发效率低，类型映射繁琐 |

### 2.3 依赖关系

```
fox-agent-sdk (Python package)
  └── _core.pyd / _core.so          # Rust 编译产物 (PyO3)
        └── fox-agent-py (新 crate)  # 薄绑定层
              ├── fox-agent-sdk       # 复用所有 Rust 逻辑
              ├── fox-agent-core
              ├── fox-agent-tools
              └── fox-agent-providers
```

### 2.4 平台支持

| 平台 | Python 版本 | 架构 |
|------|------------|------|
| Linux | 3.10–3.13 | x86_64, aarch64 |
| macOS | 3.10–3.13 | x86_64, arm64 |
| Windows | 3.10–3.13 | x86_64 |

---

## 3. 架构设计

### 3.1 分层架构

```
┌─────────────────────────────────────────────────────┐
│  Python 层 (fox_agent_sdk/)                         │
│  ┌──────────┐ ┌──────────┐ ┌─────────┐ ┌─────────┐ │
│  │ agent.py │ │ config.py│ │tools.py │ │events.py│ │
│  │ Agent    │ │ Config   │ │Tool基类 │ │ 事件类型 │ │
│  └────┬─────┘ └────┬─────┘ └────┬────┘ └────┬────┘ │
│       │            │            │           │       │
├───────┼────────────┼────────────┼───────────┼───────┤
│  Rust 绑定层 (fox-agent-py/)                       │
│  ┌────┴────────────┴────────────┴───────────┴─────┐ │
│  │  PyO3 #[pyclass] / #[pymethods] / #[pyfunction] │ │
│  │  - PyAgent, PyAgentBuilder, PyConfig            │ │
│  │  - PyTool (Python→Rust adapter)                 │ │
│  │  - tokio-asyncio runtime bridge                 │ │
│  └──────────────────────┬─────────────────────────┘ │
│                         │                           │
├─────────────────────────┼───────────────────────────┤
│  Rust 核心层 (现有代码)  │                           │
│  ┌──────────────────────┴─────────────────────────┐ │
│  │  fox-agent-sdk / fox-agent-core / ...           │ │
│  │  Agent, AgentBuilder, Harness, Tools, Memory... │ │
│  └────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────┘
```

### 3.2 crate 结构

新增 crate `crates/fox-agent-py/`：

```
crates/fox-agent-py/
├── Cargo.toml
├── src/
│   ├── lib.rs              # #[pymodule] 入口，注册所有类
│   ├── agent.rs            # PyAgent: run(), run_streaming(), resume()
│   ├── builder.rs          # PyAgentBuilder: builder 模式绑定
│   ├── config.rs           # PyProviderConfig, PySdkConfig 等
│   ├── events.rs           # AgentEvent → Python 事件投递
│   ├── tools.rs            # Python 自定义 Tool 适配器
│   ├── types.rs            # Message, ContentBlock, TurnOutcome 等
│   └── runtime.rs          # tokio runtime 单例 + asyncio 桥接
```

### 3.3 异步桥接设计

核心挑战：Rust 的 tokio runtime 和 Python 的 asyncio event loop 需要共存。

```
Python asyncio event loop
  │
  │  await agent.run("prompt")
  │       │
  │       ▼
  │  pyo3-asyncio::tokio::into_future()
  │       │
  │       ▼
  │  tokio::task::spawn(async { agent.run_once_streaming(...) })
  │       │
  │       ▼  ┌──────────────────────────┐
  │       │  │ Rust Agent Loop           │
  │       │  │  Model.complete() → 事件  │
  │       │  │  Tool.execute() → 结果    │
  │       │  │  Safety.check() → 决策    │
  │       │  └──────────┬───────────────┘
  │       │             │
  │       │  事件通过 Python callback / async generator 返回
  │       ▼
  │  async for event in agent.run("prompt"):
  │      match event:
  │          case TextDelta(text=t): ...
  │          case ToolCallStart(name=n): ...
```

方案：在 `crates/fox-agent-py/src/runtime.rs` 中维护一个全局 tokio runtime 单例。Python 的 `async fn` 通过 `pyo3-asyncio` 将 Rust future 挂到 asyncio event loop 上。

---

## 4. API 设计

### 4.1 Quick Start

```python
import asyncio
from fox_agent_sdk import AgentBuilder, ProviderConfig

async def main():
    agent = (
        AgentBuilder()
        .provider_config(ProviderConfig.deepseek("sk-xxx"))
        .model_id("deepseek-v4-flash")
        .working_dir("./workspace")
        .with_default_tools()
        .build()
    )

    async for event in agent.run("Create a Rust project named hello"):
        match event:
            case {"type": "text_delta", "text": text}:
                print(text, end="")
            case {"type": "tool_start", "name": name, "input": input_}:
                print(f"\n[Using {name}...]")
            case {"type": "tool_end", "output": output}:
                if output.get("is_error"):
                    print(f"\n[Error: {output['text'][:100]}]")
            case {"type": "error", "error": error}:
                print(f"\n[Agent Error: {error}]")
            case {"type": "usage", "input": inp, "output": out}:
                print(f"\n[Tokens: {inp} in / {out} out]")

asyncio.run(main())
```

### 4.2 核心 API

#### AgentBuilder

```python
class AgentBuilder:
    def __init__(self) -> None: ...
    
    # Provider 配置（三选一）
    def provider_config(self, config: ProviderConfig) -> AgentBuilder: ...
    def sdk_config(self, config: SdkConfig) -> AgentBuilder: ...
    def sdk_config_file(self, path: str) -> AgentBuilder: ...  # 从 agent.toml 加载
    
    # 模型
    def model_id(self, id: str) -> AgentBuilder: ...
    
    # 工作目录
    def working_dir(self, dir: str) -> AgentBuilder: ...
    
    # 工具
    def with_default_tools(self) -> AgentBuilder: ...
    def with_tool(self, tool: Tool) -> AgentBuilder: ...
    
    # MCP
    def with_mcp_server(self, config: McpServerConfig) -> AgentBuilder: ...
    
    # 安全
    def with_safety_policy(self, config: SafetyConfig) -> AgentBuilder: ...
    
    # 系统提示词
    def with_system_prompt(self, template: str) -> AgentBuilder: ...
    
    # 构建
    async def build(self) -> Agent: ...
```

#### Agent

```python
class Agent:
    # 运行一个 turn（返回 async generator）
    async def run(self, user_message: str) -> AsyncGenerator[Event, None]: ...
    
    # 权限恢复
    async def resume(
        self, decision: PermissionDecision
    ) -> AsyncGenerator[Event, None]: ...
    
    # 会话管理
    async def snapshot(self) -> SessionSnapshot: ...
    def session_id(self) -> str: ...
```

#### Event 类型

事件以类型化 dict 或 dataclass 形式暴露给 Python：

```python
# 文本输出
{"type": "text_delta", "text": str}

# 思考过程
{"type": "thinking_delta", "text": str}

# 工具调用
{"type": "tool_start", "call_id": str, "name": str, "input": dict}
{"type": "tool_end", "call_id": str, "output": ToolOutput}
{"type": "tool_progress", "call_id": str, "elapsed_secs": int}

# Token 用量
{"type": "usage", "input": int, "output": int, "total": int}

# 权限请求
{"type": "permission_request", "request_id": str, "tool_name": str, "prompt": str}

# 错误
{"type": "error", "error": str}

# Turn 生命周期
{"type": "turn_start", "turn_id": int}
{"type": "turn_end", "turn_id": int, "outcome": str}

# Artifact 相关
{"type": "artifact_stored", "artifact_id": str, "tool_name": str, "size_bytes": int}
{"type": "artifact_read", "artifact_id": str, "returned_chars": int}

# MCP
{"type": "mcp_connected", "server_name": str}
{"type": "mcp_disconnected", "server_name": str}
```

### 4.3 配置设计

#### ProviderConfig

```python
@dataclass
class ProviderConfig:
    provider_name: str           # "openai" | "deepseek" | "anthropic"
    base_url: str
    auth: AuthConfig             # BearerToken | ApiKeyHeader
    timeout_secs: int = 120
    default_headers: list[tuple[str, str]] = field(default_factory=list)
    use_streaming_api: bool = True

    @staticmethod
    def deepseek(api_key: str) -> ProviderConfig: ...
    @staticmethod
    def openai(api_key: str) -> ProviderConfig: ...
    @staticmethod
    def anthropic(api_key: str) -> ProviderConfig: ...
```

#### SdkConfig

从 `agent.toml` 自动加载，覆盖所有子配置。等价于 `AgentBuilder.sdk_config_file("agent.toml")`。

```python
@dataclass
class SdkConfig:
    """从 FoxAgentSdkConfig 映射的 Python 配置"""
    provider: Optional[ProviderConfig] = None
    default_model: Optional[str] = None
    memory: MemoryConfig = field(default_factory=MemoryConfig)
    safety: SafetyConfig = field(default_factory=SafetyConfig)
    budget: BudgetConfig = field(default_factory=BudgetConfig)
    mcp: McpConfig = field(default_factory=McpConfig)
    # ...
```

### 4.4 自定义工具

Python 开发者通过继承 `Tool` 基类添加自定义工具：

```python
from fox_agent_sdk import Tool, ToolContext, ToolOutput
import httpx

class WeatherTool(Tool):
    def name(self) -> str:
        return "get_weather"
    
    def description(self) -> str:
        return "Get current weather for a city"
    
    def parameters_schema(self) -> dict:
        return {
            "type": "object",
            "properties": {
                "city": {"type": "string", "description": "City name"}
            },
            "required": ["city"]
        }
    
    async def execute(self, input: dict, ctx: ToolContext) -> ToolOutput:
        city = input["city"]
        async with httpx.AsyncClient() as client:
            resp = await client.get(f"https://api.weather.com/{city}")
        return ToolOutput(text=resp.text)

# 使用
agent = (
    AgentBuilder()
    .provider_config(ProviderConfig.deepseek("sk-xxx"))
    .with_tool(WeatherTool())
    .build()
)
```

### 4.5 Memory 系统

```python
from fox_agent_sdk import MemoryManager, MemoryEntry, MemoryScope

manager = MemoryManager(MemoryConfig(enabled=True))

# 记忆操作
manager.remember("Rust projects use Cargo.toml for config", scope=MemoryScope.PROJECT)
results = manager.recall("build system", limit=5, scope=MemoryScope.PROJECT)
manager.forget(memory_id)

# session 级别注入
agent = (
    AgentBuilder()
    .provider_config(...)
    .with_default_tools()
    .build()
)
agent.harness().set_memory_manager(manager)  # agent 自动注入相关记忆
```

### 4.6 MCP 集成

```python
agent = (
    AgentBuilder()
    .provider_config(...)
    .with_mcp_server(
        McpServerConfig.stdio(
            name="filesystem",
            command="npx",
            args=["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
        )
    )
    .with_mcp_server(
        McpServerConfig.sse(
            name="remote-tools",
            url="http://localhost:8080/sse"
        )
    )
    .build()
)
```

### 4.7 会话恢复

```python
# 保存
snapshot = await agent.snapshot()
snapshot.save("session.json")

# 恢复（新进程）
agent = Agent.from_snapshot("session.json", provider_config)
async for event in agent.run("Continue what you were doing"):
    ...
```

---

## 5. 与 Rust SDK 的 API 映射对照

| Rust API | Python API | 说明 |
|----------|-----------|------|
| `AgentBuilder::new()` | `AgentBuilder()` | 构造函数 |
| `.provider_config(cfg)` | `.provider_config(cfg)` | 配置 LLM 后端 |
| `.with_default_tools()` | `.with_default_tools()` | 注册内置工具 |
| `.with_tool(tool)` | `.with_tool(tool)` | 注册自定义工具 |
| `.build().await` | `await .build()` | 构建 Agent |
| `agent.run_once_streaming(msg, tx)` | `async for e in agent.run(msg)` | 流式运行 |
| `agent.resume_streaming(decision, tx)` | `async for e in agent.resume(decision)` | 权限恢复 |
| `agent.harness().session_store()` | `agent.session_id` | Session ID |
| `Tool` trait | `Tool` (Python ABC) | 工具接口 |
| `AgentEvent::TextDelta { text }` | `{"type": "text_delta", "text": ...}` | 文本事件 |
| `AgentEvent::ToolCallStart { ... }` | `{"type": "tool_start", ...}` | 工具开始 |
| `AgentEvent::ToolCallEnd { ... }` | `{"type": "tool_end", ...}` | 工具完成 |

---

## 6. 实现计划

### Phase 1: MVP（核心 Agent 能力 —— 2-3 天）

**目标**：能创建 Agent、运行 turn、接收事件流。

**任务**：

1. 创建 `crates/fox-agent-py/` crate，配置 PyO3 + maturin 依赖
2. 实现 `runtime.rs`：全局 tokio runtime 单例 + pyo3-asyncio 桥接
3. 实现 `config.rs`：`ProviderConfig`、`SdkConfig` 的 Python 类绑定
4. 实现 `builder.rs`：`AgentBuilder` 的 builder 方法链
5. 实现 `agent.rs`：`Agent.run()` → async generator of events
6. 实现 `events.rs`：`AgentEvent` → Python dict 转换
7. 实现 `types.rs`：`ToolOutput`、`PermissionDecision` 等基本类型
8. 编写 Python 包结构（`fox_agent_sdk/__init__.py`）
9. 编写示例：`examples/python/hello_agent.py`
10. 单元测试：MockProvider 下的完整 Agent 运行流程

**产出物**：
- `crates/fox-agent-py/` crate
- `fox_agent_sdk/` Python 包
- `examples/python/hello_agent.py`
- `pip install` 可用的 wheel

### Phase 2: 自定义工具 + MCP + Memory（2-3 天）

**目标**：Python 开发者可编写自定义 Tool，集成 MCP server，使用 Memory。

**任务**：

1. 实现 `tools.rs`：`PyTool` 适配器 — 将 Python 对象转为 `Arc<dyn Tool>`
2. 实现 MCP 配置绑定（`McpServerConfig`、`McpTransportMode`）
3. 实现 `MemoryManager` 绑定（CRUD + recall + ingest）
4. 实现 `SessionSnapshot` 序列化/反序列化
5. 编写示例：`examples/python/custom_tool.py`、`examples/python/mcp_demo.py`

**产出物**：
- 自定义 Tool 支持
- MCP 集成
- Memory 操作 API

### Phase 3: 完整生态 + 发布（1-2 天）

**目标**：Skills、Hooks、Swarm、CI 发布流水线。

**任务**：

1. 实现 Skills 注册表绑定
2. 实现评估体系绑定（TaskJudge、BehaviorRules）
3. 完善 `.pyi` 类型桩
4. 编写 API 文档（Sphinx）
5. CI 流水线：多平台 wheel 构建与发布到 PyPI
6. Docker 镜像：预装 fox-agent-sdk 的 Python 开发环境

**产出物**：
- PyPI 包 `fox-agent-sdk`
- Sphinx 在线文档
- CI 自动发布

---

## 7. 目录结构

```
fox-agent-sdk/
├── crates/
│   ├── fox-agent-py/               # 新增：Python 绑定 crate
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── agent.rs
│   │       ├── builder.rs
│   │       ├── config.rs
│   │       ├── events.rs
│   │       ├── tools.rs
│   │       ├── types.rs
│   │       └── runtime.rs
│   ├── fox-agent-sdk/              # 现有：Rust SDK
│   ├── fox-agent-core/             # 现有：核心类型
│   ├── fox-agent-tools/            # 现有：工具实现
│   └── fox-agent-providers/        # 现有：Provider 实现
│
├── python/                         # 新增：Python 包
│   └── fox_agent_sdk/
│       ├── __init__.py             # 重新导出
│       ├── _core.pyi               # 类型桩（自动生成 + 手写）
│       ├── agent.py                # Pythonic 高层封装
│       └── config.py               # 配置 dataclass
│
├── examples/
│   └── python/                     # 新增：Python 示例
│       ├── hello_agent.py
│       ├── custom_tool.py
│       └── mcp_demo.py
│
├── docs/
│   └── sdk-py.md                   # 本文档
│
└── pyproject.toml                  # 新增：Python 项目元数据
```

---

## 8. 验收标准

### Phase 1 MVP

| ID | 标准 | 验证方式 |
|----|------|---------|
| AC-01 | `pip install fox-agent-sdk` 可安装 | 在 Linux/macOS/Windows 上执行 |
| AC-02 | 使用 MockProvider 创建 Agent 并运行一个 turn | `pytest` 集成测试 |
| AC-03 | `async for event in agent.run("hello")` 可接收所有事件类型 | 验证 TextDelta、ToolCallStart、ToolCallEnd、Usage |
| AC-04 | 通过 `agent.toml` 文件加载配置 | `AgentBuilder().sdk_config_file("agent.toml").build()` |
| AC-05 | 权限请求能被正确捕获 | MockProvider 下模拟 RequiresUserDecision，调用 `agent.resume()` |

### Phase 2 自定义工具 + MCP + Memory

| ID | 标准 | 验证方式 |
|----|------|---------|
| AC-06 | Python 自定义 Tool 可用于 Agent 执行 | 自定义天气查询 Tool，验证 Agent 调用了它 |
| AC-07 | MCP stdio server 可正常连接和调用 | 启动一个 MCP echo server，Agent 成功调用其工具 |
| AC-08 | Memory recall 返回相关记忆 | 添加记忆后查询，验证返回结果 |
| AC-09 | Session 快照可保存和恢复 | `agent.snapshot()` → 新建 Agent → `Agent.from_snapshot()` → 恢复上下文 |

### Phase 3 完整生态

| ID | 标准 | 验证方式 |
|----|------|---------|
| AC-10 | Skills 从 `.claude/skills/` 加载 | 在 working_dir 放置 skill 文件，验证 agent 可用 |
| AC-11 | IDE 类型提示完整 | VS Code / PyCharm 中 `.` 后有正确补全 |
| AC-12 | CI 自动发布 wheel 到 PyPI | 推送 tag 后自动构建并发布 |

---

## 9. 风险与注意事项

- **Python GIL**：PyO3 在调用 Python 回调时需要获取 GIL，自定义 Tool 的 `execute()` 会持有 GIL。对于长时间运行的 Python 工具，需要考虑释放 GIL 或使用 `allow_threads`。
- **tokio runtime 单例**：全局 tokio runtime 必须在任何 Agent 操作前初始化，且不能嵌套创建。在 `fox_agent_sdk/__init__.py` 中自动初始化。
- **跨平台 wheel**：Windows 上 maturin 需 Visual Studio Build Tools；macOS arm64 需交叉编译或 CI runner。建议使用 `cibuildwheel` + GitHub Actions 自动化。
- **Rust panic 边界**：所有 `#[pyfunction]` 和 `#[pymethods]` 必须捕获 panic 并转为 Python 异常（PyO3 默认行为，但需确保 `catch_unwind` 生效）。
- **版本同步**：Python 包版本号应与 `fox-agent-sdk` crate 版本保持一致，发布流程绑定在一起。

---

## 附录 A. Cargo.toml 参考

```toml
# crates/fox-agent-py/Cargo.toml
[package]
name = "fox-agent-py"
version = "0.1.0"
edition = "2024"

[lib]
name = "_core"
crate-type = ["cdylib"]

[dependencies]
pyo3 = { version = "0.23", features = ["extension-module", "async"] }
pyo3-asyncio = { version = "0.23", features = ["tokio-runtime"] }
tokio = { workspace = true }
futures = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }

fox-agent-sdk = { path = "../fox-agent-sdk" }
fox-agent-core = { path = "../fox-agent-core" }
fox-agent-tools = { path = "../fox-agent-tools" }
fox-agent-providers = { path = "../fox-agent-providers" }
```

## 附录 B. pyproject.toml 参考

```toml
# pyproject.toml (仓库根目录)
[build-system]
requires = ["maturin>=1.7,<2.0"]
build-backend = "maturin"

[project]
name = "fox-agent-sdk"
version = "0.1.0"
requires-python = ">=3.10"
dependencies = [
    "httpx>=0.27",       # 自定义 Tool 示例用
]

[tool.maturin]
features = ["pyo3/extension-module"]
module-name = "fox_agent_sdk._core"
bindings = "pyo3"
manifest-path = "crates/fox-agent-py/Cargo.toml"
```
