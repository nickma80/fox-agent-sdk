# Fox Agent SDK — Application Developer's Guide

This guide walks you through building real applications with the Fox Agent SDK,
from a simple CLI bot to a permission-aware, multi-agent system with
observability.

---

## 1. Your First Agent

```rust
use fox_agent_sdk::{AgentBuilder, AgentEvent, ProviderConfig, TurnOutcome};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("DEEPSEEK_API_KEY")
        .expect("Set DEEPSEEK_API_KEY");

    let mut agent = AgentBuilder::new()
        .provider_config(ProviderConfig::deepseek(api_key))
        .model_id("deepseek-v4-flash")
        .with_default_tools()
        .build()
        .await?;

    let (tx, mut rx) = tokio::sync::mpsc::channel::<AgentEvent>(64);

    // Spawn event display
    tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::ModelTextDelta { text } => print!("{text}"),
                AgentEvent::ToolCallStart { name, .. } => println!("[tool:{name}]"),
                AgentEvent::ModelUsage { usage } => {
                    eprintln!("(tokens: {}/{})", usage.input_tokens, usage.output_tokens);
                }
                _ => {}
            }
        }
    });

    let prompt = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "What files are in this directory?".into());

    let outcome = agent.run_once_streaming(&prompt, &tx).await?;

    match outcome {
        TurnOutcome::Completed { text } => println!("\nDone: {}", text),
        TurnOutcome::RequiresUserDecision { request } => {
            println!("\nPermission needed: {} ({})", request.tool_name, request.risk_level);
        }
        TurnOutcome::Failed { error } => eprintln!("Error: {error}"),
        TurnOutcome::Cancelled => eprintln!("Cancelled"),
    }

    Ok(())
}
```

### What's happening

| Line | What it does |
|------|-------------|
| `AgentBuilder::new()` | Starts the builder with sensible defaults |
| `.provider_config(...)` | Sets up the LLM provider (DeepSeek / OpenAI / Anthropic) |
| `.model_id(...)` | Chooses which model to use |
| `.with_default_tools()` | Registers `read`, `write`, `bash`, `grep`, `todo`, `plan`, `memory`, `skill` |
| `.build().await` | Assembles `Provider → Model → Harness → Agent` |
| `.run_once_streaming(...)` | Runs one turn, emitting events to the channel |

---

## 2. Working with Tools

### 2.1 Built-in Tools

When you call `.with_default_tools()`, these tools are available to the agent:

| Tool | What it does | Sandboxed |
|------|-------------|-----------|
| `read` | Read file contents | Yes |
| `write` | Create or overwrite files | Yes |
| `edit` | Apply string replacements | Yes |
| `bash` | Execute shell commands | Yes |
| `grep` | Search file contents | Yes |
| `glob` | Find files by pattern | Yes |
| `todo` | Maintain a task list | — |
| `plan` | Manage a shared plan | — |
| `goal` | Track goals with checkpoints | — |
| `memory` | Cross-session learning | — |
| `skill` | On-demand domain expertise | — |

### 2.2 Registering Custom Tools

```rust
use fox_agent_sdk::{AgentBuilder, ProviderConfig, Tool, ToolContext, ToolError, ToolOutput};
use serde_json::{json, Value};
use std::sync::Arc;

struct WeatherTool;

#[async_trait::async_trait]
impl Tool for WeatherTool {
    fn name(&self) -> &str {
        "get_weather"
    }

    fn description(&self) -> &str {
        "Get current weather for a city"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "city": { "type": "string", "description": "City name" }
            },
            "required": ["city"]
        })
    }

    async fn execute(
        &self,
        input: Value,
        _ctx: ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let city = input["city"].as_str().unwrap_or("unknown");
        // In production: call a real weather API
        Ok(ToolOutput {
            text: format!("Sunny, 22C in {city}"),
            is_error: false,
            json: Some(json!({"city": city, "temp_c": 22, "condition": "sunny"})),
        })
    }
}

// Register it
let agent = AgentBuilder::new()
    .provider_config(ProviderConfig::deepseek(key))
    .model_id("deepseek-v4-flash")
    .with_tool(Arc::new(WeatherTool))
    .build()
    .await?;
```

