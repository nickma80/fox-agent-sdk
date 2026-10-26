//! Maps `AgentEvent` streams into OTLP spans following the OTel GenAI
//! semantic conventions ([semantic-conventions-genai](https://github.com/open-telemetry/semantic-conventions-genai))
//! plus `fox_agent.*` extensions for non-standard data.
//!
//! Span taxonomy (latest conventions):
//! - `invoke_agent {agent.name}` — one per turn, `INTERNAL`, trace root
//! - `chat {model}` — per LLM message, `CLIENT`
//! - `execute_tool {tool.name}` — per tool call, `INTERNAL`
//!
//! Content capture (`gen_ai.output.messages`) is opt-in and off by default.

use std::collections::HashMap;

use fox_agent_core::{AgentError, AgentEvent, TokenUsage, ToolOutput, TurnOutcome};

use super::otlp::{
    OtValue, OtlpEvent, OtlpSpan, OtlpSpanKind, OtlpStatusCode, hex_span_id, hex_trace_id,
};

/// Runtime identity supplied by the running agent (real values, never defaults).
#[derive(Debug, Clone)]
pub struct ExportIdentity {
    /// `gen_ai.provider.name` (e.g. `deepseek`, `openai`).
    pub provider_name: String,
    /// `gen_ai.request.model`.
    pub model_id: String,
    /// `gen_ai.conversation.id` (session id).
    pub session_id: String,
}

/// Static mapper settings derived from `[observability]`.
#[derive(Debug, Clone)]
pub struct MapperConfig {
    /// `gen_ai.agent.name`.
    pub agent_name: String,
    /// `gen_ai.agent.id` (optional).
    pub agent_id: Option<String>,
    /// Opt-In content capture (`gen_ai.output.messages`).
    pub capture_content: bool,
    /// Maximum characters kept per captured content chunk.
    pub max_content_chars: usize,
}

/// A score pushed by the evaluation layer (`gen_ai.evaluation.result`).
#[derive(Debug, Clone)]
pub struct EvaluationRecord {
    /// `gen_ai.evaluation.name`.
    pub name: String,
    /// `gen_ai.evaluation.score.value`.
    pub score: f64,
    /// `gen_ai.evaluation.score.label` (optional).
    pub label: Option<String>,
    /// `gen_ai.evaluation.explanation` (optional).
    pub explanation: Option<String>,
}

/// An in-progress span held until its closing event arrives.
#[derive(Debug, Clone)]
struct OpenSpan {
    name: String,
    span_id: String,
    start: u64,
    kind: OtlpSpanKind,
    attributes: Vec<(String, OtValue)>,
    events: Vec<OtlpEvent>,
    status: OtlpStatusCode,
    status_message: String,
    output_text: String,
    thinking_text: String,
}

impl OpenSpan {
    fn new(name: String, kind: OtlpSpanKind, start: u64) -> Self {
        Self {
            name,
            span_id: hex_span_id(),
            start,
            kind,
            attributes: Vec::new(),
            events: Vec::new(),
            status: OtlpStatusCode::Unset,
            status_message: String::new(),
            output_text: String::new(),
            thinking_text: String::new(),
        }
    }

    fn push(&mut self, key: impl Into<String>, value: OtValue) {
        self.attributes.push((key.into(), value));
    }

    fn event(&mut self, name: &str, ts: u64, attributes: Vec<(String, OtValue)>) {
        self.events.push(OtlpEvent {
            name: name.to_string(),
            time_unix_nano: ts,
            attributes,
        });
    }

    fn close(self, end: u64, trace_id: String, parent_span_id: Option<String>) -> OtlpSpan {
        OtlpSpan {
            trace_id,
            span_id: self.span_id,
            parent_span_id,
            name: self.name,
            kind: self.kind,
            start_time_unix_nano: self.start,
            end_time_unix_nano: end.max(self.start),
            attributes: self.attributes,
            events: self.events,
            status_code: self.status,
            status_message: self.status_message,
        }
    }
}

