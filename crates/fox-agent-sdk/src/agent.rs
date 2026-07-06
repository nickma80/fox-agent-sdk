use fox_agent_core::{
    AgentError, AgentEvent, AgentEventTx, CompactionConfig, ContentBlock, GoalCheckpoint,
    GoalScope, GoalStatus, Message, Model, PermissionDecision, PermissionRequest,
    PermissionResult, PendingToolCallSnapshot, ProviderError, Role, SessionSnapshot, Skill,
    StreamEvent, ToolContext, ToolError, ToolExecutionMode, ToolOutput, TurnOutcome,
    load_goals_with_store, now_secs, save_goals_with_store,
};
use fox_agent_mcp::McpClient;
use futures::StreamExt;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tracing::{debug, error, info, span, trace, warn, Level};

use crate::harness::Harness;

// ── Loop limits (P0) ──

/// Maximum number of tool-loop iterations per turn (API call + tool exec cycles).
///
/// 500 is high enough for complex multi-step tasks (e.g. search that spawns
/// file reads) while still preventing true infinite loops. The turn will
/// naturally terminate once the model produces a text response without a
/// tool call.
const MAX_TOOL_LOOP_ITERATIONS: u32 = 500;
/// Maximum number of context-limit compaction retries before giving up.
const MAX_CONTEXT_LIMIT_RETRIES: u32 = 5;
/// Maximum number of incomplete / degenerate continuation attempts.
const MAX_INCOMPLETE_CONTINUATION_ATTEMPTS: u32 = 3;
/// Number of consecutive identical tool calls before injecting a warning.
const DUPLICATE_TOOL_CALL_WARN_THRESHOLD: u32 = 3;
/// Substrings that indicate a context-limit error from the provider.
const CTRL_LIMIT_KEYWORDS: &[&str] = &[
    "context_length_exceeded",
    "max_context_length",
    "too many tokens",
    "maximum context length",
    "context_overflow",
];

// ── Network / provider retry configuration ──

/// Number of fast retries with exponential backoff (250ms → 8s) for
/// transient provider errors (connection refused, timeout, 5xx, DNS).
const FAST_RETRY_MAX: u32 = 5;
/// Maximum backoff for the fast-retry phase (milliseconds).
const FAST_RETRY_MAX_BACKOFF_MS: u64 = 8_000;

/// After the fast-retry budget is exhausted, we switch to slow retries
/// that wait this many seconds between attempts — long enough for a
/// temporary network outage or DNS propagation to resolve.
const SLOW_RETRY_INTERVAL_SECS: u64 = 30;
/// Maximum number of slow retries before giving up permanently.
const SLOW_RETRY_MAX: u32 = 10;

// ---------------------------------------------------------------------------
// Internal state: a tool call awaiting user permission
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct PendingToolCall {
    call_id: String,
    name: String,
    input: serde_json::Value,
}

impl From<&PendingToolCall> for PendingToolCallSnapshot {
    fn from(value: &PendingToolCall) -> Self {
        Self {
            call_id: value.call_id.clone(),
            name: value.name.clone(),
            input: value.input.clone(),
        }
    }
}

impl From<PendingToolCallSnapshot> for PendingToolCall {
    fn from(value: PendingToolCallSnapshot) -> Self {
        Self {
            call_id: value.call_id,
            name: value.name,
            input: value.input,
        }
    }
}

// ---------------------------------------------------------------------------
// Agent — the main entry point for running the Agent Loop
// ---------------------------------------------------------------------------

use crate::governance::GovernanceGuard;

pub struct Agent {
    pub model: Arc<dyn Model>,
    pub harness: Harness,
    pending_permission: Option<PermissionRequest>,
    pending_tool_calls: Vec<PendingToolCall>,
    next_turn_id: u64,
    /// Optional budget governance guard.
    governance: Option<GovernanceGuard>,
    /// MCP client for external tool servers.
    pub mcp_client: Option<McpClient>,
    /// Currently active skill (loaded on-demand by Agent via `skill` tool).
    pub active_skill: Arc<RwLock<Option<Skill>>>,
}

impl Agent {
    pub fn new(model: Arc<dyn Model>, harness: Harness, active_skill: Arc<RwLock<Option<Skill>>>) -> Self {
        debug!(session_id = %harness.session_state.id, "Agent created");
        Self {
            model,
            harness,
            pending_permission: None,
            pending_tool_calls: Vec::new(),
            next_turn_id: 1,
            governance: None,
            mcp_client: None,
            active_skill,
        }
    }

    /// Attach a budget governance guard.
    pub fn set_governance(&mut self, guard: GovernanceGuard) {
        self.governance = Some(guard);
    }

    /// Get the budget governance guard, if attached.
    pub fn governance(&self) -> Option<&GovernanceGuard> {
        self.governance.as_ref()
    }

    pub fn harness(&self) -> &Harness { &self.harness }
    pub fn model(&self) -> &Arc<dyn Model> { &self.model }

    /// Set MCP resources/prompts context for the system prompt.
    pub(crate) fn set_mcp_context(&mut self, summary: String) {
        self.harness.prompt_builder.set_mcp_context(summary);
    }

    pub fn snapshot(&self) -> SessionSnapshot {
        SessionSnapshot {
            session_id: self.harness.session_state.id.clone(),
            parent_id: self.harness.session_state.parent_id.clone(),
            title: self.harness.session_state.title.clone(),
            model: self.harness.session_state.model.clone().or_else(|| Some(self.model.model_id())),
            provider_key: self.harness.session_state.provider_key.clone(),
            status: self.harness.session_state.status,
            working_dir: self.harness.session_state.working_dir.clone(),
            messages: self.harness.session_state.messages.clone(),
            env_snapshots: self.harness.session_state.env_snapshots.clone(),
            model_runtime_state: self.model.runtime_state(),
            pending_permission: self.pending_permission.clone(),
            pending_tool_calls: self
                .pending_tool_calls
                .iter()
                .map(PendingToolCallSnapshot::from)
                .collect(),
            interrupt_state: self
                .harness
                .interrupt_manager
                .try_read()
                .map(|guard| guard.snapshot())
                .unwrap_or_default(),
            next_turn_id: self.next_turn_id,
            metadata: None,
            updated_at: now_secs(),
            created_at: self.harness.session_state.created_at,
        }
    }

    pub fn from_session_snapshot(
        model: Arc<dyn Model>,
        mut harness: Harness,
        snapshot: SessionSnapshot,
    ) -> Self {
        harness.session_state = crate::session::SessionState::from_snapshot(&snapshot);
        if let Some(model_id) = &snapshot.model {
            let _ = model.set_model(model_id);
        }
        model.apply_state_event(fox_agent_core::ModelStateEvent::SetResumeSessionId(
            snapshot.model_runtime_state.resume_session_id.clone(),
        ));
        if let Ok(mut interrupts) = harness.interrupt_manager.try_write() {
            interrupts.restore(snapshot.interrupt_state.clone());
        }
        Self {
            model,
            harness,
            pending_permission: snapshot.pending_permission,
            pending_tool_calls: snapshot
                .pending_tool_calls
                .into_iter()
                .map(PendingToolCall::from)
                .collect(),
            next_turn_id: snapshot.next_turn_id.max(1),
            governance: None,
            mcp_client: None,
            active_skill: Arc::new(RwLock::new(None)),
        }
    }

