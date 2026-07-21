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
use tracing::{debug, error, info, span, trace, warn, Instrument, Level};

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
/// Number of consecutive auto-turns (no new user message) before drift
/// detection injects a soft interrupt reminder.
const DRIFT_DETECTION_THRESHOLD: u32 = 5;
/// Interval (in auto-turns) between drift-detection reminders.
const DRIFT_DETECTION_INTERVAL: u32 = 3;
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
/// While waiting for the next model stream event, emit a `WaitingForModel`
/// heartbeat every this many seconds so the UI can distinguish "slow model"
/// from "frozen". Informational only — does not abort or retry the request.
const MODEL_WAIT_HEARTBEAT_SECS: u64 = 8;
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
    /// Per-turn mutable state behind interior mutability so turn-driving
    /// methods can take `&self`. Guarded by a `std::sync::Mutex` and never
    /// held across an `.await`.
    pending_permission: std::sync::Mutex<Option<PermissionRequest>>,
    pending_tool_calls: std::sync::Mutex<Vec<PendingToolCall>>,
    next_turn_id: std::sync::atomic::AtomicU64,
    /// Counter for consecutive turns without a new user message.
    /// Used by drift detection to inject periodic reminders.
    consecutive_auto_turns: std::sync::atomic::AtomicU32,
    /// Optional budget governance guard.
    governance: Option<GovernanceGuard>,
    /// MCP client for external tool servers.
    pub mcp_client: Option<McpClient>,
    /// Currently active skill (loaded on-demand by Agent via `skill` tool).
    pub active_skill: Arc<RwLock<Option<Skill>>>,
}