/// Stateful mapper converting an `AgentEvent` stream into completed spans.
///
/// Open spans are flushed defensively whenever a structural boundary (new
/// turn, explicit flush) is crossed, so no event ordering can wedge the state.
pub struct SpanMapper {
    identity: ExportIdentity,
    cfg: MapperConfig,
    trace_id: String,
    turn: Option<OpenSpan>,
    chats: HashMap<String, OpenSpan>,
    /// Chat spans that ended but may still receive `ModelUsage`.
    finished_chats: Vec<OpenSpan>,
    tools: HashMap<String, OpenSpan>,
    subagents: HashMap<String, OpenSpan>,
    last_chat_message_id: Option<String>,
    conversation_compacted: bool,
}

impl SpanMapper {
    pub fn new(identity: ExportIdentity, cfg: MapperConfig) -> Self {
        Self {
            identity,
            cfg,
            trace_id: hex_trace_id(),
            turn: None,
            chats: HashMap::new(),
            finished_chats: Vec::new(),
            tools: HashMap::new(),
            subagents: HashMap::new(),
            last_chat_message_id: None,
            conversation_compacted: false,
        }
    }

    /// Consume one event; returns any spans that completed as a result.
    pub fn on_event(&mut self, ev: &AgentEvent, ts_ns: u64) -> Vec<OtlpSpan> {
        let mut out = Vec::new();
        match ev {
            AgentEvent::TurnStart { .. } => out.extend(self.start_turn(ts_ns)),
            AgentEvent::TurnEnd { outcome, .. } => {
                out.extend(self.flush_finished_chats(ts_ns));
                out.extend(self.end_turn(outcome, ts_ns));
            }
            AgentEvent::ModelMessageStart { message_id } => {
                out.extend(self.flush_finished_chats(ts_ns));
                out.extend(self.start_chat(message_id, ts_ns));
            }
            AgentEvent::ModelTextDelta { text } => out.extend(self.chat_delta(text, false, ts_ns)),
            AgentEvent::ModelThinkingDelta { text } => {
                out.extend(self.chat_delta(text, true, ts_ns))
            }
            AgentEvent::ModelMessageEnd { message_id } => out.extend(self.end_chat(message_id)),
            AgentEvent::ModelUsage { usage } => out.extend(self.attach_usage(usage)),
            AgentEvent::ToolCallStart { call_id, name, .. } => {
                out.extend(self.flush_finished_chats(ts_ns));
                out.extend(self.start_tool(call_id, name, ts_ns));
            }
            AgentEvent::ToolCallEnd { call_id, output } => {
                out.extend(self.flush_finished_chats(ts_ns));
                out.extend(self.end_tool(call_id, output, ts_ns));
            }
            AgentEvent::Compaction { event } => {
                self.conversation_compacted = true;
                out.extend(self.turn_event(
                    "fox_agent.compaction",
                    ts_ns,
                    vec![
                        (
                            "fox_agent.compaction.removed_messages".into(),
                            OtValue::Int(event.removed_messages as i64),
                        ),
                        (
                            "fox_agent.compaction.kept_messages".into(),
                            OtValue::Int(event.kept_messages as i64),
                        ),
                        (
                            "fox_agent.compaction.summary_chars".into(),
                            OtValue::Int(event.summary_chars as i64),
                        ),
                    ],
                ));
            }
            AgentEvent::MemoryInjected { count, .. } => out.extend(self.turn_event(
                "fox_agent.memory.injected",
                ts_ns,
                vec![("fox_agent.memory.count".into(), OtValue::Int(*count as i64))],
            )),
            AgentEvent::MemoryStateChanged { .. } => {
                out.extend(self.turn_event("fox_agent.memory.state", ts_ns, Vec::new()));
            }
            AgentEvent::PermissionRequest {
                tool_name,
                risk_level,
                request_id,
                ..
            } => {
                out.extend(self.turn_event(
                    "fox_agent.permission",
                    ts_ns,
                    vec![
                        (
                            "fox_agent.permission.tool".into(),
                            OtValue::Str(tool_name.clone()),
                        ),
                        (
                            "fox_agent.permission.risk".into(),
                            OtValue::Str(risk_level.clone()),
                        ),
                        (
                            "fox_agent.permission.request_id".into(),
                            OtValue::Str(request_id.clone()),
                        ),
                    ],
                ));
            }
            AgentEvent::ArtifactStored {
                artifact_id,
                tool_name,
                size_bytes,
                artifact_type,
                ..
            } => {
                out.extend(self.turn_event(
                    "fox_agent.artifact.stored",
                    ts_ns,
                    vec![
                        (
                            "fox_agent.artifact.id".into(),
                            OtValue::Str(artifact_id.clone()),
                        ),
                        (
                            "fox_agent.artifact.tool".into(),
                            OtValue::Str(tool_name.clone()),
                        ),
                        (
                            "fox_agent.artifact.type".into(),
                            OtValue::Str(artifact_type.clone()),
                        ),
                        (
                            "fox_agent.artifact.size_bytes".into(),
                            OtValue::Int(*size_bytes as i64),
                        ),
                    ],
                ));
            }
            AgentEvent::ArtifactRead {
                artifact_id,
                returned_chars,
                offset_chars,
                limit_chars,
                ..
            } => {
                out.extend(self.turn_event(
                    "fox_agent.artifact.read",
                    ts_ns,
                    vec![
                        (
                            "fox_agent.artifact.id".into(),
                            OtValue::Str(artifact_id.clone()),
                        ),
                        (
                            "fox_agent.artifact.returned_chars".into(),
                            OtValue::Int(*returned_chars as i64),
                        ),
                        (
                            "fox_agent.artifact.offset_chars".into(),
                            OtValue::Int(*offset_chars as i64),
                        ),
                        (
                            "fox_agent.artifact.limit_chars".into(),
                            OtValue::Int(*limit_chars as i64),
                        ),
                    ],
                ));
            }
            AgentEvent::ArtifactGc {
                scope,
                deleted,
                bytes_freed,
                ..
            } => out.extend(self.turn_event(
                "fox_agent.artifact.gc",
                ts_ns,
                vec![
                    (
                        "fox_agent.artifact.scope".into(),
                        OtValue::Str(scope.clone()),
                    ),
                    (
                        "fox_agent.artifact.deleted".into(),
                        OtValue::Int(*deleted as i64),
                    ),
                    (
                        "fox_agent.artifact.bytes_freed".into(),
                        OtValue::Int(*bytes_freed as i64),
                    ),
                ],
            )),
            AgentEvent::PlanProgress {
                completed, total, ..
            } => out.extend(self.turn_event(
                "fox_agent.plan.progress",
                ts_ns,
                vec![
                    (
                        "fox_agent.plan.completed".into(),
                        OtValue::Int(*completed as i64),
                    ),
                    ("fox_agent.plan.total".into(), OtValue::Int(*total as i64)),
                ],
            )),
            AgentEvent::RoutingDecision {
                tool_name,
                call_id,
                routing,
                reason,
                ..
            } => {
                out.extend(self.turn_event(
                    "fox_agent.routing",
                    ts_ns,
                    vec![
                        (
                            "fox_agent.routing.tool".into(),
                            OtValue::Str(tool_name.clone()),
                        ),
                        (
                            "fox_agent.routing.call_id".into(),
                            OtValue::Str(call_id.clone()),
                        ),
                        (
                            "fox_agent.routing.decision".into(),
                            OtValue::Str(format!("{routing:?}")),
                        ),
                        (
                            "fox_agent.routing.reason".into(),
                            OtValue::Str(reason.clone().unwrap_or_default()),
                        ),
                    ],
                ));
            }
            AgentEvent::SubagentTaskStarted {
                task_id, objective, ..
            } => {
                out.extend(self.start_subagent(task_id, objective, ts_ns));
            }
            AgentEvent::SubagentTaskCompleted {
                task_id, outcome, ..
            } => {
                out.extend(self.end_subagent(task_id, outcome, ts_ns));
            }
            AgentEvent::McpServerConnected { server_name } => out.extend(self.turn_event(
                "fox_agent.mcp.connected",
                ts_ns,
                vec![(
                    "fox_agent.mcp.server".into(),
                    OtValue::Str(server_name.clone()),
                )],
            )),
            AgentEvent::McpServerDisconnected { server_name, .. } => out.extend(self.turn_event(
                "fox_agent.mcp.disconnected",
                ts_ns,
                vec![(
                    "fox_agent.mcp.server".into(),
                    OtValue::Str(server_name.clone()),
                )],
            )),
            AgentEvent::SoftInterruptInjected { .. } => {
                out.extend(self.turn_event("fox_agent.interrupt", ts_ns, Vec::new()));
            }
            AgentEvent::Error { error } => out.extend(self.record_error(error)),
            // Purely informational / noisy events are intentionally skipped.
            AgentEvent::WaitingForModel { .. }
            | AgentEvent::ToolInputDelta { .. }
            | AgentEvent::ToolExecutionProgress { .. }
            | AgentEvent::TurnSummary { .. } => {}
        }
        out
    }

