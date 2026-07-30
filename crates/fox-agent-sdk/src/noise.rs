//! L2 Noise Removal — automatically identify and remove low-value tool output
//! lines that were never referenced by the agent in subsequent messages.
//!
//! ## Strategy
//!
//! | Tool output size | Action |
//! |------------------|--------|
//! | < 1000 chars     | Skip — too small to matter |
//! | 1000–8000 chars  | Check reference ratio |
//! | > 8000 chars     | Skip — handled by L1 routing engine (externalize) |
//!
//! When the agent references < 20% of output lines, unreferenced lines are
//! replaced with an omission marker. The full content is always preserved
//! via `artifact_read`.

use fox_agent_core::{ContentBlock, Message, Role};

/// Noise tools — only these tool types are eligible for noise removal.
const NOISE_TOOLS: &[&str] = &["grep", "glob", "read", "web_search", "web_fetch"];

/// Minimum output length (chars) before noise check kicks in.
const NOISE_MIN_CHARS: usize = 1000;

/// Maximum output length (chars) — beyond this, L1 routing handles it.
const NOISE_MAX_CHARS: usize = 8000;

/// Default reference ratio threshold.
const DEFAULT_NOISE_THRESHOLD: f64 = 0.20;

/// Result of noise cleaning pass.
#[derive(Debug, Clone)]
pub struct NoiseCleanResult {
    /// Number of tool outputs cleaned.
    pub tools_cleaned: usize,
    /// Total lines removed across all cleaned outputs.
    pub lines_removed: usize,
    /// Total characters saved.
    pub chars_saved: usize,
}

impl NoiseCleanResult {
    pub fn empty() -> Self {
        Self {
            tools_cleaned: 0,
            lines_removed: 0,
            chars_saved: 0,
        }
    }

    #[expect(dead_code)]
    pub fn merge(&mut self, other: &Self) {
        self.tools_cleaned += other.tools_cleaned;
        self.lines_removed += other.lines_removed;
        self.chars_saved += other.chars_saved;
    }
}

/// Configuration for L2 noise removal.
#[derive(Debug, Clone)]
pub struct NoiseCleanConfig {
    /// Whether noise removal is enabled.
    pub enabled: bool,
    /// Reference ratio threshold — if the agent references fewer than this
    /// fraction of output lines, unreferenced lines are removed.
    pub reference_threshold: f64,
    /// Minimum output characters to trigger noise check.
    pub min_output_chars: usize,
}

impl Default for NoiseCleanConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            reference_threshold: DEFAULT_NOISE_THRESHOLD,
            min_output_chars: NOISE_MIN_CHARS,
        }
    }
}

/// Clean noise from tool results in the message history.
///
/// Scans messages for tool results eligible for noise removal, computes the
/// reference ratio against subsequent assistant messages, and removes
/// unreferenced lines from low-reference outputs.
///
/// This is designed to be called before compaction or at the end of each turn.
pub fn clean_noise_from_messages(
    messages: &mut [Message],
    config: &NoiseCleanConfig,
) -> NoiseCleanResult {
    if !config.enabled || messages.is_empty() {
        return NoiseCleanResult::empty();
    }

    let mut result = NoiseCleanResult::empty();

    // Collect indices of tool result messages eligible for noise check
    let mut candidates: Vec<(usize, String, usize)> = Vec::new(); // (index, tool_name, original_len)

    for (i, msg) in messages.iter().enumerate() {
        if msg.role != Role::Tool {
            continue;
        }
        let text = msg.tool_text();
        if text.is_empty() || text.len() < config.min_output_chars || text.len() > NOISE_MAX_CHARS {
            continue;
        }
        let tool_name = extract_tool_name(msg);
        if !is_noise_tool(&tool_name) {
            continue;
        }
        candidates.push((i, tool_name, text.len()));
    }

    for (msg_idx, _tool_name, _original_len) in candidates {
        let text = messages[msg_idx].tool_text();

        // Gather subsequent assistant messages for reference analysis
        let subsequent: Vec<&Message> = messages[msg_idx + 1..]
            .iter()
            .filter(|m| m.role == Role::Assistant || m.role == Role::User)
            .take(5) // look at up to 5 subsequent messages
            .collect();

        if subsequent.is_empty() {
            continue;
        }

        let output_lines: Vec<&str> = text.lines().collect();
        if output_lines.len() < 5 {
            continue; // too few lines for meaningful noise removal
        }

        let (ref_count, total_lines) = noise_ratio(&output_lines, &subsequent);

        let ratio = if total_lines == 0 {
            0.0
        } else {
            ref_count as f64 / total_lines as f64
        };

        if ratio >= config.reference_threshold {
            continue; // enough lines referenced, keep as-is
        }

        // Remove unreferenced lines
        let cleaned = remove_noise_lines(&text, &output_lines, &subsequent);
        let chars_saved = text.len().saturating_sub(cleaned.len());

        if chars_saved > 0 {
            // Replace the tool result text
            replace_tool_result_text(&mut messages[msg_idx], &cleaned);
            result.tools_cleaned += 1;
            result.lines_removed += total_lines.saturating_sub(ref_count);
            result.chars_saved += chars_saved;
        }
    }

    result
}