    pub fn load_from_store(
        model: Arc<dyn Model>,
        harness: Harness,
        session_id: &str,
    ) -> Result<Option<Self>, String> {
        let snapshot = harness.session_store.load_session(session_id)?;
        Ok(snapshot.map(|snapshot| Self::from_session_snapshot(model, harness, snapshot)))
    }

    pub fn set_model(&self, model: &str) -> Result<(), ProviderError> {
        info!(from = %self.model.provider_name(), to = %model, "Switching model");
        self.model.set_model(model)
    }

    // ── Public entry points ──

    pub async fn run_once(&mut self, user_message: &str) -> Result<(), AgentError> {
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let _ = self.run_once_streaming(user_message, &tx).await?;
        Ok(())
    }

    pub async fn run_once_capture(&mut self, user_message: &str) -> Result<TurnOutcome, AgentError> {
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        self.run_once_streaming(user_message, &tx).await
    }

    pub async fn run_once_streaming(
        &mut self,
        user_message: &str,
        event_tx: &AgentEventTx,
    ) -> Result<TurnOutcome, AgentError> {
        // Governance: check budget before starting
        if let Some(ref guard) = self.governance {
            let snap = guard.snapshot().await;
            if let Some(msg) = snap.exceeds_budget(guard.budget()) {
                return Err(AgentError::BudgetExceeded { message: msg });
            }
            guard.turn_begin().await;
        }

        info!(msg_preview = %truncate(user_message, 120), "Processing user message");
        self.pending_permission = None;
        self.pending_tool_calls.clear();
        self.harness.session_state.messages.push(Message::user(user_message));
        self.persist_snapshot("user_message");
        self.run_turn_streaming(event_tx).await
    }

    /// Resume a turn after the user made a permission decision.
    pub async fn resume_streaming(
        &mut self,
        decision: PermissionDecision,
        event_tx: &AgentEventTx,
    ) -> Result<TurnOutcome, AgentError> {
        let Some(pending) = self.pending_tool_calls.first().cloned() else {
            return Err(AgentError::Internal {
                message: "no pending tool call".to_string(),
            });
        };
        self.pending_tool_calls.remove(0);

        self.execute_single_tool(pending, decision, event_tx).await?;

        self.pending_permission = None;

        // Process remaining buffered tool calls from the same model response.
        while !self.pending_tool_calls.is_empty() {
            let next = self.pending_tool_calls[0].clone();
            let name = next.name.clone();

            match self.harness.check_tool_permission(&name, &next.input).await {
                PermissionResult::Allow => {
                    self.pending_tool_calls.remove(0);
                    self.execute_single_tool(next, PermissionDecision::Allow, event_tx).await?;
                }
                PermissionResult::Deny { reason } => {
                    self.pending_tool_calls.remove(0);
                    info!(tool = %name, reason = %reason, "Remaining tool denied by policy");
                    self.harness.session_state.messages.push(
                        Message::tool_result(&next.call_id, reason, true),
                    );
                }
                PermissionResult::AskUser { request } => {
                    info!(tool = %name, "Remaining tool requires user permission");
                    self.pending_permission = Some(request.clone());
                    return Ok(TurnOutcome::RequiresUserDecision { request });
                }
            }
        }

        self.run_turn_streaming(event_tx).await
    }

    /// Execute (or deny) a single tool call and push the result message. (P2: duration)
    async fn execute_single_tool(
        &mut self,
        pending: PendingToolCall,
        decision: PermissionDecision,
        event_tx: &AgentEventTx,
    ) -> Result<(), AgentError> {
        match decision {
            PermissionDecision::Allow => {
                info!(tool = %pending.name, "Executing tool");
                let ctx = ToolContext {
                    session_id: self.harness.session_state.id.clone(),
                    message_id: uuid::Uuid::new_v4().to_string(),
                    tool_call_id: pending.call_id.clone(),
                    working_dir: self.harness.session_state.working_dir.clone(),
                    execution_mode: ToolExecutionMode::Foreground,
                    graceful_shutdown_requested: self.harness.is_graceful_shutdown_requested().await,
                };

                let start = Instant::now();

                // Concurrency control — respect tool_concurrency_limit
                let _permit = if let Some(ref guard) = self.governance {
                    let slots = guard.tool_slots();
                    match slots.acquire_owned().await {
                        Ok(permit) => Some(permit),
                        Err(_) => {
                            error!(tool = %pending.name, "Tool concurrency semaphore closed unexpectedly");
                            return Err(AgentError::Internal {
                                message: "tool execution aborted: concurrency semaphore closed".into(),
                            });
                        }
                    }
                } else {
                    None
                };

                // Timeout enforcement
                let timeout_dur = self.governance.as_ref()
                    .map(|g| std::time::Duration::from_secs(g.budget().tool_timeout_secs))
                    .unwrap_or(std::time::Duration::from_secs(60));

                // ── PreToolUse hooks ──
                let mut effective_input = pending.input.clone();
                {
                    let (allowed, block_reason, modified) = self.harness.run_pre_tool_hooks(&pending.name, &effective_input).await;
                    if !allowed {
                        let reason = block_reason.unwrap_or_else(|| "hook blocked".into());
                        info!(tool = %pending.name, reason = %reason, "Tool blocked by PreToolUse hook");
                        self.harness.session_state.messages.push(
                            Message::tool_result(&pending.call_id, reason.clone(), true),
                        );
                        return Ok(());
                    }
                    if let Some(mod_input) = modified {
                        effective_input = mod_input;
                    }
                }

                let output = match tokio::time::timeout(
                    timeout_dur,
                    self.harness.execute_tool(&pending.name, effective_input, ctx),
                ).await {
                    Ok(Ok(output)) => {
                        if let Some(ref guard) = self.governance {
                            guard.record_tool_success().await;
                        }
                        output
                    }
                    Ok(Err(err)) => {
                        error!(tool = %pending.name, error = %err, "Tool execution failed");
                        if let Some(ref guard) = self.governance {
                            guard.record_tool_error().await;
                        }
                        // Push error tool result so conversation history stays valid
                        // for the next API call.
                        self.harness.session_state.messages.push(
                            Message::tool_result(&pending.call_id, format!("tool error: {}", err), true),
                        );
                        let _ = event_tx
                            .send(AgentEvent::ToolCallEnd {
                                call_id: pending.call_id.clone(),
                                output: ToolOutput {
                                    text: format!("tool error: {}", err),
                                    is_error: true,
                                    json: None,
                                },
                            })
                            .await;
                        return Ok(());
                    }
                    Err(_elapsed) => {
                        error!(tool = %pending.name, timeout_secs = timeout_dur.as_secs(), "Tool timed out");
                        let timeout_err = ToolError::Timeout { timeout_secs: timeout_dur.as_secs() };
                        if let Some(ref guard) = self.governance {
                            guard.record_tool_error().await;
                        }
                        // Push timeout tool result so conversation history stays valid.
                        self.harness.session_state.messages.push(
                            Message::tool_result(&pending.call_id, format!("tool timed out after {}s", timeout_dur.as_secs()), true),
                        );
                        let _ = event_tx
                            .send(AgentEvent::ToolCallEnd {
                                call_id: pending.call_id.clone(),
                                output: ToolOutput {
                                    text: format!("tool timed out after {}s", timeout_dur.as_secs()),
                                    is_error: true,
                                    json: None,
                                },
                            })
                            .await;
                        self.emit_error_event(event_tx, AgentError::Tool(timeout_err.clone())).await;
                        return Ok(());
                    }
                };
                let elapsed_ms = start.elapsed().as_millis() as u64;

                // ── PostToolUse hooks ──
                {
                    let (allowed, block_reason) = self.harness.run_post_tool_hooks(&pending.name, &output.text).await;
                    if !allowed {
                        let reason = block_reason.unwrap_or_else(|| "hook blocked".into());
                        info!(tool = %pending.name, reason = %reason, "Tool result blocked by PostToolUse hook");
                        self.harness.session_state.messages.push(
                            Message::tool_result(&pending.call_id, reason.clone(), true),
                        );
                        let _ = event_tx
                            .send(AgentEvent::ToolCallEnd {
                                call_id: pending.call_id.clone(),
                                output: ToolOutput {
                                    text: reason,
                                    is_error: true,
                                    json: None,
                                },
                            })
                            .await;
                        return Ok(());
                    }
                }

                debug!(
                    tool = %pending.name,
                    out_preview = %truncate(&output.text, 200),
                    is_error = %output.is_error,
                    duration_ms = elapsed_ms,
                    "Tool result"
                );
                let _ = event_tx
                    .send(AgentEvent::ToolCallEnd {
                        call_id: pending.call_id.clone(),
                        output: output.clone(),
                    })
                    .await;
                // P2: Push result with duration metadata
                self.harness.session_state.messages.push(
                    tool_result_msg(pending.call_id, output.text, output.is_error, elapsed_ms),
                );
            }
            PermissionDecision::Deny { reason } => {
                info!(reason = %reason, "Permission denied");
                self.harness.session_state.messages.push(
                    Message::tool_result(pending.call_id, reason, true),
                );
            }
        }
        Ok(())
    }