    /// Attach an evaluation score to the most relevant open span as a
    /// `gen_ai.evaluation.result` event (defaults to the turn span).
    pub fn push_evaluation(&mut self, record: EvaluationRecord, ts_ns: u64) {
        let mut attrs = vec![
            ("gen_ai.evaluation.name".into(), OtValue::Str(record.name)),
            (
                "gen_ai.evaluation.score.value".into(),
                OtValue::Double(record.score),
            ),
        ];
        if let Some(label) = record.label {
            attrs.push(("gen_ai.evaluation.score.label".into(), OtValue::Str(label)));
        }
        if let Some(explanation) = record.explanation {
            attrs.push((
                "gen_ai.evaluation.explanation".into(),
                OtValue::Str(explanation),
            ));
        }
        if let Some(turn) = self.turn.as_mut() {
            turn.event("gen_ai.evaluation.result", ts_ns, attrs);
        } else if let Some(chat) = self.last_open_chat() {
            chat.event("gen_ai.evaluation.result", ts_ns, attrs);
        }
    }

    /// Close every open span; returns all completed spans (end of session).
    pub fn flush(&mut self, ts_ns: u64) -> Vec<OtlpSpan> {
        let mut out = self.flush_finished_chats(ts_ns);
        let parent = self.turn_span_id();
        let chats = std::mem::take(&mut self.chats);
        for (_, chat) in chats {
            out.push(self.close_chat(chat, ts_ns, parent.clone()));
        }
        let tools = std::mem::take(&mut self.tools);
        for (_, tool) in tools {
            out.push(tool.close(ts_ns, self.trace_id.clone(), parent.clone()));
        }
        let subagents = std::mem::take(&mut self.subagents);
        for (_, sub) in subagents {
            out.push(sub.close(ts_ns, self.trace_id.clone(), parent.clone()));
        }
        if let Some(turn) = self.turn.take() {
            out.push(turn.close(ts_ns, self.trace_id.clone(), None));
        }
        out
    }

