# Fox Agent SDK

A production-grade Agent SDK for building AI applications with full lifecycle
management — from rapid prototyping to deployment-ready governance.

## Installation

```toml
[dependencies]
fox-agent-sdk = "0.1.0"
```

## Quick Start

```rust
use fox_agent_sdk::{AgentBuilder, AgentEvent, ProviderConfig, TurnOutcome};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("DEEPSEEK_API_KEY")?;

    let mut agent = AgentBuilder::new()
        .provider_config(ProviderConfig::deepseek(api_key))
        .model_id("deepseek-v4-flash")
        .with_default_tools()
        .build()
        .await?;

    let outcome = agent
        .run_once("What's the weather like in Tokyo?")
        .await?;

    match outcome {
        TurnOutcome::Completed { text } => println!("{}", text),
        TurnOutcome::RequiresUserDecision { request } => {
            println!("Permission needed: {}", request.prompt);
        }
        _ => {}
    }

    Ok(())
}
```

## Architecture

```
fox-agent-sdk (facade)
├── fox-agent-core        # Provider, Model, Agent loop, Event types, Config
├── fox-agent-providers   # DeepSeek, OpenAI, Anthropic, Mock
├── fox-agent-tools       # Built-in tools (bash, read, write, todo, plan, goal...)
└── fox-agent-swarm       # Multi-agent coordinator, supervisor, retry
```

## Features

### Builder API

Initialize a fully-configured Agent in a few lines with sensible defaults:

```rust
AgentBuilder::new()
    .provider_config(ProviderConfig::deepseek(key))
    .model_id("deepseek-v4-flash")
    .working_dir(".")
    .with_default_tools()
    .with_safety_policy(SafetyConfig::default())
    // For non-coding agents: .with_system_prompt("You are a customer support agent...")
    .build()
    .await?;
```

### Multi-Provider Support

| Provider   | `provider_name` | Constructor |
|------------|-----------------|-------------|
| DeepSeek   | `deepseek`      | `ProviderConfig::deepseek(key)` |
| OpenAI     | `openai`        | `ProviderConfig::new("openai", base_url, key)` |
| Anthropic  | `anthropic`     | `ProviderConfig::new("anthropic", base_url, key)` |
| Mock       | N/A             | `builder.with_provider(Arc::new(MockProvider::new("mock")))` |

### Tool System

**Built-in tools** included with `with_default_tools()`:

- `read` / `write` / `edit` — file operations
- `bash` — shell command execution (sandboxed)
- `grep` / `glob` — code search
- `todo` / `plan` / `goal` — planning and task tracking
- `memory` — cross-session learning

**Custom tools** via the `Tool` trait:

```rust
struct MyTool;

#[async_trait]
impl Tool for MyTool {
    fn name(&self) -> &str { "my_tool" }
    fn description(&self) -> &str { "Does something useful" }
    fn parameters_schema(&self) -> Value {
        json!({"type": "object", "properties": {...}})
    }
    async fn execute(
        &self,
        input: Value,
        ctx: ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        // ... your logic ...
        Ok(ToolOutput {
            text: "done".into(),
            is_error: false,
            json: None,
        })
    }
}

// Register
builder.with_tool(Arc::new(MyTool));
```

### Permission & Approval

Fine-grained access control with caching and audit:

```rust
let safety = SafetyConfig {
    default_policy: DefaultSafetyPolicy::Confirm, // ask user by default
    tool_denylist: Some(vec!["delete".into()]),    // always block
    tool_allowlist: Some(vec!["read".into()]),      // always allow
    ..Default::default()
};

// Approval cache: skip re-prompting within a session/workspace
let approval = ApprovalManager::new("session-1", safety);
approval.cache_decision(
    "read",
    &PermissionResult::Allow,
    ApprovalScope::ThisSession,
).await;
```

### Event Recording & Replay

Capture every round for debugging, audit, or CI:

```rust
let recorder = EventRecorder::new("session-1", 1);
// ... run agent ...
recorder.export_to_file(PathBuf::from("events.jsonl")).await.unwrap();

// Replay
let loaded = EventRecorder::load_from_file(&PathBuf::from("events.jsonl")).unwrap();
```

Secrets in event exports are automatically scrubbed (`[REDACTED]`, `[API_KEY]`, `[JWT]`).

### Governance & Observability

Runtime budget enforcement and metrics:

```rust
let guard = GovernanceGuard::new(BudgetConfig {
    token_budget: Some(1_000_000),
    cost_budget_cents: Some(5000),
    tool_timeout_secs: 30,
    ..Default::default()
});

guard.add_metrics_hook(|metrics| {
    println!("Tokens: {}, Cost: {}c, Errors: {}%",
        metrics.total_tokens,
        metrics.estimated_cost_cents,
        metrics.tool_error_rate() * 100.0,
    );
}).await;
```

### Swarm (Multi-Agent)

Coordinate multiple agents with supervisor oversight:

```rust
let coordinator = Arc::new(SwarmCoordinator::new());
let supervisor = SwarmSupervisor::with_defaults(coordinator.clone());

coordinator.spawn("worker-1", "researcher").await;
coordinator.spawn("worker-2", "coder").await;

coordinator.upsert_plan(vec![
    PlanItem { id: "t1".into(), content: "Research".into(), status: PlanStatus::Pending, ... },
    PlanItem { id: "t2".into(), content: "Implement".into(), status: PlanStatus::Pending, ... },
]);
```

The supervisor handles health checks, timeouts, retries, and task reassignment
automatically.

## Running Examples

```bash
# Single agent with DeepSeek
DEEPSEEK_API_KEY="sk-xxx" cargo run --example simple_agent

# General agent (customer support bot with custom system prompt)
cargo run --example general_agent

# Permission approval flow
cargo run --example permission_flow

# Swarm multi-agent demo
cargo run --example swarm_workflow

# Custom tool registration
cargo run --example custom_tool
```

## API Summary

| Module | Key Types | Purpose |
|--------|-----------|---------|
| `AgentBuilder` | `builder::AgentBuilder` | One-liner agent construction |
| `Agent` | `agent::Agent` | Run single-turn or streaming agent |
| `Harness` | `harness::Harness` | Tool/safety/memory/compaction container |
| `GovernanceGuard` | `governance::GovernanceGuard` | Budget, metrics, cost tracking |
| `EventRecorder` | `event_recorder::EventRecorder` | JSONL export and replay |
| `ApprovalManager` | `approval_manager::ApprovalManager` | 3-tier cache, timeout auto-deny, audit |
| `ReplayRunner` | `replay_runner::ReplayRunner` | Golden transcript verification |
| `SwarmSupervisor` | `swarm::SwarmSupervisor` | Health, retry, reassignment, reporting |
| `PromptBuilder` | `prompt_builder::PromptBuilder` | Split prompt with planning + memory injection |
| `mask_secrets` | `scrub::mask_secrets` | Secret scrubbing for event/log export |

## Non-Functional Properties

- **Async-first** — all I/O and LLM calls are non-blocking with `tokio`
- **Testable** — `MockProvider` for deterministic unit/integration tests
- **Observable** — structured events, metrics hooks, budget enforcement
- **Secure** — permission workflow with denylist/allowlist, secret scrubbing
- **Replayable** — golden transcript replay for CI regression testing

## Requirements

- Rust 2024 edition (1.85+)
- `tokio` async runtime
- `DEEPSEEK_API_KEY` / `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` for real providers

## License

MIT OR Apache-2.0
