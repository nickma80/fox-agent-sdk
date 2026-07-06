use async_trait::async_trait;
use fox_agent_core::{Tool, ToolContext, ToolError, ToolOutput, intent_schema_property};
use serde::Deserialize;
use serde_json::{Value, json};
use similar::{ChangeTag, TextDiff};
use std::path::Path;

const FILE_TOUCH_PREVIEW_MAX_LINES: usize = 6;
const FILE_TOUCH_PREVIEW_MAX_BYTES: usize = 240;

pub struct EditTool;

impl EditTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct EditInput {
    file_path: String,
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
    #[serde(default)]
    intent: Option<String>,
}

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "Replace text in a file. Provides exact string replacement with context display."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["file_path", "old_string", "new_string"],
            "properties": {
                "intent": intent_schema_property(),
                "file_path": {
                    "type": "string",
                    "description": "File path."
                },
                "old_string": {
                    "type": "string",
                    "description": "Text to replace."
                },
                "new_string": {
                    "type": "string",
                    "description": "Replacement text."
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Replace all matches."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let params: EditInput = serde_json::from_value(input).map_err(|e| ToolError::Message {
            message: format!("invalid edit input: {e}"),
        })?;

        if params.old_string == params.new_string {
            return Err(ToolError::Message {
                message: "old_string and new_string must be different".to_string(),
            });
        }

        let path = ctx.resolve_path(Path::new(&params.file_path));

        if !path.exists() {
            return Err(ToolError::Message {
                message: format!("File not found: {}", params.file_path),
            });
        }

        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| ToolError::Message {
                message: format!("failed to read `{}`: {e}", path.display()),
            })?;

        // Count occurrences
        let occurrences = content.matches(&params.old_string).count();

        if occurrences == 0 {
            return try_flexible_match(&content, &params.old_string, &params.file_path);
        }

        if occurrences > 1 && !params.replace_all {
            return Err(ToolError::Message {
                message: format!(
                    "old_string found {} times in the file. Either:\n\
                     1. Provide more context to make it unique, or\n\
                     2. Set replace_all: true to replace all occurrences",
                    occurrences
                ),
            });
        }

        // Perform replacement
        let new_content = if params.replace_all {
            content.replace(&params.old_string, &params.new_string)
        } else {
            content.replacen(&params.old_string, &params.new_string, 1)
        };

        // Find line number where edit starts
        let start_line = find_line_number(&content, &params.old_string);

        // Write back
        tokio::fs::write(&path, &new_content)
            .await
            .map_err(|e| ToolError::Message {
                message: format!("failed to write `{}`: {e}", path.display()),
            })?;

        // Generate a diff with line numbers
        let diff = generate_diff(&params.old_string, &params.new_string, start_line);

        // Extract context around the edit to help with consecutive edits
        let end_line = start_line + params.new_string.lines().count().saturating_sub(1);
        let context = extract_context(&new_content, start_line, end_line, 3);

        let detail = build_file_touch_preview(&diff);

        Ok(ToolOutput {
            text: format!(
                "Edited {}: replaced {} occurrence(s)\n{}\n\nContext after edit (lines {}-{}):\n{}",
                params.file_path,
                occurrences,
                diff,
                context.0,
                context.1,
                context.2
            ),
            is_error: false,
            json: Some(json!({
                "file_path": params.file_path,
                "occurrences": occurrences,
                "start_line": start_line,
                "end_line": end_line,
                "replace_all": params.replace_all,
                "diff": diff,
                "detail": detail,
            })),
        })
    }
}

/// Find the 1-based line number where a substring starts
fn find_line_number(content: &str, substring: &str) -> usize {
    if let Some(pos) = content.find(substring) {
        content[..pos].lines().count() + 1
    } else {
        1
    }
}