    // ── turn ──

    fn start_turn(&mut self, ts: u64) -> Vec<OtlpSpan> {
        let out = self.flush(ts);
        self.trace_id = hex_trace_id();
        let mut span = OpenSpan::new(
            format!("invoke_agent {}", self.cfg.agent_name),
            OtlpSpanKind::Internal,
            ts,
        );
        span.push("gen_ai.operation.name", OtValue::Str("invoke_agent".into()));
        span.push(
            "gen_ai.agent.name",
            OtValue::Str(self.cfg.agent_name.clone()),
        );
        span.push(
            "gen_ai.provider.name",
            OtValue::Str(self.identity.provider_name.clone()),
        );
        span.push(
            "gen_ai.conversation.id",
            OtValue::Str(self.identity.session_id.clone()),
        );
        if let Some(id) = &self.cfg.agent_id {
            span.push("gen_ai.agent.id", OtValue::Str(id.clone()));
        }
        self.conversation_compacted = false;
        self.turn = Some(span);
        out
    }

    fn end_turn(&mut self, outcome: &TurnOutcome, ts: u64) -> Vec<OtlpSpan> {
        let mut out = Vec::new();
        if let Some(mut turn) = self.turn.take() {
            match outcome {
                TurnOutcome::Failed { error } => {
                    turn.status = OtlpStatusCode::Error;
                    turn.status_message = format!("{error:?}");
                    turn.push("error.type", OtValue::Str(agent_error_type(error).into()));
                }
                _ => turn.status = OtlpStatusCode::Ok,
            }
            out.push(turn.close(ts, self.trace_id.clone(), None));
        }
        out
    }

    // ── chat ──

