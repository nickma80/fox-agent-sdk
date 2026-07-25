//! Sub-agent runtime for isolated task exploration (Phase 3).
//!
//! When the main agent needs to perform a high-noise exploration task
//! (multi-round search, large file traversal, MCP browser/filesystem work),
//! it dispatches a [`SubagentTask`] to a forked sub-agent. The sub-agent
//! runs in its own context and returns only a structured [`SubagentSummary`]
//! (a few hundred tokens). Raw intermediate results are stored as artifacts.

use fox_agent_core::{
    AgentError, AgentEvent, Message, Model, Role, SubagentOutcome,
    SubagentSummary, SubagentTask, ToolDefinition, TokenUsage,
};
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, info, warn};

use crate::artifact_store::ArtifactStore;
use crate::harness::Harness;

/// Runtime that executes a [`SubagentTask`] in an isolated context.
///
/// # Isolation guarantees
/// - The sub-agent has its own `Harness` (forked session state).
/// - The sub-agent has its own `Model` (forked from parent).
/// - Tool outputs from the sub-agent are NOT written into the main agent's
///   message stream.
/// - Only the [`SubagentSummary`] is returned to the caller.
pub struct SubagentRuntime;

impl SubagentRuntime {
    /// Run a sub-agent task and return a structured summary.
    pub async fn run(
        task: SubagentTask,
        parent_harness: &Harness,
        parent_model: Arc<dyn Model>,
        artifact_store: Arc<dyn ArtifactStore>,
    ) -> Result<SubagentSummary, AgentError> {
        let start = Instant::now();
        let task_id = task.task_id.clone();
        info!(
            task_id = %task_id,
            objective = %task.objective,
            max_turns = task.max_turns,
            "sub-agent task started"
        );

        // Fork model and harness
        let model = parent_model.fork();
        let harness = parent_harness.fork_session_state().await;

        // Inject task context as the first user message
        let task_prompt = build_task_prompt(&task);
        harness.push_message(Message::user(&task_prompt)).await;

        // Build tool definitions
        let all_tool_defs = harness.tool_definitions().await;
        let tool_defs: Vec<ToolDefinition> = if task.tools.is_empty() {
            all_tool_defs
        } else {
            all_tool_defs
                .into_iter()
                .filter(|td| task.tools.contains(&td.name))
                .collect()
        };

        if tool_defs.is_empty() {
            return Ok(SubagentSummary {
                task_id: task.task_id.clone(),
                objective: task.objective.clone(),
                outcome: SubagentOutcome::Error("no tools available".into()),
                findings: Vec::new(),
                evidence_refs: Vec::new(),
                recommendations: Vec::new(),
                uncertainties: vec!["Sub-agent had no tools to work with".into()],
                next_queries: Vec::new(),
                token_usage: None,
                turns_used: 0,
                elapsed_secs: start.elapsed().as_secs(),
            });
        }

        // Build system prompt
        let system_prompt = build_subagent_system_prompt(&task);

        // Run turns
        let max_turns = task.max_turns.max(1).min(50);
        let mut turns_used: u32 = 0;
        let mut total_token_usage: Option<TokenUsage> = None;
        let mut last_error: Option<String> = None;

        let timeout = tokio::time::Duration::from_secs(task.timeout_secs.max(30));
        let run_future = async {
            for turn in 0..max_turns {
                turns_used = turn + 1;
                debug!(task_id = %task_id, turn = turns_used, "sub-agent turn");

                let session_messages = harness.session_messages().await;
                if session_messages.is_empty() {
                    break;
                }

                let (split_prompt, _ctx_info) = harness
                    .build_system_prompt_split(None, None, None)
                    .await;
                let dynamic_str = &split_prompt.static_part;

                let stream = match model
                    .complete(
                        &session_messages,
                        &tool_defs,
                        &system_prompt,
                        &dynamic_str,
                        None,
                    )
                    .await
                {
                    Ok(s) => s,
                    Err(e) => {
                        warn!(task_id = %task_id, error = %e, "sub-agent model error");
                        last_error = Some(e.to_string());
                        break;
                    }
                };

                // Collect streaming response
                use futures::StreamExt;
                let mut stream = Box::pin(stream);
                let mut full_text = String::new();
                let mut tool_uses: Vec<(String, String, serde_json::Value)> = Vec::new();

                while let Some(event) = stream.next().await {
                    match event {
                        Ok(fox_agent_core::StreamEvent::TextDelta { text }) => {
                            full_text.push_str(&text);
                        }
                        Ok(fox_agent_core::StreamEvent::ToolUse {
                            id,
                            name,
                            input,
                        }) => {
                            tool_uses.push((id.clone(), name.clone(), input.clone()));
                        }
                        Ok(fox_agent_core::StreamEvent::Usage { usage }) => {
                            total_token_usage = Some(match total_token_usage.take() {
                                Some(mut acc) => {
                                    acc.input_tokens += usage.input_tokens;
                                    acc.output_tokens += usage.output_tokens;
                                    acc
                                }
                                None => usage,
                            });
                        }
                        Ok(fox_agent_core::StreamEvent::MessageStop { .. }) => {
                            break;
                        }
                        Err(e) => {
                            warn!(task_id = %task_id, error = %e, "sub-agent stream error");
                            last_error = Some(e.to_string());
                            break;
                        }
                        _ => {}
                    }
                }

                // Save assistant message
                let assistant_msg = if tool_uses.is_empty() {
                    Message::assistant(&full_text)
                } else {
                    use fox_agent_core::ContentBlock;
                    let mut blocks = Vec::new();
                    if !full_text.is_empty() {
                        blocks.push(ContentBlock::Text { text: full_text.clone() });
                    }
                    for (id, name, input) in &tool_uses {
                        blocks.push(ContentBlock::ToolUse { id: id.clone(), name: name.clone(), input: input.clone() });
                    }
                    Message {
                        role: Role::Assistant,
                        content: blocks,
                    }
                };
                harness.push_message(assistant_msg).await;

                let had_tool_calls = !tool_uses.is_empty();

                // Execute tools
                for (call_id, name, input) in tool_uses {
                    let ctx = fox_agent_core::ToolContext {
                        session_id: harness.session_id().to_string(),
                        message_id: String::new(),
                        tool_call_id: call_id.clone(),
                        working_dir: harness.session_working_dir().cloned(),
                        execution_mode: fox_agent_core::ToolExecutionMode::Foreground,
                        graceful_shutdown_requested: false,
                        progress_tx: None,
                    };

                    let result = harness.execute_tool_with_cache(&name, input.clone(), ctx).await;
                    match result {
                        Ok(output) => {
                            let text = output.text.clone();
                            let artifact_id = if text.len() > 2000 {
                                store_subagent_artifact(
                                    &artifact_store,
                                    harness.session_id(),
                                    &task_id,
                                    &name,
                                    &text,
                                ).await.ok()
                            } else {
                                None
                            };

                            let result_text = if let Some(aid) = &artifact_id {
                                format!(
                                    "[artifact:{} | tool:{}]\n{}",
                                    aid,
                                    name,
                                    &text[..text.len().min(500)]
                                )
                            } else {
                                text
                            };

                            harness.push_message(Message::tool_result(
                                &call_id,
                                &result_text,
                                output.is_error,
                            )).await;
                        }
                        Err(e) => {
                            let err_text = format!("tool error: {e}");
                            harness.push_message(Message::tool_result(
                                &call_id,
                                &err_text,
                                true,
                            )).await;
                        }
                    }
                }

                // If no tool calls, the sub-agent is done
                if !had_tool_calls {
                    break;
                }
            }

            SubagentSummary {
                task_id: task.task_id.clone(),
                objective: task.objective.clone(),
                outcome: SubagentOutcome::Completed,
                findings: Vec::new(),
                evidence_refs: Vec::new(),
                recommendations: Vec::new(),
                uncertainties: last_error
                    .map(|e| vec![e])
                    .unwrap_or_default(),
                next_queries: Vec::new(),
                token_usage: total_token_usage.map(|u| {
                    serde_json::json!({
                        "input_tokens": u.input_tokens,
                        "output_tokens": u.output_tokens,
                    })
                }),
                turns_used,
                elapsed_secs: start.elapsed().as_secs(),
            }
        };

        let result = tokio::time::timeout(timeout, run_future).await;

        match result {
            Ok(mut summary) => {
                summary.elapsed_secs = start.elapsed().as_secs();
                info!(
                    task_id = %task_id,
                    turns = summary.turns_used,
                    elapsed = summary.elapsed_secs,
                    outcome = ?summary.outcome,
                    "sub-agent task finished"
                );
                Ok(summary)
            }
            Err(_elapsed) => {
                warn!(task_id = %task_id, "sub-agent task timed out");
                Ok(SubagentSummary {
                    task_id: task.task_id.clone(),
                    objective: task.objective.clone(),
                    outcome: SubagentOutcome::TimeoutReached,
                    findings: Vec::new(),
                    evidence_refs: Vec::new(),
                    recommendations: Vec::new(),
                    uncertainties: vec!["Task exceeded time limit".into()],
                    next_queries: Vec::new(),
                    token_usage: None,
                    turns_used,
                    elapsed_secs: start.elapsed().as_secs(),
                })
            }
        }
    }
}