    // ── Core turn loop (P0: retry, continuation, filtering) ──

    async fn run_turn_streaming(
        &mut self,
        event_tx: &AgentEventTx,
    ) -> Result<TurnOutcome, AgentError> {
        let session_id = self.harness.session_state.id.clone();
        let mut context_limit_retries = 0u32;
        let mut incomplete_continuations = 0u32;
        let mut tool_loop_iterations = 0u32;
        let mut provider_retry_count = 0u32;

        // Track recent tool call fingerprints (name + query) to detect
        // duplicate-call spirals (e.g. model repeatedly calls agentgrep
        // with the same query, getting 0 results each time).
        let mut prev_tool_fingerprints: Vec<(String, String)> = Vec::new();

        loop {
            let turn_id = self.next_turn_id;
            self.next_turn_id += 1;
            tool_loop_iterations += 1;

            // P1: Tool loop upper limit
            if tool_loop_iterations > MAX_TOOL_LOOP_ITERATIONS {
                warn!(
                    iterations = tool_loop_iterations,
                    "Tool loop iteration limit reached"
                );
                return Err(self.handle_error(event_tx, turn_id, AgentError::Internal {
                    message: format!(
                        "Exceeded maximum tool loop iterations ({})",
                        MAX_TOOL_LOOP_ITERATIONS
                    ),
                }));
            }

            let turn_span = span!(Level::INFO, "turn", session = %session_id, turn = turn_id);
            let _guard = turn_span.enter();

            info!("Turn loop start");
            let _ = event_tx.send(AgentEvent::TurnStart { turn_id }).await;

            if self.harness.is_graceful_shutdown_requested().await {
                warn!("Graceful shutdown requested, cancelling turn");
                return self.finish_cancelled_turn(turn_id, event_tx, None).await;
            }

            if let Some(compaction) = self.harness.maybe_compact_messages().await {
                // ── PreCompact hooks: inject context before compaction ──
                {
                    let hm = self.harness.hook_manager.read().await;
                    let session_id = self.harness.session_state.id.clone();
                    let working_dir = self.harness.session_state.working_dir
                        .as_ref()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let ctx = crate::hooks::HookContext {
                        session_id: &session_id,
                        event: "pre-compact",
                        working_dir: &working_dir,
                        tool_name: None,
                        tool_input: None,
                        tool_output: None,
                        hook_event_name: "PreCompact",
                    };
                    if let Ok(decision) = hm.execute(crate::hooks::HookEvent::PreCompact, ctx).await {
                        if let crate::hooks::HookDecision::InjectContext { context } = decision {
                            if !context.is_empty() {
                                info!(chars = context.len(), "PreCompact hook injected context");
                                self.harness.session_state.messages.push(
                                    Message::user(format!("[PreCompact hook context]\n{context}")),
                                );
                            }
                        }
                    }
                }

                info!(
                    trigger = ?compaction.trigger,
                    removed = compaction.removed_messages,
                    kept = compaction.kept_messages,
                    "Compaction triggered"
                );
                if let Some(ref guard) = self.governance {
                    guard.record_compaction().await;
                }
                let _ = event_tx
                    .send(AgentEvent::Compaction { event: compaction })
                    .await;
            }

            for interrupt in self.harness.take_pending_interrupts().await {
                info!(
                    content = %truncate(&interrupt.content, 200),
                    urgent = interrupt.urgent,
                    "Injecting soft interrupt"
                );
                self.harness.session_state.messages.push(
                    Message::user(format!("Interrupt: {}", interrupt.content)),
                );
                let _ = event_tx
                    .send(AgentEvent::SoftInterruptInjected { interrupt })
                    .await;
            }

            self.harness.trigger_memory_for_next_turn();
            let memory_injection = self.harness.take_memory_injection_for_prompt().await;
            let memory_prompt: Option<String> = memory_injection.as_ref().map(|(inj, _)| inj.prompt.clone());
            if let Some((inj, memory_state_event)) = memory_injection {
                debug!(count = inj.count, chars = inj.prompt.len(), "Memory injected into prompt");
                let _ = event_tx
                    .send(AgentEvent::MemoryStateChanged { event: memory_state_event })
                    .await;
                let _ = event_tx
                    .send(AgentEvent::MemoryInjected {
                        count: inj.count,
                        memory_ids: inj.memory_ids.clone(),
                    })
                    .await;
            }

            let active_skill_prompt = self
                .active_skill
                .read()
                .await
                .as_ref()
                .map(|s| s.prompt.clone());

            let (split, _context_info) = self
                .harness
                .build_system_prompt_split(memory_prompt.as_deref(), active_skill_prompt.as_deref())
                .await;
            let tools = self.harness.tool_definitions().await;
            let messages = self.harness.session_state.messages.clone();

            info!(
                msg_count = messages.len(),
                tool_count = tools.len(),
                static_prompt_chars = split.static_part.len(),
                dynamic_prompt_chars = split.dynamic_part.len(),
                "Sending to model"
            );
            debug!(
                system_static = %truncate(&split.static_part, 500),
                system_dynamic = %truncate(&split.dynamic_part, 500),
                "System prompt"
            );
            if !tools.is_empty() {
                let tool_names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
                debug!(tools = ?tool_names, "Available tools");
            }
            trace!(messages = ?format_message_summaries(&messages), "Full message history");

            // ── 2. API call with P0 context-limit retry ──

            let stream = match self.model.complete(
                &messages, &tools,
                &split.static_part, &split.dynamic_part,
                self.model.runtime_state().resume_session_id.as_deref(),
            ).await {
                Ok(stream) => {
                    context_limit_retries = 0;
                    provider_retry_count = 0;
                    stream
                }
                Err(err) => {
                    let err_str = err.to_string();
                    if detect_context_limit(&err_str)
                        && context_limit_retries < MAX_CONTEXT_LIMIT_RETRIES
                    {
                        context_limit_retries += 1;
                        warn!(
                            retry = context_limit_retries,
                            error = %err_str,
                            "Context limit detected, compacting and retrying"
                        );
                        if let Some(compaction) = self.harness.maybe_compact_messages().await {
                            // ── PreCompact hooks ──
                            {
                                let hm = self.harness.hook_manager.read().await;
                                let session_id = self.harness.session_state.id.clone();
                                let working_dir = self.harness.session_state.working_dir
                                    .as_ref()
                                    .map(|p| p.to_string_lossy().to_string())
                                    .unwrap_or_default();
                                let ctx = crate::hooks::HookContext {
                                    session_id: &session_id,
                                    event: "pre-compact",
                                    working_dir: &working_dir,
                                    tool_name: None,
                                    tool_input: None,
                                    tool_output: None,
                                    hook_event_name: "PreCompact",
                                };
                                if let Ok(decision) = hm.execute(crate::hooks::HookEvent::PreCompact, ctx).await {
                                    if let crate::hooks::HookDecision::InjectContext { context } = decision {
                                        if !context.is_empty() {
                                            self.harness.session_state.messages.push(
                                                Message::user(format!("[PreCompact hook context]\n{context}")),
                                            );
                                        }
                                    }
                                }
                            }

                            if let Some(ref guard) = self.governance {
                                guard.record_compaction().await;
                            }
                            let _ = event_tx
                                .send(AgentEvent::Compaction { event: compaction })
                                .await;
                        }
                        continue;
                    }
                    // ── Provider transient error retry (network, 5xx, etc.) ──
                    //
                    // Two-phase strategy:
                    //   Phase 1 (fast): up to FAST_RETRY_MAX quick retries with
                    //     exponential backoff (250ms → 8s).  Handles brief
                    //     hiccups like connection resets or 503 spikes.
                    //   Phase 2 (slow): after fast retries are exhausted, wait
                    //     SLOW_RETRY_INTERVAL_SECS between attempts for up to
                    //     SLOW_RETRY_MAX iterations.  This gives the network time
                    //     to recover from an outage (VPN drop, DNS propagation,
                    //     proxy restart, etc.) without the agent permanently
                    //     entering a "graceful shutdown" state.
                    //
                    // Non-retryable errors (4xx auth, model-not-found, etc.)
                    // are NOT retried — they fail immediately.
                    if !err.is_retryable() {
                        warn!(error = %err_str, "Non-retryable provider error — giving up");
                        return Err(self.handle_error(event_tx, turn_id, AgentError::Provider(err)));
                    }

                    // Phase 1 — fast retries
                    if provider_retry_count < FAST_RETRY_MAX {
                        provider_retry_count += 1;
                        let backoff_ms = (250u64 * (1u64 << provider_retry_count.min(6)))
                            .min(FAST_RETRY_MAX_BACKOFF_MS);
                        warn!(
                            retry = provider_retry_count,
                            max_fast = FAST_RETRY_MAX,
                            backoff_ms = backoff_ms,
                            error = %err_str,
                            "Provider transient error, fast retry"
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                        continue;
                    }

                    // Phase 2 — slow retries (waiting for network recovery)
                    let slow_attempt = provider_retry_count - FAST_RETRY_MAX;
                    if slow_attempt < SLOW_RETRY_MAX {
                        provider_retry_count += 1;
                        warn!(
                            slow_attempt = slow_attempt,
                            max_slow = SLOW_RETRY_MAX,
                            wait_secs = SLOW_RETRY_INTERVAL_SECS,
                            error = %err_str,
                            "Network appears down — waiting for recovery before retry"
                        );
                        tokio::time::sleep(
                            std::time::Duration::from_secs(SLOW_RETRY_INTERVAL_SECS)
                        ).await;
                        continue;
                    }

                    warn!(
                        retry_count = provider_retry_count,
                        fast_max = FAST_RETRY_MAX,
                        slow_max = SLOW_RETRY_MAX,
                        error = %err_str,
                        "Provider retries exhausted, giving up"
                    );
                    return Err(self.handle_error(event_tx, turn_id, AgentError::Provider(err)));
                }
            };

            // ── 3. Process streaming response (P1: stop_reason tracking) ──

            let mut collected_tool_calls: Vec<PendingToolCall> = Vec::new();
            let mut final_text = String::new();
            let mut thinking_text = String::new();
            let mut stop_reason: Option<String> = None;
            let model_message_id = uuid::Uuid::new_v4().to_string();
            let _ = event_tx
                .send(AgentEvent::ModelMessageStart {
                    message_id: model_message_id.clone(),
                })
                .await;

            tokio::pin!(stream);
            while let Some(ev) = stream.next().await {
                if self.harness.is_graceful_shutdown_requested().await {
                    warn!("Graceful shutdown during streaming");
                    let _ = event_tx
                        .send(AgentEvent::ModelMessageEnd {
                            message_id: model_message_id.clone(),
                        })
                        .await;
                    return self.finish_cancelled_turn(turn_id, event_tx, Some(final_text.clone())).await;
                }

                let ev = ev.map_err(|err| {
                    // P0: Detect context limit mid-stream
                    if detect_context_limit(&err.to_string())
                        && context_limit_retries < MAX_CONTEXT_LIMIT_RETRIES
                    {
                        context_limit_retries += 1;
                        warn!(
                            retry = context_limit_retries,
                            "Stream error due to context limit, will retry after compaction"
                        );
                    }
                    self.handle_error(event_tx, turn_id, AgentError::Provider(err))
                })?;

                match ev {
                    StreamEvent::TextDelta { text } => {
                        final_text.push_str(&text);
                        trace!(delta = %text, "TextDelta");
                        let _ = event_tx.send(AgentEvent::ModelTextDelta { text }).await;
                    }
                    StreamEvent::ThinkingDelta { text } => {
                        thinking_text.push_str(&text);
                        trace!(delta = %text, "ThinkingDelta");
                        let _ = event_tx.send(AgentEvent::ModelThinkingDelta { text }).await;
                    }
                    StreamEvent::Usage { usage } => {
                        info!(
                            input_tokens = usage.input_tokens,
                            output_tokens = usage.output_tokens,
                            total_tokens = usage.total_tokens,
                            ?usage.cache_read_input_tokens,
                            ?usage.cache_creation_input_tokens,
                            "Token usage"
                        );
                        let _ = event_tx.send(AgentEvent::ModelUsage { usage: usage.clone() }).await;

                        // Governance: record usage & check budget
                        if let Some(ref guard) = self.governance {
                            let cost = crate::governance::estimate_cost_cents(
                                &self.model.model_id(), &usage);
                            if let Err(msg) = guard.record_usage(&usage, 0, cost).await {
                                return Err(AgentError::BudgetExceeded { message: msg });
                            }
                        }
                    }
                    StreamEvent::ToolUse { id, name, input } => {
                        info!(
                            tool = %name,
                            input = %serde_json::to_string(&input).unwrap_or_default(),
                            "Model requested tool call (buffered)"
                        );
                        let _ = event_tx
                            .send(AgentEvent::ToolCallStart {
                                call_id: id.clone(),
                                name: name.clone(),
                                input: input.clone(),
                            })
                            .await;
                        collected_tool_calls.push(PendingToolCall { call_id: id, name, input });
                    }
                    StreamEvent::MessageStop { stop_reason: reason } => {
                        stop_reason = reason;
                        debug!(?stop_reason, "Model response complete");
                        let _ = event_tx
                            .send(AgentEvent::ModelMessageEnd {
                                message_id: model_message_id.clone(),
                            })
                            .await;
                        break;
                    }
                    // P2: Handle provider-side compaction notification
                    StreamEvent::Compaction { trigger, pre_tokens } => {
                        info!(?trigger, ?pre_tokens, "Provider-side compaction");
                        let _ = event_tx
                            .send(AgentEvent::Compaction {
                                event: fox_agent_core::CompactionEvent {
                                    trigger: fox_agent_core::CompactionTrigger::Provider,
                                    removed_messages: 0,
                                    kept_messages: 0,
                                    summary_chars: 0,
                                },
                            })
                            .await;
                    }
                    _ => {
                        trace!("Ignored stream event: {ev:?}");
                    }
                }
            }

            // ── 4. Process tool calls with P0 filtering and continuation ──

            // P0: Filter truncated tool calls (null/empty input from max_tokens truncation)
            let before = collected_tool_calls.len();
            filter_truncated_tool_calls(&stop_reason, &mut collected_tool_calls);
            let filtered = before - collected_tool_calls.len();
            if filtered > 0 {
                info!(filtered, "Filtered truncated tool calls");
            }

            // P3: Duplicate tool call detection — detect when the model is
            // stuck in a loop of the same tool calls that return empty results.
            let fingerprints: Vec<(String, String)> = collected_tool_calls
                .iter()
                .map(|tc| {
                    let query = tc.input.get("query")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| {
                            tc.input.to_string().chars().take(80).collect::<String>()
                        });
                    (tc.name.clone(), query)
                })
                .collect();

            // Count how many fingerprints from this turn match the previous turn.
            if !prev_tool_fingerprints.is_empty() {
                let dup_count = fingerprints
                    .iter()
                    .filter(|fp| prev_tool_fingerprints.contains(fp))
                    .count();
                if dup_count as u32 >= DUPLICATE_TOOL_CALL_WARN_THRESHOLD {
                    let dup_names: Vec<&str> = fingerprints.iter().map(|(n, _)| n.as_str()).collect();
                    info!(
                        dup_count = dup_count,
                        ?dup_names,
                        "Duplicate tool calls detected across turns, injecting soft interrupt"
                    );
                    self.harness
                        .interrupt_manager
                        .write()
                        .await
                        .queue_soft_interrupt(
                            format!("重复工具调用警告: 工具名称={:?}, 重复次数={}", dup_names, dup_count),
                            false,
                        );
                }
            }

            prev_tool_fingerprints = fingerprints;

            if collected_tool_calls.is_empty() {
                // P0: Check for incomplete continuation
                if maybe_continue_incomplete(&stop_reason, &mut incomplete_continuations, &mut self.harness) {
                    info!("Requesting continuation for incomplete response");
                    continue;
                }
                // P0: Check for degenerate (empty) response
                if maybe_continue_degenerate(&final_text, &thinking_text, &mut incomplete_continuations, &mut self.harness) {
                    info!("Requesting continuation for degenerate response");
                    continue;
                }

                // Pure text response — save and return.
                self.push_assistant_message(final_text.clone(), thinking_text.clone());
                self.harness.memory_manager.trigger_ingestion_for_turn(
                    self.harness.session_state.messages.clone(),
                    self.model.clone(),
                    event_tx.clone(),
                );
                // Governance: record turn completion (enforces max_turns)
                if let Some(ref guard) = self.governance {
                    if let Err(msg) = guard.turn_end().await {
                        return Err(AgentError::BudgetExceeded { message: msg });
                    }
                }
                // Auto-checkpoint: record progress on focused goals
                self.auto_checkpoint_focused_goals().await;
                info!(final_chars = final_text.len(), thinking_chars = thinking_text.len(), "Turn completed");
                let outcome = TurnOutcome::Completed { text: final_text };
                let _ = event_tx
                    .send(AgentEvent::TurnEnd { turn_id, outcome: outcome.clone() })
                    .await;
                self.persist_snapshot("turn_completed");
                return Ok(outcome);
            }

            // Save the assistant message with ALL tool calls.
            let mut content: Vec<ContentBlock> = Vec::new();
            if !final_text.is_empty() {
                content.push(ContentBlock::Text { text: final_text });
            }
            if !thinking_text.is_empty() {
                content.push(ContentBlock::Reasoning { text: thinking_text });
            }
            for tc in &collected_tool_calls {
                content.push(ContentBlock::ToolUse {
                    id: tc.call_id.clone(),
                    name: tc.name.clone(),
                    input: tc.input.clone(),
                });
            }
            self.harness.session_state.messages.push(
                Message { role: Role::Assistant, content },
            );

            // Execute each tool call.
            let total = collected_tool_calls.len();
            for idx in 0..total {
                let tc = &collected_tool_calls[idx];
                let name = tc.name.clone();
                let call_id = tc.call_id.clone();
                let input = tc.input.clone();

                match self.harness.check_tool_permission(&name, &input).await {
                    PermissionResult::Allow => { debug!(tool = %name, "Tool auto-allowed"); }
                    PermissionResult::Deny { reason } => {
                        info!(tool = %name, reason = %reason, "Tool denied by policy");
                        self.harness.session_state.messages.push(
                            Message::tool_result(&call_id, reason, true),
                        );
                        continue;
                    }
                    PermissionResult::AskUser { request } => {
                        info!(tool = %name, "Tool requires user permission");
                        self.pending_permission = Some(request.clone());
                        self.pending_tool_calls = collected_tool_calls.drain(idx..).collect();
                        let _ = event_tx
                            .send(AgentEvent::PermissionRequest {
                                request_id: request.request_id.clone(),
                                tool_name: request.tool_name.clone(),
                                prompt: request.prompt.clone(),
                                risk_level: request.risk_level.to_string(),
                                policy_source: request.policy_source.clone(),
                                tool_summary: request.tool_summary.clone(),
                            })
                            .await;
                        let outcome = TurnOutcome::RequiresUserDecision { request };
                        let _ = event_tx
                            .send(AgentEvent::TurnEnd { turn_id, outcome: outcome.clone() })
                            .await;
                        info!("Turn paused awaiting user decision");
                        self.persist_snapshot("awaiting_permission");
                        return Ok(outcome);
                    }
                }

                let ctx = ToolContext {
                    session_id: self.harness.session_state.id.clone(),
                    message_id: uuid::Uuid::new_v4().to_string(),
                    tool_call_id: call_id.clone(),
                    working_dir: self.harness.session_state.working_dir.clone(),
                    execution_mode: ToolExecutionMode::Foreground,
                    graceful_shutdown_requested: self.harness.is_graceful_shutdown_requested().await,
                };

                if ctx.graceful_shutdown_requested {
                    return self.finish_cancelled_turn(turn_id, event_tx, Some(String::new())).await;
                }

                // Concurrency + timeout enforcement
                let start = Instant::now();

                let _permit = if let Some(ref guard) = self.governance {
                    let slots = guard.tool_slots();
                    match slots.acquire_owned().await {
                        Ok(permit) => Some(permit),
                        Err(_) => {
                            error!(tool = %name, "Tool concurrency semaphore closed unexpectedly");
                            return Err(self.handle_error(event_tx, turn_id, AgentError::Internal {
                                message: "tool execution aborted: concurrency semaphore closed".into(),
                            }));
                        }
                    }
                } else {
                    None
                };

                let timeout_dur = self.governance.as_ref()
                    .map(|g| std::time::Duration::from_secs(g.budget().tool_timeout_secs))
                    .unwrap_or(std::time::Duration::from_secs(60));

                // ── PreToolUse hooks ──
                let mut effective_input = input.clone();
                {
                    let (allowed, block_reason, modified) = self.harness.run_pre_tool_hooks(&name, &effective_input).await;
                    if !allowed {
                        let reason = block_reason.unwrap_or_else(|| "hook blocked".into());
                        info!(tool = %name, reason = %reason, "Tool blocked by PreToolUse hook");
                        self.harness.session_state.messages.push(
                            Message::tool_result(&call_id, reason.clone(), true),
                        );
                        continue;
                    }
                    if let Some(mod_input) = modified {
                        effective_input = mod_input;
                    }
                }

                debug!(tool = %name, "Executing tool");
                let output = match tokio::time::timeout(
                    timeout_dur,
                    self.harness.execute_tool(&name, effective_input, ctx),
                ).await {
                    Ok(Ok(output)) => {
                        if let Some(ref guard) = self.governance {
                            guard.record_tool_success().await;
                        }
                        info!(
                            tool = %name,
                            is_error = output.is_error,
                            out_preview = %truncate(&output.text, 300),
                            "Tool executed"
                        );
                        output
                    }
                    Ok(Err(err)) => {
                        error!(tool = %name, error = %err, "Tool execution failed");
                        if let Some(ref guard) = self.governance {
                            guard.record_tool_error().await;
                        }
                        // Push error tool result so conversation history stays valid.
                        self.harness.session_state.messages.push(
                            Message::tool_result(&call_id, format!("tool error: {}", err), true),
                        );
                        let _ = event_tx
                            .send(AgentEvent::ToolCallEnd {
                                call_id: call_id.clone(),
                                output: ToolOutput {
                                    text: format!("tool error: {}", err),
                                    is_error: true,
                                    json: None,
                                },
                            })
                            .await;
                        // Push error results for remaining tools in the batch.
                        for j in (idx+1)..total {
                            let tc2 = &collected_tool_calls[j];
                            self.harness.session_state.messages.push(
                                Message::tool_result(&tc2.call_id, format!("skipped: earlier tool '{}' failed", name), true),
                            );
                            let _ = event_tx
                                .send(AgentEvent::ToolCallEnd {
                                    call_id: tc2.call_id.clone(),
                                    output: ToolOutput {
                                        text: "skipped due to earlier tool error".to_string(),
                                        is_error: true,
                                        json: None,
                                    },
                                })
                                .await;
                        }
                        break;
                    }
                    Err(_elapsed) => {
                        error!(tool = %name, timeout_secs = timeout_dur.as_secs(), "Tool timed out");
                        if let Some(ref guard) = self.governance {
                            guard.record_tool_error().await;
                        }
                        // Push timeout tool result so conversation history stays valid.
                        self.harness.session_state.messages.push(
                            Message::tool_result(&call_id, format!("tool timed out after {}s", timeout_dur.as_secs()), true),
                        );
                        let _ = event_tx
                            .send(AgentEvent::ToolCallEnd {
                                call_id: call_id.clone(),
                                output: ToolOutput {
                                    text: format!("tool timed out after {}s", timeout_dur.as_secs()),
                                    is_error: true,
                                    json: None,
                                },
                            })
                            .await;
                        // Push error results for remaining tools in the batch.
                        for j in (idx+1)..total {
                            let tc2 = &collected_tool_calls[j];
                            self.harness.session_state.messages.push(
                                Message::tool_result(&tc2.call_id, format!("skipped: earlier tool '{}' timed out", name), true),
                            );
                            let _ = event_tx
                                .send(AgentEvent::ToolCallEnd {
                                    call_id: tc2.call_id.clone(),
                                    output: ToolOutput {
                                        text: "skipped due to earlier tool timeout".to_string(),
                                        is_error: true,
                                        json: None,
                                    },
                                })
                                .await;
                        }
                        break;
                    }
                };
                let elapsed_ms = start.elapsed().as_millis() as u64;

                // ── PostToolUse hooks ──
                {
                    let (allowed, block_reason) = self.harness.run_post_tool_hooks(&name, &output.text).await;
                    if !allowed {
                        let reason = block_reason.unwrap_or_else(|| "hook blocked".into());
                        info!(tool = %name, reason = %reason, "Tool result blocked by PostToolUse hook");
                        self.harness.session_state.messages.push(
                            Message::tool_result(&call_id, reason.clone(), true),
                        );
                        let _ = event_tx
                            .send(AgentEvent::ToolCallEnd {
                                call_id: call_id.clone(),
                                output: ToolOutput {
                                    text: reason,
                                    is_error: true,
                                    json: None,
                                },
                            })
                            .await;
                        continue;
                    }
                }

                let _ = event_tx
                    .send(AgentEvent::ToolCallEnd {
                        call_id: call_id.clone(),
                        output: output.clone(),
                    })
                    .await;
                // P2: context guard — truncate huge tool outputs before they
                // enter the message stream (avoids wasting tokens and memory on
                // 10 MB file reads / grep explosions).  Mirrors jcode's
                // `CONTEXT_GUARD_THRESHOLD` / `SINGLE_OUTPUT_MAX_FRACTION`.
                let output_text = guard_tool_output(
                    &self.harness.cfg.compaction,
                    &self.harness.session_state.messages,
                    &name,
                    &output.text,
                );
                self.harness.session_state.messages.push(
                    tool_result_msg(call_id, output_text, output.is_error, elapsed_ms),
                );
            }

            info!("Tool calls processed, continuing turn loop");
        }
    }