    fn start_chat(&mut self, message_id: &str, ts: u64) -> Vec<OtlpSpan> {
        let mut span = OpenSpan::new(
            format!("chat {}", self.identity.model_id),
            OtlpSpanKind::Client,
            ts,
        );
        span.push("gen_ai.operation.name", OtValue::Str("chat".into()));
        span.push(
            "gen_ai.request.model",
            OtValue::Str(self.identity.model_id.clone()),
        );
        span.push(
            "gen_ai.provider.name",
            OtValue::Str(self.identity.provider_name.clone()),
        );
        span.push(
            "gen_ai.conversation.id",
            OtValue::Str(self.identity.session_id.clone()),
        );
        if self.conversation_compacted {
            span.push("gen_ai.conversation.compacted", OtValue::Bool(true));
        }
        self.chats.insert(message_id.to_string(), span);
        self.last_chat_message_id = Some(message_id.to_string());
        Vec::new()
    }

    fn chat_delta(&mut self, text: &str, thinking: bool, _ts: u64) -> Vec<OtlpSpan> {
        if !self.cfg.capture_content {
            return Vec::new();
        }
        let Some(message_id) = self.last_chat_message_id.clone() else {
            return Vec::new();
        };
        let Some(chat) = self.chats.get_mut(&message_id) else {
            return Vec::new();
        };
        let buffer = if thinking {
            &mut chat.thinking_text
        } else {
            &mut chat.output_text
        };
        if buffer.chars().count() < self.cfg.max_content_chars {
            buffer.push_str(text);
        }
        Vec::new()
    }

    /// `ModelMessageEnd` moves the chat span into `finished_chats` so a
    /// trailing `ModelUsage` event can still attach usage attributes.
    /// `last_chat_message_id` is intentionally kept until the next
    /// `ModelMessageStart` overwrites it.
    fn end_chat(&mut self, message_id: &str) -> Vec<OtlpSpan> {
        if let Some(chat) = self.chats.remove(message_id) {
            self.finished_chats.push(chat);
        }
        Vec::new()
    }

    fn flush_finished_chats(&mut self, ts: u64) -> Vec<OtlpSpan> {
        if self.finished_chats.is_empty() {
            return Vec::new();
        }
        let parent = self.turn_span_id();
        let mut out = Vec::new();
        for chat in std::mem::take(&mut self.finished_chats) {
            out.push(self.close_chat(chat, ts, parent.clone()));
        }
        out
    }

    fn close_chat(&mut self, mut chat: OpenSpan, ts: u64, parent: Option<String>) -> OtlpSpan {
        if self.cfg.capture_content {
            let output = std::mem::take(&mut chat.output_text);
            let thinking = std::mem::take(&mut chat.thinking_text);
            if !output.is_empty() {
                let messages = serde_json::json!([{
                    "role": "assistant",
                    "parts": [{ "type": "text", "content": output }],
                }]);
                chat.push("gen_ai.output.messages", OtValue::Str(messages.to_string()));
            }
            if !thinking.is_empty() {
                chat.push("fox_agent.output.thinking", OtValue::Str(thinking));
            }
        }
        chat.close(ts, self.trace_id.clone(), parent)
    }

    fn attach_usage(&mut self, usage: &TokenUsage) -> Vec<OtlpSpan> {
        let Some(message_id) = self.last_chat_message_id.clone() else {
            return Vec::new();
        };
        let target = self
            .chats
            .get_mut(&message_id)
            .or_else(|| self.finished_chats.iter_mut().last());
        let Some(chat) = target else {
            return Vec::new();
        };
        chat.push(
            "gen_ai.usage.input_tokens",
            OtValue::Int(usage.input_tokens as i64),
        );
        chat.push(
            "gen_ai.usage.output_tokens",
            OtValue::Int(usage.output_tokens as i64),
        );
        if let Some(v) = usage.cache_read_input_tokens {
            chat.push(
                "gen_ai.usage.cache_read.input_tokens",
                OtValue::Int(v as i64),
            );
        }
        if let Some(v) = usage.cache_creation_input_tokens {
            chat.push(
                "gen_ai.usage.cache_creation.input_tokens",
                OtValue::Int(v as i64),
            );
        }
        Vec::new()
    }

    // ── tools ──