The agent now sees `get_weather` in its tool list and can call it naturally:
> User: "What's the weather in Tokyo?"
> Agent: calls `get_weather({city: "Tokyo"})` → "Sunny, 22C in Tokyo"

---

## 3. Working with Skills

Skills are domain expertise modules your agent loads **on demand** — they are
not pre-loaded into the system prompt. Fox Agent SDK skills use the
**Claude Code skill format** (YAML frontmatter + Markdown body), so you can
reuse skills from Claude Code projects directly.

### 3.1 Creating a Skill

Create a `.md` file in `.claude/skills/`:

```markdown
---
name: sql-analyst
description: SQL query analysis and optimization
allowed-tools: [read, grep, glob]
---

You are a SQL analyst. When asked to review or write queries:

## Instructions
1. First read the schema files in `migrations/` using grep or glob.
2. Validate syntax against the target dialect.
3. Suggest indexes for any query touching >10k rows.
4. Flag N+1 query patterns.
```

**Frontmatter fields**:

| Field | Required | Description |
|-------|----------|-------------|
| `name` | No | Unique name. Defaults to filename (without `.md`). Frontmatter overrides filename. |
| `description` | No | Human-readable description. Defaults to `name`. |
| `allowed-tools` | No | Tools the skill may use, e.g. `[read, grep, bash]`. Empty = no restriction. |
| `model` | No | Preferred model. Fox Agent SDK preserves this field but does not enforce it. |

### 3.2 How Skills Are Loaded

Skills are loaded automatically when you call `with_default_tools()`:

```rust
let agent = AgentBuilder::new()
    .provider_config(ProviderConfig::deepseek(key))
    .model_id("deepseek-v4-flash")
    .working_dir(".")             // .claude/skills/*.md scanned here
    .with_default_tools()         // loads skills + registers SkillTool
    .build()
    .await?;

// Skills are loaded from <.working_dir>/.claude/skills/*.md.
// Only .md files are loaded. Other files are ignored.

### 3.3 How the Agent Uses Skills

Skills are activated on demand via the built-in `skill` tool. The agent
sees the skill list in its system prompt and can activate skills when needed:

```
Agent: skill(action="list")
  → Available skills:
       /sql-analyst  — SQL query analysis and optimization
     ★ /pdf          — PDF manipulation expert
    Use action="activate" with name to load a skill.

Agent: skill(action="activate", name="sql-analyst")
  → Skill `/sql-analyst` activated (856 chars of expertise loaded).

