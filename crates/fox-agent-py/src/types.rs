//! Python type conversions: convert Rust types → Python dicts/objects.
//!
//! These conversions are the bridge between the Rust Agent event stream
//! and the Python list consumed by `events = agent.run(msg)`.
//!
//! Also provides reverse conversion (Python dict → AgentEvent) for
//! evaluation tools (BehaviorRuleEngine, EvalReport).

use fox_agent_core::{AgentEvent, PermissionDecision, ToolOutput, TurnOutcome};
use pyo3::prelude::*;
use pyo3::types::PyDict;

/// Convert a [`ToolOutput`] to a Python dict.
pub fn tool_output_to_py(py: Python<'_>, output: &ToolOutput) -> Py<PyDict> {
    let d = PyDict::new(py);
    d.set_item("text", &output.text).ok();
    d.set_item("is_error", output.is_error).ok();
    if let Some(ref json) = output.json {
        d.set_item("json", json.to_string()).ok();
    }
    d.into()
}

/// Convert a [`TurnOutcome`] to a Python dict.
pub fn turn_outcome_to_py(py: Python<'_>, outcome: &TurnOutcome) -> Py<PyDict> {
    let d = PyDict::new(py);
    match outcome {
        TurnOutcome::Completed { text } => {
            d.set_item("type", "completed").ok();
            d.set_item("text", text).ok();
        }
        TurnOutcome::Cancelled => {
            d.set_item("type", "cancelled").ok();
        }
        TurnOutcome::RequiresUserDecision { request } => {
            d.set_item("type", "requires_user_decision").ok();
            d.set_item("request_id", &request.request_id).ok();
            d.set_item("tool_name", &request.tool_name).ok();
            d.set_item("prompt", &request.prompt).ok();
        }
        TurnOutcome::Failed { error } => {
            d.set_item("type", "failed").ok();
            d.set_item("error", error.to_string()).ok();
        }
    }
    d.into()
}

/// Convert a [`PermissionDecision`] into its string representation for Python.
#[allow(dead_code)]
pub fn permission_decision_to_py_str(decision: &PermissionDecision) -> &'static str {
    match decision {
        PermissionDecision::Allow => "allow",
        PermissionDecision::Deny { .. } => "deny",
    }
}