    // ── Cancellation ──

    async fn finish_cancelled_turn(
        &mut self, turn_id: u64, event_tx: &AgentEventTx, partial_text: Option<String>,
    ) -> Result<TurnOutcome, AgentError> {
        warn!("Turn cancelled");
        if let Some(text) = partial_text.filter(|text| !text.is_empty()) {
            self.harness.session_state.messages.push(Message::assistant(text));
        }
        let _ = event_tx
            .send(AgentEvent::Error {
                error: AgentError::Internal { message: "graceful shutdown requested".to_string() },
            })
            .await;
        let outcome = TurnOutcome::Cancelled;
        let _ = event_tx
            .send(AgentEvent::TurnEnd { turn_id, outcome: outcome.clone() })
            .await;
        self.persist_snapshot("turn_cancelled");
        Ok(outcome)
    }

    // ── Error helpers ──

    async fn emit_error_event(&self, event_tx: &AgentEventTx, error: AgentError) {
        error!(kind = ?error.kind(), message = %error, "Emitting agent error event");
        let _ = event_tx.send(AgentEvent::Error { error }).await;
    }

    fn push_assistant_message(&mut self, text: String, thinking: String) {
        let mut content = vec![ContentBlock::Text { text }];
        if !thinking.is_empty() {
            content.push(ContentBlock::Reasoning { text: thinking });
        }
        self.harness.session_state.messages.push(
            Message { role: Role::Assistant, content },
        );
    }

