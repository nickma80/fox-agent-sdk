use async_trait::async_trait;
use fox_agent_core::{Tool, ToolContext, ToolError, ToolOutput};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::process::Command as TokioCommand;

const MAX_OUTPUT_LEN: usize = 30000;
const DEFAULT_TIMEOUT_MS: u64 = 120000;
const BASH_TOOL_DESCRIPTION: &str =
    "Run a bash command. Supports foreground and background execution with timeout.";

fn build_shell_command(cmd_str: &str) -> TokioCommand {
    #[cfg(windows)]
    {
        let mut cmd = TokioCommand::new("cmd.exe");
        cmd.arg("/C").arg(cmd_str);
        cmd
    }
    #[cfg(not(windows))]
    {
        let mut cmd = TokioCommand::new("bash");
        cmd.arg("-c").arg(cmd_str);
        cmd
    }
}

pub struct BashTool;

impl BashTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct BashInput {
    command: String,
    #[serde(default)]
    intent: Option<String>,
    #[serde(default)]
    timeout: Option<u64>,
    #[serde(default)]
    run_in_background: Option<bool>,
    #[serde(default = "default_true")]
    notify: bool,
    #[serde(default)]
    wake: bool,
}

fn default_true() -> bool {
    true
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        BASH_TOOL_DESCRIPTION
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["command"],
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The bash command to execute."
                },
                "timeout": {
                    "type": "integer",
                    "description": "Timeout in ms (default 120000)."
                },
                "run_in_background": {
                    "type": "boolean",
                    "description": "Run in background. Returns immediately with a task ID."
                },
                "notify": {
                    "type": "boolean",
                    "description": "Notify on completion (for background tasks)."
                },
                "wake": {
                    "type": "boolean",
                    "description": "Wake agent on completion (for background tasks)."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let params: BashInput = serde_json::from_value(input).map_err(|e| ToolError::Message {
            message: format!("invalid bash input: {e}"),
        })?;

        let run_in_background = params.run_in_background.unwrap_or(false);

        if run_in_background {
            self.execute_background(params, ctx).await
        } else {
            self.execute_foreground(params, ctx).await
        }
    }
}

impl BashTool {
    async fn execute_foreground(
        &self,
        params: BashInput,
        ctx: ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let timeout_ms = params.timeout.unwrap_or(DEFAULT_TIMEOUT_MS).min(600000);
        let timeout_duration = Duration::from_millis(timeout_ms);

        let mut command = build_shell_command(&params.command);
        command
            .kill_on_drop(true)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        if let Some(ref dir) = ctx.working_dir {
            command.current_dir(dir);
        }

        let result = tokio::time::timeout(timeout_duration, async {
            let output = command
                .output()
                .await
                .map_err(|e| ToolError::Message {
                    message: format!("failed to execute command: {e}"),
                })?;

            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let exit_code = output.status.code();

            let mut text = stdout.clone();
            if !stderr.is_empty() {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&stderr);
            }
            let text = format_command_output(text, exit_code);

            Ok::<ToolOutput, ToolError>(ToolOutput {
                text,
                is_error: !output.status.success(),
                json: Some(json!({
                    "exit_code": exit_code,
                    "stdout": stdout,
                    "stderr": stderr,
                })),
            })
        })
        .await;

