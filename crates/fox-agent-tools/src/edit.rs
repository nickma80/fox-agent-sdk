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

impl Default for EditTool {
    fn default() -> Self {
        Self::new()
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
    #[expect(dead_code)]
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

        // Count occurrences (exact match)
        let mut old_string = params.old_string;
        let mut occurrences = content.matches(&old_string).count();
        let mut whitespace_corrected = false;

        if occurrences == 0 {
            // Try flexible match — returns corrected old_string on success
            old_string = try_flexible_match(&content, &old_string)?;
            whitespace_corrected = true;
            occurrences = content.matches(&old_string).count();
            if occurrences == 0 {
                return Err(ToolError::Message {
                    message: format!(
                        "old_string not found in {}. Use the read tool to see the current file contents.",
                        params.file_path
                    ),
                });
            }
            if occurrences > 1 && !params.replace_all {
                return Err(ToolError::Message {
                    message: format!(
                        "old_string found {} times in the file (whitespace auto-corrected). Either:\n\
                         1. Provide more context to make it unique, or\n\
                         2. Set replace_all: true to replace all occurrences",
                        occurrences
                    ),
                });
            }
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
            content.replace(&old_string, &params.new_string)
        } else {
            content.replacen(&old_string, &params.new_string, 1)
        };

        // Find line number where edit starts
        let start_line = find_line_number(&content, &old_string);

        // Write back
        tokio::fs::write(&path, &new_content)
            .await
            .map_err(|e| ToolError::Message {
                message: format!("failed to write `{}`: {e}", path.display()),
            })?;

        // Generate a diff with line numbers
        let diff = generate_diff(&old_string, &params.new_string, start_line);

        // Extract context around the edit to help with consecutive edits
        let end_line = start_line + params.new_string.lines().count().saturating_sub(1);
        let context = extract_context(&new_content, start_line, end_line, 3);

        let detail = build_file_touch_preview(&diff);

        let note = if whitespace_corrected {
            "(whitespace auto-corrected in old_string)\n"
        } else {
            ""
        };

        Ok(ToolOutput {
            text: format!(
                "Edited {}: {}replaced {} occurrence(s)\n{}\n\nContext after edit (lines {}-{}):\n{}",
                params.file_path, note, occurrences, diff, context.0, context.1, context.2
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

fn try_flexible_match(content: &str, old_string: &str) -> Result<String, ToolError> {
    // Try trimmed matching (single-line)
    let trimmed = old_string.trim();
    if content.contains(trimmed) && trimmed != old_string {
        // Extract the actual content from the file with correct whitespace
        for line in content.lines() {
            if line.trim() == trimmed {
                return Ok(line.to_string());
            }
        }
        // Fallback: use trimmed version
        return Ok(trimmed.to_string());
    }

    // Try line-by-line matching with normalized whitespace
    let old_lines: Vec<&str> = old_string.lines().collect();
    let content_lines: Vec<&str> = content.lines().collect();
    if old_lines.is_empty() {
        return Err(ToolError::Message {
            message: "old_string is empty; nothing to replace".to_string(),
        });
    }

    let mut best: Option<(usize, usize)> = None; // (start_line_1based, matched_lines)
    for (i, window) in content_lines.windows(old_lines.len()).enumerate() {
        let matched = window
            .iter()
            .zip(old_lines.iter())
            .filter(|(a, b)| a.trim() == b.trim())
            .count();
        let all_match = matched == old_lines.len();
        let exact_match = window.join("\n") == old_string;

        if all_match && !exact_match {
            // Found with different indentation — auto-correct
            return Ok(window.join("\n"));
        }

        // Track the closest approximate window for diagnostics
        if best.map(|(_, c)| matched > c).unwrap_or(true) {
            best = Some((i + 1, matched));
        }
    }

    // Close-but-not-exact window: guide the model with the location
    if let Some((start_line, matched)) = best
        && matched as f64 / old_lines.len() as f64 >= 0.5
    {
        return Err(ToolError::Message {
            message: format!(
                "old_string not found exactly. Closest match around line {start_line} \
                 ({matched}/{} lines matched). The file may have changed since the \
                 model last read it — use the read tool to fetch the current content.",
                old_lines.len()
            ),
        });
    }

    Err(ToolError::Message {
        message:
            "old_string not found in the file. The file may have changed since the model last \
             read it — use the read tool to fetch the current content."
                .to_string(),
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
        let content =
            "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\nline 8\nline 9\nline 10";
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

    // ── try_flexible_match tests ──

    #[test]
    fn test_flexible_match_auto_corrects_single_line_trimmed() {
        // old_string has extra leading spaces; file has different indentation
        let content = "    actual line\nother stuff";
        let old_string = "  actual line";
        let result = try_flexible_match(content, old_string);
        assert!(result.is_ok(), "Expected Ok, got {result:?}");
        assert_eq!(result.unwrap(), "    actual line");
    }

    #[test]
    fn test_flexible_match_auto_corrects_multi_line_indentation() {
        // old_string uses 2-space indent; file uses 4-space indent
        let content = "before\n    line one\n    line two\nafter";
        let old_string = "  line one\n  line two";
        let result = try_flexible_match(content, old_string);
        assert!(result.is_ok(), "Expected Ok, got {result:?}");
        assert_eq!(result.unwrap(), "    line one\n    line two");
    }

    #[test]
    fn test_flexible_match_returns_error_when_not_found() {
        let content = "totally different content";
        let old_string = "not in file";
        let result = try_flexible_match(content, old_string);
        assert!(result.is_err());
    }

    #[test]
    fn test_flexible_match_skips_exact_match() {
        // When old_string exactly matches content, the caller already counts it
        // This test verifies flexible match handles the case gracefully
        let content = "function foo() {\n    bar();\n}";
        // This string doesn't exist at all
        let old_string = "function baz() {";
        let result = try_flexible_match(content, old_string);
        assert!(result.is_err());
    }

    #[test]
    fn test_flexible_match_trailing_whitespace_trimmed() {
        let content = "hello world\n";
        let old_string = "hello world  ";
        let result = try_flexible_match(content, old_string);
        assert!(result.is_ok(), "Expected Ok, got {result:?}");
        assert_eq!(result.unwrap(), "hello world");
    }

    #[test]
    fn test_flexible_match_reports_closest_window() {
        // 3 of 4 lines match → diagnostic should point at the closest line
        let content = "a = 1\nb = 2\nc = 3\nd = 4\ne = 5";
        let old_string = "a = 1\nb = 2\nc = 999"; // `c` differs from file
        let result = try_flexible_match(content, old_string);
        let err = result.expect_err("Expected an error for non-matching window");
        let msg = err.to_string();
        assert!(
            msg.contains("line 1") && msg.contains("2/3"),
            "Expected location diagnostics in: {msg}"
        );
    }

    #[test]
    fn test_flexible_match_empty_string_errors() {
        let content = "some content";
        let old_string = "";
        let result = try_flexible_match(content, old_string);
        assert!(result.is_err());
    }
}