    /// Record a progress checkpoint on any focused goal.
    ///
    /// Called automatically after each turn completes. If a goal has
    /// `focused: true` and is `Active`, we append a `GoalCheckpoint`
    /// with the current timestamp. The goal's `progress` and `status`
    /// are **not** modified — the Agent (or user via goal tool) is
    /// responsible for explicit progress updates.
    async fn auto_checkpoint_focused_goals(&self) {
        let session_id = &self.harness.session_state.id;
        let store = &self.harness.planning_store;

        for scope in [GoalScope::Session, GoalScope::Global] {
            let goals = load_goals_with_store(store.as_ref(), session_id, scope.clone());
            let has_focused = goals.iter().any(|g| g.focused && g.status == GoalStatus::Active);
            if !has_focused {
                continue;
            }

            let mut goals = goals;
            for goal in &mut goals {
                if goal.focused && goal.status == GoalStatus::Active {
                    goal.checkpoints.push(GoalCheckpoint {
                        at_secs: now_secs(),
                        summary: "auto-checkpoint after turn".into(),
                        progress: Some(goal.progress),
                    });
                    // Cap checkpoints to avoid unbounded growth
                    if goal.checkpoints.len() > 32 {
                        goal.checkpoints = goal.checkpoints
                            .split_at(goal.checkpoints.len() - 16).1.to_vec();
                    }
                }
            }
            let scope_str = match &scope {
                GoalScope::Session => "session",
                GoalScope::Global => "global",
            };
            let _ = save_goals_with_store(
                store.as_ref(), session_id, scope, goals, false, Some(scope_str),
            );
        }
    }