[Next turn: the agent's system prompt includes the SQL analyst instructions]
```

The agent can deactivate a skill when it's no longer needed:

```
Agent: skill(action="deactivate")
  → Skill `/sql-analyst` deactivated.
```

### 3.4 Design Rationale

Why on-demand activation instead of pre-loading all skills into the system
prompt?

| Approach | Prompt Size | Context Efficiency | Claude Code Compat |
|----------|------------|-------------------|--------------------|
| Pre-load all skills | O(N) grows with each skill | Wastes context on unused skills | No |
| On-demand activation (this SDK) | O(1) independent of skill count | Only active skill uses context | Yes |

With 20 skills averaging 2000 chars each, pre-loading wastes ~40K chars of
context. On-demand activation keeps the prompt lean and only pays the context
cost for skills the agent actually uses.

### 3.5 Programmatic Access

```rust
use fox_agent_core::{Skill, SkillRegistry};

// Manual skill loading
let mut registry = SkillRegistry::default();
registry.load_from_working_dir(Some(std::path::Path::new(".")))?;

// Check what's available
for skill in registry.list() {
    println!("  {} — {}", skill.name, skill.description);
}

// Activate a skill
let pdf = registry.get("pdf").unwrap();
println!("Prompt: {} chars", pdf.prompt.len());
println!("Allowed tools: {:?}", pdf.allowed_tools);
```

---

## 4. Permission & Approval System

### 4.1 Default Policy

Choose how the agent handles untrusted tool calls:

```rust
use fox_agent_sdk::SafetyConfig;

// Liberal: let the agent use any tool it wants
SafetyConfig {
    default_policy: DefaultSafetyPolicy::Allow,
    ..Default::default()
}

// Strict: ask the user before every tool call
SafetyConfig {
    default_policy: DefaultSafetyPolicy::Confirm,
    ..Default::default()
}

// Most restrictive: deny all tools unless explicitly allowed
SafetyConfig {
    default_policy: DefaultSafetyPolicy::Deny,
    tool_allowlist: Some(vec!["read".into(), "grep".into()]),
    ..Default::default()
}
```

### 4.2 Denylist & Allowlist

```rust
SafetyConfig {
    default_policy: DefaultSafetyPolicy::Confirm,
    tool_denylist: Some(vec!["bash".into(), "write".into()]),   // never allowed
    tool_allowlist: Some(vec!["read".into(), "grep".into()]),    // always allowed
    ..Default::default()
}
```

### 4.3 Handling Permission Requests

When the agent hits a tool that needs user approval, it returns
`TurnOutcome::RequiresUserDecision`:

```rust
loop {
    match agent.run_once_streaming(&user_input, &tx).await? {
        TurnOutcome::Completed { text } => {
            println!("Agent: {text}");
            break;
        }
        TurnOutcome::RequiresUserDecision { request } => {
            // Show the risk level and prompt to the user
            println!("Risk: {}", request.risk_level);
            println!("Source: {}", request.policy_source);
            println!("{}", request.prompt);

            // Get user decision
            let allowed = ask_user_yes_no("Allow?");
            let decision = if allowed {
                PermissionDecision::Allow
            } else {
                PermissionDecision::Deny
            };

            // Resume with decision
            agent.resume_streaming(decision, &tx).await?;
        }
        TurnOutcome::Failed { error } => {
            eprintln!("Error: {error}");
            break;
        }
        _ => break,
    }
}
```

### 4.4 Approval Caching (Skip re-prompting)

```rust
use fox_agent_sdk::{ApprovalManager, ApprovalScope};

let approval = ApprovalManager::new("session-001", safety_config);

// Approve "read" for the entire session
approval
    .cache_decision("read", &PermissionResult::Allow, ApprovalScope::ThisSession)
    .await;

// Later calls to "read" skip the permission check
let cached = approval.check_cache("read").await;
assert!(cached.is_some()); // Returns PermissionResult::Allow
```

Cache scopes:

| Scope | Lifetime |
|-------|----------|
| `ThisTurn` | Cleared at end of the current turn |
| `ThisSession` | Persists across turns within the session |
| `ThisWorkspace` | Persists across session restarts |

### 4.5 Audit Trail

```rust
let request = PermissionRequest::new("bash", "Execute: rm -rf /tmp/cache");

approval
    .record_audit(&request, &PermissionResult::Allow, 42)
    .await;

let trail = approval.dump_audit().await;
for entry in trail {
    println!("[{ts}] {tool} → {decision}", ts = entry.timestamp, tool = entry.tool_name, decision = entry.decision);
}
```

---

## 5. Session & Planning State Persistence

### 5.1 Auto-Snapshot

Enable automatic persistence of session state (messages, model state, pending
interrupts) after each turn:

```rust
use fox_agent_sdk::FoxAgentSdkConfig;

let config = FoxAgentSdkConfig {
    session_storage_dir: Some(PathBuf::from("./sessions")),
    planning_storage_dir: Some(PathBuf::from("./planning")),
    auto_snapshot: true,
    ..Default::default()
};

let agent = AgentBuilder::new()
    .provider_config(ProviderConfig::deepseek(key))
    .sdk_config(config)
    .build()
    .await?;
```

### 5.2 Manual Save/Load

```rust
use fox_agent_sdk::{FileSessionStore, SessionStore};

let store = FileSessionStore::new(PathBuf::from("./sessions"));

// Save after a turn
let snapshot = agent.harness().dump_snapshot();
store.save_snapshot(&snapshot).unwrap();

// Restore later
let session_ids = store.list_session_ids().unwrap();
let restored = store.load_snapshot(&session_ids[0]).unwrap();
agent.restore_from_complete_snapshot(restored);
```

### 5.3 Planning Store

```rust
use fox_agent_sdk::FilePlanningStore;

let planning = FilePlanningStore::new(PathBuf::from("./planning"));

// Read back todos/plans/goals from a previous session
let todos = planning.load_todos("session-001", PlanningScope::Session).unwrap();
let plan = planning.load_plan("session-001", PlanningScope::Session).unwrap();
```

---

## 6. Event Recording & Replay

### 6.1 Recording to JSONL

```rust
use fox_agent_sdk::EventRecorder;
use std::path::PathBuf;

let recorder = EventRecorder::new("my-session", 1);
let (tx, rx) = tokio::sync::mpsc::channel::<AgentEvent>(64);

// Spawn the recorder to consume events
tokio::spawn(recorder.clone().run(rx, Some(PathBuf::from("trace.jsonl"))));

// Run agent — events are automatically recorded
agent.run_once_streaming("Hello", &tx).await?;

// The JSONL file now contains one EventEnvelope per event with:
// event_id, session_id, turn_id, seq, timestamp, trace_id,
// parent_event_id, source, and the event payload.
```

### 6.2 Replay for Testing

```rust
use fox_agent_sdk::ReplayRunner;

let runner = ReplayRunner::from_file(
    &PathBuf::from("trace.jsonl")
).unwrap();

// Add assertions on the transcript
let mut transcript = runner.transcript().clone();
transcript.verification_checks.push(TranscriptCheck {
    description: "agent must call the read tool".into(),
    jsonpath: "$[?(@.payload.ToolCallStart.name == 'read')]".into(),
    min_occurrences: 1,
    max_occurrences: None,
});

// Run verification
let report = runner.verify(&transcript);
assert!(report.all_passed());
```

### 6.3 Secret Scrubbing

Export files are automatically scrubbed. You can also scrub manually:

```rust
use fox_agent_sdk::mask_secrets;

let safe = mask_secrets(
    "curl -H 'Authorization: Bearer eyJhbG...' https://api.example.com"
);
// → "curl -H 'Authorization: Bearer [JWT]' https://api.example.com"
```

Detected patterns: API keys (`sk-...`), JWT tokens, `Authorization:` headers,
`x-api-key:` headers, `password=` assignments, PEM private keys.

---

## 7. Governance & Observability

### 7.1 Budget Enforcement

Prevent runaway costs:

```rust
use fox_agent_sdk::{BudgetConfig, GovernanceGuard};

let guard = GovernanceGuard::new(BudgetConfig {
    token_budget: Some(500_000),       // max tokens per session
    cost_budget_cents: Some(2000),     // max $20.00 per session
    tool_timeout_secs: 30,             // kill tools after 30s
    provider_timeout_secs: 120,        // HTTP timeout for LLM calls
    provider_retries: 2,               // retry on transient errors
    max_turns: 50,                     // hard cap on turns
    ..Default::default()
});

// Wire into agent
agent.set_governance(Some(guard.clone()));
```

The agent will return `AgentError::BudgetExceeded` when limits are hit.

### 7.2 Metrics Hooks

```rust
guard.add_metrics_hook(|snap: &MetricsSnapshot| {
    println!(
        "tokens={} cost={}c tools={} errors={:.1}% compaction={}",
        snap.total_tokens,
        snap.estimated_cost_cents,
        snap.tool_calls,
        snap.tool_error_rate() * 100.0,
        snap.compaction_count,
    );
}).await;
```

### 7.3 Metrics Snapshot

```rust
let snap = guard.snapshot().await;
println!("{snap:#?}");
// MetricsSnapshot {
//     total_tokens: 12450,
//     total_input_tokens: 8340,
//     total_output_tokens: 4110,
//     estimated_cost_cents: 45,
//     tool_calls: 12,
//     tool_success_count: 10,
//     tool_error_count: 2,
//     compaction_count: 1,
//     turns_completed: 5,
//     total_latency_ms: 8230,
// }
```

---

## 8. Multi-Agent (Swarm)

### 8.1 Setting Up a Swarm

```rust
use fox_agent_sdk::{
    PlanItem, PlanPriority, PlanStatus,
    SwarmCoordinator, SwarmSupervisor,
};
use std::sync::Arc;

let coordinator = Arc::new(SwarmCoordinator::new());
let supervisor = SwarmSupervisor::with_defaults(coordinator.clone());

// Spawn workers
coordinator.spawn("researcher", "researcher").await;
coordinator.spawn("coder", "coder").await;
coordinator.spawn("reviewer", "reviewer").await;

// Define tasks with dependencies
coordinator.upsert_plan(vec![
    PlanItem {
        id: "research".into(),
        content: "Research the approach".into(),
        status: PlanStatus::Pending,
        priority: PlanPriority::High,
        assigned_to: Some("researcher".into()),
        blocked_by: vec![],
    },
    PlanItem {
        id: "implement".into(),
        content: "Implement the feature".into(),
        status: PlanStatus::Pending,
        priority: PlanPriority::High,
        assigned_to: Some("coder".into()),
        blocked_by: vec!["research".into()], // depends on research
    },
    PlanItem {
        id: "review".into(),
        content: "Code review".into(),
        status: PlanStatus::Pending,
        priority: PlanPriority::Medium,
        assigned_to: Some("reviewer".into()),
        blocked_by: vec!["implement".into()], // depends on implementation
    },
]);
```

### 8.2 Task Lifecycle

Each task flows through these states:

```
Pending → Assigned → Running → Completed
                       |          Failed → [Retry] → Running
                       |          TimedOut → [Reassign] → Running
                       ↓
                    Blocked (waiting for dependency)
```

### 8.3 Supervisor

The [`SwarmSupervisor`] provides:

| Feature | Description |
|---------|-------------|
| **Health checks** | Monitors running workers; restarts or reassigns |
| **Retry** | Auto-retries failed tasks with configurable policy |
| **Reassignment** | Moves tasks to different workers on exhaustion |
| **Timeout** | Detects tasks that exceed their time limit |
| **Summary report** | Aggregates results across all workers |

```rust
// Configure retry policy
use fox_agent_sdk::RetryPolicy;

let supervisor = SwarmSupervisor::new(
    coordinator,
    RetryPolicy {
        max_retries: 3,
        backoff_ms: 1000,
        timeout_secs: 300,
        reassign_on_exhaust: true,
    },
);

// Generate summary after all work completes
let report = supervisor.generate_summary().await;
println!("Tasks: {} completed, {} failed, {} timed out",
    report.completed, report.failed, report.timed_out);

// Worker status
for (id, status) in report.worker_statuses {
    println!("  {id}: {status:?}");
}
```

---

## 9. Testing Your Agent

### 9.1 Using MockProvider

```rust
use fox_agent_sdk::{AgentBuilder, MockProvider, StreamEvent};

let provider = Arc::new(MockProvider::new("mock"));

// Script the agent's behavior
provider.push_script(vec![
    StreamEvent::ToolUse {
        id: "call1".into(),
        name: "read".into(),
        input: json!({"path": "Cargo.toml"}),
    },
    StreamEvent::MessageStop { stop_reason: None },
]);

provider.push_script(vec![
    StreamEvent::TextDelta { text: "This is a Rust project.".into() },
    StreamEvent::MessageStop { stop_reason: None },
]);

let mut agent = AgentBuilder::new()
    .with_provider(provider)
    .model_id("mock-1")
    .with_default_tools()
    .build()
    .await?;

let outcome = agent.run_once("What's in Cargo.toml?").await.unwrap();

match outcome {
    TurnOutcome::Completed { text } => {
        assert!(text.contains("Rust project"));
    }
    _ => panic!("expected Completed"),
}
```

### 9.2 Golden Transcript Testing

```rust
use fox_agent_sdk::{EventRecorder, ReplayRunner};

// Record a known-good trace
let recorder = EventRecorder::new("golden", 1);
// ... run agent with known input ...

// Save as golden file
recorder.export_to_file(PathBuf::from("golden.jsonl")).await.unwrap();

// In CI: verify behavior matches golden
let runner = ReplayRunner::from_file(&PathBuf::from("golden.jsonl")).unwrap();
let passes = runner.check_event_types(&[
    "TurnStart", "ModelTextDelta", "ToolCallStart", "ModelTextDelta", "TurnEnd"
]);
assert!(passes, "Event sequence changed from golden transcript");
```

---

## 10. Provider Configuration Reference

### DeepSeek

```rust
ProviderConfig {
    provider_name: "deepseek".into(),
    base_url: "https://api.deepseek.com".into(),
    api_key: key,
    ..ProviderConfig::default()
}
// Shortcut:
ProviderConfig::deepseek(key)
```

### OpenAI

```rust
ProviderConfig::new("openai", "https://api.openai.com/v1", key)
```

### Anthropic

```rust
ProviderConfig::new("anthropic", "https://api.anthropic.com", key)
```

### Custom / Self-hosted

```rust
ProviderConfig {
    provider_name: "openai".into(),
    base_url: "http://localhost:8080/v1".into(),
    api_key: "not-needed".into(),
    ..ProviderConfig::default()
}
```

---

## 11. Configuration Cheat Sheet

```rust
FoxAgentSdkConfig {
    // Memory: cross-session learning
    memory: MemoryConfig {
        enabled: true,
        auto_extract: true,           // auto-learn from conversations
        embedding_enabled: false,     // set true for semantic search
        storage_dir: Some(PathBuf::from("./memory")),
        ..Default::default()
    },

    // Compaction: context window management
    compaction: CompactionConfig {
        enabled: true,
        auto_compact: true,
        ..Default::default()
    },

    // Safety: tool permission
    safety: SafetyConfig {
        default_policy: DefaultSafetyPolicy::Confirm,
        tool_denylist: Some(vec!["delete".into()]),
        approval_cache: ApprovalCacheConfig {
            enabled: true,
            ttl_secs: 3600,
        },
        approval_timeout_secs: 30,
        ..Default::default()
    },

    // Budget: cost control
    budget: BudgetConfig {
        token_budget: Some(1_000_000),
        cost_budget_cents: Some(5000),
        provider_timeout_secs: 120,
        tool_timeout_secs: 30,
        ..Default::default()
    },

    session_storage_dir: Some(PathBuf::from("./sessions")),
    planning_storage_dir: Some(PathBuf::from("./planning")),
    auto_snapshot: true,
}
```

---

## 12. Domain Adaptation — Making Your Agent Work in Any Domain

Fox Agent SDK is a **general-purpose agent runtime**. The same binary can work
in coding, quantitative trading, data analysis, SRE, or document writing —
without changing SDK code. The Agent adapts through three layered mechanisms.

### 12.1 How It Works

```
┌───────────────────────────────────────────────────────┐
│              Domain Adaptation Layers                   │
│                                                         │
│ Layer 1: AGENTS.md    (Domain instructions)             │
│   project/AGENTS.md        Project-level conventions    │
│   ~/.fox-agent/AGENTS.md   Personal global preferences  │
│   → Injected into static_part, prefix-cacheable         │
│                                                         │
│ Layer 2: Prompt Overlay  (Override directives)          │
│   project/.fox/prompt-overlay.md                        │
│   ~/.fox-agent/prompt-overlay.md                        │
│   → Appended to static_part with highest priority       │
│                                                         │
│ Layer 3: Planning Guidance  (system.md built-in)        │
│   system.md §Planning + §Domain Adaptation              │
│   → Tells Agent to read AGENTS.md and self-adapt        │
└───────────────────────────────────────────────────────┘
```

### 12.2 Step-by-step: From Coding Agent to Trading Agent

**Start with a coding project** (the default case):

```
project/
├── AGENTS.md          ← "Use Rust. Follow idiomatic patterns."
├── Cargo.toml
└── src/
```

The Agent reads `AGENTS.md` and acts as a Rust developer. No configuration needed.

**Switch to quantitative trading** — just replace `AGENTS.md`:

```markdown
# AGENTS.md (quantitative trading project)

You are a quantitative trading strategy analyst.
- Data sources: CSV files in ./data/ (OHLCV daily bars)
- Backtesting engine: use `backtrader` Python library
- Performance metrics: Sharpe ratio, max drawdown, win rate
- NEVER execute live trades without explicit user confirmation
- Output strategy reports to ./reports/ as markdown
- Reference: strategy parameters are defined in ./config/strategy.yaml
```

```rust
let agent = AgentBuilder::new()
    .provider_config(ProviderConfig::deepseek(api_key))
    .working_dir("./trading-project")  // ← point to trading project
    .with_default_tools()
    .build()
    .await?;
```

That's it. The same `AgentBuilder` code, same tools — different domain behavior driven entirely by `AGENTS.md`.

### 12.3 Best Practices

| Practice | Why |
|----------|-----|
| **Keep AGENTS.md domain-focused** | Don't repeat tool instructions; system.md already covers those. Focus on domain rules, data sources, terminology, and constraints. |
| **Use Prompt Overlay for system.md overrides** | If system.md says "Commit as you go" but your domain never uses git, add a `.fox/prompt-overlay.md` that overrides it. |
| **One project, one domain** | Don't try to make one `AGENTS.md` cover multiple domains. Create separate project directories. |
| **Global AGENTS.md for personal preferences** | Put language preferences, code style, and toolchain choices in `~/.fox-agent/AGENTS.md`. They apply to all projects. |
| **Planning tiers are domain-agnostic** | `goal`/`plan`/`todo` work the same way whether the goal is "ship a feature" or "find an alpha signal". |

### 12.4 Domain Examples

| Domain | AGENTS.md key content | Example skills |
|--------|----------------------|----------------|
| **Coding** | Language, framework, testing conventions, linting rules | `code-review`, `refactoring`, `api-design` |
| **Quant Trading** | Data sources, backtesting engine, risk limits, execution rules | `portfolio-optimization`, `market-microstructure` |
| **Data Analysis** | Tools (pandas, matplotlib), data locations, report format, citation rules | `sql-analyst`, `statistical-modeling` |
| **SRE / Operations** | Cluster endpoints, read-only constraints, alert thresholds, runbook locations | `incident-response`, `capacity-planning` |
| **Documentation** | Style guide, target audience, output format, review checklist | `api-docs`, `release-notes` |
| **Research** | Literature sources, experiment methodology, note-taking conventions | `literature-review`, `experiment-design` |

### 12.5 How the Agent Reads AGENTS.md

The system prompt tells the Agent explicitly:

```
## Domain Adaptation

The domain (coding, trading, research, operations, etc.) is defined by the
tools, skills, and project context available to you — not by your identity.
Read project instructions (AGENTS.md, prompt overlay) to understand the
current domain's conventions. Adapt your behavior accordingly.
```

This is in `static_part`, cached by the provider across turns — the Agent
reads it once at session start and carries the domain knowledge through the
entire session.

---

## 13. Troubleshooting

| Problem | Likely cause | Solution |
|---------|-------------|----------|
| Agent returns empty text | Model didn't connect | Check API key and base URL |
| `BudgetExceeded` error | Token/cost limit hit | Raise `token_budget` or `cost_budget_cents` |
| Permission requests loop forever | Default policy is `Confirm` with no cache | Use `ApprovalManager` caching |
| Tool calls hang | Tool timeout | Set `budget.tool_timeout_secs` |
| Memory not persisting | `storage_dir` not set | Set `memory.storage_dir` |
| Compilation error: `enum` not found | Tool name not registered | Call `.with_default_tools()` or `.with_tool(...)` |
