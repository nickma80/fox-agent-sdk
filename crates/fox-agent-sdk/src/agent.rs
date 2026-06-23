use fox_agent_core::{
    AgentError, AgentEvent, AgentEventTx, ContentBlock, Message, Model, PermissionDecision,
    PermissionRequest, PermissionResult, PendingToolCallSnapshot, ProviderError, Role,
    SessionSnapshot, StreamEvent, ToolContext, TurnOutcome, ToolExecutionMode, now_secs,
};
use futures::StreamExt;
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, error, info, span, trace, warn, Level};

use crate::harness::Harness;

// ── Loop limits (P0) ──

/// Maximum number of tool-loop iterations (API call + tool execution cycles).
const MAX_TOOL_LOOP_ITERATIONS: u32 = 100;
/// Maximum number of context-limit compaction retries before giving up.
const MAX_CONTEXT_LIMIT_RETRIES: u32 = 5;
/// Maximum number of incomplete / degenerate continuation attempts.
const MAX_INCOMPLETE_CONTINUATION_ATTEMPTS: u32 = 3;
/// Substrings that indicate a context-limit error from the provider.
const CTRL_LIMIT_KEYWORDS: &[&str] = &[
    "context_length_exceeded",
    "max_context_length",
    "too many tokens",
    "maximum context length",
    "context_overflow",
];

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
}

impl Agent {
    pub fn new(model: Arc<dyn Model>, harness: Harness) -> Self {
        debug!(session_id = %harness.session_state.id, "Agent created");
        Self {
            model, harness,
            pending_permission: None,
            pending_tool_calls: Vec::new(),
            next_turn_id: 1,
            governance: None,
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
                let output = match self.harness.execute_tool(&pending.name, pending.input, ctx).await {
                    Ok(output) => output,
                    Err(err) => {
                        error!(tool = %pending.name, error = %err, "Tool execution failed");
                        self.emit_error_event(event_tx, AgentError::Tool(err.clone())).await;
                        return Err(AgentError::Tool(err));
                    }
                };
                let elapsed_ms = start.elapsed().as_millis() as u64;

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
                info!(
                    trigger = ?compaction.trigger,
                    removed = compaction.removed_messages,
                    kept = compaction.kept_messages,
                    "Compaction triggered"
                );
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

            let (split, _context_info) = self
                .harness
                .build_system_prompt_split(memory_prompt.as_deref(), None)
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
                            let _ = event_tx
                                .send(AgentEvent::Compaction { event: compaction })
                                .await;
                        }
                        continue;
                    }
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

            if collected_tool_calls.is_empty() {
                // P0: Check for incomplete continuation
                if maybe_continue_incomplete(&stop_reason, &mut incomplete_continuations, &mut self.harness) {
                    info!("Requesting continuation for incomplete response");
                    continue;
                }
                // P0: Check for degenerate (empty) response
                if maybe_continue_degenerate(&final_text, &mut incomplete_continuations, &mut self.harness) {
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

                // P2: Track tool duration
                let start = Instant::now();
                debug!(tool = %name, "Executing tool");
                let output = match self.harness.execute_tool(&name, input, ctx).await {
                    Ok(output) => {
                        info!(
                            tool = %name,
                            is_error = output.is_error,
                            out_preview = %truncate(&output.text, 300),
                            "Tool executed"
                        );
                        output
                    }
                    Err(err) => {
                        error!(tool = %name, error = %err, "Tool execution failed");
                        return Err(self.handle_error(event_tx, turn_id, AgentError::Tool(err)));
                    }
                };
                let elapsed_ms = start.elapsed().as_millis() as u64;

                let _ = event_tx
                    .send(AgentEvent::ToolCallEnd {
                        call_id: call_id.clone(),
                        output: output.clone(),
                    })
                    .await;
                // P2: result with duration
                self.harness.session_state.messages.push(
                    tool_result_msg(call_id, output.text, output.is_error, elapsed_ms),
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
fn maybe_continue_degenerate(
    text: &str,
    attempts: &mut u32,
    harness: &mut Harness,
) -> bool {
    if *attempts >= MAX_INCOMPLETE_CONTINUATION_ATTEMPTS {
        return false;
    }
    if text.trim().is_empty() {
        *attempts += 1;
        harness.session_state.messages.push(
            Message::user("Your response was empty. Please try again."),
        );
        true
    } else {
        false
    }
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

// ── Logging helpers ──

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() }
    else { format!("{}... ({}/{})", &s[..max], max, s.len()) }
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
