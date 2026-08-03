//! Deterministic per-turn summary extraction (no LLM calls).
//!
//! The agent emits `AgentEvent::TurnSummary` immediately before `TurnEnd` so
//! the application layer (e.g. fox-code) can render a "how was the goal
//! accomplished" panel: the user's intent, which files were created/modified,
//! key actions, failures, and a preview of the final response — instead of a
//! raw tool-call histogram.
//!
//! Everything here is best-effort and pure: the summary is derived only from
//! the turn's `Message` history, so it is cheap, deterministic and testable.

use std::collections::HashMap;
use std::sync::Arc;

use fox_agent_core::{AgentError, ContentBlock, Message, Model, Role, StreamEvent, TurnSummary};
use futures::StreamExt;
use tracing::warn;

/// Number of unique files/actions/failures kept in a summary.
const MAX_FILES: usize = 20;
const MAX_ACTIONS: usize = 12;
const MAX_FAILURES: usize = 6;
/// Truncation limits for free-text fields.
const INTENT_MAX_CHARS: usize = 200;
const RESPONSE_MAX_CHARS: usize = 200;
const ACTION_MAX_CHARS: usize = 60;
const FAILURE_MAX_CHARS: usize = 100;

/// Build a `TurnSummary` by deterministically scanning the turn's messages.
///
/// Scanning starts at the last `Role::User` message, which is treated as the
/// user intent and the beginning of this turn's transcript.
pub fn build_turn_summary(turn_id: u64, messages: &[Message], completed: bool) -> TurnSummary {
    let last_user_idx = messages
        .iter()
        .rposition(|m| m.role == Role::User)
        .unwrap_or(0);
    let turn_msgs = &messages[last_user_idx..];

    let mut summary = TurnSummary::empty(turn_id);
    summary.completed = completed;

    let mut call_id_to_name: HashMap<&str, &str> = HashMap::new();

    for msg in turn_msgs {
        match msg.role {
            Role::User => {
                let text = text_of(&msg.content);
                if summary.user_intent.is_empty() && !text.trim().is_empty() {
                    summary.user_intent = truncate(text.trim(), INTENT_MAX_CHARS);
                }
            }
            Role::Assistant => {
                for block in &msg.content {
                    match block {
                        ContentBlock::ToolUse { id, name, input } => {
                            summary.tool_call_count += 1;
                            call_id_to_name.insert(id.as_str(), name.as_str());
                            let (file, label) = describe_tool(name, input);
                            match name.as_str() {
                                "write" | "edit" | "patch" | "apply_patch" => {
                                    if let Some(ref f) = file {
                                        push_unique_capped(
                                            &mut summary.files_modified,
                                            f,
                                            MAX_FILES,
                                        );
                                    }
                                }
                                "read" => {
                                    if let Some(ref f) = file {
                                        push_unique_capped(&mut summary.files_read, f, MAX_FILES);
                                    }
                                }
                                _ => {
                                    push_unique_capped(&mut summary.actions, &label, MAX_ACTIONS);
                                }
                            }
                        }
                        ContentBlock::Text { text } if !text.trim().is_empty() => {
                            summary.response_preview = truncate(text.trim(), RESPONSE_MAX_CHARS);
                        }
                        ContentBlock::Text { .. } => {}
                        _ => {}
                    }
                }
            }
            Role::Tool => {
                for block in &msg.content {
                    if let ContentBlock::ToolResult {
                        call_id,
                        text,
                        is_error: true,
                    } = block
                    {
                        let name = call_id_to_name.get(call_id.as_str()).copied().unwrap_or("");
                        let failure = if name.is_empty() {
                            truncate(text.trim(), FAILURE_MAX_CHARS)
                        } else {
                            format!("{name}: {}", truncate(text.trim(), FAILURE_MAX_CHARS))
                        };
                        push_unique_capped(&mut summary.failures, &failure, MAX_FAILURES);
                    }
                }
            }
            Role::System => {}
        }
    }

    summary
}

// ── Helpers ──

