use async_trait::async_trait;
use fox_agent_core::{Tool, ToolContext, ToolError, ToolOutput, intent_schema_property};
use serde::Deserialize;
use serde_json::{Value, json};
use similar::{ChangeTag, TextDiff};
use std::path::Path;

const FILE_TOUCH_PREVIEW_MAX_LINES: usize = 6;
const FILE_TOUCH_PREVIEW_MAX_BYTES: usize = 240;

pub struct WriteTool;

impl WriteTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct WriteInput {
    file_path: String,
    content: String,
    #[serde(default)]
    intent: Option<String>,
}

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn description(&self) -> &str {
        "Write a file with content. Creates parent directories if needed."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["file_path", "content"],
            "properties": {
                "intent": intent_schema_property(),
                "file_path": {
                    "type": "string",
                    "description": "File path."
                },
                "content": {
                    "type": "string",
                    "description": "File content."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let params: WriteInput = serde_json::from_value(input).map_err(|e| ToolError::Message {
            message: format!("invalid write input: {e}"),
        })?;

        let path = ctx.resolve_path(Path::new(&params.file_path));

        // Create parent directories if needed
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| ToolError::Message {
                        message: format!("failed to create parent dir `{}`: {e}", parent.display()),
                    })?;
            }
        }

        // Check if file existed before and read old content for diff
        let existed = path.exists();
        let old_content = if existed {
            tokio::fs::read_to_string(&path).await.ok()
        } else {
            None
        };

        // Write the file
        tokio::fs::write(&path, &params.content)
            .await
            .map_err(|e| ToolError::Message {
                message: format!("failed to write `{}`: {e}", path.display()),
            })?;

        let line_count = params.content.lines().count();
        let diff = if let Some(ref old) = old_content {
            generate_diff_summary(old, &params.content)
        } else {
            generate_diff_summary("", &params.content)
        };

        let detail = build_file_touch_preview(&diff);

        if existed {
            let mut text = format!(
                "Updated {} ({} lines){}",
                params.file_path,
                line_count,
                if diff.is_empty() { "" } else { ":" }
            );
            if !diff.is_empty() {
                text.push('\n');
                text.push_str(&diff);
            }
            Ok(ToolOutput {
                text,
                is_error: false,
                json: Some(json!({
                    "file_path": params.file_path,
                    "action": "updated",
                    "line_count": line_count,
                    "diff": diff,
                    "detail": detail,
                })),
            })
        } else {
            let diff = generate_diff_summary("", &params.content);
            Ok(ToolOutput {
                text: format!("Created {} ({} lines):\n{}", params.file_path, line_count, diff),
                is_error: false,
                json: Some(json!({
                    "file_path": params.file_path,
                    "action": "created",
                    "line_count": line_count,
                    "diff": diff,
                })),
            })
        }
    }
}

/// Generate a compact diff: "42- old" / "42+ new" (max 20 lines)
fn generate_diff_summary(old: &str, new: &str) -> String {
    let diff = TextDiff::from_lines(old, new);
    let mut output = String::new();
    let mut lines_shown = 0;
    const MAX_LINES: usize = 20;

    let mut old_line = 1usize;
    let mut new_line = 1usize;

    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Equal => {
                old_line += 1;
                new_line += 1;
                continue;
            }
            ChangeTag::Delete => {
                let content = change.value().trim();
                old_line += 1;
                if content.is_empty() {
                    continue;
                }
                if lines_shown >= MAX_LINES {
                    output.push_str("...\n");
                    break;
                }
                output.push_str(&format!("{}- {}\n", old_line - 1, content));
                lines_shown += 1;
            }
            ChangeTag::Insert => {
                let content = change.value().trim();
                new_line += 1;
                if content.is_empty() {
                    continue;
                }
                if lines_shown >= MAX_LINES {
                    output.push_str("...\n");
                    break;
                }
                output.push_str(&format!("{}+ {}\n", new_line - 1, content));
                lines_shown += 1;
            }
        }
    }

    output.trim_end().to_string()
}

fn build_file_touch_preview(diff: &str) -> Option<String> {
    let trimmed = diff.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut lines = trimmed.lines();
    let mut preview = lines
        .by_ref()
        .take(FILE_TOUCH_PREVIEW_MAX_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    let mut truncated = lines.next().is_some();

    if preview.len() > FILE_TOUCH_PREVIEW_MAX_BYTES {
        preview = truncate_str(&preview, FILE_TOUCH_PREVIEW_MAX_BYTES)
            .trim_end()
            .to_string();
        truncated = true;
    }

    if truncated {
        preview.push_str("\n…");
    }

    Some(preview)
}

fn truncate_str(s: &str, max_len: usize) -> &str {
    fox_agent_core::truncate_to_bytes(s, max_len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_diff_summary_single_change() {
        let old = "hello world";
        let new = "hello rust";
        let diff = generate_diff_summary(old, new);
        assert!(diff.contains("1- hello world"));
        assert!(diff.contains("1+ hello rust"));
    }

    #[test]
    fn test_generate_diff_summary_multi_line() {
        let old = "line one\nline two\nline three";
        let new = "line one\nchanged two\nline three";
        let diff = generate_diff_summary(old, new);
        assert!(diff.contains("2- line two"));
        assert!(diff.contains("2+ changed two"));
        assert!(!diff.contains("line one"));
    }

    #[test]
    fn test_generate_diff_summary_new_file() {
        let old = "";
        let new = "line one\nline two\nline three";
        let diff = generate_diff_summary(old, new);
        assert!(diff.contains("1+ line one"));
        assert!(diff.contains("2+ line two"));
        assert!(diff.contains("3+ line three"));
    }

    #[test]
    fn test_generate_diff_summary_truncation() {
        let old = (1..=25).map(|i| format!("old line {}", i)).collect::<Vec<_>>().join("\n");
        let new = (1..=25).map(|i| format!("new line {}", i)).collect::<Vec<_>>().join("\n");
        let diff = generate_diff_summary(&old, &new);
        assert!(diff.contains("..."));
    }

    #[test]
    fn test_generate_diff_summary_empty_result() {
        let old = "same content";
        let new = "same content";
        let diff = generate_diff_summary(old, new);
        assert!(diff.is_empty());
    }
}
