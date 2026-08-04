use fox_agent_core::{
    AgentError, AgentEvent, AgentEventTx, AgentStatus, ArtifactProducer, ArtifactRetentionClass,
    ArtifactType, CompactionConfig, ContentBlock, GoalCheckpoint, GoalScope, GoalStatus,
    McpServerKind, McpServerProfile, McpToolDescriptorSnapshot, Message, Model,
    PendingToolCallSnapshot, PermissionDecision, PermissionRequest, PermissionResult,
    ProviderError, Role, SessionSnapshot, Skill, StreamEvent, ToolContext, ToolError,
    ToolExecutionMode, ToolOutput, ToolResultRouting, TurnOutcome, TurnSummary,
    load_goals_with_store, now_secs, save_goals_with_store,
};
use fox_agent_mcp::McpClient;
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tracing::{Instrument, Level, debug, error, info, span, trace, warn};

use crate::harness::Harness;
use crate::turn_summary::{build_turn_summary, enhance_with_llm};

// ── Type aliases ──

/// Callback invoked on every user permission decision.
///
/// Parameters: the original `PermissionRequest`, the user's `PermissionDecision`,
/// and the `turn_id` at the time of the decision.
pub type AuditHandlerFn = Arc<dyn Fn(&PermissionRequest, &PermissionDecision, u64) + Send + Sync>;

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
    /// Server-level MCP profile registry keyed by server name.
    mcp_profiles: HashMap<String, McpServerProfile>,
    /// Descriptor snapshots keyed by sanitised tool name.
    mcp_descriptors: HashMap<String, McpToolDescriptorSnapshot>,
    /// Currently active skill (loaded on-demand by Agent via `skill` tool).
    pub active_skill: Arc<RwLock<Option<Skill>>>,
    /// Sub-agent runtime for isolated task exploration (Phase 3).
    pub subagent_runtime_enabled: bool,
    /// Agent status bar — renders task progress and runtime counters
    /// at the end of the dynamic prompt section.
    pub status: Arc<RwLock<AgentStatus>>,
    /// Optional audit handler — invoked on every user permission decision.
    audit_handler: Option<AuditHandlerFn>,
    /// When enabled, the final turn of a task emits an LLM-enhanced
    /// `AgentEvent::TurnSummary` (accomplishment / changes / caveats /
    /// known_limitations / decisions). Off by default — the deterministic
    /// summary is always emitted regardless.
    final_turn_summary_enabled: std::sync::atomic::AtomicBool,
}