/// Heuristic: count how many lines of a tool output are referenced by
/// subsequent assistant messages (token-based fuzzy match).
///
/// A line is considered "referenced" if at least half of its significant
/// tokens (words >= 3 chars) appear in the subsequent messages.
fn noise_ratio(output_lines: &[&str], subsequent_messages: &[&Message]) -> (usize, usize) {
    let mut referenced = vec![false; output_lines.len()];
    let combined_text: String = subsequent_messages
        .iter()
        .map(|m| m.text_content())
        .collect::<Vec<_>>()
        .join(" ");

    for (i, line) in output_lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.len() < 3 {
            // Very short lines (blank, punctuation, etc.) count as referenced
            referenced[i] = true;
            continue;
        }
        if is_line_referenced(trimmed, &combined_text) {
            referenced[i] = true;
        }
    }

    let ref_count = referenced.iter().filter(|&&r| r).count();
    (ref_count, output_lines.len())
}

/// Check if a line is referenced by token-level fuzzy match.
/// A line is referenced if >= 50% of its significant tokens (>= 3 chars)
/// appear in the combined subsequent text.
fn is_line_referenced(line: &str, combined_text: &str) -> bool {
    let tokens: Vec<&str> = line
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-' && c != '.' && c != '/')
        .map(|t| t.trim())
        .filter(|t| t.len() >= 3)
        .collect();

    if tokens.is_empty() {
        return false;
    }

    let matched = tokens.iter().filter(|t| combined_text.contains(*t)).count();
    (matched as f64) >= (tokens.len() as f64 * 0.5)
}

/// Remove unreferenced lines from a tool output, replacing them with an
/// omission marker that tells the agent how to retrieve the full content.
fn remove_noise_lines(
    original: &str,
    output_lines: &[&str],
    subsequent_messages: &[&Message],
) -> String {
    let combined_text: String = subsequent_messages
        .iter()
        .map(|m| m.text_content())
        .collect::<Vec<_>>()
        .join(" ");

    let referenced: Vec<bool> = output_lines
        .iter()
        .map(|line| {
            let trimmed = line.trim();
            trimmed.len() < 3 || is_line_referenced(trimmed, &combined_text)
        })
        .collect();

    let mut result = String::with_capacity(original.len());
    let mut in_omitted_block = false;
    let mut omitted_start = 0usize;
    let mut omitted_count = 0usize;

    for (i, line) in output_lines.iter().enumerate() {
        if referenced[i] {
            if in_omitted_block {
                // Close the omission block
                result.push_str(&format!(
                    "\n... {} lines omitted (lines {}-{}), use artifact_read for full content ...\n",
                    omitted_count,
                    omitted_start + 1,
                    omitted_start + omitted_count,
                ));
                in_omitted_block = false;
            }
            result.push_str(line);
            result.push('\n');
        } else {
            if !in_omitted_block {
                in_omitted_block = true;
                omitted_start = i;
                omitted_count = 0;
            }
            omitted_count += 1;
        }
    }

    if in_omitted_block {
        result.push_str(&format!(
            "\n... {} lines omitted (lines {}-{}), use artifact_read for full content ...\n",
            omitted_count,
            omitted_start + 1,
            omitted_start + omitted_count,
        ));
    }

    // Strip trailing newline that the loop adds
    result.trim_end().to_string()
}

/// Check if a tool name is eligible for noise removal.
fn is_noise_tool(name: &str) -> bool {
    NOISE_TOOLS.contains(&name)
}

/// Extract the tool name from a tool result message.
fn extract_tool_name(msg: &Message) -> String {
    for block in &msg.content {
        if let ContentBlock::ToolResult { .. } = block {
            // Tool result blocks don't directly store the tool name in the current schema.
            // We inspect the preceding assistant message's tool_use block.
            // For now, return empty; the caller identifies the tool name contextually.
            return String::new();
        }
    }
    String::new()
}

/// Replace the text content of a tool result message.
fn replace_tool_result_text(msg: &mut Message, new_text: &str) {
    for block in &mut msg.content {
        match block {
            ContentBlock::ToolResult { text, .. } => {
                *text = new_text.to_string();
            }
            ContentBlock::Text { text } => {
                *text = new_text.to_string();
            }
            _ => {}
        }
    }
}

/// Extracts tool text from a message (handles both ToolResult and Text blocks).
trait ToolText {
    fn tool_text(&self) -> String;
    fn text_content(&self) -> String;
}