    fn start_tool(&mut self, call_id: &str, name: &str, ts: u64) -> Vec<OtlpSpan> {
        let mut span = OpenSpan::new(format!("execute_tool {name}"), OtlpSpanKind::Internal, ts);
        span.push("gen_ai.operation.name", OtValue::Str("execute_tool".into()));
        span.push("gen_ai.tool.name", OtValue::Str(name.to_string()));
        span.push("gen_ai.tool.call.id", OtValue::Str(call_id.to_string()));
        self.tools.insert(call_id.to_string(), span);
        Vec::new()
    }

    fn end_tool(&mut self, call_id: &str, output: &ToolOutput, ts: u64) -> Vec<OtlpSpan> {
        let mut out = Vec::new();
        if let Some(mut tool) = self.tools.remove(call_id) {
            if output.is_error {
                tool.status = OtlpStatusCode::Error;
                tool.status_message = truncate(&output.text, self.cfg.max_content_chars);
                tool.push("error.type", OtValue::Str("tool_error".into()));
            } else {
                tool.status = OtlpStatusCode::Ok;
            }
            out.push(tool.close(ts, self.trace_id.clone(), self.turn_span_id()));
        }
        out
    }

    // ── subagents ──

    fn start_subagent(&mut self, task_id: &str, objective: &str, ts: u64) -> Vec<OtlpSpan> {
        let short_id = truncate(task_id, 24);
        let mut span = OpenSpan::new(
            format!("invoke_agent subagent-{short_id}"),
            OtlpSpanKind::Internal,
            ts,
        );
        span.push("gen_ai.operation.name", OtValue::Str("invoke_agent".into()));
        span.push(
            "gen_ai.agent.name",
            OtValue::Str(self.cfg.agent_name.clone()),
        );
        span.push(
            "gen_ai.agent.id",
            OtValue::Str(format!("subagent:{task_id}")),
        );
        span.push(
            "gen_ai.conversation.id",
            OtValue::Str(self.identity.session_id.clone()),
        );
        span.push(
            "fox_agent.subagent.task_id",
            OtValue::Str(task_id.to_string()),
        );
        span.push(
            "fox_agent.subagent.objective",
            OtValue::Str(truncate(objective, 512)),
        );
        self.subagents.insert(task_id.to_string(), span);
        Vec::new()
    }

    fn end_subagent(&mut self, task_id: &str, outcome: &str, ts: u64) -> Vec<OtlpSpan> {
        let mut out = Vec::new();
        if let Some(mut sub) = self.subagents.remove(task_id) {
            sub.push(
                "fox_agent.subagent.outcome",
                OtValue::Str(outcome.to_string()),
            );
            sub.status = OtlpStatusCode::Ok;
            out.push(sub.close(ts, self.trace_id.clone(), self.turn_span_id()));
        }
        out
    }

    // ── misc ──

    fn turn_event(
        &mut self,
        name: &str,
        ts: u64,
        attributes: Vec<(String, OtValue)>,
    ) -> Vec<OtlpSpan> {
        if let Some(turn) = self.turn.as_mut() {
            turn.event(name, ts, attributes);
        }
        Vec::new()
    }

    fn record_error(&mut self, error: &AgentError) -> Vec<OtlpSpan> {
        if let Some(turn) = self.turn.as_mut() {
            turn.status = OtlpStatusCode::Error;
            turn.status_message = format!("{error:?}");
            turn.push("error.type", OtValue::Str(agent_error_type(error).into()));
        }
        Vec::new()
    }

    fn turn_span_id(&self) -> Option<String> {
        self.turn.as_ref().map(|t| t.span_id.clone())
    }

    /// Last open chat span (for `push_evaluation` when no turn is open).
    fn last_open_chat(&mut self) -> Option<&mut OpenSpan> {
        let id = self.last_chat_message_id.clone()?;
        if self.chats.contains_key(&id) {
            self.chats.get_mut(&id)
        } else {
            self.chats.values_mut().last()
        }
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        s.chars().take(max_chars).collect()
    }
}