    fn handle_error(&self, event_tx: &AgentEventTx, turn_id: u64, error: AgentError) -> AgentError {
        error!(turn = turn_id, kind = ?error.kind(), message = %error, "Turn loop error");
        let tx = event_tx.clone();
        let err_event = error.clone();
        let outcome = TurnOutcome::Failed { error: error.clone() };
        tokio::spawn(async move {
            let _ = tx.send(AgentEvent::Error { error: err_event }).await;
            let _ = tx.send(AgentEvent::TurnEnd { turn_id, outcome }).await;
        });
        error
    }

    fn persist_snapshot(&self, trigger: &str) {
        if !self.harness.cfg.auto_snapshot {
            return;
        }
        let mut snapshot = self.snapshot();
        snapshot.metadata = Some(serde_json::json!({ "trigger": trigger }));
        if let Err(err) = self.harness.session_store.save_session(&snapshot) {
            warn!(session_id = %snapshot.session_id, trigger, error = %err, "failed to persist session snapshot");
        }
    }
}

// ── P0 helpers ──

/// Detect context-limit errors from provider error messages.
fn detect_context_limit(error: &str) -> bool {
    CTRL_LIMIT_KEYWORDS.iter().any(|kw| error.contains(kw))
}

/// Filter out tool calls that were truncated mid-generation (null/empty input).
fn filter_truncated_tool_calls(stop_reason: &Option<String>, calls: &mut Vec<PendingToolCall>) {
    let should_filter = match stop_reason.as_deref() {
        Some("max_tokens" | "length" | "tool_use") => true,
        _ => false,
    };
    if !should_filter {
        return;
    }
    calls.retain(|tc| {
        if tc.input.is_null() {
            return false;
        }
        if let serde_json::Value::Object(ref m) = tc.input {
            return !m.is_empty();
        }
        true
    });
}