// ── Helpers ──

fn build_task_prompt(task: &SubagentTask) -> String {
    let mut prompt = String::new();
    prompt.push_str("You are working as a sub-agent on the following task.\n\n");
    prompt.push_str(&format!("**Objective**: {}\n\n", task.objective));
    if !task.context.is_empty() {
        prompt.push_str(&format!("**Context**: {}\n\n", task.context));
    }
    prompt.push_str(
        "**Instructions**:\n\
         - Explore thoroughly using the tools available.\n\
         - When you have gathered enough information, stop making tool calls \
           and write a final text response.\n\
         - Your final response should summarise:\n\
           1. What you found (key findings)\n\
           2. Any evidence references (mention artifact IDs if applicable)\n\
           3. Recommendations for the main agent\n\
           4. Anything you are uncertain about\n\
           5. Suggested next steps\n\n\
         Begin your exploration now."
    );
    prompt
}

fn build_subagent_system_prompt(task: &SubagentTask) -> String {
    format!(
        "You are a focused research sub-agent. Your job is to explore \
         a specific question and report back with a concise summary.\n\
         Task: {}\n\
         Be thorough but efficient. Use tools to gather information. \
         When done, write a final text response summarising your findings.",
        task.objective
    )
}

async fn store_subagent_artifact(
    store: &Arc<dyn ArtifactStore>,
    session_id: &str,
    task_id: &str,
    tool_name: &str,
    text: &str,
) -> Result<String, String> {
    let result = store
        .put_text(
            session_id,
            fox_agent_core::ArtifactProducer::Subagent {
                task_id: task_id.to_string(),
            },
            fox_agent_core::ArtifactType::SubagentIntermediate,
            fox_agent_core::ArtifactRetentionClass::Referenced,
            text.to_string(),
            serde_json::json!({
                "task_id": task_id,
                "tool_name": tool_name,
            }),
        )
        .await?;
    Ok(result.record.artifact_id)
}