impl ToolText for Message {
    fn tool_text(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolResult { text, .. } => Some(text.as_str()),
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn text_content(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                ContentBlock::Reasoning { text } => Some(text.as_str()),
                ContentBlock::ToolUse { name, input, .. } => Some(Box::leak(
                    format!(
                        "[{name}: {}]",
                        serde_json::to_string(input).unwrap_or_default()
                    )
                    .into_boxed_str(),
                )),
                ContentBlock::ToolResult { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tool_msg(text: &str) -> Message {
        Message {
            role: Role::Tool,
            content: vec![ContentBlock::ToolResult {
                call_id: "call_1".into(),
                text: text.to_string(),
                is_error: false,
            }],
        }
    }

    fn make_assistant_msg(text: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
        }
    }

    #[test]
    fn test_noise_ratio_all_referenced() {
        let output = "line1: foo\nline2: bar\nline3: baz";
        let lines: Vec<&str> = output.lines().collect();
        let subsequent = vec![make_assistant_msg(
            "I found foo, bar, and baz in the results",
        )];
        let msgs: Vec<&Message> = subsequent.iter().collect();
        let (ref_count, total) = noise_ratio(&lines, &msgs);
        assert_eq!(ref_count, total, "all lines should be referenced");
    }

    #[test]
    fn test_noise_ratio_none_referenced() {
        let output = "line1: foo\nline2: bar\nline3: baz";
        let lines: Vec<&str> = output.lines().collect();
        let subsequent = vec![make_assistant_msg("I didn't find anything useful")];
        let msgs: Vec<&Message> = subsequent.iter().collect();
        let (ref_count, _total) = noise_ratio(&lines, &msgs);
        assert_eq!(ref_count, 0, "no lines should be referenced");
    }

    #[test]
    fn test_noise_ratio_short_lines_always_referenced() {
        let output = "a\nb\nc\nlong_useful_line";
        let lines: Vec<&str> = output.lines().collect();
        let subsequent = vec![make_assistant_msg("Found long_useful_line")];
        let msgs: Vec<&Message> = subsequent.iter().collect();
        let (ref_count, total) = noise_ratio(&lines, &msgs);
        // a, b, c are < 3 chars → auto-referenced; long_useful_line is referenced
        assert_eq!(ref_count, total);
    }

    #[test]
    fn test_remove_noise_lines_partial() {
        let output = "line1: keep this\nline2: noise\nline3: noise\nline4: also keep";
        let lines: Vec<&str> = output.lines().collect();
        let subsequent = vec![make_assistant_msg(
            "I see keep this and also keep in the results",
        )];
        let msgs: Vec<&Message> = subsequent.iter().collect();
        let cleaned = remove_noise_lines(output, &lines, &msgs);
        assert!(cleaned.contains("line1: keep this"));
        assert!(cleaned.contains("line4: also keep"));
        assert!(!cleaned.contains("line2: noise"));
        assert!(!cleaned.contains("line3: noise"));
        assert!(cleaned.contains("lines omitted"));
    }

    #[test]
    fn test_remove_noise_lines_none_referenced() {
        let output = "noise1\nnoise2\nnoise3";
        let lines: Vec<&str> = output.lines().collect();
        let subsequent = vec![make_assistant_msg("Nothing useful found")];
        let msgs: Vec<&Message> = subsequent.iter().collect();
        let cleaned = remove_noise_lines(output, &lines, &msgs);
        assert!(!cleaned.contains("noise1"));
        assert!(cleaned.contains("3 lines omitted"));
    }

    #[test]
    fn test_clean_noise_from_messages_below_min_chars() {
        let config = NoiseCleanConfig {
            enabled: true,
            reference_threshold: 0.20,
            min_output_chars: 1000,
        };
        let mut messages = vec![make_tool_msg("short output")];
        let result = clean_noise_from_messages(&mut messages, &config);
        assert_eq!(result.tools_cleaned, 0, "short output should be skipped");
    }

    #[test]
    fn test_clean_noise_from_messages_disabled() {
        let config = NoiseCleanConfig {
            enabled: false,
            ..Default::default()
        };
        let long_output = "line ".repeat(200); // > 1000 chars
        let mut messages = vec![make_tool_msg(&long_output)];
        let result = clean_noise_from_messages(&mut messages, &config);
        assert_eq!(result.tools_cleaned, 0, "disabled config should skip");
    }

    #[test]
    fn test_is_noise_tool() {
        assert!(is_noise_tool("grep"));
        assert!(is_noise_tool("glob"));
        assert!(is_noise_tool("read"));
        assert!(is_noise_tool("web_search"));
        assert!(is_noise_tool("web_fetch"));
        assert!(!is_noise_tool("bash"));
        assert!(!is_noise_tool("write"));
    }
}