impl Agent {
    pub fn new(model: Arc<dyn Model>, harness: Harness, active_skill: Arc<RwLock<Option<Skill>>>) -> Self {
        debug!(session_id = %harness.session_id(), "Agent created");
        Self {
            model,
            harness,
            pending_permission: std::sync::Mutex::new(None),
            pending_tool_calls: std::sync::Mutex::new(Vec::new()),
            next_turn_id: std::sync::atomic::AtomicU64::new(1),
            consecutive_auto_turns: std::sync::atomic::AtomicU32::new(0),
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

    // ── Per-turn mutable state accessors (interior mutability) ──
    // These use short synchronous critical sections and are never held
    // across an `.await`, so turn-driving methods can take `&self`.

    fn allocate_turn_id(&self) -> u64 {
        self.next_turn_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    fn peek_turn_id(&self) -> u64 {
        self.next_turn_id.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn pending_permission_snapshot(&self) -> Option<PermissionRequest> {
        self.pending_permission.lock().unwrap().clone()
    }

    fn set_pending_permission(&self, value: Option<PermissionRequest>) {
        *self.pending_permission.lock().unwrap() = value;
    }

    fn pending_tool_calls_snapshot(&self) -> Vec<PendingToolCall> {
        self.pending_tool_calls.lock().unwrap().clone()
    }

    fn set_pending_tool_calls(&self, value: Vec<PendingToolCall>) {
        *self.pending_tool_calls.lock().unwrap() = value;
    }

    fn clear_pending_tool_calls(&self) {
        self.pending_tool_calls.lock().unwrap().clear();
    }

    fn pending_tool_calls_is_empty(&self) -> bool {
        self.pending_tool_calls.lock().unwrap().is_empty()
    }

    /// Clone the first pending tool call without removing it.
    fn first_pending_tool_call(&self) -> Option<PendingToolCall> {
        self.pending_tool_calls.lock().unwrap().first().cloned()
    }

    /// Remove and return the first pending tool call.
    fn pop_first_pending_tool_call(&self) -> Option<PendingToolCall> {
        let mut guard = self.pending_tool_calls.lock().unwrap();
        if guard.is_empty() {
            None
        } else {
            Some(guard.remove(0))
        }
    }

    /// Test-only: run a turn directly on the streaming loop, bypassing
    /// `run_once_streaming`'s graceful-shutdown clearing, so in-progress
    /// turn cancellation can be exercised.
    #[cfg(test)]
    pub(crate) async fn run_turn_for_test(
        &self,
        user_message: &str,
        event_tx: &AgentEventTx,
    ) -> Result<TurnOutcome, AgentError> {
        self.harness.push_message(Message::user(user_message)).await;
        self.run_turn_streaming(event_tx).await
    }

    /// Set MCP resources/prompts context for the system prompt.
    pub(crate) fn set_mcp_context(&mut self, summary: String) {
        self.harness.prompt_builder.set_mcp_context(summary);
    }

    pub async fn snapshot(&self) -> SessionSnapshot {
        let ss = self.harness.session_state_read().await;
        SessionSnapshot {
            session_id: ss.id.clone(),
            parent_id: ss.parent_id.clone(),
            title: ss.title.clone(),
            model: ss.model.clone().or_else(|| Some(self.model.model_id())),
            provider_key: ss.provider_key.clone(),
            status: ss.status,
            working_dir: ss.working_dir.clone(),
            messages: ss.messages.clone(),
            full_messages: ss.full_messages.clone(),
            env_snapshots: ss.env_snapshots.clone(),
            model_runtime_state: self.model.runtime_state(),
            pending_permission: self.pending_permission_snapshot(),
            pending_tool_calls: self
                .pending_tool_calls_snapshot()
                .iter()
                .map(PendingToolCallSnapshot::from)
                .collect(),
            interrupt_state: self
                .harness
                .interrupt_manager
                .try_read()
                .map(|guard| guard.snapshot())
                .unwrap_or_default(),
            next_turn_id: self.peek_turn_id(),
            metadata: None,
            updated_at: now_secs(),
            created_at: ss.created_at,
        }
    }

    pub fn from_session_snapshot(
        model: Arc<dyn Model>,
        mut harness: Harness,
        snapshot: SessionSnapshot,
    ) -> Self {
        let restored_messages = snapshot.messages.clone();
        harness.reset_session_state(crate::session::SessionState::from_snapshot(&snapshot));
        // Repopulate first/latest user message tracking from restored messages
        // so Intent Guard and Intent Anchor work after session restore.
        harness.repopulate_user_message_tracking_sync(&restored_messages);
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
            pending_permission: std::sync::Mutex::new(snapshot.pending_permission),
            pending_tool_calls: std::sync::Mutex::new(
                snapshot
                    .pending_tool_calls
                    .into_iter()
                    .map(PendingToolCall::from)
                    .collect(),
            ),
            next_turn_id: std::sync::atomic::AtomicU64::new(snapshot.next_turn_id.max(1)),
            consecutive_auto_turns: std::sync::atomic::AtomicU32::new(0),
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

    pub async fn run_once(&self, user_message: &str) -> Result<(), AgentError> {
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let _ = self.run_once_streaming(user_message, &tx).await?;
        Ok(())
    }

    pub async fn run_once_capture(&self, user_message: &str) -> Result<TurnOutcome, AgentError> {
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        self.run_once_streaming(user_message, &tx).await
    }

    pub async fn run_once_streaming(
        &self,
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
        // A new user message means the user wants to continue — clear any
        // leftover graceful-shutdown flag from a previously cancelled turn,
        // otherwise the new turn would be cancelled immediately.
        self.harness.clear_graceful_shutdown().await;
        self.set_pending_permission(None);
        self.clear_pending_tool_calls();
        // Reset drift detection — new user message means the user is engaged
        self.consecutive_auto_turns.store(0, std::sync::atomic::Ordering::SeqCst);
        self.harness.push_message(Message::user(user_message)).await;
        self.persist_snapshot("user_message").await;
        self.run_turn_streaming(event_tx).await
    }

    /// Resume a turn after the user made a permission decision.
    pub async fn resume_streaming(
        &self,
        decision: PermissionDecision,
        event_tx: &AgentEventTx,
    ) -> Result<TurnOutcome, AgentError> {
        let Some(pending) = self.pop_first_pending_tool_call() else {
            return Err(AgentError::Internal {
                message: "no pending tool call".to_string(),
            });
        };

        self.execute_single_tool(pending, decision, event_tx).await?;

        self.set_pending_permission(None);

        // Process remaining buffered tool calls from the same model response.
        while !self.pending_tool_calls_is_empty() {
            let Some(next) = self.first_pending_tool_call() else { break };
            let name = next.name.clone();

            match self.harness.check_tool_permission(&name, &next.input).await {
                PermissionResult::Allow => {
                    let _ = self.pop_first_pending_tool_call();
                    self.execute_single_tool(next, PermissionDecision::Allow, event_tx).await?;
                }
                PermissionResult::Deny { reason } => {
                    let _ = self.pop_first_pending_tool_call();
                    info!(tool = %name, reason = %reason, "Remaining tool denied by policy");
                    self.harness.push_message(
                        Message::tool_result(&next.call_id, reason, true),
                    ).await;
                }
                PermissionResult::AskUser { request } => {
                    info!(tool = %name, "Remaining tool requires user permission");
                    self.set_pending_permission(Some(request.clone()));
                    return Ok(TurnOutcome::RequiresUserDecision { request });
                }
            }
        }

        self.run_turn_streaming(event_tx).await
    }

    /// Execute (or deny) a single tool call and push the result message. (P2: duration)
    async fn execute_single_tool(
        &self,
        pending: PendingToolCall,
        decision: PermissionDecision,
        event_tx: &AgentEventTx,
    ) -> Result<(), AgentError> {
        match decision {
            PermissionDecision::Allow => {
                info!(tool = %pending.name, "Executing tool");
                let ctx = ToolContext {
                    session_id: self.harness.session_id().to_string(),
                    message_id: uuid::Uuid::new_v4().to_string(),
                    tool_call_id: pending.call_id.clone(),
                    working_dir: self.harness.session_working_dir().cloned(),
                    execution_mode: ToolExecutionMode::Foreground,
                    graceful_shutdown_requested: self.harness.is_graceful_shutdown_requested().await,
                    progress_tx: None,
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
                        self.harness.push_message(
                            Message::tool_result(&pending.call_id, reason.clone(), true),
                        ).await;
                        return Ok(());
                    }
                    if let Some(mod_input) = modified {
                        effective_input = mod_input;
                    }
                }

                let output = match tokio::time::timeout(
                    timeout_dur,
                    async {
                        // Start heartbeat for progress UI
                        let hb_call_id = pending.call_id.clone();
                        let hb_name = pending.name.clone();
                        let hb_tx = event_tx.clone();
                        let hb_start = Instant::now();
                        let hb_handle = tokio::spawn(async move {
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                            loop {
                                let _ = hb_tx.send(AgentEvent::ToolExecutionProgress {
                                    call_id: hb_call_id.clone(),
                                    tool_name: hb_name.clone(),
                                    elapsed_secs: hb_start.elapsed().as_secs(),
                                }).await;
                                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                            }
                        });
                        let result = self.harness.execute_tool_with_cache(&pending.name, effective_input, ctx).await;
                        hb_handle.abort();
                        result
                    }.in_current_span(),
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
                        self.harness.push_message(
                            Message::tool_result(&pending.call_id, format!("tool error: {}", err), true),
                        ).await;
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
                        self.harness.push_message(
                            Message::tool_result(&pending.call_id, format!("tool timed out after {}s", timeout_dur.as_secs()), true),
                        ).await;
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
                        self.harness.push_message(
                            Message::tool_result(&pending.call_id, reason.clone(), true),
                        ).await;
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
                self.harness.push_message(
                    tool_result_msg(pending.call_id, output.text, output.is_error, elapsed_ms),
                ).await;
            }
            PermissionDecision::Deny { reason } => {
                info!(reason = %reason, "Permission denied");
                self.harness.push_message(
                    Message::tool_result(pending.call_id, reason, true),
                ).await;
            }
        }
        Ok(())
    }

    // ── Core turn loop (P0: retry, continuation, filtering) ──

    async fn run_turn_streaming(
        &self,
        event_tx: &AgentEventTx,
    ) -> Result<TurnOutcome, AgentError> {
        let session_id = self.harness.session_id().to_string();
        let mut context_limit_retries = 0u32;
        let mut incomplete_continuations = 0u32;
        let mut tool_loop_iterations = 0u32;
        let mut provider_retry_count = 0u32;

        // Track recent tool call fingerprints (name + query) to detect
        // duplicate-call spirals (e.g. model repeatedly calls agentgrep
        // with the same query, getting 0 results each time).
        let mut prev_tool_fingerprints: Vec<(String, String)> = Vec::new();

        loop {
            let turn_id = self.allocate_turn_id();
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

            // NOTE: `run` is the SDK-internal run/session id from the harness,
            // NOT fox-code's server_session_id. Named `run` (not `session`) to
            // avoid colliding with the outer `session{id=...}` span that
            // embedders like fox-code attach with their own session id.
            let turn_span = span!(Level::INFO, "turn", run = %session_id, turn = turn_id);
            let _guard = turn_span.enter();

            info!("Turn loop start");
            let _ = event_tx.send(AgentEvent::TurnStart { turn_id }).await;

            if self.harness.is_graceful_shutdown_requested().await {
                warn!("Graceful shutdown requested, cancelling turn");
                return self.finish_cancelled_turn(turn_id, event_tx, None).await;
            }

            // ── Drift detection: inject ONE reminder after N consecutive auto-turns ──
            // The reminder persists in the conversation as a user message, so we only
            // re-inject when compaction may have evicted it.  Check whether a focus
            // reminder already exists in recent history — if so, skip to avoid filling
            // context with redundant copies.
            let auto_turns = self.consecutive_auto_turns.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            if auto_turns >= DRIFT_DETECTION_THRESHOLD && (auto_turns - DRIFT_DETECTION_THRESHOLD) % DRIFT_DETECTION_INTERVAL == 0 {
                let already_reminded = self.harness.session_messages().await
                    .iter()
                    .rev()
                    .take(8)
                    .any(|m| m.content.iter().any(|b| matches!(b, ContentBlock::Text { text } if text.starts_with("Interrupt: Focus Reminder:"))));
                if !already_reminded {
                    if let Some(anchor) = self.harness.latest_user_message_text().await {
                        info!(
                            auto_turns = auto_turns,
                            "Drift detection: injecting focus reminder"
                        );
                        self.harness.queue_soft_interrupt(
                            format!(
                                "Focus Reminder: Your current task is:\n\"{anchor}\"\n\n\
                                 Are you still working toward this goal? If the task is complete, \
                                 stop and report your findings. If not, what specific step are you on?",
                            ),
                            false, // not urgent
                        ).await;
                    }
                } else {
                    trace!(auto_turns, "Drift detection: focus reminder already present — skipping");
                }
            }

            let summarizer = Self::make_summarizer(
                self.model.clone(),
                self.harness.cfg.compaction.llm_summary_enabled,
            );
            // Pre-send overflow safety net: only compacts if the context is
            // strictly over budget (e.g. a huge accumulated history, or the
            // first turn after restoring a large session). A follow-up that
            // still fits keeps the full evidence from previous turns.
            if let Some((compaction, narratives)) = self
                .harness
                .maybe_compact_messages(summarizer, crate::compaction::CompactionMode::PreSend, turn_id, turn_id)
                .await
            {
                // ── PreCompact hooks: inject context before compaction ──
                {
                    let hm = self.harness.hook_manager.read().await;
                    let session_id = self.harness.session_id().to_string();
                    let working_dir = self.harness.session_working_dir()
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
                                self.harness.push_message(
                                    Message::user(format!("[PreCompact hook context]\n{context}")),
                                ).await;
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
                // Store narrative records for cross-turn/session memory
                let session_id = self.harness.session_id().to_string();
                for rec in &narratives {
                    if let Err(e) = self.harness.memory_manager.core().remember_narrative(rec, &session_id) {
                        warn!(error = %e, "Failed to store narrative record");
                    }
                }
            }

            for interrupt in self.harness.take_pending_interrupts().await {
                info!(
                    content = %truncate(&interrupt.content, 200),
                    urgent = interrupt.urgent,
                    "Injecting soft interrupt"
                );
                self.harness.push_message(
                    Message::user(format!("Interrupt: {}", interrupt.content)),
                ).await;
                let _ = event_tx
                    .send(AgentEvent::SoftInterruptInjected { interrupt })
                    .await;
            }

            self.harness.trigger_memory_for_next_turn().await;
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
            let messages = self.harness.session_messages().await;

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
                        let summarizer = Self::make_summarizer(
                            self.model.clone(),
                            self.harness.cfg.compaction.llm_summary_enabled,
                        );
                        // Reactive recovery after the provider reported a
                        // context-limit error: compact as aggressively as the
                        // proactive path (any trigger) to make the retry fit.
                        if let Some((compaction, _narratives)) = self
                            .harness
                            .maybe_compact_messages(summarizer, crate::compaction::CompactionMode::Proactive, turn_id, turn_id)
                            .await
                        {
                            // ── PreCompact hooks ──
                            {
                                let hm = self.harness.hook_manager.read().await;
                                let session_id = self.harness.session_id().to_string();
                                let working_dir = self.harness.session_working_dir()
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
                                            self.harness.push_message(
                                                Message::user(format!("[PreCompact hook context]\n{context}")),
                                            ).await;
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
            loop {
                // Await the next stream event, emitting a heartbeat every
                // MODEL_WAIT_HEARTBEAT_SECS seconds while waiting so a slow or
                // stalled model does not appear frozen to the UI. The pending
                // stream future is preserved across heartbeats (not cancelled),
                // so this does not change blocking/cancellation semantics.
                let ev = {
                    let wait_start = Instant::now();
                    let next_ev = stream.next();
                    tokio::pin!(next_ev);
                    loop {
                        tokio::select! {
                            biased;
                            ev = &mut next_ev => break ev,
                            _ = tokio::time::sleep(
                                std::time::Duration::from_secs(MODEL_WAIT_HEARTBEAT_SECS)
                            ) => {
                                let elapsed = wait_start.elapsed().as_secs();
                                warn!(elapsed_secs = elapsed, "Still waiting for model response");
                                let _ = event_tx
                                    .send(AgentEvent::WaitingForModel { elapsed_secs: elapsed })
                                    .await;
                            }
                        }
                    }
                };
                let Some(ev) = ev else { break };
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
                    StreamEvent::ToolInputDelta { index, id, name, delta } => {
                        trace!(index, ?name, "ToolInputDelta");
                        let _ = event_tx
                            .send(AgentEvent::ToolInputDelta {
                                index,
                                call_id: id,
                                tool_name: name,
                                delta,
                            })
                            .await;
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
                if maybe_continue_incomplete(&stop_reason, &mut incomplete_continuations, &self.harness).await {
                    info!("Requesting continuation for incomplete response");
                    continue;
                }
                // P0: Check for degenerate (empty) response
                if maybe_continue_degenerate(&final_text, &thinking_text, &mut incomplete_continuations, &self.harness).await {
                    info!("Requesting continuation for degenerate response");
                    continue;
                }

                // Pure text response — save and return.
                self.push_assistant_message(final_text.clone(), thinking_text.clone()).await;
                self.harness.memory_manager.trigger_ingestion_for_turn(
                    self.harness.session_messages().await,
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
                // Proactive convergence: now that the user has received the
                // answer, preemptively compact so the NEXT turn starts small
                // and this latency is hidden. Fires on budget / approaching /
                // turn-count (gap-gated). full_messages stays intact, so the
                // persisted snapshot keeps the complete transcript.
                let summarizer = Self::make_summarizer(
                    self.model.clone(),
                    self.harness.cfg.compaction.llm_summary_enabled,
                );
                if let Some((compaction, narratives)) = self
                    .harness
                    .maybe_compact_messages(summarizer, crate::compaction::CompactionMode::Proactive, turn_id, turn_id)
                    .await
                {
                    // Store narrative records for cross-turn/session memory
                    let session_id = self.harness.session_id().to_string();
                    for rec in &narratives {
                        if let Err(e) = self.harness.memory_manager.core().remember_narrative(rec, &session_id) {
                            warn!(error = %e, "Failed to store narrative record");
                        }
                    }
                    if !narratives.is_empty() {
                        info!(count = narratives.len(), "Stored compaction narrative records");
                    }
                    info!(
                        trigger = ?compaction.trigger,
                        removed = compaction.removed_messages,
                        kept = compaction.kept_messages,
                        "Proactive compaction after turn"
                    );
                    if let Some(ref guard) = self.governance {
                        guard.record_compaction().await;
                    }
                    let _ = event_tx
                        .send(AgentEvent::Compaction { event: compaction })
                        .await;
                }
                self.persist_snapshot("turn_completed").await;
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
            self.harness.push_message(
                Message { role: Role::Assistant, content },
            ).await;

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
                        self.harness.push_message(
                            Message::tool_result(&call_id, reason, true),
                        ).await;
                        continue;
                    }
                    PermissionResult::AskUser { request } => {
                        info!(tool = %name, "Tool requires user permission");
                        self.set_pending_permission(Some(request.clone()));
                        self.set_pending_tool_calls(collected_tool_calls.drain(idx..).collect());
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
                        self.persist_snapshot("awaiting_permission").await;
                        return Ok(outcome);
                    }
                }

                let ctx = ToolContext {
                    session_id: self.harness.session_id().to_string(),
                    message_id: uuid::Uuid::new_v4().to_string(),
                    tool_call_id: call_id.clone(),
                    working_dir: self.harness.session_working_dir().cloned(),
                    execution_mode: ToolExecutionMode::Foreground,
                    graceful_shutdown_requested: self.harness.is_graceful_shutdown_requested().await,
                    progress_tx: None,
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
                        self.harness.push_message(
                            Message::tool_result(&call_id, reason.clone(), true),
                        ).await;
                        continue;
                    }
                    if let Some(mod_input) = modified {
                        effective_input = mod_input;
                    }
                }

                debug!(tool = %name, "Executing tool");
                let output = match tokio::time::timeout(
                    timeout_dur,
                    async {
                        // Start heartbeat for progress UI
                        let hb_call_id = call_id.clone();
                        let hb_name = name.clone();
                        let hb_tx = event_tx.clone();
                        let hb_start = Instant::now();
                        let hb_handle = tokio::spawn(async move {
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                            loop {
                                let _ = hb_tx.send(AgentEvent::ToolExecutionProgress {
                                    call_id: hb_call_id.clone(),
                                    tool_name: hb_name.clone(),
                                    elapsed_secs: hb_start.elapsed().as_secs(),
                                }).await;
                                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                            }
                        });
                        let result = self.harness.execute_tool_with_cache(&name, effective_input, ctx).await;
                        hb_handle.abort();
                        result
                    }.in_current_span(),
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
                        self.harness.push_message(
                            Message::tool_result(&call_id, format!("tool error: {}", err), true),
                        ).await;
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
                            self.harness.push_message(
                                Message::tool_result(&tc2.call_id, format!("skipped: earlier tool '{}' failed", name), true),
                            ).await;
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
                        self.harness.push_message(
                            Message::tool_result(&call_id, format!("tool timed out after {}s", timeout_dur.as_secs()), true),
                        ).await;
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
                            self.harness.push_message(
                                Message::tool_result(&tc2.call_id, format!("skipped: earlier tool '{}' timed out", name), true),
                            ).await;
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
                        self.harness.push_message(
                            Message::tool_result(&call_id, reason.clone(), true),
                        ).await;
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
                let session_messages = self.harness.session_messages().await;
                let (output_text, trunc_info) = guard_tool_output(
                    &self.harness.cfg.compaction,
                    &session_messages,
                    &name,
                    &output.text,
                );
                // Context pressure feedback: inject soft interrupt when context is tight
                if let Some(info) = trunc_info {
                    let pct = info.projected as f64 / info.budget as f64;
                    let (level, urgent) = if pct > 0.9 {
                        ("CRITICAL", true)
                    } else if pct > 0.7 {
                        ("HIGH", false)
                    } else if pct > 0.5 {
                        ("MODERATE", false)
                    } else {
                        ("", false)
                    };
                    if !level.is_empty() {
                        self.harness.queue_soft_interrupt(
                            format!(
                                "[Context {level}: {:.0}% full ({}/{})]\n\
                                 Stop reading large files. Instead:\n\
                                 - Use grep with targeted patterns to find specific code\n\
                                 - Read only the specific lines you need (use offset + limit)\n\
                                 - Summarize what you already know and decide next action",
                                pct * 100.0,
                                info.current + output_text.len(),
                                info.budget,
                            ),
                            urgent,
                        ).await;
                    }
                }
                self.harness.push_message(
                    tool_result_msg(call_id, output_text, output.is_error, elapsed_ms),
                ).await;
            }

            info!("Tool calls processed, continuing turn loop");
        }
    }

    // ── Cancellation ──

    async fn finish_cancelled_turn(
        &self, turn_id: u64, event_tx: &AgentEventTx, partial_text: Option<String>,
    ) -> Result<TurnOutcome, AgentError> {
        warn!("Turn cancelled");
        if let Some(text) = partial_text.filter(|text| !text.is_empty()) {
            self.harness.push_message(Message::assistant(text)).await;
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
        self.persist_snapshot("turn_cancelled").await;
        Ok(outcome)
    }

    // ── Error helpers ──

    async fn emit_error_event(&self, event_tx: &AgentEventTx, error: AgentError) {
        error!(kind = ?error.kind(), message = %error, "Emitting agent error event");
        let _ = event_tx.send(AgentEvent::Error { error }).await;
    }

    async fn push_assistant_message(&self, text: String, thinking: String) {
        let mut content = vec![ContentBlock::Text { text }];
        if !thinking.is_empty() {
            content.push(ContentBlock::Reasoning { text: thinking });
        }
        self.harness.push_message(
            Message { role: Role::Assistant, content },
        ).await;
    }

    /// Record a progress checkpoint on any focused goal.
    ///
    /// Called automatically after each turn completes. If a goal has
    /// `focused: true` and is `Active`, we append a `GoalCheckpoint`
    /// with the current timestamp. The goal's `progress` and `status`
    /// are **not** modified — the Agent (or user via goal tool) is
    /// responsible for explicit progress updates.
    async fn auto_checkpoint_focused_goals(&self) {
        let session_id = self.harness.session_id();
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
        tokio::spawn(
            async move {
                let _ = tx.send(AgentEvent::Error { error: err_event }).await;
                let _ = tx.send(AgentEvent::TurnEnd { turn_id, outcome }).await;
            }
            .in_current_span(),
        );
        error
    }

    /// Build a summarizer closure for compaction that uses the LLM (when
    /// enabled) to produce a semantic summary of dropped messages. Falls back
    /// to mechanical truncation (via returning `None`) when disabled or the
    /// model call fails.
    ///
    /// Takes owned `model` + `enabled` (not `&self`) so the returned closure
    /// does not borrow the agent — letting the caller pass it into
    /// `self.harness.maybe_compact_messages(..)` which needs `&mut self.harness`.
    fn make_summarizer(
        model: Arc<dyn Model>,
        enabled: bool,
    ) -> impl FnOnce(Vec<Message>) -> crate::compaction::SummarizerFuture {
        move |old_messages: Vec<Message>| {
            Box::pin(async move {
                if !enabled {
                    return None;
                }
                let prompt = crate::compaction::build_summarization_prompt(&old_messages);
                let system = "You are a precise conversation summarizer for a coding agent.";
                let messages = vec![Message::user(prompt)];
                let mut stream = match model.complete(&messages, &[], system, "", None).await {
                    Ok(s) => s,
                    Err(e) => {
                        warn!(error = %e, "LLM compaction summary failed; falling back to mechanical");
                        return None;
                    }
                };
                let mut output = String::new();
                while let Some(event) = stream.next().await {
                    match event {
                        Ok(StreamEvent::TextDelta { text }) => output.push_str(&text),
                        Ok(_) => {}
                        Err(e) => {
                            warn!(error = %e, "LLM compaction summary stream error; using partial/mechanical");
                            break;
                        }
                    }
                }
                if output.trim().is_empty() { None } else { Some(output) }
            }) as crate::compaction::SummarizerFuture
        }
    }

    async fn persist_snapshot(&self, trigger: &str) {
        if !self.harness.cfg.auto_snapshot {
            return;
        }
        let mut snapshot = self.snapshot().await;
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
async fn maybe_continue_incomplete(
    stop_reason: &Option<String>,
    attempts: &mut u32,
    harness: &Harness,
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
        harness.push_message(
            Message::user("Please continue."),
        ).await;
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
async fn maybe_continue_degenerate(
    text: &str,
    thinking: &str,
    attempts: &mut u32,
    harness: &Harness,
) -> bool {
    if *attempts >= MAX_INCOMPLETE_CONTINUATION_ATTEMPTS {
        return false;
    }
    let trimmed = text.trim();
    if trimmed.is_empty() {
        *attempts += 1;
        harness.push_message(
            Message::user("Your response was empty. Please try again."),
        ).await;
        return true;
    }
    // Text-only response where the model planned to use tools in thinking
    // but never executed them — BUT only when it did NOT also plan to create
    // a goal/plan/todo. The system prompt instructs "plan first, then tools",
    // so thinking that mixes planning tools + execution tags is legitimate
    // staged workflow, not a degenerate response.
    if thinking_contains_tool_plan(thinking) && !thinking_contains_planning(thinking) {
        *attempts += 1;
        harness.push_message(
            Message::user(
                "Your response did not include any tool calls, but your \
                 thinking shows you planned to inspect the codebase or \
                 execute commands. Please actually issue the tool calls \
                 now — do not speculate about what the code does without \
                 checking it first."
            ),
        ).await;
        return true;
    }
    false
}

/// Check whether the thinking text contains planning tools (goal/plan/todo).
///
/// When the model is in the "plan first, then execute" phase, we should NOT
/// treat the absence of tool calls as degenerate — the system prompt
/// explicitly instructs the model to create goals and plans before using
/// execution tools.
fn thinking_contains_planning(thinking: &str) -> bool {
    let lower = thinking.to_lowercase();
    lower.contains("<goal")
        || lower.contains("<plan")
        || lower.contains("<todo")
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
) -> (String, Option<GuardTruncationInfo>) {
    if !compaction_cfg.enabled {
        return (output_text.to_string(), None);
    }

    let budget = compaction_cfg.token_budget;
    let current = super::compaction::message_chars(messages);
    let output_len = output_text.len();

    let single_max = (budget as f32 * SINGLE_OUTPUT_MAX_FRACTION) as usize;
    let threshold = (budget as f32 * CONTEXT_GUARD_THRESHOLD) as usize;
    let projected = current + output_len;

    let needs_trunc = output_len > single_max || projected > threshold;

    if !needs_trunc {
        return (output_text.to_string(), None);
    }

    // How much room do we have?
    let remaining = if current < threshold {
        threshold.saturating_sub(current)
    } else {
        budget / 50 // ~2% of budget for truncation notice
    };
    let max_chars = remaining.min(single_max);

    if output_text.len() <= max_chars {
        return (output_text.to_string(), None);
    }

    // Smart truncation: keep head (25%) + tail (60%) to preserve both
    // structural context (imports, definitions) and the most relevant
    // content at the end of the file.
    let head_chars = (max_chars as f64 * 0.25) as usize;
    let tail_chars = (max_chars as f64 * 0.60) as usize;

    // Head: first `head_chars` bytes at a safe UTF-8 boundary
    let head = fox_agent_core::truncate_to_bytes(output_text, head_chars);

    // Tail: find the safe boundary for the start of the tail section,
    // then take everything from that boundary to the end.
    let tail_start_raw = output_text.len().saturating_sub(tail_chars);
    // Walk forward from tail_start_raw to the first valid char boundary
    let mut tail_start = tail_start_raw;
    while tail_start < output_text.len() && !output_text.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    let tail = &output_text[tail_start..];

    // Count omitted lines for structured truncation notice
    let head_lines = head.lines().count();
    let total_lines = output_text.lines().count();
    let tail_lines = tail.lines().count();
    let omitted_lines = total_lines.saturating_sub(head_lines + tail_lines);
    let omitted_start = head_lines + 1;
    let omitted_end = omitted_start + omitted_lines.saturating_sub(1);

    let result = if max_chars > 500 {
        format!(
            "{head}\n\n\
             ─── Lines {omitted_start}-{omitted_end} omitted ({omitted_lines} lines) ───\n\
             [Use offset={omitted_start} limit=300 to read this section]\n\n\
             {tail}\n\n\
             [OUTPUT TRUNCATED: {:.0}k → {:.0}k chars | Context {:.0}% full ({}/{})]",
            output_len as f64 / 1000.0,
            (head.len() + tail.len()) as f64 / 1000.0,
            projected as f64 / budget as f64 * 100.0,
            current,
            budget,
        )
    } else {
        format!(
            "[OUTPUT TRUNCATED: {:.0}k chars. Context {:.0}% full ({}/{}). \
             Use more targeted tool queries.]",
            output_len as f64 / 1000.0,
            projected as f64 / budget as f64 * 100.0,
            current,
            budget,
        )
    };

    warn!(
        tool = %tool_name,
        original = output_len,
        truncated = result.len(),
        current = current,
        budget = budget,
        "Context guard truncated tool output"
    );

    let info = GuardTruncationInfo {
        budget,
        current,
        projected,
    };

    (result, Some(info))
}

/// Info about a guard_tool_output truncation, used by the caller to inject
/// context-pressure soft interrupts.
struct GuardTruncationInfo {
    budget: usize,
    current: usize,
    projected: usize,
}

// ── Logging helpers ──

/// Safe string truncation: returns at most `max_bytes` bytes, always on a
/// valid UTF-8 character boundary. Uses `fox_agent_core::truncate_to_bytes`
/// which walks back from `max_bytes` to the nearest boundary.
fn truncate(s: &str, max: usize) -> String {
    let truncated = fox_agent_core::truncate_to_bytes(s, max);
    if truncated.len() == s.len() {
        s.to_string()
    } else {
        format!("{}... ({}/{})", truncated, truncated.len(), s.len())
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
            ContentBlock::Text { text } | ContentBlock::Reasoning { text } => {
                let (short, _) = fox_agent_core::format_truncated(text, 80);
                Some(short)
            }
            ContentBlock::ToolResult { text, .. } => {
                let (short, _) = fox_agent_core::format_truncated(text, 60);
                Some(format!("[result: {}]", short))
            }
            ContentBlock::ToolUse { name, .. } => Some(format!("[tool_call: {name}]")),
            ContentBlock::Image { .. } => Some("[image]".to_string()),
        }).collect::<Vec<_>>().join(" | ");
        format!("[{role}] {preview}")
    }).collect()
}