/// Check if the model response was truncated (max_tokens) and request continuation.
fn maybe_continue_incomplete(
    stop_reason: &Option<String>,
    attempts: &mut u32,
    harness: &mut Harness,
) -> bool {
    if *attempts >= MAX_INCOMPLETE_CONTINUATION_ATTEMPTS {
        return false;
    }
    let should_continue = match stop_reason.as_deref() {
        Some("max_tokens" | "length") => true,
        _ => false,
    };
    if should_continue {
        *attempts += 1;
        harness.session_state.messages.push(
            Message::user("Please continue."),
        );
        true
    } else {
        false
    }
}

/// Check if the model produced a degenerate (empty) response and retry.
///
/// A "degenerate" response is one that provides no real value:
/// - Completely empty text.
/// - Text-only response where thinking contains planned but unexecuted
///   tool usage (`<bash>`, `<grep>`, `<write>`, `<read>`, etc.).
///   The text length is irrelevant — a 600-char response describing
///   what the model "found" is still hollow if no tools were actually run.
///   This catches the common pattern where reasoning planned concrete
///   actions but the output was only speculating about the codebase.
fn maybe_continue_degenerate(
    text: &str,
    thinking: &str,
    attempts: &mut u32,
    harness: &mut Harness,
) -> bool {
    if *attempts >= MAX_INCOMPLETE_CONTINUATION_ATTEMPTS {
        return false;
    }
    let trimmed = text.trim();
    if trimmed.is_empty() {
        *attempts += 1;
        harness.session_state.messages.push(
            Message::user("Your response was empty. Please try again."),
        );
        return true;
    }
    // Text-only response where the model planned to use tools
    // in thinking but never executed them.  Regardless of how long
    // the text is, the answer is based on speculation rather than
    // actual tool results.
    if thinking_contains_tool_plan(thinking) {
        *attempts += 1;
        harness.session_state.messages.push(
            Message::user(
                "Your response did not include any tool calls, but your \
                 thinking shows you planned to inspect the codebase or \
                 execute commands. Please actually issue the tool calls \
                 now — do not speculate about what the code does without \
                 checking it first."
            ),
        );
        return true;
    }
    false
}

