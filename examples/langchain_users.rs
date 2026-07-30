/// LangChain/LangGraph User's Guide — 从 Python 生态迁移到 Fox Agent SDK
///
/// 本示例针对熟悉 LangChain / LangGraph 的用户，演示 fox-agent-sdk 的核心模式，
/// 并在注释中标注对应的 Python 等价写法。
///
/// 运行：cargo run --example langchain_users
///
/// ── 对照速查表 ──
///
/// | 概念               | LangChain/LangGraph            | Fox Agent SDK                     |
/// |---------------------|--------------------------------|-----------------------------------|
/// | 构建 Agent          | create_react_agent(model,tools)| AgentBuilder::new().build()       |
/// | 自定义工具          | @tool 装饰器 / BaseTool        | impl Tool trait                   |
/// | 流式输出            | .stream() / .astream_events()  | agent.run_streaming() + AgentEvent|
/// | 多 Agent 编排       | StateGraph / Supervisor 节点    | SwarmCoordinator + SwarmSupervisor|
/// | 权限 / 人机协作     | interrupt() / checkpointer     | PermissionResult + permission_hook|
/// | MCP 协议            | langchain-mcp-adapters         | with_mcp_server() (内建)          |
/// | 系统提示词          | ChatPromptTemplate             | with_system_prompt()              |
///
/// 使用 MockProvider，无需真实 LLM 凭证即可运行。
use fox_agent_sdk::{
    AgentBuilder, AgentEvent, AgentReport, FoxAgentSdkConfig, MockProvider, PermissionResult,
    PlanItem, PlanPriority, PlanStatus, StreamEvent, SwarmCoordinator, SwarmSupervisor, Tool,
    ToolContext, ToolError, ToolOutput, TurnOutcome, WorkerStatus,
};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;

// ═══════════════════════════════════════════════════════════════════════════════
// Part 1: 自定义工具 — 对应 LangChain 的 @tool 装饰器
// ═══════════════════════════════════════════════════════════════════════════════
//
// LangChain (Python):
//   @tool
//   def weather(city: str) -> str:
//       """Get current weather for a city."""
//       return f"{city}: 22°C, sunny"
//
// Fox Agent SDK (Rust): 实现 Tool trait

struct WeatherTool;

#[async_trait::async_trait]
impl Tool for WeatherTool {
    fn name(&self) -> &str {
        "get_weather" // 对应 @tool 装饰后的函数名
    }

    fn description(&self) -> &str {
        "Get current weather for a given city. Returns temperature and conditions."
        // 对应 @tool 的 docstring
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "city": {
                    "type": "string",
                    "description": "City name, e.g. 'Beijing'"
                }
            },
            "required": ["city"]
        })
        // 对应 Pydantic 自动推断的 input schema
    }

    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let city = input["city"].as_str().unwrap_or("unknown");
        Ok(ToolOutput {
            text: format!("{city}: 22°C, sunny, humidity 45%"),
            is_error: false,
            json: Some(json!({
                "city": city,
                "temperature_c": 22,
                "condition": "sunny",
                "humidity_pct": 45
            })),
        })
    }
}

/// 带结构化输出的工具 —— 对应 LangChain 的 StructuredTool
///
/// LangChain:
///   class CalcInput(BaseModel):
///       expression: str = Field(description="Math expression")
///
///   @tool(args_schema=CalcInput)
///   def calculator(expression: str) -> float:
///       return eval(expression)

struct CalculatorTool;

#[async_trait::async_trait]
impl Tool for CalculatorTool {
    fn name(&self) -> &str {
        "calculator"
    }

    fn description(&self) -> &str {
        "Evaluate a mathematical expression. Supports +, -, *, /, parentheses."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "expression": {
                    "type": "string",
                    "description": "Math expression to evaluate, e.g. '(3 + 5) * 2'"
                }
            },
            "required": ["expression"]
        })
    }

    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let expr = input["expression"].as_str().unwrap_or("0");
        // Simplified evaluator — production code would use a safe math lib
        let result = match expr {
            "(3 + 5) * 2" => 16.0,
            "100 / 7" => 14.2857,
            _ => 0.0,
        };
        Ok(ToolOutput {
            text: format!("{expr} = {result}"),
            is_error: false,
            json: Some(json!({"expression": expr, "result": result})),
        })
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Part 2: AgentBuilder — 对应 LangChain 的 create_agent / LCEL 链
// ═══════════════════════════════════════════════════════════════════════════════
//
// LangChain:
//   from langchain.agents import create_react_agent
//   agent = create_react_agent(model, tools, prompt)
//   result = agent.invoke({"input": "..."})
//
// Fox Agent SDK: 链式 Builder，每步类型安全且有 IDE 补全