impl Agent {
    pub fn new(
        model: Arc<dyn Model>,
        harness: Harness,
        active_skill: Arc<RwLock<Option<Skill>>>,
    ) -> Self {
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
            mcp_profiles: HashMap::new(),
            mcp_descriptors: HashMap::new(),
            active_skill,
            subagent_runtime_enabled: false,
            status: Arc::new(RwLock::new(AgentStatus::default())),
            audit_handler: None,
            final_turn_summary_enabled: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Enable/disable LLM-enhanced final-turn summaries.
    ///
    /// When enabled, the final turn of a task (`run_once_streaming`) emits an
    /// `AgentEvent::TurnSummary` enriched with `accomplishment`, `changes`,
    /// `caveats`, `known_limitations` and `decisions` (one extra LLM call,
    /// best-effort — falls back to the deterministic fields on failure).
    pub fn with_final_turn_summary(self, enabled: bool) -> Self {
        self.final_turn_summary_enabled
            .store(enabled, std::sync::atomic::Ordering::SeqCst);
        self
    }

    /// Set an optional audit handler that is automatically invoked on every user
    /// permission decision (Allow/Deny). The handler receives the original
    /// `PermissionRequest`, the user's `PermissionDecision`, and the `turn_id`.
    pub fn set_audit_handler(&mut self, handler: AuditHandlerFn) {
        self.audit_handler = Some(handler);
    }

    /// Attach a budget governance guard.
    pub fn set_governance(&mut self, guard: GovernanceGuard) {
        self.governance = Some(guard);
    }

    pub fn set_mcp_runtime_metadata(
        &mut self,
        profiles: HashMap<String, McpServerProfile>,
        descriptors: Vec<McpToolDescriptorSnapshot>,
    ) {
        self.mcp_profiles = profiles;
        self.mcp_descriptors = descriptors
            .into_iter()
            .map(|snapshot| (snapshot.tool_name.clone(), snapshot))
            .collect();
    }

    /// Get the budget governance guard, if attached.
    pub fn governance(&self) -> Option<&GovernanceGuard> {
        self.governance.as_ref()
    }

    pub fn harness(&self) -> &Harness {
        &self.harness
    }
    pub fn model(&self) -> &Arc<dyn Model> {
        &self.model
    }

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
        self.run_turn_streaming(event_tx, false).await
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
            mcp_profiles: HashMap::new(),
            mcp_descriptors: HashMap::new(),
            active_skill: Arc::new(RwLock::new(None)),
            subagent_runtime_enabled: false,
            status: Arc::new(RwLock::new(AgentStatus::default())),
            audit_handler: None,
            final_turn_summary_enabled: std::sync::atomic::AtomicBool::new(false),
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
        self.consecutive_auto_turns
            .store(0, std::sync::atomic::Ordering::SeqCst);
        // Update status bar with current objective
        {
            let mut status = self.status.write().await;
            status.consecutive_auto_turns = 0;
            status.current_objective = user_message.to_string();
        }
        self.harness.push_message(Message::user(user_message)).await;
        self.persist_snapshot("user_message");
        self.run_turn_streaming(event_tx, true).await
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

        // Invoke audit handler with the original request and the user's decision
        if let Some(ref handler) = self.audit_handler
            && let Some(ref request) = self.pending_permission_snapshot()
        {
            let turn_id = self.next_turn_id.load(std::sync::atomic::Ordering::SeqCst);
            handler(request, &decision, turn_id);
        }

        self.execute_single_tool(pending, decision, event_tx)
            .await?;

        self.set_pending_permission(None);

        // Process remaining buffered tool calls from the same model response.
        while !self.pending_tool_calls_is_empty() {
            let Some(next) = self.first_pending_tool_call() else {
                break;
            };
            let name = next.name.clone();

            match self.harness.check_tool_permission(&name, &next.input).await {
                PermissionResult::Allow => {
                    let _ = self.pop_first_pending_tool_call();
                    self.execute_single_tool(next, PermissionDecision::Allow, event_tx)
                        .await?;
                }
                PermissionResult::Deny { reason } => {
                    let _ = self.pop_first_pending_tool_call();
                    info!(tool = %name, reason = %reason, "Remaining tool denied by policy");
                    self.harness
                        .push_message(Message::tool_result(&next.call_id, reason, true))
                        .await;
                }
                PermissionResult::AskUser { request } => {
                    info!(tool = %name, "Remaining tool requires user permission");
                    self.set_pending_permission(Some(request.clone()));
                    return Ok(TurnOutcome::RequiresUserDecision { request });
                }
            }
        }

        self.run_turn_streaming(event_tx, false).await
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
                    graceful_shutdown_requested: self
                        .harness
                        .is_graceful_shutdown_requested()
                        .await,
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
                                message: "tool execution aborted: concurrency semaphore closed"
                                    .into(),
                            });
                        }
                    }
                } else {
                    None
                };

                // Timeout enforcement
                let timeout_dur = self
                    .governance
                    .as_ref()
                    .map(|g| std::time::Duration::from_secs(g.budget().tool_timeout_secs))
                    .unwrap_or(std::time::Duration::from_secs(60));

                // ── PreToolUse hooks ──
                let mut effective_input = pending.input.clone();
                {
                    let (allowed, block_reason, modified) = self
                        .harness
                        .run_pre_tool_hooks(&pending.name, &effective_input)
                        .await;
                    if !allowed {
                        let reason = block_reason.unwrap_or_else(|| "hook blocked".into());
                        info!(tool = %pending.name, reason = %reason, "Tool blocked by PreToolUse hook");
                        self.harness
                            .push_message(Message::tool_result(
                                &pending.call_id,
                                reason.clone(),
                                true,
                            ))
                            .await;
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
                                let _ = hb_tx
                                    .send(AgentEvent::ToolExecutionProgress {
                                        call_id: hb_call_id.clone(),
                                        tool_name: hb_name.clone(),
                                        elapsed_secs: hb_start.elapsed().as_secs(),
                                    })
                                    .await;
                                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                            }
                        });
                        let result = self
                            .harness
                            .execute_tool_with_cache(&pending.name, effective_input, ctx)
                            .await;
                        hb_handle.abort();
                        result
                    }
                    .in_current_span(),
                )
                .await
                {
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
                        self.harness
                            .push_message(Message::tool_result(
                                &pending.call_id,
                                format!("tool error: {}", err),
                                true,
                            ))
                            .await;
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
                        let timeout_err = ToolError::Timeout {
                            timeout_secs: timeout_dur.as_secs(),
                        };
                        if let Some(ref guard) = self.governance {
                            guard.record_tool_error().await;
                        }
                        // Push timeout tool result so conversation history stays valid.
                        self.harness
                            .push_message(Message::tool_result(
                                &pending.call_id,
                                format!("tool timed out after {}s", timeout_dur.as_secs()),
                                true,
                            ))
                            .await;
                        let _ = event_tx
                            .send(AgentEvent::ToolCallEnd {
                                call_id: pending.call_id.clone(),
                                output: ToolOutput {
                                    text: format!(
                                        "tool timed out after {}s",
                                        timeout_dur.as_secs()
                                    ),
                                    is_error: true,
                                    json: None,
                                },
                            })
                            .await;
                        self.emit_error_event(event_tx, AgentError::Tool(timeout_err.clone()))
                            .await;
                        return Ok(());
                    }
                };
                let elapsed_ms = start.elapsed().as_millis() as u64;

                // ── PostToolUse hooks ──
                {
                    let (allowed, block_reason) = self
                        .harness
                        .run_post_tool_hooks(&pending.name, &output.text)
                        .await;
                    if !allowed {
                        let reason = block_reason.unwrap_or_else(|| "hook blocked".into());
                        info!(tool = %pending.name, reason = %reason, "Tool result blocked by PostToolUse hook");
                        self.harness
                            .push_message(Message::tool_result(
                                &pending.call_id,
                                reason.clone(),
                                true,
                            ))
                            .await;
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
                self.harness
                    .push_message(tool_result_msg(
                        pending.call_id,
                        output.text,
                        output.is_error,
                        elapsed_ms,
                    ))
                    .await;
            }
            PermissionDecision::Deny { reason } => {
                info!(reason = %reason, "Permission denied");
                self.harness
                    .push_message(Message::tool_result(pending.call_id, reason, true))
                    .await;
            }
        }
        Ok(())
    }

    // ── Core turn loop (P0: retry, continuation, filtering) ──

    async fn run_turn_streaming(
        &self,
        event_tx: &AgentEventTx,
        final_turn: bool,
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

            // P1: Tool loop upper limit (configurable via max_turns in BudgetConfig)
            let effective_max = self
                .governance
                .as_ref()
                .and_then(|g| {
                    let max = g.budget().max_turns;
                    if max > 0 { Some(max as u32) } else { None }
                })
                .unwrap_or(MAX_TOOL_LOOP_ITERATIONS)
                .min(MAX_TOOL_LOOP_ITERATIONS);
            if tool_loop_iterations > effective_max {
                warn!(
                    iterations = tool_loop_iterations,
                    limit = effective_max,
                    "Tool loop iteration limit reached"
                );
                return Err(self.handle_error(
                    event_tx,
                    turn_id,
                    AgentError::BudgetExceeded {
                        message: format!("Exceeded maximum tool loop iterations ({effective_max})"),
                    },
                ));
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

            // ── Drift detection: increment auto-turn counter for status bar ──
            // The status bar (rendered at the end of the dynamic prompt) will
            // display a ⚠️ WARNING when consecutive_auto_turns approaches the limit.
            // No soft interrupt messages are injected — the status bar provides
            // continuous awareness without polluting the message history.
            self.consecutive_auto_turns
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

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
                .maybe_compact_messages(
                    summarizer,
                    crate::compaction::CompactionMode::PreSend,
                    turn_id,
                    turn_id,
                )
                .await
            {
                // ── PreCompact hooks: inject context before compaction ──
                {
                    let hm = self.harness.hook_manager.read().await;
                    let session_id = self.harness.session_id().to_string();
                    let working_dir = self
                        .harness
                        .session_working_dir()
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
                    if let Ok(crate::hooks::HookDecision::InjectContext { context }) =
                        hm.execute(crate::hooks::HookEvent::PreCompact, ctx).await
                        && !context.is_empty()
                    {
                        info!(chars = context.len(), "PreCompact hook injected context");
                        self.harness
                            .push_message(Message::user(format!(
                                "[PreCompact hook context]\n{context}"
                            )))
                            .await;
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
                self.status.write().await.record_compaction();
                let _ = event_tx
                    .send(AgentEvent::Compaction { event: compaction })
                    .await;
                // Store narrative records for cross-turn/session memory
                let session_id = self.harness.session_id().to_string();
                for rec in &narratives {
                    if let Err(e) = self
                        .harness
                        .memory_manager
                        .core()
                        .remember_narrative(rec, &session_id)
                    {
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
                self.harness
                    .push_message(Message::user(format!("Interrupt: {}", interrupt.content)))
                    .await;
                let _ = event_tx
                    .send(AgentEvent::SoftInterruptInjected { interrupt })
                    .await;
            }

            self.harness.trigger_memory_for_next_turn().await;
            let memory_injection = self.harness.take_memory_injection_for_prompt().await;
            let memory_prompt: Option<String> =
                memory_injection.as_ref().map(|(inj, _)| inj.prompt.clone());
            if let Some((inj, memory_state_event)) = memory_injection {
                debug!(
                    count = inj.count,
                    chars = inj.prompt.len(),
                    "Memory injected into prompt"
                );
                let _ = event_tx
                    .send(AgentEvent::MemoryStateChanged {
                        event: memory_state_event,
                    })
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

            let status_bar = if self.harness.cfg.context.status_bar_enabled {
                self.status.read().await.render()
            } else {
                None
            };
            // Apply auto_turn_limit from config
            {
                let mut s = self.status.write().await;
                s.auto_turn_limit = self.harness.cfg.context.status_bar_warn_auto_turns;
            }

            let (split, _context_info) = self
                .harness
                .build_system_prompt_split(
                    memory_prompt.as_deref(),
                    active_skill_prompt.as_deref(),
                    status_bar.as_deref(),
                )
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

            let stream = match self
                .model
                .complete(
                    &messages,
                    &tools,
                    &split.static_part,
                    &split.dynamic_part,
                    self.model.runtime_state().resume_session_id.as_deref(),
                )
                .await
            {
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
                            .maybe_compact_messages(
                                summarizer,
                                crate::compaction::CompactionMode::Proactive,
                                turn_id,
                                turn_id,
                            )
                            .await
                        {
                            // ── PreCompact hooks ──
                            {
                                let hm = self.harness.hook_manager.read().await;
                                let session_id = self.harness.session_id().to_string();
                                let working_dir = self
                                    .harness
                                    .session_working_dir()
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
                                if let Ok(crate::hooks::HookDecision::InjectContext { context }) =
                                    hm.execute(crate::hooks::HookEvent::PreCompact, ctx).await
                                    && !context.is_empty()
                                {
                                    self.harness
                                        .push_message(Message::user(format!(
                                            "[PreCompact hook context]\n{context}"
                                        )))
                                        .await;
                                }
                            }

                            if let Some(ref guard) = self.governance {
                                guard.record_compaction().await;
                            }
                            self.status.write().await.record_compaction();
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
                        return Err(self.handle_error(
                            event_tx,
                            turn_id,
                            AgentError::Provider(err),
                        ));
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
                        tokio::time::sleep(std::time::Duration::from_secs(
                            SLOW_RETRY_INTERVAL_SECS,
                        ))
                        .await;
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
                    return self
                        .finish_cancelled_turn(turn_id, event_tx, Some(final_text.clone()))
                        .await;
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
                        let _ = event_tx
                            .send(AgentEvent::ModelUsage {
                                usage: usage.clone(),
                            })
                            .await;

                        // Governance: record usage & check budget
                        if let Some(ref guard) = self.governance {
                            let cost = crate::governance::estimate_cost_cents(
                                &self.model.model_id(),
                                &usage,
                            );
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
                        collected_tool_calls.push(PendingToolCall {
                            call_id: id,
                            name,
                            input,
                        });
                    }
                    StreamEvent::MessageStop {
                        stop_reason: reason,
                    } => {
                        stop_reason = reason;
                        debug!(?stop_reason, "Model response complete");
                        let _ = event_tx
                            .send(AgentEvent::ModelMessageEnd {
                                message_id: model_message_id.clone(),
                            })
                            .await;
                        break;
                    }
                    StreamEvent::ToolInputDelta {
                        index,
                        id,
                        name,
                        delta,
                    } => {
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
                    StreamEvent::Compaction {
                        trigger,
                        pre_tokens,
                    } => {
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
                    let query = tc
                        .input
                        .get("query")
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
                    let dup_names: Vec<&str> =
                        fingerprints.iter().map(|(n, _)| n.as_str()).collect();
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
                            format!(
                                "重复工具调用警告: 工具名称={:?}, 重复次数={}",
                                dup_names, dup_count
                            ),
                            false,
                        );
                }
            }

            prev_tool_fingerprints = fingerprints;

            if collected_tool_calls.is_empty() {
                // P0: Check for incomplete continuation
                if maybe_continue_incomplete(
                    &stop_reason,
                    &mut incomplete_continuations,
                    &self.harness,
                )
                .await
                {
                    info!("Requesting continuation for incomplete response");
                    continue;
                }
                // P0: Check for degenerate (empty) response
                if maybe_continue_degenerate(
                    &final_text,
                    &thinking_text,
                    &mut incomplete_continuations,
                    &self.harness,
                )
                .await
                {
                    info!("Requesting continuation for degenerate response");
                    continue;
                }

                // Pure text response — save and return.
                self.push_assistant_message(final_text.clone(), thinking_text.clone())
                    .await;
                self.harness.memory_manager.trigger_ingestion_for_turn(
                    self.harness.session_messages().await,
                    self.model.clone(),
                    event_tx.clone(),
                );
                // Governance: record turn completion (enforces max_turns)
                if let Some(ref guard) = self.governance
                    && let Err(msg) = guard.turn_end().await
                {
                    return Err(AgentError::BudgetExceeded { message: msg });
                }
                // Auto-checkpoint: record progress on focused goals
                self.auto_checkpoint_focused_goals().await;
                // Update status bar: increment turn + sync drift counter
                {
                    let mut status = self.status.write().await;
                    status.turn = status.turn.saturating_add(1);
                    status.consecutive_auto_turns = self
                        .consecutive_auto_turns
                        .load(std::sync::atomic::Ordering::SeqCst);
                }
                info!(
                    final_chars = final_text.len(),
                    thinking_chars = thinking_text.len(),
                    "Turn completed"
                );
                let outcome = TurnOutcome::Completed { text: final_text };
                let semantic = final_turn
                    && self
                        .final_turn_summary_enabled
                        .load(std::sync::atomic::Ordering::SeqCst);
                self.emit_turn_summary(event_tx, turn_id, true, semantic)
                    .await;
                let _ = event_tx
                    .send(AgentEvent::TurnEnd {
                        turn_id,
                        outcome: outcome.clone(),
                    })
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
                    .maybe_compact_messages(
                        summarizer,
                        crate::compaction::CompactionMode::Proactive,
                        turn_id,
                        turn_id,
                    )
                    .await
                {
                    // Store narrative records for cross-turn/session memory
                    let session_id = self.harness.session_id().to_string();
                    for rec in &narratives {
                        if let Err(e) = self
                            .harness
                            .memory_manager
                            .core()
                            .remember_narrative(rec, &session_id)
                        {
                            warn!(error = %e, "Failed to store narrative record");
                        }
                    }
                    if !narratives.is_empty() {
                        info!(
                            count = narratives.len(),
                            "Stored compaction narrative records"
                        );
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
                    self.status.write().await.record_compaction();
                    let _ = event_tx
                        .send(AgentEvent::Compaction { event: compaction })
                        .await;
                }
                self.persist_snapshot("turn_completed");
                return Ok(outcome);
            }

            // Save the assistant message with ALL tool calls.
            let mut content: Vec<ContentBlock> = Vec::new();
            if !final_text.is_empty() {
                content.push(ContentBlock::Text { text: final_text });
            }
            if !thinking_text.is_empty() {
                content.push(ContentBlock::Reasoning {
                    text: thinking_text,
                });
            }
            for tc in &collected_tool_calls {
                content.push(ContentBlock::ToolUse {
                    id: tc.call_id.clone(),
                    name: tc.name.clone(),
                    input: tc.input.clone(),
                });
            }
            self.harness
                .push_message(Message {
                    role: Role::Assistant,
                    content,
                })
                .await;

            // Execute each tool call.
            let total = collected_tool_calls.len();
            for idx in 0..total {
                let tc = &collected_tool_calls[idx];
                let name = tc.name.clone();
                let call_id = tc.call_id.clone();
                let input = tc.input.clone();

                let permission_profile = parse_mcp_tool_name(&name)
                    .and_then(|(server, _)| self.mcp_profiles.get(server));
                let permission_descriptor = self.mcp_descriptors.get(&name);
                match self
                    .harness
                    .check_tool_permission_with_mcp_metadata(
                        &name,
                        &input,
                        permission_profile,
                        permission_descriptor,
                    )
                    .await
                {
                    PermissionResult::Allow => {
                        debug!(tool = %name, "Tool auto-allowed");
                    }
                    PermissionResult::Deny { reason } => {
                        info!(tool = %name, reason = %reason, "Tool denied by policy");
                        self.harness
                            .push_message(Message::tool_result(&call_id, reason.clone(), true))
                            .await;
                        // Keep the event stream balanced: ToolCallStart was
                        // already emitted while streaming, so emit the matching
                        // end for consumers (TUI, eval behavior rules).
                        let _ = event_tx
                            .send(AgentEvent::ToolCallEnd {
                                call_id: call_id.clone(),
                                output: ToolOutput {
                                    text: format!("tool denied: {}", reason),
                                    is_error: true,
                                    json: None,
                                },
                            })
                            .await;
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
                        self.emit_turn_summary(event_tx, turn_id, false, false)
                            .await;
                        let _ = event_tx
                            .send(AgentEvent::TurnEnd {
                                turn_id,
                                outcome: outcome.clone(),
                            })
                            .await;
                        info!("Turn paused awaiting user decision");
                        self.persist_snapshot("awaiting_permission");
                        return Ok(outcome);
                    }
                }

                let ctx = ToolContext {
                    session_id: self.harness.session_id().to_string(),
                    message_id: uuid::Uuid::new_v4().to_string(),
                    tool_call_id: call_id.clone(),
                    working_dir: self.harness.session_working_dir().cloned(),
                    execution_mode: ToolExecutionMode::Foreground,
                    graceful_shutdown_requested: self
                        .harness
                        .is_graceful_shutdown_requested()
                        .await,
                    progress_tx: None,
                };

                if ctx.graceful_shutdown_requested {
                    return self
                        .finish_cancelled_turn(turn_id, event_tx, Some(String::new()))
                        .await;
                }

                // Concurrency + timeout enforcement
                let start = Instant::now();

                let _permit = if let Some(ref guard) = self.governance {
                    let slots = guard.tool_slots();
                    match slots.acquire_owned().await {
                        Ok(permit) => Some(permit),
                        Err(_) => {
                            error!(tool = %name, "Tool concurrency semaphore closed unexpectedly");
                            return Err(self.handle_error(
                                event_tx,
                                turn_id,
                                AgentError::Internal {
                                    message: "tool execution aborted: concurrency semaphore closed"
                                        .into(),
                                },
                            ));
                        }
                    }
                } else {
                    None
                };

                let timeout_dur = self
                    .governance
                    .as_ref()
                    .map(|g| std::time::Duration::from_secs(g.budget().tool_timeout_secs))
                    .unwrap_or(std::time::Duration::from_secs(60));

                // ── PreToolUse hooks ──
                let mut effective_input = input.clone();
                {
                    let (allowed, block_reason, modified) = self
                        .harness
                        .run_pre_tool_hooks(&name, &effective_input)
                        .await;
                    if !allowed {
                        let reason = block_reason.unwrap_or_else(|| "hook blocked".into());
                        info!(tool = %name, reason = %reason, "Tool blocked by PreToolUse hook");
                        self.harness
                            .push_message(Message::tool_result(&call_id, reason.clone(), true))
                            .await;
                        // Keep the event stream balanced (see Deny path above).
                        let _ = event_tx
                            .send(AgentEvent::ToolCallEnd {
                                call_id: call_id.clone(),
                                output: ToolOutput {
                                    text: format!("tool blocked: {}", reason),
                                    is_error: true,
                                    json: None,
                                },
                            })
                            .await;
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
                                let _ = hb_tx
                                    .send(AgentEvent::ToolExecutionProgress {
                                        call_id: hb_call_id.clone(),
                                        tool_name: hb_name.clone(),
                                        elapsed_secs: hb_start.elapsed().as_secs(),
                                    })
                                    .await;
                                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                            }
                        });
                        let result = self
                            .harness
                            .execute_tool_with_cache(&name, effective_input, ctx)
                            .await;
                        hb_handle.abort();
                        result
                    }
                    .in_current_span(),
                )
                .await
                {
                    Ok(Ok(output)) => {
                        if let Some(ref guard) = self.governance {
                            guard.record_tool_success().await;
                        }
                        // Record MCP call metrics
                        if name.starts_with("mcp__") {
                            self.harness.governance_metrics.record_mcp_call();
                        }
                        // Update status bar tool counter
                        self.status.write().await.record_tool_call();
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
                        self.harness
                            .push_message(Message::tool_result(
                                &call_id,
                                format!("tool error: {}", err),
                                true,
                            ))
                            .await;
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
                        for tc2 in collected_tool_calls[(idx + 1)..total].iter() {
                            self.harness
                                .push_message(Message::tool_result(
                                    &tc2.call_id,
                                    format!("skipped: earlier tool '{}' failed", name),
                                    true,
                                ))
                                .await;
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
                        self.harness
                            .push_message(Message::tool_result(
                                &call_id,
                                format!("tool timed out after {}s", timeout_dur.as_secs()),
                                true,
                            ))
                            .await;
                        let _ = event_tx
                            .send(AgentEvent::ToolCallEnd {
                                call_id: call_id.clone(),
                                output: ToolOutput {
                                    text: format!(
                                        "tool timed out after {}s",
                                        timeout_dur.as_secs()
                                    ),
                                    is_error: true,
                                    json: None,
                                },
                            })
                            .await;
                        // Push error results for remaining tools in the batch.
                        for tc2 in collected_tool_calls[(idx + 1)..total].iter() {
                            self.harness
                                .push_message(Message::tool_result(
                                    &tc2.call_id,
                                    format!("skipped: earlier tool '{}' timed out", name),
                                    true,
                                ))
                                .await;
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
                    let (allowed, block_reason) =
                        self.harness.run_post_tool_hooks(&name, &output.text).await;
                    if !allowed {
                        let reason = block_reason.unwrap_or_else(|| "hook blocked".into());
                        info!(tool = %name, reason = %reason, "Tool result blocked by PostToolUse hook");
                        self.harness
                            .push_message(Message::tool_result(&call_id, reason.clone(), true))
                            .await;
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

                // P2: context guard — truncate huge tool outputs before they
                // enter the message stream (avoids wasting tokens and memory on
                // 10 MB file reads / grep explosions).  Mirrors jcode's
                // `CONTEXT_GUARD_THRESHOLD` / `SINGLE_OUTPUT_MAX_FRACTION`.
                let session_messages = self.harness.session_messages().await;
                let raw_output_text = output.text.clone();
                let (mut output_text, is_truncated) = guard_tool_output(
                    &self.harness.cfg.compaction,
                    &session_messages,
                    &name,
                    &raw_output_text,
                );

                let externalize_decision = should_externalize_tool_result(
                    &self.harness.cfg.artifact_store,
                    &self.mcp_profiles,
                    &self.mcp_descriptors,
                    &name,
                    &raw_output_text,
                    is_truncated,
                );

                // Phase 4: unified routing decision + metrics
                let context_pressure = self.harness.context_pressure().await;
                let routing_input = crate::routing::RoutingInput {
                    tool_name: &name,
                    raw_output_text: &raw_output_text,
                    truncated_by_context_guard: is_truncated,
                    context_pressure,
                    mcp_profile: parse_mcp_tool_name(&name)
                        .map(|(server, _)| server)
                        .and_then(|s| self.mcp_profiles.get(s)),
                    mcp_descriptor: self.mcp_descriptors.get(&name),
                    consecutive_exploration_turns: if name == "grep"
                        || name == "glob"
                        || name == "read"
                    {
                        1
                    } else {
                        0
                    },
                };
                let routing_decision = self
                    .harness
                    .routing_engine
                    .decide(&routing_input, &self.harness.cfg.artifact_store);
                self.harness
                    .governance_metrics
                    .record_routing(routing_decision);

                let _ = event_tx
                    .send(AgentEvent::RoutingDecision {
                        tool_name: name.clone(),
                        call_id: call_id.clone(),
                        routing: routing_decision,
                        context_pressure,
                        output_size: raw_output_text.len(),
                        reason: externalize_decision.reason.map(|s| s.to_string()),
                    })
                    .await;

                if self.harness.cfg.artifact_store.enabled
                    && (externalize_decision.should_externalize
                        || routing_decision == ToolResultRouting::Externalize)
                {
                    let raw_bytes = raw_output_text.len() as u64;
                    if raw_bytes <= self.harness.cfg.artifact_store.max_artifact_bytes {
                        let producer = artifact_producer_from_tool_name(&name);
                        let artifact_type = artifact_type_from_tool_name(&self.mcp_profiles, &name);
                        let storage_policy = artifact_storage_policy(
                            &self.harness.cfg.artifact_store,
                            &self.mcp_profiles,
                            &self.mcp_descriptors,
                            &name,
                        );
                        let artifact_type_label = format!("{:?}", artifact_type);
                        let retention_class_label = format!("{:?}", storage_policy.class);
                        let metadata = serde_json::json!({
                            "tool_name": name,
                            "tool_call_id": call_id.clone(),
                            "raw_bytes": raw_bytes,
                            "output_chars": raw_output_text.chars().count(),
                            "elapsed_ms": elapsed_ms,
                            "artifact_type": artifact_type_label,
                            "retention_class": retention_class_label,
                            "server_name": storage_policy.server_name,
                            "server_kind": storage_policy.server_kind,
                            "transport": storage_policy.transport,
                            "original_tool_name": storage_policy.original_tool_name,
                            "ttl_hours_override": storage_policy.ttl_hours_override,
                            "externalized_reason": externalize_decision.reason,
                        });
                        match self
                            .harness
                            .artifact_store
                            .put_text(
                                self.harness.session_id(),
                                producer,
                                artifact_type,
                                storage_policy.class,
                                raw_output_text.clone(),
                                metadata,
                            )
                            .await
                        {
                            Ok(put_result) => {
                                let record = put_result.record;
                                // Phase 4: record artifact write metrics
                                self.harness
                                    .governance_metrics
                                    .record_artifact_write(raw_bytes);
                                output_text = format!(
                                    "[OUTPUT EXTERNALIZED: artifact_id={} | raw_bytes={}]\n{}",
                                    record.artifact_id, raw_bytes, output_text,
                                );
                                let _ = event_tx
                                    .send(AgentEvent::ArtifactStored {
                                        artifact_id: record.artifact_id.clone(),
                                        tool_name: name.clone(),
                                        call_id: call_id.clone(),
                                        size_bytes: raw_bytes,
                                        artifact_type: record
                                            .metadata
                                            .get("artifact_type")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("ToolOutput")
                                            .to_string(),
                                        retention_class: record
                                            .metadata
                                            .get("retention_class")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("Ephemeral")
                                            .to_string(),
                                        server_name: storage_policy.server_name.map(str::to_string),
                                        server_kind: storage_policy.server_kind.map(str::to_string),
                                        transport: storage_policy.transport.map(str::to_string),
                                        original_tool_name: storage_policy
                                            .original_tool_name
                                            .map(str::to_string),
                                        externalized_reason: externalize_decision
                                            .reason
                                            .map(str::to_string),
                                    })
                                    .await;
                                if let Some(gc) = put_result.gc_report
                                    && (gc.deleted > 0 || gc.bytes_freed > 0)
                                {
                                    let _ = event_tx
                                        .send(AgentEvent::ArtifactGc {
                                            scope: format!("session:{}", self.harness.session_id()),
                                            deleted: gc.deleted,
                                            kept: gc.kept,
                                            bytes_freed: gc.bytes_freed,
                                            session_quota_evictions: gc.session_quota_evictions,
                                            store_quota_evictions: gc.store_quota_evictions,
                                        })
                                        .await;
                                }
                            }
                            Err(e) => {
                                if self.harness.cfg.artifact_store.allow_summary_only_fallback {
                                    output_text =
                                        format!("[OUTPUT NOT STORED: {}]\n{}", e, output_text,);
                                } else {
                                    return Err(self.handle_error(
                                        event_tx,
                                        turn_id,
                                        AgentError::Internal {
                                            message: format!("artifact store failed: {e}"),
                                        },
                                    ));
                                }
                            }
                        }
                    } else if self.harness.cfg.artifact_store.allow_summary_only_fallback {
                        output_text = format!(
                            "[OUTPUT TOO LARGE TO STORE: raw_bytes={} > max_artifact_bytes={}]\n{}",
                            raw_bytes,
                            self.harness.cfg.artifact_store.max_artifact_bytes,
                            output_text,
                        );
                    }
                }

                let output = ToolOutput {
                    text: output_text.clone(),
                    is_error: output.is_error,
                    json: output.json.clone(),
                };

                if name == "artifact_read"
                    && !output.is_error
                    && let Some(details) = artifact_read_event_details(&output)
                {
                    self.harness.governance_metrics.record_artifact_read();
                    let _ = event_tx
                        .send(AgentEvent::ArtifactRead {
                            artifact_id: details.artifact_id,
                            tool_name: name.clone(),
                            returned_chars: details.returned_chars,
                            offset_chars: details.offset_chars,
                            limit_chars: details.limit_chars,
                            source_tool_name: details.source_tool_name,
                            artifact_type: details.artifact_type,
                            server_name: details.server_name,
                            server_kind: details.server_kind,
                            transport: details.transport,
                            original_tool_name: details.original_tool_name,
                        })
                        .await;
                }

                let _ = event_tx
                    .send(AgentEvent::ToolCallEnd {
                        call_id: call_id.clone(),
                        output: output.clone(),
                    })
                    .await;
                self.harness
                    .push_message(tool_result_msg(
                        call_id,
                        output_text,
                        output.is_error,
                        elapsed_ms,
                    ))
                    .await;
            }

            info!("Tool calls processed, continuing turn loop");
        }
    }

    // ── Cancellation ──

    async fn finish_cancelled_turn(
        &self,
        turn_id: u64,
        event_tx: &AgentEventTx,
        partial_text: Option<String>,
    ) -> Result<TurnOutcome, AgentError> {
        warn!("Turn cancelled");
        if let Some(text) = partial_text.filter(|text| !text.is_empty()) {
            self.harness.push_message(Message::assistant(text)).await;
        }
        let _ = event_tx
            .send(AgentEvent::Error {
                error: AgentError::Internal {
                    message: "graceful shutdown requested".to_string(),
                },
            })
            .await;
        let outcome = TurnOutcome::Cancelled;
        self.emit_turn_summary(event_tx, turn_id, false, false)
            .await;
        let _ = event_tx
            .send(AgentEvent::TurnEnd {
                turn_id,
                outcome: outcome.clone(),
            })
            .await;
        self.persist_snapshot("turn_cancelled");
        Ok(outcome)
    }

    // ── Error helpers ──

    async fn emit_error_event(&self, event_tx: &AgentEventTx, error: AgentError) {
        error!(kind = ?error.kind(), message = %error, "Emitting agent error event");
        let _ = event_tx.send(AgentEvent::Error { error }).await;
    }

    /// Deterministically summarize the current turn and emit it as
    /// `AgentEvent::TurnSummary` immediately before the matching `TurnEnd`.
    /// Best-effort: the application layer (e.g. fox-code) renders this instead
    /// of a raw tool-call histogram. When `semantic` is set, the summary is
    /// additionally enriched by the LLM (accomplishment / changes / caveats /
    /// known_limitations / decisions); on failure the deterministic fields are
    /// kept.
    async fn emit_turn_summary(
        &self,
        event_tx: &AgentEventTx,
        turn_id: u64,
        completed: bool,
        semantic: bool,
    ) {
        let messages = self.harness.session_messages().await;
        let mut summary = build_turn_summary(turn_id, &messages, completed);
        if semantic && let Err(e) = enhance_with_llm(&mut summary, &messages, &self.model).await {
            warn!(error = %e, "final turn summary LLM enhancement failed; using deterministic fields");
        }
        let _ = event_tx.send(AgentEvent::TurnSummary { summary }).await;
    }

    async fn push_assistant_message(&self, text: String, thinking: String) {
        let mut content = vec![ContentBlock::Text { text }];
        if !thinking.is_empty() {
            content.push(ContentBlock::Reasoning { text: thinking });
        }
        self.harness
            .push_message(Message {
                role: Role::Assistant,
                content,
            })
            .await;
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
            let has_focused = goals
                .iter()
                .any(|g| g.focused && g.status == GoalStatus::Active);
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
                        goal.checkpoints = goal
                            .checkpoints
                            .split_at(goal.checkpoints.len() - 16)
                            .1
                            .to_vec();
                    }
                }
            }
            let scope_str = match &scope {
                GoalScope::Session => "session",
                GoalScope::Global => "global",
            };
            let _ = save_goals_with_store(
                store.as_ref(),
                session_id,
                scope,
                goals,
                false,
                Some(scope_str),
            );
        }
    }

    fn handle_error(&self, event_tx: &AgentEventTx, turn_id: u64, error: AgentError) -> AgentError {
        error!(turn = turn_id, kind = ?error.kind(), message = %error, "Turn loop error");
        let tx = event_tx.clone();
        let err_event = error.clone();
        let outcome = TurnOutcome::Failed {
            error: error.clone(),
        };
        tokio::spawn(
            async move {
                // Error turns carry minimal summary data (no completed transcript);
                // the `Error` event above already carries the failure details.
                let _ = tx
                    .send(AgentEvent::TurnSummary {
                        summary: TurnSummary::empty(turn_id),
                    })
                    .await;
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
                if output.trim().is_empty() {
                    None
                } else {
                    Some(output)
                }
            }) as crate::compaction::SummarizerFuture
        }
    }

    /// Persist a session snapshot to disk asynchronously.
    ///
    /// The heavy work (JSON serialisation + file I/O) is spawned onto a
    /// background task so it never blocks the main Agent Loop.
    fn persist_snapshot(&self, trigger: &str) {
        if !self.harness.cfg.auto_snapshot {
            return;
        }
        let trigger = trigger.to_string();
        let store = Arc::clone(&self.harness.session_store);
        let harness = self.harness.clone();
        let model = self.model.clone();
        let pending_permission = self.pending_permission_snapshot();
        let pending_tool_calls: Vec<PendingToolCallSnapshot> = self
            .pending_tool_calls_snapshot()
            .iter()
            .map(PendingToolCallSnapshot::from)
            .collect();
        let next_turn_id = self.peek_turn_id();

        tokio::spawn(async move {
            let ss = harness.session_state_read().await;
            let snapshot = SessionSnapshot {
                session_id: ss.id.clone(),
                parent_id: ss.parent_id.clone(),
                title: ss.title.clone(),
                model: ss.model.clone().or_else(|| Some(model.model_id())),
                provider_key: ss.provider_key.clone(),
                status: ss.status,
                working_dir: ss.working_dir.clone(),
                messages: ss.messages.clone(),
                full_messages: ss.full_messages.clone(),
                env_snapshots: ss.env_snapshots.clone(),
                model_runtime_state: model.runtime_state(),
                pending_permission,
                pending_tool_calls,
                interrupt_state: harness
                    .interrupt_manager
                    .try_read()
                    .map(|guard| guard.snapshot())
                    .unwrap_or_default(),
                next_turn_id,
                metadata: Some(serde_json::json!({ "trigger": trigger })),
                updated_at: now_secs(),
                created_at: ss.created_at,
            };
            if let Err(err) = store.save_session(&snapshot) {
                warn!(session_id = %snapshot.session_id, trigger = %snapshot.metadata.as_ref().and_then(|v| v["trigger"].as_str()).unwrap_or(""), error = %err, "failed to persist session snapshot");
            }
        });
    }
}

// ── P0 helpers ──

/// Detect context-limit errors from provider error messages.
fn detect_context_limit(error: &str) -> bool {
    CTRL_LIMIT_KEYWORDS.iter().any(|kw| error.contains(kw))
}

/// Filter out tool calls that were truncated mid-generation (null/empty input).
fn filter_truncated_tool_calls(stop_reason: &Option<String>, calls: &mut Vec<PendingToolCall>) {
    let should_filter = matches!(
        stop_reason.as_deref(),
        Some("max_tokens" | "length" | "tool_use")
    );
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
    let should_continue = matches!(stop_reason.as_deref(), Some("max_tokens" | "length"));
    if should_continue {
        *attempts += 1;
        harness
            .push_message(Message::user("Please continue."))
            .await;
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
        harness
            .push_message(Message::user("Your response was empty. Please try again."))
            .await;
        return true;
    }
    // Text-only response where the model planned to use tools in thinking
    // but never executed them — BUT only when it did NOT also plan to create
    // a goal/plan/todo. The system prompt instructs "plan first, then tools",
    // so thinking that mixes planning tools + execution tags is legitimate
    // staged workflow, not a degenerate response.
    if thinking_contains_tool_plan(thinking) && !thinking_contains_planning(thinking) {
        *attempts += 1;
        harness
            .push_message(Message::user(
                "Your response did not include any tool calls, but your \
                 thinking shows you planned to inspect the codebase or \
                 execute commands. Please actually issue the tool calls \
                 now — do not speculate about what the code does without \
                 checking it first.",
            ))
            .await;
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
    lower.contains("<goal") || lower.contains("<plan") || lower.contains("<todo")
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

fn artifact_producer_from_tool_name(tool_name: &str) -> ArtifactProducer {
    if let Some((server, tool)) = parse_mcp_tool_name(tool_name) {
        return ArtifactProducer::Mcp {
            server_name: server.to_string(),
            tool_name: tool.to_string(),
        };
    }
    ArtifactProducer::Tool {
        tool_name: tool_name.to_string(),
    }
}

fn artifact_type_from_tool_name(
    mcp_profiles: &HashMap<String, McpServerProfile>,
    tool_name: &str,
) -> ArtifactType {
    match tool_name {
        "read" => ArtifactType::FileChunk,
        "web_fetch" | "webfetch" => ArtifactType::WebPage,
        "grep" | "glob" => ArtifactType::SearchResults,
        _ if tool_name.starts_with("mcp__") => {
            let kind = parse_mcp_tool_name(tool_name)
                .and_then(|(server_name, _)| mcp_profiles.get(server_name))
                .map(|profile| profile.kind)
                .unwrap_or(McpServerKind::Unknown);
            match kind {
                McpServerKind::ReadOnly => ArtifactType::McpReadOnlyPayload,
                McpServerKind::Filesystem => ArtifactType::McpFilesystemSnapshot,
                McpServerKind::Browser => ArtifactType::McpBrowserSnapshot,
                McpServerKind::ExternalApi => ArtifactType::McpExternalApiPayload,
                McpServerKind::Shell => ArtifactType::McpShellTranscript,
                McpServerKind::Unknown => ArtifactType::McpPayload,
            }
        }
        _ => ArtifactType::ToolOutput,
    }
}

fn parse_mcp_tool_name(tool_name: &str) -> Option<(&str, &str)> {
    let rest = tool_name.strip_prefix("mcp__")?;
    let (server, tool) = rest.split_once("__")?;
    Some((server, tool))
}

#[derive(Debug, Clone)]
struct ArtifactReadEventDetails {
    artifact_id: String,
    returned_chars: usize,
    offset_chars: usize,
    limit_chars: usize,
    source_tool_name: Option<String>,
    artifact_type: Option<String>,
    server_name: Option<String>,
    server_kind: Option<String>,
    transport: Option<String>,
    original_tool_name: Option<String>,
}

fn artifact_read_event_details(output: &ToolOutput) -> Option<ArtifactReadEventDetails> {
    let json = output.json.as_ref()?;
    Some(ArtifactReadEventDetails {
        artifact_id: json.get("artifact_id")?.as_str()?.to_string(),
        returned_chars: json.get("returned_chars")?.as_u64()? as usize,
        offset_chars: json.get("offset_chars")?.as_u64()? as usize,
        limit_chars: json.get("limit_chars")?.as_u64()? as usize,
        source_tool_name: json
            .get("source_tool_name")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        artifact_type: json
            .get("artifact_type")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        server_name: json
            .get("server_name")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        server_kind: json
            .get("server_kind")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        transport: json
            .get("transport")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        original_tool_name: json
            .get("original_tool_name")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    })
}

struct ArtifactStoragePolicy<'a> {
    class: ArtifactRetentionClass,
    ttl_hours_override: Option<u64>,
    server_name: Option<&'a str>,
    server_kind: Option<&'static str>,
    transport: Option<&'a str>,
    original_tool_name: Option<&'a str>,
}

fn artifact_storage_policy<'a>(
    artifact_cfg: &fox_agent_core::ArtifactStoreConfig,
    mcp_profiles: &'a HashMap<String, McpServerProfile>,
    mcp_descriptors: &'a HashMap<String, McpToolDescriptorSnapshot>,
    tool_name: &'a str,
) -> ArtifactStoragePolicy<'a> {
    let Some((server_name, _)) = parse_mcp_tool_name(tool_name) else {
        return ArtifactStoragePolicy {
            class: ArtifactRetentionClass::Ephemeral,
            ttl_hours_override: None,
            server_name: None,
            server_kind: None,
            transport: None,
            original_tool_name: None,
        };
    };

    let profile = mcp_profiles.get(server_name);
    let descriptor = mcp_descriptors.get(tool_name);
    let transport = profile.map(|p| match p.transport {
        fox_agent_core::McpTransportKind::Stdio => "stdio",
        fox_agent_core::McpTransportKind::Sse => "sse",
        fox_agent_core::McpTransportKind::Unknown => "unknown",
    });
    let ttl_hours_override = matches!(
        profile.map(|p| p.transport),
        Some(fox_agent_core::McpTransportKind::Sse)
    )
    .then_some(artifact_cfg.mcp_remote_ttl_hours);
    let kind = profile.map(|p| p.kind).unwrap_or(McpServerKind::Unknown);
    let class = match kind {
        McpServerKind::Filesystem | McpServerKind::Browser | McpServerKind::ReadOnly => {
            ArtifactRetentionClass::Referenced
        }
        _ => ArtifactRetentionClass::Ephemeral,
    };
    ArtifactStoragePolicy {
        class,
        ttl_hours_override,
        server_name: Some(server_name),
        server_kind: Some(mcp_server_kind_label(kind)),
        transport,
        original_tool_name: descriptor.map(|d| d.original_name.as_str()),
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ExternalizeDecision {
    pub(crate) should_externalize: bool,
    pub(crate) reason: Option<&'static str>,
}

pub(crate) fn should_externalize_tool_result(
    artifact_cfg: &fox_agent_core::ArtifactStoreConfig,
    mcp_profiles: &HashMap<String, McpServerProfile>,
    mcp_descriptors: &HashMap<String, McpToolDescriptorSnapshot>,
    tool_name: &str,
    raw_output_text: &str,
    truncated_by_context_guard: bool,
) -> ExternalizeDecision {
    if truncated_by_context_guard {
        return ExternalizeDecision {
            should_externalize: true,
            reason: Some("context-guard-truncated"),
        };
    }

    let Some((server_name, mcp_tool_name)) = parse_mcp_tool_name(tool_name) else {
        return ExternalizeDecision {
            should_externalize: false,
            reason: None,
        };
    };

    let output_chars = raw_output_text.chars().count();
    let profile = mcp_profiles.get(server_name);
    let descriptor = mcp_descriptors.get(tool_name);
    let descriptor_text = descriptor
        .map(|d| {
            format!(
                "{} {}",
                d.description.to_lowercase(),
                d.original_name.to_lowercase()
            )
        })
        .unwrap_or_else(|| mcp_tool_name.to_lowercase());
    let is_html_payload = raw_output_text.to_lowercase().contains("<html");

    let noisy_descriptor = ["read", "search", "list", "fetch", "html", "resource"]
        .iter()
        .any(|kw| descriptor_text.contains(kw));

    if matches!(
        profile.map(|p| p.transport),
        Some(fox_agent_core::McpTransportKind::Sse)
    ) && output_chars > 1_000
    {
        return ExternalizeDecision {
            should_externalize: true,
            reason: Some("mcp:sse-large"),
        };
    }

    if matches!(profile.map(|p| p.kind), Some(McpServerKind::Browser))
        && !artifact_cfg.mcp_browser_store_full_html
        && is_html_payload
    {
        return ExternalizeDecision {
            should_externalize: true,
            reason: Some("mcp:browser-html"),
        };
    }

    if descriptor_text.contains("search")
        && !artifact_cfg.mcp_search_store_full_payload
        && output_chars > 800
    {
        return ExternalizeDecision {
            should_externalize: true,
            reason: Some("mcp:search-payload"),
        };
    }

    let decision = match profile.map(|p| p.kind).unwrap_or(McpServerKind::Unknown) {
        McpServerKind::Filesystem => output_chars > 1_500 || noisy_descriptor,
        McpServerKind::Browser => output_chars > 1_500 || noisy_descriptor,
        McpServerKind::ExternalApi => output_chars > 2_000 || noisy_descriptor,
        McpServerKind::ReadOnly => output_chars > 4_000 && noisy_descriptor,
        McpServerKind::Shell => output_chars > 2_000,
        McpServerKind::Unknown => output_chars > 5_000 && noisy_descriptor,
    };
    let reason = if decision {
        Some(
            match profile.map(|p| p.kind).unwrap_or(McpServerKind::Unknown) {
                McpServerKind::Filesystem => "mcp:filesystem-large",
                McpServerKind::Browser => "mcp:browser-large",
                McpServerKind::ExternalApi => "mcp:external-api-large",
                McpServerKind::ReadOnly => "mcp:readonly-large",
                McpServerKind::Shell => "mcp:shell-large",
                McpServerKind::Unknown => "mcp:unknown-large",
            },
        )
    } else {
        None
    };
    ExternalizeDecision {
        should_externalize: decision,
        reason,
    }
}

fn mcp_server_kind_label(kind: McpServerKind) -> &'static str {
    match kind {
        McpServerKind::ReadOnly => "readonly",
        McpServerKind::ExternalApi => "external_api",
        McpServerKind::Filesystem => "filesystem",
        McpServerKind::Shell => "shell",
        McpServerKind::Browser => "browser",
        McpServerKind::Unknown => "unknown",
    }
}

// ── P2 helper: tool result message with duration ──

/// Create a tool result message with duration appended as metadata text.
fn tool_result_msg(call_id: String, text: String, is_error: bool, duration_ms: u64) -> Message {
    // Append duration metadata so the model sees execution timing.
    let duration_suffix = if duration_ms > 100 {
        format!(
            "\n[Tool execution duration: {:.2}s]",
            duration_ms as f64 / 1000.0
        )
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
) -> (String, bool) {
    if !compaction_cfg.enabled {
        return (output_text.to_string(), false);
    }

    let budget = compaction_cfg.token_budget;
    let current = super::compaction::message_chars(messages);
    let output_len = output_text.len();

    let single_max = (budget as f32 * SINGLE_OUTPUT_MAX_FRACTION) as usize;
    let threshold = (budget as f32 * CONTEXT_GUARD_THRESHOLD) as usize;
    let projected = current + output_len;

    let needs_trunc = output_len > single_max || projected > threshold;

    if !needs_trunc {
        return (output_text.to_string(), false);
    }

    // How much room do we have?
    let remaining = if current < threshold {
        threshold.saturating_sub(current)
    } else {
        budget / 50 // ~2% of budget for truncation notice
    };
    let max_chars = remaining.min(single_max);

    if output_text.len() <= max_chars {
        return (output_text.to_string(), false);
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
    (result, true)
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
    messages
        .iter()
        .map(|msg| {
            let role = match msg.role {
                Role::System => "sys",
                Role::User => "usr",
                Role::Assistant => "asst",
                Role::Tool => "tool",
            };
            let preview: String = msg
                .content
                .iter()
                .map(|block| match block {
                    ContentBlock::Text { text } | ContentBlock::Reasoning { text } => {
                        let (short, _) = fox_agent_core::format_truncated(text, 80);
                        short
                    }
                    ContentBlock::ToolResult { text, .. } => {
                        let (short, _) = fox_agent_core::format_truncated(text, 60);
                        format!("[result: {}]", short)
                    }
                    ContentBlock::ToolUse { name, .. } => format!("[tool_call: {name}]"),
                    ContentBlock::Image { .. } => "[image]".to_string(),
                    ContentBlock::NarrativeSummary { text } => {
                        format!("[narrative: {}...]", &text[..text.len().min(80)])
                    }
                })
                .collect::<Vec<_>>()
                .join(" | ");
            format!("[{role}] {preview}")
        })
        .collect()
}