/// Build a single AgentEvent into a Python dict.
///
/// Returns `Some(dict)` for events that should be yielded to Python,
/// or `None` for internal/framework events that don't need to surface.
pub fn agent_event_to_py(py: Python<'_>, event: &AgentEvent) -> Option<Py<PyDict>> {
    let d = PyDict::new(py);

    match event {
        AgentEvent::TurnStart { turn_id } => {
            d.set_item("type", "turn_start").ok();
            d.set_item("turn_id", *turn_id).ok();
        }

        AgentEvent::TurnEnd { turn_id, outcome } => {
            d.set_item("type", "turn_end").ok();
            d.set_item("turn_id", *turn_id).ok();
            d.set_item("outcome", turn_outcome_to_py(py, outcome)).ok();
        }

        AgentEvent::ModelTextDelta { text } => {
            d.set_item("type", "text_delta").ok();
            d.set_item("text", text).ok();
        }

        AgentEvent::ModelThinkingDelta { text } => {
            d.set_item("type", "thinking_delta").ok();
            d.set_item("text", text).ok();
        }

        AgentEvent::ModelUsage { usage } => {
            d.set_item("type", "usage").ok();
            d.set_item("input", usage.input_tokens).ok();
            d.set_item("output", usage.output_tokens).ok();
            d.set_item("total", usage.total_tokens).ok();
            if let Some(cache_read) = usage.cache_read_input_tokens {
                d.set_item("cache_read", cache_read).ok();
            }
        }

        AgentEvent::ToolCallStart {
            call_id,
            name,
            input,
            ..
        } => {
            d.set_item("type", "tool_start").ok();
            d.set_item("call_id", call_id).ok();
            d.set_item("name", name).ok();
            d.set_item("input", serde_json::to_string(input).unwrap_or_default())
                .ok();
        }

        AgentEvent::ToolCallEnd { call_id, output } => {
            d.set_item("type", "tool_end").ok();
            d.set_item("call_id", call_id).ok();
            d.set_item("output", tool_output_to_py(py, output)).ok();
        }

        AgentEvent::PermissionRequest {
            request_id,
            tool_name,
            prompt,
            ..
        } => {
            d.set_item("type", "permission_request").ok();
            d.set_item("request_id", request_id).ok();
            d.set_item("tool_name", tool_name).ok();
            d.set_item("prompt", prompt).ok();
        }

        AgentEvent::Error { error } => {
            d.set_item("type", "error").ok();
            d.set_item("error", error.to_string()).ok();
        }

        AgentEvent::ToolExecutionProgress {
            call_id,
            tool_name,
            elapsed_secs,
            ..
        } => {
            d.set_item("type", "tool_progress").ok();
            d.set_item("call_id", call_id).ok();
            d.set_item("tool_name", tool_name).ok();
            d.set_item("elapsed_secs", *elapsed_secs).ok();
        }

        AgentEvent::ArtifactStored {
            artifact_id,
            tool_name: _tn,
            size_bytes,
            ..
        } => {
            d.set_item("type", "artifact_stored").ok();
            d.set_item("artifact_id", artifact_id).ok();
            d.set_item("size_bytes", *size_bytes).ok();
        }

        AgentEvent::ArtifactRead {
            artifact_id,
            returned_chars,
            tool_name,
            ..
        } => {
            d.set_item("type", "artifact_read").ok();
            d.set_item("artifact_id", artifact_id).ok();
            d.set_item("returned_chars", *returned_chars).ok();
            d.set_item("tool_name", tool_name).ok();
        }

        AgentEvent::McpServerConnected { server_name } => {
            d.set_item("type", "mcp_connected").ok();
            d.set_item("server_name", server_name).ok();
        }

        AgentEvent::McpServerDisconnected {
            server_name, error, ..
        } => {
            d.set_item("type", "mcp_disconnected").ok();
            d.set_item("server_name", server_name).ok();
            d.set_item("error", error.as_deref().unwrap_or("")).ok();
        }

        AgentEvent::PlanProgress {
            completed, total, ..
        } => {
            d.set_item("type", "plan_progress").ok();
            d.set_item("completed", *completed).ok();
            d.set_item("total", *total).ok();
        }

        AgentEvent::TurnSummary { summary } => {
            d.set_item("type", "turn_summary").ok();
            d.set_item("turn_id", summary.turn_id).ok();
            d.set_item("user_intent", &summary.user_intent).ok();
            d.set_item("files_modified", summary.files_modified.clone())
                .ok();
            d.set_item("files_read", summary.files_read.clone()).ok();
            d.set_item("actions", summary.actions.clone()).ok();
            d.set_item("failures", summary.failures.clone()).ok();
            d.set_item("response_preview", &summary.response_preview)
                .ok();
            d.set_item("tool_call_count", summary.tool_call_count).ok();
            d.set_item("completed", summary.completed).ok();
            if let Some(ref a) = summary.accomplishment {
                d.set_item("accomplishment", a).ok();
            }
            d.set_item("changes", summary.changes.clone()).ok();
            d.set_item("caveats", summary.caveats.clone()).ok();
            d.set_item("known_limitations", summary.known_limitations.clone())
                .ok();
            d.set_item("decisions", summary.decisions.clone()).ok();
        }

        // Internal events — don't surface to Python consumer
        AgentEvent::ToolInputDelta { .. }
        | AgentEvent::ModelMessageStart { .. }
        | AgentEvent::ModelMessageEnd { .. }
        | AgentEvent::WaitingForModel { .. }
        | AgentEvent::Compaction { .. }
        | AgentEvent::SoftInterruptInjected { .. }
        | AgentEvent::MemoryStateChanged { .. }
        | AgentEvent::MemoryInjected { .. }
        | AgentEvent::RoutingDecision { .. }
        | AgentEvent::ArtifactGc { .. }
        | AgentEvent::SubagentTaskStarted { .. }
        | AgentEvent::SubagentTaskCompleted { .. } => {
            return None;
        }
    }

    Some(d.into())
}

