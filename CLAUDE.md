# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build/Test Commands

```bash
# Build all crates
cargo build

# Run all tests
cargo test

# Run tests for a specific crate
cargo test -p fox-agent-sdk
cargo test -p fox-agent-core

# Run a single test
cargo test -p fox-agent-sdk -- tool_call_then_text_completes

# Check compilation without building
cargo check

# Lint
cargo clippy

# Run examples (requires provider API key)
DEEPSEEK_API_KEY="sk-xxx" cargo run --example simple_agent
cargo run --example governance
cargo run --example swarm_workflow
cargo run --example custom_tool
```

## Design Principles (from AGENTS.md)

1. **Config over env vars** — All configuration comes from `agent.toml` / `FoxAgentSdkConfig`, not environment variables.
2. **No hidden defaults** — Required values must be explicitly provided; don't rely on `Default::default()` for critical paths.
3. **No external config** — The SDK is self-contained; don't reach out to external config services.
4. **No external service dependencies** — Core logic must not require external services to function (except the LLM provider itself).
5. **No re-exports in library code** — Each crate's public API should be defined in that crate, not re-exported through intermediate crates (except the top-level `fox-agent-sdk` facade).

## Architecture

### Workspace Structure (6 crates)

```
fox-agent-sdk (facade)  ← public API, re-exports everything
├── fox-agent-core       ← abstractions: Provider, Model, Tool, Config, Event, Memory, Planning
├── fox-agent-providers  ← DeepSeek, OpenAI, Anthropic, Mock provider implementations
├── fox-agent-tools      ← built-in tools: bash, read, write, edit, grep, glob, ls, etc.
├── fox-agent-swarm      ← multi-agent: SwarmCoordinator, SwarmSupervisor
└── fox-agent-mcp        ← MCP client: stdio/SSE transport, tool adapter
```

### Core Data Flow

```
AgentBuilder::build()
  → constructs Provider from ProviderConfig (provider_name → deepseek/openai/anthropic)
  → wraps Provider in DefaultModel
  → creates Harness (tool executor, safety, memory, compaction, prompt builder, hooks, plugins, skills)
  → creates Agent { model, harness, governance }

Agent::run_once_streaming()
  → push user message
  → loop:
      → maybe_compact (PreSend overflow safety net)
      → inject interrupts / memory / active skill into system prompt
      → model.complete(messages, tools, system_prompt_static, system_prompt_dynamic)
      → process stream events (TextDelta, ToolUse, ThinkingDelta, Usage)
      → filter truncated tool calls
      → execute tools with permission checks, pre/post hooks, timeout, concurrency semaphore
      → if text-only response: return TurnOutcome::Completed
      → if tool calls: push assistant message + tool results, loop again
```

### Key Types

- **`Agent`** (`crates/fox-agent-sdk/src/agent.rs`) — The main agent loop. Uses interior mutability (`std::sync::Mutex`) for per-turn state so methods take `&self`. Contains: model, harness, governance, pending_permission, pending_tool_calls, active_skill.
- **`Harness`** (`crates/fox-agent-sdk/src/harness.rs`) — Container holding all subsystems: `ToolExecutor`, `SafetySystem`, `MemoryManager`, `CompactionManager`, `PromptBuilder`, `SkillRegistry`, `InterruptManager`, `HookManager`, `PluginManager`. Cloning shares the underlying `Arc<RwLock<SessionState>>`.
- **`AgentBuilder`** (`crates/fox-agent-sdk/src/builder.rs`) — Chainable builder that wires Provider → Model → Harness → Agent. Provider selection happens via `build_provider()` factory matching on `provider_name`.
- **`Provider`** trait (`crates/fox-agent-core/src/provider.rs`) — Abstraction over LLM backends. `ProviderError::is_retryable()` classifies errors for the two-phase retry strategy.
- **`Model`** trait — Wraps a Provider with model_id, handles prompt assembly and streaming. `DefaultModel` is the standard implementation.

### Retry Strategy (agent.rs)

Two-phase retry for transient provider errors:
1. **Fast phase** (up to 5 retries): exponential backoff 250ms → 8s
2. **Slow phase** (up to 10 retries): 30s fixed interval, for network recovery

Non-retryable errors (4xx auth, model-not-found) fail immediately.

### Compaction Strategy

Compaction is a **last resort** safety net (default `token_budget`: 3.2M chars ≈ 800K tokens).
Two modes in `CompactionManager`:
- **PreSend** — Overflow safety net right before sending to model. Bypasses gap-gate.
- **Proactive** — After turn completes. Gap-gated to prevent thrashing.

When compaction fires, dropped messages are converted to **structured `NarrativeRecord`s** via LLM extraction (or mechanical fallback). These records capture turn-by-turn narrative: user intent → agent actions → findings → decisions → pending work. They are stored in `MemoryGraph` (Session scope) and injected as `## Session History` in subsequent prompts, enabling cross-session context restoration without full message replay.

### Claude Code Compatibility

The SDK implements three Claude Code-compatible extension systems, loaded from `.claude/` directories:
- **Hooks** (`hooks.rs`) — Shell scripts at lifecycle events: `PreToolUse`, `PostToolUse`, `PreCompact`, `SessionStart`, `Stop`, `SubagentStop`, `Notification`, `PreFileWrite`, `PostFileWrite`. Defined in JSON files under `.claude/hooks/`.
- **Skills** (in `fox-agent-core`) — YAML-frontmatter markdown files under `.claude/skills/`. On-demand activation via the `skill` tool. Claude Code skill files work directly.
- **Plugins** (`plugin.rs`) — Git-cloned from marketplaces, bundle skills + hooks. Marketplace format: `.claude-plugin/marketplace.json`.

### Permission System

`SafetySystem` enforces tool permissions with three-tier policy:
- **Allowlist** — Always allowed
- **Denylist** — Always denied
- **Default policy** — `Confirm` (ask user), `Allow`, or `Deny`

`ApprovalManager` caches decisions with TTL (session/workspace scope) to avoid repeated prompts.

### Memory System

`fox_agent_core::memory` provides graph-based long-term memory with:
- **Knowledge Memory**: facts, preferences, entities, corrections — LLM wiki semantic search (query expansion + lexical prefilter + rerank, no embedding)
- **Narrative Memory** (`MemoryCategory::Narrative`): structured turn-by-turn records produced by compaction — captures "user intent → actions → findings → decisions". Stored as JSON in `MemoryGraph`, injected as `## Session History` in prompts.
- Auto-extraction from conversation, deduplication, contradiction handling

### Governance

`GovernanceGuard` enforces runtime budgets: token budget, cost budget (cents), tool timeout, tool concurrency limit (`Semaphore`), max turns. Metrics hook for observability.

## Testing with MockProvider

Tests use `MockProvider` which accepts scripted `StreamEvent` sequences via `push_script()`. Agent loop processes these as if from a real provider. See `crates/fox-agent-sdk/src/tests.rs` for patterns.