/// Generate a compact diff: "42- old" / "42+ new"
fn generate_diff(old: &str, new: &str, start_line: usize) -> String {
    let diff = TextDiff::from_lines(old, new);
    let mut output = String::new();

    let mut old_line = start_line;
    let mut new_line = start_line;

    for change in diff.iter_all_changes() {
        let content = change.value().trim();
        let (prefix, line_num) = match change.tag() {
            ChangeTag::Delete => {
                let num = old_line;
                old_line += 1;
                if content.is_empty() {
                    continue;
                }
                ("-", num)
            }
            ChangeTag::Insert => {
                let num = new_line;
                new_line += 1;
                if content.is_empty() {
                    continue;
                }
                ("+", num)
            }
            ChangeTag::Equal => {
                old_line += 1;
                new_line += 1;
                continue;
            }
        };

        output.push_str(&format!("{}{} {}\n", line_num, prefix, content));
    }

    if output.is_empty() {
        String::new()
    } else {
        output.trim_end().to_string()
    }
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

/// Extract lines around the edited region, returns (start_line, end_line, content)
fn extract_context(
    content: &str,
    edit_start: usize,
    edit_end: usize,
    padding: usize,
) -> (usize, usize, String) {
    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();

    // Calculate range with padding (1-indexed to 0-indexed)
    let start = edit_start.saturating_sub(padding + 1);
    let end = (edit_end + padding).min(total_lines);

    let context_lines: Vec<String> = lines[start..end]
        .iter()
        .enumerate()
        .map(|(i, line)| format!("{:>4}│ {}", start + i + 1, line))
        .collect();

    (start + 1, end, context_lines.join("\n"))
}

fn try_flexible_match(content: &str, old_string: &str, file_path: &str) -> Result<ToolOutput, ToolError> {
    // Try trimmed matching
    let trimmed = old_string.trim();
    if content.contains(trimmed) && trimmed != old_string {
        return Err(ToolError::Message {
            message: format!(
                "old_string not found exactly, but found after trimming whitespace.\n\
                 Try using the exact string from the file, including leading/trailing whitespace."
            ),
        });
    }

    // Try line-by-line matching with normalized whitespace
    let old_lines: Vec<&str> = old_string.lines().collect();
    let content_lines: Vec<&str> = content.lines().collect();

    for (i, window) in content_lines.windows(old_lines.len()).enumerate() {
        let matches = window.iter().zip(old_lines.iter()).all(|(a, b)| a.trim() == b.trim());

        if matches {
            return Err(ToolError::Message {
                message: format!(
                    "old_string not found exactly, but found with different indentation around line {}.\n\
                     Make sure to preserve the exact whitespace from the file.",
                    i + 1
                ),
            });
        }
    }

    Err(ToolError::Message {
        message: format!(
            "old_string not found in {}.\nUse the read tool to see the current file contents.",
            file_path
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_diff_single_line_change() {
        let old = "hello world";
        let new = "hello rust";
        let diff = generate_diff(old, new, 10);
        assert!(diff.contains("10- hello world"));
        assert!(diff.contains("10+ hello rust"));
    }

    #[test]
    fn test_generate_diff_multi_line() {
        let old = "line one\nline two\nline three";
        let new = "line one\nmodified two\nline three";
        let diff = generate_diff(old, new, 5);
        assert!(diff.contains("6- line two"));
        assert!(diff.contains("6+ modified two"));
        assert!(!diff.contains("line one"));
        assert!(!diff.contains("line three"));
    }

    #[test]
    fn test_generate_diff_no_changes() {
        let old = "same content";
        let new = "same content";
        let diff = generate_diff(old, new, 1);
        assert!(diff.is_empty());
    }

    #[test]
    fn test_find_line_number() {
        let content = "line 1\nline 2\nline 3\nline 4";
        assert_eq!(find_line_number(content, "line 1"), 1);
        assert_eq!(find_line_number(content, "line 2"), 2);
        assert_eq!(find_line_number(content, "line 3"), 3);
        assert_eq!(find_line_number(content, "line 4"), 4);
        assert_eq!(find_line_number(content, "not found"), 1);
    }

    #[test]
    fn test_extract_context() {
        let content = "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\nline 8\nline 9\nline 10";
        let (start, end, ctx) = extract_context(content, 5, 5, 2);
        assert_eq!(start, 3);
        assert_eq!(end, 7);
        assert!(ctx.contains("line 3"));
        assert!(ctx.contains("line 5"));
        assert!(ctx.contains("line 7"));
        assert!(!ctx.contains("line 2"));
        assert!(!ctx.contains("line 8"));
    }

    #[test]
    fn test_extract_context_at_start() {
        let content = "line 1\nline 2\nline 3\nline 4\nline 5";
        let (start, _end, ctx) = extract_context(content, 1, 1, 2);
        assert_eq!(start, 1);
        assert!(ctx.contains("line 1"));
        assert!(ctx.contains("line 3"));
    }

    #[test]
    fn test_extract_context_at_end() {
        let content = "line 1\nline 2\nline 3\nline 4\nline 5";
        let (_start, end, ctx) = extract_context(content, 5, 5, 2);
        assert_eq!(end, 5);
        assert!(ctx.contains("line 5"));
        assert!(ctx.contains("line 3"));
    }
}