/// Reconstruct an [`AgentEvent`] from a Python event dict (reverse of [`agent_event_to_py`]).
///
/// Only reconstructs fields needed by evaluation rules (BehaviorRuleEngine, EvalReport).
/// Returns `None` for internal events or unparseable dicts.
pub fn py_event_to_agent_event(dict: &Bound<'_, PyDict>) -> Option<AgentEvent> {
    let event_type: String = dict.get_item("type").ok()??.extract().ok()?;
    match event_type.as_str() {
        "turn_start" => {
            let turn_id: u64 = dict.get_item("turn_id").ok()??.extract().ok()?;
            Some(AgentEvent::TurnStart { turn_id })
        }
        "turn_end" => {
            let turn_id: u64 = dict.get_item("turn_id").ok()??.extract().ok()?;
            let outcome = TurnOutcome::Completed {
                text: String::new(),
            };
            Some(AgentEvent::TurnEnd { turn_id, outcome })
        }
        "text_delta" => {
            let text: String = dict.get_item("text").ok()??.extract().ok()?;
            Some(AgentEvent::ModelTextDelta { text })
        }
        "tool_start" => {
            let call_id: String = dict.get_item("call_id").ok()??.extract().ok()?;
            let name: String = dict.get_item("name").ok()??.extract().ok()?;
            let input_str: String = dict
                .get_item("input")
                .ok()
                .flatten()
                .and_then(|v| v.extract().ok())
                .unwrap_or_default();
            let input: serde_json::Value =
                serde_json::from_str(&input_str).unwrap_or(serde_json::Value::Null);
            Some(AgentEvent::ToolCallStart {
                call_id,
                name,
                input,
            })
        }
        "tool_end" => {
            let call_id: String = dict.get_item("call_id").ok()??.extract().ok()?;
            let output = match dict.get_item("output") {
                Ok(Some(val)) => {
                    let text = val
                        .getattr("text")
                        .ok()
                        .and_then(|v| v.extract().ok())
                        .unwrap_or_default();
                    let is_error = val
                        .getattr("is_error")
                        .ok()
                        .and_then(|v| v.extract().ok())
                        .unwrap_or(false);
                    ToolOutput {
                        text,
                        is_error,
                        json: None,
                    }
                }
                _ => ToolOutput {
                    text: String::new(),
                    is_error: false,
                    json: None,
                },
            };
            Some(AgentEvent::ToolCallEnd { call_id, output })
        }
        "error" => Some(AgentEvent::Error {
            error: fox_agent_core::AgentError::PermissionDenied {
                reason: "unknown".into(),
            },
        }),
        "compaction" | "compaction_triggered" => Some(AgentEvent::Compaction {
            event: fox_agent_core::CompactionEvent {
                trigger: fox_agent_core::CompactionTrigger::TokenBudget,
                removed_messages: 0,
                kept_messages: 0,
                summary_chars: 0,
            },
        }),
        "artifact_read" => {
            let artifact_id: String = dict.get_item("artifact_id").ok()??.extract().ok()?;
            Some(AgentEvent::ArtifactRead {
                artifact_id,
                tool_name: String::new(),
                returned_chars: 0,
                offset_chars: 0,
                limit_chars: 0,
                source_tool_name: None,
                artifact_type: None,
                server_name: None,
                server_kind: None,
                transport: None,
                original_tool_name: None,
            })
        }
        "subagent_started" => Some(AgentEvent::SubagentTaskStarted {
            task_id: String::new(),
            objective: String::new(),
            max_turns: 1,
        }),
        "subagent_completed" => Some(AgentEvent::SubagentTaskCompleted {
            task_id: String::new(),
            outcome: String::new(),
            findings_count: 0,
            evidence_count: 0,
            turns_used: 0,
            elapsed_secs: 0u64,
            summary_text: String::new(),
        }),
        _ => None,
    }
}