// ── SubagentTool (callable by main agent) ──

use fox_agent_core::AgentEventTx;

/// Tool that the main agent can call to delegate exploration to a sub-agent.
pub struct SubagentTool {
    pub parent_harness: Harness,
    pub parent_model: Arc<dyn Model>,
    pub artifact_store: Arc<dyn ArtifactStore>,
    pub event_tx: Option<AgentEventTx>,
}

#[async_trait::async_trait]
impl fox_agent_core::Tool for SubagentTool {
    fn name(&self) -> &str {
        "subagent"
    }

    fn description(&self) -> &str {
        "Delegate a research/exploration task to a sub-agent that runs in \
         isolation. The sub-agent explores independently and returns a \
         concise summary. Use this for tasks like searching the codebase, \
         reading many files, or fetching large web pages."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "objective": {
                    "type": "string",
                    "description": "One sentence describing what the sub-agent should accomplish"
                },
                "context": {
                    "type": "string",
                    "description": "Background information to help the sub-agent understand the task"
                },
                "expected_output": {
                    "type": "string",
                    "description": "What kind of output you expect (e.g. 'list of files', 'summary of findings')"
                }
            },
            "required": ["objective"]
        })
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        _ctx: fox_agent_core::ToolContext,
    ) -> Result<fox_agent_core::ToolOutput, fox_agent_core::ToolError> {
        let objective = input
            .get("objective")
            .and_then(|v| v.as_str())
            .unwrap_or("explore")
            .to_string();
        let context = input
            .get("context")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let expected_output = input
            .get("expected_output")
            .and_then(|v| v.as_str())
            .unwrap_or("summary")
            .to_string();

        let task_id = format!("subagent_{}", uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("task"));

        let task = SubagentTask {
            task_id: task_id.clone(),
            objective: format!("{objective}\nExpected output: {expected_output}"),
            context,
            tools: Vec::new(),
            max_turns: 20,
            timeout_secs: 120,
        };

        // Emit started event
        if let Some(tx) = &self.event_tx {
            let _ = tx
                .send(AgentEvent::SubagentTaskStarted {
                    task_id: task_id.clone(),
                    objective: task.objective.clone(),
                    max_turns: task.max_turns,
                })
                .await;
        }

        let summary = SubagentRuntime::run(
            task,
            &self.parent_harness,
            self.parent_model.clone(),
            self.artifact_store.clone(),
        )
        .await
        .map_err(|e| fox_agent_core::ToolError::Message {
            message: format!("sub-agent failed: {e}"),
        })?;

        let summary_text = summary.format_for_main_context();

        // Emit completed event
        if let Some(tx) = &self.event_tx {
            let _ = tx
                .send(AgentEvent::SubagentTaskCompleted {
                    task_id: task_id.clone(),
                    outcome: format!("{:?}", summary.outcome),
                    findings_count: summary.findings.len() as u32,
                    evidence_count: summary.evidence_refs.len() as u32,
                    turns_used: summary.turns_used,
                    elapsed_secs: summary.elapsed_secs,
                    summary_text: summary_text.clone(),
                })
                .await;
        }

        // Fire SubagentStop hook
        {
            let hm = self.parent_harness.hook_manager.read().await;
            let ctx = crate::hooks::HookContext {
                session_id: self.parent_harness.session_id(),
                event: "SubagentStop",
                working_dir: self
                    .parent_harness
                    .session_working_dir()
                    .and_then(|p| p.to_str())
                    .unwrap_or("."),
                tool_name: Some("subagent"),
                tool_input: None,
                tool_output: Some(summary_text.clone()),
                hook_event_name: "SubagentStop",
            };
            let _ = hm.execute(crate::hooks::HookEvent::SubagentStop, ctx).await;
        }

        Ok(fox_agent_core::ToolOutput {
            text: summary_text,
            is_error: matches!(summary.outcome, SubagentOutcome::Error(_) | SubagentOutcome::TimeoutReached),
            json: Some(serde_json::json!({
                "task_id": summary.task_id,
                "outcome": format!("{:?}", summary.outcome),
                "findings": summary.findings,
                "evidence_count": summary.evidence_refs.len(),
                "turns_used": summary.turns_used,
                "elapsed_secs": summary.elapsed_secs,
            })),
        })
    }
}