/// Check whether the thinking text contains planned but unexecuted tool usage.
fn thinking_contains_tool_plan(thinking: &str) -> bool {
    let lower = thinking.to_lowercase();
    lower.contains("<bash")
        || lower.contains("<read")
        || lower.contains("<write")
        || lower.contains("<edit")
        || lower.contains("<grep")
        || lower.contains("<glob")
        || lower.contains("<web_search")
        || lower.contains("<web_fetch")
        || lower.contains("<task")
        || lower.contains("<run")
        || lower.contains("<ls")
        || lower.contains("<cat")
}

// ── P2 helper: tool result message with duration ──

/// Create a tool result message with duration appended as metadata text.
fn tool_result_msg(call_id: String, text: String, is_error: bool, duration_ms: u64) -> Message {
    // Append duration metadata so the model sees execution timing.
    let duration_suffix = if duration_ms > 100 {
        format!("\n[Tool execution duration: {:.2}s]", duration_ms as f64 / 1000.0)
    } else {
        String::new()
    };
    let enhanced = format!("{}{}", text, duration_suffix);
    Message::tool_result(call_id, enhanced, is_error)
}

// ── Context guard (tool output truncation) ──
//
// Mirrors jcode's `guard_context_overflow`.  Prevents huge tool outputs
// (e.g. reading a 10 MB file, grep matching thousands of lines) from
// entering the message stream at all — saving tokens and preventing
// memory exhaustion / compaction thrashing.

/// Maximum fraction of the compaction token budget a single tool output
/// may occupy before it is unconditionally truncated (even if there
/// appears to be room).
const SINGLE_OUTPUT_MAX_FRACTION: f32 = 0.30;

/// If the projected total context after adding this output would exceed
/// this fraction of the budget, truncate.
const CONTEXT_GUARD_THRESHOLD: f32 = 0.85;

fn guard_tool_output(
    compaction_cfg: &CompactionConfig,
    messages: &[Message],
    tool_name: &str,
    output_text: &str,
) -> String {
    if !compaction_cfg.enabled {
        return output_text.to_string();
    }

    let budget = compaction_cfg.token_budget;
    let current = super::compaction::message_chars(messages);
    let output_len = output_text.len();

    let single_max = (budget as f32 * SINGLE_OUTPUT_MAX_FRACTION) as usize;
    let threshold = (budget as f32 * CONTEXT_GUARD_THRESHOLD) as usize;
    let projected = current + output_len;

    let needs_trunc = output_len > single_max || projected > threshold;

    if !needs_trunc {
        return output_text.to_string();
    }

    // How much room do we have?
    let remaining = if current < threshold {
        threshold.saturating_sub(current)
    } else {
        budget / 50 // ~2% of budget for truncation notice
    };
    let max_chars = remaining.min(single_max);

    if output_text.len() <= max_chars {
        return output_text.to_string();
    }

    // Keep the beginning of the output (most relevant)
    let prefix = if max_chars > 300 {
        // Safe UTF-8 boundary: find last char boundary at or before the limit
        let cut = max_chars.saturating_sub(200);
        let boundary = if let Some((idx, _)) = output_text.char_indices().take_while(|(i, _)| *i < cut).last() {
            idx
        } else {
            cut.min(output_text.len())
        };
        format!(
            "{}\n\n[OUTPUT TRUNCATED: {:.0}k → {:.0}k chars. Context {}/{}. Use more targeted tool queries.]",
            &output_text[..boundary],
            output_len as f64 / 1000.0,
            max_chars as f64 / 1000.0,
            current,
            budget,
        )
    } else {
        format!(
            "[OUTPUT TRUNCATED: {:.0}k chars. Context {}/{} almost full. Use more targeted tool queries.]",
            output_len as f64 / 1000.0,
            current,
            budget,
        )
    };

    warn!(
        tool = %tool_name,
        original = output_len,
        truncated = prefix.len(),
        current = current,
        budget = budget,
        "Context guard truncated tool output"
    );

    prefix
}

// ── Logging helpers ──

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() }
    else {
        let boundary = char_boundary_before(s, max);
        format!("{}... ({}/{})", &s[..boundary], boundary, s.len())
    }
}

/// Returns the largest byte index ≤ `pos` that is a valid UTF-8 char boundary.
fn char_boundary_before(s: &str, pos: usize) -> usize {
    let pos = pos.min(s.len());
    if s.is_char_boundary(pos) { pos }
    else {
        // Walk back one byte at a time until we hit a boundary
        (0..pos).rev().find(|&i| s.is_char_boundary(i)).unwrap_or(0)
    }
}

fn format_message_summaries(messages: &[Message]) -> Vec<String> {
    messages.iter().map(|msg| {
        let role = match msg.role {
            Role::System => "sys",
            Role::User => "usr",
            Role::Assistant => "asst",
            Role::Tool => "tool",
        };
        let preview: String = msg.content.iter().filter_map(|block| match block {
            ContentBlock::Text { text } | ContentBlock::Reasoning { text } => Some(truncate(text, 80)),
            ContentBlock::ToolResult { text, .. } => Some(format!("[result: {}]", truncate(text, 60))),
            ContentBlock::ToolUse { name, .. } => Some(format!("[tool_call: {name}]")),
            ContentBlock::Image { .. } => Some("[image]".to_string()),
        }).collect::<Vec<_>>().join(" | ");
        format!("[{role}] {preview}")
    }).collect()
}