fn agent_error_type(error: &AgentError) -> &'static str {
    match error {
        AgentError::Provider(_) => "provider_error",
        AgentError::Tool(_) => "tool_error",
        AgentError::PermissionDenied { .. } => "permission_denied",
        AgentError::Internal { .. } => "internal_error",
        AgentError::BudgetExceeded { .. } => "budget_exceeded",
        AgentError::Mcp { .. } => "mcp_error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapper() -> SpanMapper {
        SpanMapper::new(
            ExportIdentity {
                provider_name: "deepseek".into(),
                model_id: "deepseek-chat".into(),
                session_id: "sess-1".into(),
            },
            MapperConfig {
                agent_name: "math-tutor".into(),
                agent_id: Some("agent-7".into()),
                capture_content: true,
                max_content_chars: 4096,
            },
        )
    }

    fn run_events(m: &mut SpanMapper, events: &[AgentEvent], start_ns: u64) -> Vec<OtlpSpan> {
        let mut all = Vec::new();
        let mut t = start_ns;
        for ev in events {
            all.extend(m.on_event(ev, t));
            t += 1_000_000_000;
        }
        all.extend(m.flush(t));
        all
    }

    fn attr<'a>(span: &'a OtlpSpan, key: &str) -> Option<&'a OtValue> {
        span.attributes
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    #[test]
    fn turn_chat_tool_produce_expected_spans() {
        let mut m = mapper();
        let usage = TokenUsage {
            input_tokens: 10,
            output_tokens: 5,
            total_tokens: 15,
            cache_read_input_tokens: Some(3),
            cache_creation_input_tokens: None,
        };
        let spans = run_events(
            &mut m,
            &[
                AgentEvent::TurnStart { turn_id: 1 },
                AgentEvent::ModelMessageStart {
                    message_id: "m1".into(),
                },
                AgentEvent::ModelTextDelta {
                    text: "hello world".into(),
                },
                AgentEvent::ModelMessageEnd {
                    message_id: "m1".into(),
                },
                AgentEvent::ModelUsage { usage },
                AgentEvent::ToolCallStart {
                    call_id: "c1".into(),
                    name: "get_weather".into(),
                    input: serde_json::json!({"city": "shanghai"}),
                },
                AgentEvent::ToolCallEnd {
                    call_id: "c1".into(),
                    output: ToolOutput::new("sunny 28°C"),
                },
                AgentEvent::TurnEnd {
                    turn_id: 1,
                    outcome: TurnOutcome::Completed {
                        text: "done".into(),
                    },
                },
            ],
            1_700_000_000_000_000_000,
        );

        // invoke_agent (turn) + chat + execute_tool
        assert_eq!(spans.len(), 3);
        let turn = spans
            .iter()
            .find(|s| s.name.starts_with("invoke_agent"))
            .unwrap();
        assert_eq!(turn.kind, OtlpSpanKind::Internal);
        assert_eq!(turn.parent_span_id, None);
        assert_eq!(
            attr(turn, "gen_ai.operation.name"),
            Some(&OtValue::Str("invoke_agent".into()))
        );
        assert_eq!(
            attr(turn, "gen_ai.agent.name"),
            Some(&OtValue::Str("math-tutor".into()))
        );
        assert_eq!(
            attr(turn, "gen_ai.conversation.id"),
            Some(&OtValue::Str("sess-1".into()))
        );
        assert_eq!(turn.status_code, OtlpStatusCode::Ok);

        let chat = spans
            .iter()
            .find(|s| s.name == "chat deepseek-chat")
            .unwrap();
        assert_eq!(chat.kind, OtlpSpanKind::Client);
        assert_eq!(chat.parent_span_id, Some(turn.span_id.clone()));
        assert_eq!(
            attr(chat, "gen_ai.usage.input_tokens"),
            Some(&OtValue::Int(10))
        );
        assert_eq!(
            attr(chat, "gen_ai.usage.cache_read.input_tokens"),
            Some(&OtValue::Int(3))
        );
        // content capture produced gen_ai.output.messages
        let content = attr(chat, "gen_ai.output.messages").unwrap();
        if let OtValue::Str(s) = content {
            assert!(s.contains("hello world"));
        } else {
            panic!("expected string content");
        }

        let tool = spans
            .iter()
            .find(|s| s.name == "execute_tool get_weather")
            .unwrap();
        assert_eq!(tool.kind, OtlpSpanKind::Internal);
        assert_eq!(tool.parent_span_id, Some(turn.span_id.clone()));
        assert_eq!(
            attr(tool, "gen_ai.tool.call.id"),
            Some(&OtValue::Str("c1".into()))
        );
        assert_eq!(tool.status_code, OtlpStatusCode::Ok);
    }

    #[test]
    fn failed_turn_marks_error_status() {
        let mut m = mapper();
        let spans = run_events(
            &mut m,
            &[
                AgentEvent::TurnStart { turn_id: 1 },
                AgentEvent::TurnEnd {
                    turn_id: 1,
                    outcome: TurnOutcome::Failed {
                        error: AgentError::Tool(fox_agent_core::ToolError::Message {
                            message: "boom".into(),
                        }),
                    },
                },
            ],
            1_700_000_000_000_000_000,
        );
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].status_code, OtlpStatusCode::Error);
        assert_eq!(
            attr(&spans[0], "error.type"),
            Some(&OtValue::Str("tool_error".into()))
        );
    }

    #[test]
    fn tool_error_marks_tool_span() {
        let mut m = mapper();
        let spans = run_events(
            &mut m,
            &[
                AgentEvent::TurnStart { turn_id: 1 },
                AgentEvent::ToolCallStart {
                    call_id: "c1".into(),
                    name: "run_cmd".into(),
                    input: serde_json::json!({"cmd": "ls"}),
                },
                AgentEvent::ToolCallEnd {
                    call_id: "c1".into(),
                    output: ToolOutput::error("command not found"),
                },
                AgentEvent::TurnEnd {
                    turn_id: 1,
                    outcome: TurnOutcome::Completed {
                        text: "done".into(),
                    },
                },
            ],
            1_700_000_000_000_000_000,
        );
        let tool = spans
            .iter()
            .find(|s| s.name == "execute_tool run_cmd")
            .unwrap();
        assert_eq!(tool.status_code, OtlpStatusCode::Error);
        assert_eq!(
            attr(tool, "error.type"),
            Some(&OtValue::Str("tool_error".into()))
        );
    }

    #[test]
    fn compaction_event_sets_flag_on_following_chat() {
        let mut m = mapper();
        let spans = run_events(
            &mut m,
            &[
                AgentEvent::TurnStart { turn_id: 1 },
                AgentEvent::Compaction {
                    event: fox_agent_core::CompactionEvent {
                        trigger: fox_agent_core::CompactionTrigger::TokenBudget,
                        removed_messages: 10,
                        kept_messages: 2,
                        summary_chars: 300,
                    },
                },
                AgentEvent::ModelMessageStart {
                    message_id: "m1".into(),
                },
                AgentEvent::ModelMessageEnd {
                    message_id: "m1".into(),
                },
                AgentEvent::TurnEnd {
                    turn_id: 1,
                    outcome: TurnOutcome::Completed {
                        text: "done".into(),
                    },
                },
            ],
            1_700_000_000_000_000_000,
        );
        let turn = spans
            .iter()
            .find(|s| s.name.starts_with("invoke_agent"))
            .unwrap();
        assert!(turn.events.iter().any(|e| e.name == "fox_agent.compaction"));
        let chat = spans
            .iter()
            .find(|s| s.name == "chat deepseek-chat")
            .unwrap();
        assert_eq!(
            attr(chat, "gen_ai.conversation.compacted"),
            Some(&OtValue::Bool(true))
        );
    }

    #[test]
    fn evaluation_result_attaches_to_turn_span() {
        let mut m = mapper();
        let _ = m.on_event(
            &AgentEvent::TurnStart { turn_id: 1 },
            1_700_000_000_000_000_000,
        );
        m.push_evaluation(
            EvaluationRecord {
                name: "task_quality".into(),
                score: 0.92,
                label: Some("pass".into()),
                explanation: Some("met acceptance criteria".into()),
            },
            1_700_000_000_000_500_000,
        );
        let spans = m.flush(1_700_000_000_001_000_000);
        assert_eq!(spans.len(), 1);
        let ev = spans[0]
            .events
            .iter()
            .find(|e| e.name == "gen_ai.evaluation.result")
            .expect("evaluation event should be attached");
        assert!(ev.attributes.iter().any(
            |(k, v)| k == "gen_ai.evaluation.name" && *v == OtValue::Str("task_quality".into())
        ));
        assert!(
            ev.attributes
                .iter()
                .any(|(k, v)| k == "gen_ai.evaluation.score.value" && *v == OtValue::Double(0.92))
        );
    }
}