async fn demo_agent_builder() {
    println!("\n══════ Part 2: AgentBuilder ══════\n");

    let provider = Arc::new(MockProvider::new("mock"));
    // LangChain 等价: ChatOpenAI(model="gpt-4o")

    provider.push_script(vec![
        StreamEvent::ToolUse {
            id: "c1".into(),
            name: "get_weather".into(),
            input: json!({"city": "Tokyo"}),
        },
        StreamEvent::MessageStop { stop_reason: None },
    ]);
    provider.push_script(vec![
        StreamEvent::TextDelta {
            text: "Tokyo is currently 22°C and sunny. Enjoy your trip!".into(),
        },
        StreamEvent::MessageStop { stop_reason: None },
    ]);

    // ── 链式构建 (对应 LCEL 的 | 管道) ──
    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cfg = FoxAgentSdkConfig::load_from_file(project_root.join("agent.toml"))
        .unwrap_or_else(|_| FoxAgentSdkConfig::default());

    let agent = AgentBuilder::new()
        .working_dir(&project_root)
        .sdk_config(cfg)
        .with_global_agents_md_path(project_root.join("AGENTS.md"))
        // 注入预构建 Provider (测试用)。生产环境用 .provider_config(ProviderConfig::deepseek(key))
        .with_provider(provider.clone())
        .model_id("mock-1")
        // 注册自定义工具 — 每个 .with_tool() 对应 tool list 中的一项
        .with_tool(Arc::new(WeatherTool))
        .with_tool(Arc::new(CalculatorTool))
        // 设置系统提示词 — 对应 ChatPromptTemplate.from_messages([SystemMessage(...)])
        .with_system_prompt(
            "You are a helpful assistant. Use tools to answer user questions \
             about weather and calculations. Always respond in Chinese.",
        )
        .build()
        .await
        .expect("build agent");

    // ── 执行 (对应 agent.invoke) ──
    // 使用 streaming 模式避免 channel 死锁
    let (tx, mut rx) = tokio::sync::mpsc::channel::<AgentEvent>(64);
    let handle = tokio::spawn(async move {
        while let Some(_ev) = rx.recv().await {
            // Events consumed to prevent channel backpressure
        }
    });

    let outcome = agent
        .run_once_streaming("东京今天天气怎么样?", &tx)
        .await
        .expect("agent run");
    drop(tx);
    handle.await.ok();

    match outcome {
        TurnOutcome::Completed { text, .. } => {
            println!("[agent] {text}");
            println!("\n✓ AgentBuilder 模式运行成功\n");
        }
        other => println!("[unexpected outcome] {other:?}"),
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Part 3: 流式事件 — 对应 LangChain 的 .astream_events()
// ═══════════════════════════════════════════════════════════════════════════════
//
// LangChain:
//   async for event in agent.astream_events({"input": "..."}, version="v2"):
//       match event["event"]:
//           case "on_chat_model_stream": ...
//           case "on_tool_start": ...
//           case "on_tool_end": ...

async fn demo_streaming_events() {
    println!("\n══════ Part 3: Streaming Events ══════\n");

    let provider = Arc::new(MockProvider::new("mock"));
    provider.push_script(vec![
        StreamEvent::ToolUse {
            id: "c1".into(),
            name: "calculator".into(),
            input: json!({"expression": "(3 + 5) * 2"}),
        },
        StreamEvent::MessageStop { stop_reason: None },
    ]);
    provider.push_script(vec![
        StreamEvent::TextDelta {
            text: "The result ".into(),
        },
        StreamEvent::TextDelta {
            text: "is 16.".into(),
        },
        StreamEvent::MessageStop { stop_reason: None },
    ]);

    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cfg = FoxAgentSdkConfig::load_from_file(project_root.join("agent.toml"))
        .unwrap_or_else(|_| FoxAgentSdkConfig::default());

    let agent = AgentBuilder::new()
        .working_dir(&project_root)
        .sdk_config(cfg)
        .with_global_agents_md_path(project_root.join("AGENTS.md"))
        .with_provider(provider)
        .model_id("mock-1")
        .with_tool(Arc::new(CalculatorTool))
        .build()
        .await
        .expect("build");

    // ── 流式执行 (对应 .astream_events) ──
    let (tx, mut rx) = tokio::sync::mpsc::channel::<AgentEvent>(64);

    let handle = tokio::spawn(async move {
        let _ = agent.run_once_streaming("Calculate (3 + 5) * 2", &tx).await;
    });

    // ── 消费事件流 (对应 async for event in ...) ──
    let mut text_parts = Vec::new();
    while let Some(ev) = rx.recv().await {
        match ev {
            // 对应 on_chat_model_stream
            AgentEvent::ModelTextDelta { text } => {
                print!("{text}");
                text_parts.push(text);
            }
            // 对应 on_tool_start
            AgentEvent::ToolCallStart { name, input, .. } => {
                println!("\n  [tool-start] {name}: {input}");
            }
            // 对应 on_tool_end
            AgentEvent::ToolCallEnd { output, .. } => {
                println!(
                    "  [tool-end]   result: {} (error: {})",
                    &output.text[..output.text.len().min(80)],
                    output.is_error
                );
            }
            // 对应 usage_metadata
            AgentEvent::ModelUsage { usage } => {
                println!(
                    "\n  [usage] input={} output={} total={}",
                    usage.input_tokens, usage.output_tokens, usage.total_tokens
                );
            }
            AgentEvent::Error { error } => {
                eprintln!("\n  [error] {error}");
            }
            _ => {}
        }
    }
    handle.await.ok();
    println!("\n\n✓ Streaming 模式运行成功");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Part 4: 多 Agent Swarm — 对应 LangGraph 的 StateGraph + Supervisor 节点
// ═══════════════════════════════════════════════════════════════════════════════
//
// LangGraph 等价代码:
//   class SupervisorState(TypedDict):
//       tasks: list[Task]
//       workers: dict
//
//   builder = StateGraph(SupervisorState)
//   builder.add_node("supervisor", supervisor_node)
//   builder.add_node("worker_a", worker_node)
//   builder.add_node("worker_b", worker_node)
//   builder.add_conditional_edges("supervisor", assign_task, {...})
//   graph = builder.compile()

async fn demo_swarm() {
    println!("\n══════ Part 4: Multi-Agent Swarm ══════\n");

    // ── 初始化 Coordinator + Supervisor ──
    // 对应: builder = StateGraph(SupervisorState)
    let coordinator = Arc::new(SwarmCoordinator::new());
    let supervisor = SwarmSupervisor::with_defaults(coordinator.clone());

    // ── 创建任务计划 (对应 StateGraph 的初始 state) ──
    // 带依赖关系: p1 → p2 → p3 (p3 依赖 p1, p2 都完成)
    coordinator
        .upsert_plan(vec![
            PlanItem {
                id: "research".into(),
                content: "Research Tokyo weather forecast for next 3 days".into(),
                status: PlanStatus::Pending,
                priority: PlanPriority::High,
                assigned_to: None,
                blocked_by: vec![],
            },
            PlanItem {
                id: "calculate".into(),
                content: "Calculate travel budget: hotel ¥12000/night × 3 days".into(),
                status: PlanStatus::Pending,
                priority: PlanPriority::Medium,
                assigned_to: None,
                blocked_by: vec!["research".into()], // 依赖 research 先完成
            },
            PlanItem {
                id: "summary".into(),
                content: "Write a trip summary combining weather and budget info".into(),
                status: PlanStatus::Pending,
                priority: PlanPriority::Low,
                assigned_to: None,
                blocked_by: vec!["research".into(), "calculate".into()],
            },
        ])
        .await;

    println!(
        "[plan] {} tasks with dependency chain",
        coordinator.shared_plan.read().await.items.len()
    );

    // ── 注册 Worker (对应 add_node("worker", ...)) ──
    coordinator
        .spawn("researcher", "weather & research expert")
        .await;
    coordinator
        .spawn("analyst", "calculation & reporting expert")
        .await;
    println!("[workers] researcher + analyst registered\n");

    // ── Worker 1 领取并完成 research ──
    let task = coordinator
        .assign_next_runnable_task("researcher")
        .await
        .unwrap();
    println!("[researcher] assigned: {}", task.id);
    coordinator
        .report_completion("researcher", &task.id, "Tokyo: 22°C/24°C/21°C, all sunny")
        .await
        .unwrap();
    println!("[researcher] completed: {}", task.id);

    // ── Worker 1 尝试领取 calculate (被 blocked_by 阻塞时跳过) ──
    // 对应 LangGraph conditional edge 的判断逻辑
    let task = coordinator
        .assign_next_runnable_task("researcher")
        .await
        .unwrap();
    println!("[researcher] assigned: {}", task.id);

    // 模拟失败 → 由 Supervisor 处理
    coordinator.reports.write().await.push(AgentReport {
        worker_id: "researcher".into(),
        task_id: Some(task.id.clone()),
        status: WorkerStatus::Failed,
        summary: "Calculation error: network timeout".into(),
    });
    let handled = supervisor.handle_failure("researcher", &task.id).await;
    println!("[supervisor] failure handled: {handled} (task reset & retried)\n");

    // ── Worker 2 接管失败任务 ──
    let task = coordinator
        .assign_next_runnable_task("analyst")
        .await
        .unwrap();
    println!("[analyst] assigned: {}", task.id);
    coordinator
        .report_completion("analyst", &task.id, "Budget: ¥36,000 total")
        .await
        .unwrap();
    println!("[analyst] completed: {}", task.id);

    // ── Worker 2 继续完成 summary ──
    let task = coordinator
        .assign_next_runnable_task("analyst")
        .await
        .unwrap();
    println!("[analyst] assigned: {}", task.id);
    coordinator
        .report_completion(
            "analyst",
            &task.id,
            "Trip summary: 3 sunny days, ¥36,000 budget",
        )
        .await
        .unwrap();
    println!("[analyst] completed: {}\n", task.id);

    // ── 生成汇总报告 ──
    let summary = supervisor.generate_summary().await;
    println!("{}", summary.format());
    if summary.all_terminal() {
        println!("All tasks completed — workflow finished.");
    }

    println!("\n✓ Swarm workflow 运行成功");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Part 5: 权限 / 人机协作 — 对应 LangGraph 的 interrupt() + checkpointer
// ═══════════════════════════════════════════════════════════════════════════════
//
// LangGraph:
//   def tool_node(state):
//       if needs_approval(state["tool"]):
//           raise NodeInterrupt("Need human approval")
//       return execute(state["tool"])
//
// Fox Agent SDK: permission_hook 回调，返回 PermissionResult

async fn demo_permission_hook() {
    println!("\n══════ Part 5: Permission / Human-in-the-loop ══════\n");

    let provider = Arc::new(MockProvider::new("mock"));
    provider.push_script(vec![
        StreamEvent::ToolUse {
            id: "c1".into(),
            name: "calculator".into(),
            input: json!({"expression": "100 / 7"}),
        },
        StreamEvent::MessageStop { stop_reason: None },
    ]);
    provider.push_script(vec![
        StreamEvent::TextDelta {
            text: "100 / 7 ≈ 14.29".into(),
        },
        StreamEvent::MessageStop { stop_reason: None },
    ]);

    // ── 自定义权限策略 (对应 LangGraph 的 interrupt 逻辑) ──
    // Allow: 放行 | AskUser: 暂停等用户确认 | Deny: 拒绝
    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cfg = FoxAgentSdkConfig::load_from_file(project_root.join("agent.toml"))
        .unwrap_or_else(|_| FoxAgentSdkConfig::default());

    let agent = AgentBuilder::new()
        .working_dir(&project_root)
        .sdk_config(cfg)
        .with_global_agents_md_path(project_root.join("AGENTS.md"))
        .with_provider(provider)
        .model_id("mock-1")
        .with_tool(Arc::new(CalculatorTool))
        .with_permission_hook(move |tool_name: &str, _input: &Value| {
            println!("  [permission-hook] checking: {tool_name}");
            // 业务规则: calculator 需要审批 (危险操作), 其他自动放行
            match tool_name {
                "calculator" => {
                    // 生产环境这里会弹 UI 等用户确认
                    // 对应 LangGraph 的 raise NodeInterrupt("need approval")
                    println!("  [permission-hook] calculator requires approval → Allow");
                    PermissionResult::Allow // 演示用 Allow，实际可改为 AskUser
                }
                _ => {
                    println!("  [permission-hook] {tool_name} auto-approved");
                    PermissionResult::Allow
                }
            }
        })
        .build()
        .await
        .expect("build");

    // 使用 streaming 模式避免 channel 死锁
    let (tx, mut rx) = tokio::sync::mpsc::channel::<AgentEvent>(64);
    let handle = tokio::spawn(async move { while let Some(_ev) = rx.recv().await {} });

    let outcome = agent
        .run_once_streaming("Calculate 100 / 7", &tx)
        .await
        .expect("run");
    drop(tx);
    handle.await.ok();

    match outcome {
        TurnOutcome::Completed { text, .. } => {
            println!("\n[agent] {text}");
        }
        _ => {}
    }

    println!("\n✓ Permission hook 运行成功");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Part 6: MCP 协议集成 — 对应 langchain-mcp-adapters
// ═══════════════════════════════════════════════════════════════════════════════
//
// LangChain:
//   from langchain_mcp_adapters.client import MultiServerMCPClient
//   client = MultiServerMCPClient({
//       "filesystem": {"command": "npx", "args": [...]}
//   })
//   tools = client.get_tools()
//
// Fox Agent SDK: 内建支持，链式调用 with_mcp_server()

async fn demo_mcp_explanation() {
    println!("\n══════ Part 6: MCP Integration ══════\n");

    // Fox Agent SDK 内建 MCP 支持，无需额外安装适配器包。
    //
    // 生产环境用法:
    //
    // ```ignore
    // let agent = AgentBuilder::new()
    //     .provider_config(ProviderConfig::deepseek(key))
    //     .with_mcp_server(McpServerConfig {
    //         name: "filesystem".into(),
    //         command: "npx".into(),
    //         args: vec![
    //             "-y".into(),
    //             "@modelcontextprotocol/server-filesystem".into(),
    //             "/workspace".into(),
    //         ],
    //         ..Default::default()
    //     })
    //     .with_mcp_server(McpServerConfig {
    //         name: "github".into(),
    //         command: "npx".into(),
    //         args: vec![
    //             "-y".into(),
    //             "@modelcontextprotocol/server-github".into(),
    //         ],
    //         env: Some(vec![
    //             ("GITHUB_PERSONAL_ACCESS_TOKEN".into(), "ghp_xxx".into()),
    //         ]),
    //         // 只暴露特定工具，避免 token 过大
    //         tools_only: Some(vec![
    //             "search_repositories".into(),
    //             "get_file_contents".into(),
    //         ]),
    //         ..Default::default()
    //     })
    //     .build()
    //     .await?;
    // ```
    //
    // 与 LangChain 的差异:
    // - 无需安装额外的适配器包 (langchain-mcp-adapters)
    // - 内建 stdio transport，自动管理 MCP server 子进程生命周期
    // - tools_only 字段可精确控制暴露的工具集
    // - auto_approve 字段可批量信任某个 MCP server 的所有工具

    println!("MCP 集成说明:\n");
    println!("  1. 在 AgentBuilder 中链式调用 .with_mcp_server()");
    println!("  2. SDK 在 build() 时自动启动 MCP server 子进程");
    println!("  3. 通过 stdio (JSON-RPC 2.0) 通信，自动发现工具");
    println!("  4. 工具以 Tools trait 形式注册，Agent 可透明调用");
    println!("\n✓ MCP 集成模式说明完成 (真实连接需 MCP server 运行时)");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Main — 运行所有演示
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::main]
async fn main() {
    println!(
        "╔══════════════════════════════════════════════════════════╗\n\
         ║  Fox Agent SDK — LangChain/LangGraph User's Guide    ║\n\
         ╚══════════════════════════════════════════════════════════╝"
    );

    demo_agent_builder().await;
    demo_streaming_events().await;
    demo_swarm().await;
    demo_permission_hook().await;
    demo_mcp_explanation().await;

    println!("\n══════ 所有演示完成 ══════");
    println!(
        "更多示例: examples/simple_agent.rs, examples/custom_tool.rs, examples/swarm_workflow.rs"
    );
}