        match result {
            Ok(Ok(output)) => Ok(output),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(ToolError::Message {
                message: format!("Command timed out after {}ms", timeout_ms),
            }),
        }
    }

    /// Execute a command in the background using a spawned task
    async fn execute_background(
        &self,
        params: BashInput,
        ctx: ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let command = params.command.clone();
        let description = params.intent.clone();
        let display_name = summarize_background_command(description.as_deref(), &command);
        let working_dir = ctx.working_dir.clone();
        let timeout_ms = params.timeout.unwrap_or(DEFAULT_TIMEOUT_MS).min(600000);
        let timeout_duration = Duration::from_millis(timeout_ms);

        let temp_dir = std::env::temp_dir();
        let task_id = uuid::Uuid::new_v4().to_string();
        let output_file = temp_dir.join(format!("bg-{}.out", task_id));
        let output_file_clone = output_file.clone();
        let task_id_clone = task_id.clone();

        // Spawn the background task
        tokio::spawn(async move {
            let mut cmd = build_shell_command(&command);
            cmd.kill_on_drop(true)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());

            if let Some(ref dir) = working_dir {
                cmd.current_dir(dir);
            }

            let result = tokio::time::timeout(timeout_duration, async {
                let output = cmd.output().await.map_err(|e| ToolError::Message {
                    message: format!("failed to execute command: {e}"),
                })?;
                let exit_code = output.status.code();
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();

                let mut combined = stdout;
                if !stderr.is_empty() {
                    if !combined.is_empty() {
                        combined.push('\n');
                    }
                    combined.push_str(&stderr);
                }

                let status_line = format!(
                    "\n--- Command finished with exit code: {} ---\n",
                    exit_code.unwrap_or(-1)
                );
                combined.push_str(&status_line);

                let _ = tokio::fs::write(&output_file, &combined).await;
                Ok::<_, ToolError>(combined)
            })
            .await;

            match result {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => {
                    let err_msg = format!("Command failed: {e}");
                    let _ = tokio::fs::write(&output_file, &err_msg).await;
                }
                Err(_) => {
                    let err_msg = format!("Command timed out after {}ms", timeout_ms);
                    let _ = tokio::fs::write(&output_file, &err_msg).await;
                }
            }
        });

        let notify_msg = if params.wake {
            "The agent will be woken when the task completes."
        } else if params.notify {
            "You will be notified when the task completes."
        } else {
            "Notifications disabled."
        };

        let output = format!(
            "Command started in background.\n\n\
             Task ID: {}\n\
             Name: {}\n\
             Output file: {}\n\n\
             {}\n\
             To see output: use the `read` tool on the output file.",
            task_id,
            display_name,
            output_file_clone.display(),
            notify_msg,
        );

        Ok(ToolOutput {
            text: output,
            is_error: false,
            json: Some(json!({
                "background": true,
                "task_id": task_id_clone,
                "display_name": display_name,
                "output_file": output_file_clone.to_string_lossy().to_string(),
            })),
        })
    }
}

fn format_command_output(mut output: String, exit_code: Option<i32>) -> String {
    if output.len() > MAX_OUTPUT_LEN {
        let mut idx = MAX_OUTPUT_LEN;
        while idx > 0 && !output.is_char_boundary(idx) {
            idx -= 1;
        }
        output.truncate(idx);
        output.push_str("\n... (output truncated)");
    }

    if let Some(code) = exit_code.filter(|code| *code != 0) {
        output.push_str(&format!("\n\nExit code: {}", code));
    }

    if output.trim().is_empty() {
        "Command completed successfully (no output)".to_string()
    } else {
        output
    }
}

fn summarize_background_command(description: Option<&str>, command: &str) -> String {
    if let Some(description) = description
        .map(str::trim)
        .filter(|description| !description.is_empty())
    {
        return truncate_str(description, 28).to_string();
    }

    let trimmed = command.trim();
    if trimmed.is_empty() {
        return "bash".to_string();
    }

    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    let start = tokens
        .iter()
        .position(|token| !token.contains('='))
        .unwrap_or(0);
    let tokens = &tokens[start..];
    if tokens.is_empty() {
        return truncate_str(trimmed, 28).to_string();
    }

    let label = match tokens {
        ["python" | "python3" | "bash" | "sh" | "node", script, ..] => std::path::Path::new(script)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(script)
            .to_string(),
        ["cargo", subcommand, ..] => format!("cargo {}", subcommand),
        ["npm" | "pnpm" | "yarn", command, script, ..] if *command == "run" => {
            format!("{} {} {}", tokens[0], command, script)
        }
        [first, second, ..] => format!("{} {}", first, second),
        [first] => first.to_string(),
        [] => "bash".to_string(),
    };

    truncate_str(&label, 28).to_string()
}

fn truncate_str(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        return s;
    }
    let mut idx = max_len;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    &s[..idx]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_summarize_background_command() {
        assert_eq!(summarize_background_command(Some("run tests"), "cargo test --all"), "run tests");
        assert!(summarize_background_command(None, "cargo test --all").contains("cargo test"));
        assert!(summarize_background_command(None, "python scripts/build.py").contains("build.py"));
    }

    #[test]
    fn test_format_command_output_no_truncation() {
        let output = format_command_output("hello".to_string(), Some(0));
        assert_eq!(output, "hello");
    }

    #[test]
    fn test_format_command_output_with_exit_code() {
        let output = format_command_output("error".to_string(), Some(1));
        assert!(output.contains("Exit code: 1"));
    }

    #[test]
    fn test_format_command_output_empty() {
        let output = format_command_output(String::new(), Some(0));
        assert_eq!(output, "Command completed successfully (no output)");
    }
}