fn text_of(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Classify a tool call into `(file_path, short_label)`.
///
/// `file_path` is `Some` for file-mutating / file-reading tools so the caller
/// can bucket it into `files_modified` / `files_read`.
fn describe_tool(name: &str, input: &serde_json::Value) -> (Option<String>, String) {
    let s = |keys: &[&str]| -> Option<String> {
        keys.iter()
            .find_map(|k| input.get(*k).and_then(serde_json::Value::as_str))
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    };

    match name {
        "write" | "edit" | "patch" | "apply_patch" => {
            let file = s(&["file_path", "path", "file", "target"]).unwrap_or_default();
            let label = if file.is_empty() {
                name.to_string()
            } else {
                format!("{name} {file}")
            };
            (Some(file), truncate(&label, ACTION_MAX_CHARS))
        }
        "read" => {
            let file = s(&["file_path", "path", "file"]).unwrap_or_default();
            let label = if file.is_empty() {
                name.to_string()
            } else {
                format!("read {file}")
            };
            (Some(file), truncate(&label, ACTION_MAX_CHARS))
        }
        "glob" => {
            let p = s(&["pattern"]).unwrap_or_default();
            (None, truncate(&format!("glob {p}"), ACTION_MAX_CHARS))
        }
        "grep" => {
            let p = s(&["pattern", "query"]).unwrap_or_default();
            (None, truncate(&format!("grep {p}"), ACTION_MAX_CHARS))
        }
        "ls" => {
            let p = s(&["path"]).unwrap_or_default();
            (None, truncate(&format!("ls {p}"), ACTION_MAX_CHARS))
        }
        "webfetch" => {
            let url = s(&["url"]).unwrap_or_default();
            (None, truncate(&format!("webfetch {url}"), ACTION_MAX_CHARS))
        }
        "websearch" => {
            let q = s(&["query"]).unwrap_or_default();
            (None, truncate(&format!("search {q}"), ACTION_MAX_CHARS))
        }
        "bash" | "run_command" => {
            let cmd = s(&["command"]).unwrap_or_default();
            (None, truncate(&format!("run {cmd}"), ACTION_MAX_CHARS))
        }
        _ => (None, truncate(name, ACTION_MAX_CHARS)),
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        // Leave room for the trailing ellipsis so the total stays <= max_chars.
        let cut: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

fn push_unique_capped(v: &mut Vec<String>, item: &str, cap: usize) {
    if item.is_empty() {
        return;
    }
    if !v.contains(&item.to_string()) && v.len() < cap {
        v.push(item.to_string());
    }
}

// ── LLM semantic enhancement (final turn only) ──

/// Max items kept per semantic array (accomplishment/changes/caveats/…).
const SEMANTIC_MAX_ITEMS: usize = 5;

/// Build the LLM prompt that produces the semantic part of a final turn
/// summary: how the goal was accomplished, concrete changes, caveats,
/// known limitations, and key decisions.
pub fn build_semantic_summary_prompt(messages: &[Message]) -> String {
    let transcript = crate::compaction::mechanical_transcript(messages);
    format!(
        "You are summarizing what an agent accomplished in the conversation turn \
         below. Output ONLY a JSON object with these fields:\n\n\
         ```json\n\
         {{\n\
           \"accomplishment\": \"<one paragraph: how the goal was accomplished, concrete and grounded in the transcript>\",\n\
           \"changes\": [\"<specific change made, e.g. 'added shapefile write support in src/shp.rs'>\", ...],\n\
           \"caveats\": [\"<thing the user should watch out for, e.g. breaking change, manual step remaining>\", ...],\n\
           \"known_limitations\": [\"<what is NOT covered / not tested / deferred>\", ...],\n\
           \"decisions\": [\"<key decision made and why>\", ...]\n\
         }}\n\
         ```\n\n\
         Rules:\n\
         - accomplishment: what was actually achieved toward the user's goal (not a tool-call listing)\n\
         - changes: only real file/content changes — do NOT list reads or searches\n\
         - caveats: things to be careful about (breaking changes, manual steps, environment-specific behavior)\n\
         - known_limitations: what is not covered, not verified, or intentionally deferred\n\
         - decisions: conclusions reached and next steps committed to\n\
         - Keep arrays to at most {SEMANTIC_MAX_ITEMS} items each. Be specific, not generic.\n\n\
         ## Conversation transcript\n{transcript}\n\n## JSON Output"
    )
}

/// Deterministically parse the LLM's semantic summary JSON into `TurnSummary`
/// semantic fields. Best-effort: leaves fields untouched on failure.
pub fn apply_semantic_output(summary: &mut TurnSummary, output: &str) {
    let Some(json_str) = extract_json(output) else {
        warn!("turn summary LLM output was not valid JSON; keeping deterministic fields");
        return;
    };
    let Ok(parsed) = serde_json::from_str::<SemanticSummaryJson>(json_str) else {
        warn!("turn summary LLM output failed to parse as JSON; keeping deterministic fields");
        return;
    };
    summary.accomplishment = parsed.accomplishment.filter(|s| !s.trim().is_empty());
    summary.changes = capped(parsed.changes);
    summary.caveats = capped(parsed.caveats);
    summary.known_limitations = capped(parsed.known_limitations);
    summary.decisions = capped(parsed.decisions);
}

/// Enhance a `TurnSummary` with the LLM semantic fields. Best-effort: on any
/// model/parse failure the summary keeps its deterministic fields untouched.
pub async fn enhance_with_llm(
    summary: &mut TurnSummary,
    messages: &[Message],
    model: &Arc<dyn Model>,
) -> Result<(), AgentError> {
    let prompt = build_semantic_summary_prompt(messages);
    let system = "You are a precise task summarizer for a coding agent.";
    let req = vec![Message::user(prompt)];
    let mut stream = model.complete(&req, &[], system, "", None).await?;

    let mut output = String::new();
    while let Some(event) = stream.next().await {
        match event {
            Ok(StreamEvent::TextDelta { text }) => output.push_str(&text),
            Ok(_) => {}
            Err(e) => {
                warn!(error = %e, "turn summary LLM stream error; keeping deterministic fields");
                break;
            }
        }
    }
    if !output.trim().is_empty() {
        apply_semantic_output(summary, &output);
    }
    Ok(())
}

#[derive(serde::Deserialize)]
struct SemanticSummaryJson {
    #[serde(default)]
    accomplishment: Option<String>,
    #[serde(default)]
    changes: Vec<String>,
    #[serde(default)]
    caveats: Vec<String>,
    #[serde(default)]
    known_limitations: Vec<String>,
    #[serde(default)]
    decisions: Vec<String>,
}

fn capped(v: Vec<String>) -> Vec<String> {
    v.into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .take(SEMANTIC_MAX_ITEMS)
        .collect()
}

/// Extract a JSON object from raw LLM output, tolerating markdown fences and
/// surrounding prose. Returns the JSON substring if found.
fn extract_json(output: &str) -> Option<&str> {
    let trimmed = output.trim();
    // Plain JSON object spanning the whole output.
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        return Some(trimmed);
    }
    // JSON inside a ```json ... ``` fence.
    if let Some(start) = trimmed.find("```json") {
        let after = &trimmed[start + 7..];
        if let Some(end) = after.find("```") {
            let candidate = after[..end].trim();
            if candidate.starts_with('{') && candidate.ends_with('}') {
                return Some(candidate);
            }
        }
    }
    None
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use fox_agent_core::ContentBlock;
    use serde_json::json;

    fn tool_msg(call_id: &str, name: &str, input: serde_json::Value) -> Message {
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: call_id.to_string(),
                name: name.to_string(),
                input,
            }],
        }
    }

    fn result_msg(call_id: &str, text: &str, is_error: bool) -> Message {
        Message::tool_result(call_id, text, is_error)
    }

    #[test]
    fn extracts_intent_files_and_actions() {
        let messages = vec![
            Message::user("给项目添加 shapefile 读写支持"),
            tool_msg("c1", "glob", json!({"pattern": "**/Cargo.toml"})),
            result_msg("c1", "ok", false),
            tool_msg("c2", "read", json!({"file_path": "src/main.rs"})),
            result_msg("c2", "ok", false),
            tool_msg(
                "c3",
                "edit",
                json!({"file_path": "Cargo.toml", "old_string": "a", "new_string": "b"}),
            ),
            result_msg("c3", "ok", false),
            tool_msg(
                "c4",
                "write",
                json!({"file_path": "src/shp.rs", "content": "..."}),
            ),
            result_msg("c4", "ok", false),
            Message::assistant("已完成：新增 src/shp.rs，修改 Cargo.toml 依赖"),
        ];

        let s = build_turn_summary(1, &messages, true);

        assert!(s.completed);
        assert_eq!(s.user_intent, "给项目添加 shapefile 读写支持");
        assert_eq!(s.tool_call_count, 4);
        assert_eq!(s.files_modified, vec!["Cargo.toml", "src/shp.rs"]);
        assert_eq!(s.files_read, vec!["src/main.rs"]);
        assert!(s.actions.iter().any(|a| a.starts_with("glob")));
        assert!(s.response_preview.starts_with("已完成"));
        assert!(s.failures.is_empty());
    }

    #[test]
    fn collects_failures_with_tool_name() {
        let messages = vec![
            Message::user("读一下配置"),
            tool_msg("c1", "read", json!({"file_path": "config.yml"})),
            result_msg("c1", "file not found", true),
        ];

        let s = build_turn_summary(2, &messages, false);

        assert!(!s.completed);
        assert_eq!(s.tool_call_count, 1);
        assert_eq!(s.failures.len(), 1);
        assert!(s.failures[0].starts_with("read:"), "got {:?}", s.failures);
    }

    #[test]
    fn caps_and_dedups_long_outputs() {
        let long = "x".repeat(10_000);
        let messages = vec![
            Message::user(&long),
            tool_msg("c1", "websearch", json!({"query": &long})),
            result_msg("c1", "ok", false),
            Message::assistant(&long),
        ];

        let s = build_turn_summary(3, &messages, true);

        assert!(s.user_intent.chars().count() <= 200);
        assert!(s.response_preview.chars().count() <= 200);
        assert!(s.actions[0].chars().count() <= 60);
    }

    #[test]
    fn empty_messages_yield_empty_summary() {
        let s = build_turn_summary(4, &[], true);
        assert_eq!(s.turn_id, 4);
        assert!(s.user_intent.is_empty());
        assert_eq!(s.tool_call_count, 0);
    }

    #[test]
    fn scans_only_the_last_user_turn() {
        let messages = vec![
            Message::user("上一轮的问题"),
            tool_msg("old", "read", json!({"file_path": "old.rs"})),
            result_msg("old", "ok", false),
            Message::assistant("上一轮完成"),
            Message::user("本轮新问题"),
            tool_msg("c1", "write", json!({"file_path": "new.rs", "content": ""})),
            result_msg("c1", "ok", false),
        ];

        let s = build_turn_summary(5, &messages, true);

        assert_eq!(s.user_intent, "本轮新问题");
        assert_eq!(s.files_modified, vec!["new.rs"]);
        assert_eq!(s.tool_call_count, 1);
        assert!(!s.response_preview.starts_with("上一轮"));
    }

    #[test]
    fn applies_semantic_output_from_plain_json() {
        let mut s = build_turn_summary(6, &[Message::user("hi")], true);
        apply_semantic_output(
            &mut s,
            r#"{"accomplishment":"Added shapefile read/write support","changes":["wrote src/shp.rs","edited Cargo.toml"],"caveats":["needs geozero dep"],"known_limitations":["no write tests yet"],"decisions":["use geozero over shapefile"]}"#,
        );
        assert_eq!(
            s.accomplishment.as_deref(),
            Some("Added shapefile read/write support")
        );
        assert_eq!(s.changes, vec!["wrote src/shp.rs", "edited Cargo.toml"]);
        assert_eq!(s.caveats, vec!["needs geozero dep"]);
        assert_eq!(s.known_limitations, vec!["no write tests yet"]);
        assert_eq!(s.decisions, vec!["use geozero over shapefile"]);
    }

    #[test]
    fn applies_semantic_output_from_fenced_json() {
        let mut s = build_turn_summary(7, &[Message::user("hi")], true);
        apply_semantic_output(
            &mut s,
            "Here you go:\n```json\n{\"accomplishment\": \"done\", \"changes\": [\"a\"]}\n```\n",
        );
        assert_eq!(s.accomplishment.as_deref(), Some("done"));
        assert_eq!(s.changes, vec!["a"]);
    }

    #[test]
    fn invalid_semantic_output_leaves_fields_untouched() {
        let mut s = build_turn_summary(8, &[Message::user("hi")], true);
        assert!(s.accomplishment.is_none());
        apply_semantic_output(&mut s, "sorry, no summary");
        assert!(s.accomplishment.is_none());
        assert!(s.changes.is_empty());
        apply_semantic_output(&mut s, "```json\n{\"accomplishment\": 42}\n```");
        assert!(s.accomplishment.is_none());
    }

    #[test]
    fn semantic_arrays_are_trimmed_capped_and_deduped() {
        let mut s = build_turn_summary(9, &[Message::user("hi")], true);
        let items: Vec<String> = (0..8).map(|i| format!("item {i}")).collect();
        apply_semantic_output(
            &mut s,
            &format!(r#"{{"accomplishment":"ok","changes":{items:?},"caveats":["  padded  "]}}"#),
        );
        assert_eq!(s.changes.len(), 5, "must be capped to {SEMANTIC_MAX_ITEMS}");
        assert_eq!(s.caveats, vec!["padded"]);
    }
}
