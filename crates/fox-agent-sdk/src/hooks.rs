use fox_agent_core::HooksConfig;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::process::Command;
use tracing::Instrument;

// ── Hook event ──

/// Lifecycle events where hooks can run.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HookEvent {
    /// Fired at the beginning of a session.
    SessionStart,
    /// Fired after the user submits a prompt.
    UserPromptSubmit,
    /// Fired before a tool is executed.
    PreToolUse,
    /// Fired after a tool is executed.
    PostToolUse,
    /// One-way notification (does not alter flow).
    Notification,
    /// Agent stopped (error, budget, etc.).
    Stop,
    /// Sub-agent completed.
    SubagentStop,
    /// Before context compaction.
    PreCompact,
    /// Permission prompt triggered.
    PermissionPrompt,
    /// Before a file is written.
    PreFileWrite,
    /// After a file is written.
    PostFileWrite,
}

impl HookEvent {
    pub fn from_str_name(s: &str) -> Option<Self> {
        match s {
            "SessionStart" | "session-start" => Some(Self::SessionStart),
            "UserPromptSubmit" | "user-prompt-submit" => Some(Self::UserPromptSubmit),
            "PreToolUse" | "pre-tool-use" => Some(Self::PreToolUse),
            "PostToolUse" | "post-tool-use" => Some(Self::PostToolUse),
            "Notification" | "notification" => Some(Self::Notification),
            "Stop" | "stop" => Some(Self::Stop),
            "SubagentStop" | "subagent-stop" => Some(Self::SubagentStop),
            "PreCompact" | "pre-compact" => Some(Self::PreCompact),
            "PermissionPrompt" | "permission-prompt" => Some(Self::PermissionPrompt),
            "PreFileWrite" | "pre-file-write" => Some(Self::PreFileWrite),
            "PostFileWrite" | "post-file-write" => Some(Self::PostFileWrite),
            _ => None,
        }
    }
}

// ── Hook definition ──

/// A single hook definition (can be script-based or prompt-based).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookDefinition {
    pub event: HookEvent,
    /// Matcher: only trigger for specific tool names (e.g. "bash", "write").
    #[serde(default)]
    pub matcher: Option<String>,
    /// Shell command to run (script-based hook).
    #[serde(default)]
    pub command: Option<String>,
    /// Arguments for the command.
    #[serde(default)]
    pub args: Vec<String>,
    /// Prompt for LLM evaluation (prompt-based hook, TODO).
    #[serde(default)]
    pub prompt: Option<String>,
}

/// A group of hooks loaded from a single settings file.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HookSettings {
    #[serde(default)]
    pub hooks: Vec<HookDefinition>,
}

// ── Hook context / result ──

/// Input data passed to hooks via stdin.
#[derive(Debug, Clone, Serialize)]
pub struct HookContext<'a> {
    pub session_id: &'a str,
    pub event: &'a str,
    pub working_dir: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_input: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_output: Option<String>,
    pub hook_event_name: &'a str,
}

/// Output returned by a hook script via stdout.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct HookOutput {
    #[serde(default = "default_continue")]
    pub r#continue: bool,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub modified_input: Option<serde_json::Value>,
    #[serde(default)]
    pub system_message: Option<String>,
}

fn default_continue() -> bool {
    true
}

/// The decision after running hooks for an event.
#[derive(Debug, Clone)]
pub enum HookDecision {
    /// All hooks allowed; optionally modified input/output.
    Allow {
        modified_input: Option<serde_json::Value>,
    },
    /// One or more hooks blocked the action.
    Block { reason: String },
    /// Additional context to inject (e.g. from PreCompact).
    InjectContext { context: String },
}

// ── HookManager ──

pub struct HookManager {
    hooks: HashMap<HookEvent, Vec<HookDefinition>>,
    config: HooksConfig,
}

impl HookManager {
    pub fn new(config: HooksConfig) -> Self {
        Self {
            hooks: HashMap::new(),
            config,
        }
    }

    // ── Loading ──

    /// Load hooks from the project directory (`.claude/hooks/`).
    pub fn load_from_working_dir(&mut self, working_dir: Option<&std::path::Path>) -> usize {
        let Some(dir) = working_dir else { return 0 };
        let hooks_dir = dir.join(".claude").join("hooks");
        self.load_from_dir(&hooks_dir)
    }

    /// Load hooks from the global storage directory (`{storage_dir}/hooks/`).
    pub fn load_from_global_dir(&mut self, storage_dir: &std::path::Path) -> usize {
        let hooks_dir = storage_dir.join("hooks");
        self.load_from_dir(&hooks_dir)
    }

    /// Load hooks from all configured sources.
    pub fn load_all(
        &mut self,
        storage_dir: &std::path::Path,
        working_dir: Option<&std::path::Path>,
        config: &HooksConfig,
    ) -> usize {
        if !config.enabled {
            return 0;
        }

        let mut total = 0;

        // Project hooks
        total += self.load_from_working_dir(working_dir);

        // Global hooks
        if config.load_global {
            total += self.load_from_global_dir(storage_dir);
        }

        // Additional directories
        for dir in &config.additional_directories {
            total += self.load_from_dir(dir);
        }

        total
    }

    fn load_from_dir(&mut self, dir: &std::path::Path) -> usize {
        if !dir.exists() {
            return 0;
        }
        let mut count = 0;
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let is_json = path
                    .extension()
                    .map(|e| e == "json" || e == "jsonc")
                    .unwrap_or(false);
                if is_json && let Ok(content) = std::fs::read_to_string(&path) {
                    match serde_json::from_str::<HookSettings>(&content) {
                        Ok(settings) => {
                            for hook in settings.hooks {
                                self.hooks.entry(hook.event.clone()).or_default().push(hook);
                                count += 1;
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                path = %path.display(),
                                error = %e,
                                "failed to parse hook settings file — skipping"
                            );
                        }
                    }
                }
            }
        }
        count
    }

    /// Load hooks from raw JSON settings (for programmatic use).
    pub fn load_from_settings(&mut self, settings: &HookSettings) -> usize {
        let mut count = 0;
        for hook in &settings.hooks {
            self.hooks
                .entry(hook.event.clone())
                .or_default()
                .push(hook.clone());
            count += 1;
        }
        count
    }

    // ── Execution ──

    /// Execute all hooks for a given event.
    ///
    /// Returns `HookDecision::Block` if any hook returns `continue: false`.
    /// Returns `HookDecision::Allow` with the first `modified_input` (if any).
    pub async fn execute(
        &self,
        event: HookEvent,
        ctx: HookContext<'_>,
    ) -> Result<HookDecision, String> {
        let Some(hooks) = self.hooks.get(&event) else {
            return Ok(HookDecision::Allow {
                modified_input: None,
            });
        };

        // Run hooks concurrently up to max_concurrent
        let semaphore = Arc::new(tokio::sync::Semaphore::new(
            self.config.max_concurrent.max(1),
        ));

        let mut tasks = Vec::new();
        for hook in hooks {
            // Apply matcher filter
            if let Some(ref matcher) = hook.matcher
                && let Some(tool_name) = ctx.tool_name
                && tool_name != matcher.as_str()
            {
                continue;
            }

            let hook = hook.clone();
            let ctx_json = serde_json::to_string(&ctx).unwrap_or_default();
            let timeout = self.config.timeout_secs;
            let sem = semaphore.clone();

            tasks.push(tokio::spawn(
                async move {
                    let _permit = sem.acquire().await;
                    execute_script_hook(&hook, &ctx_json, timeout).await
                }
                .in_current_span(),
            ));
        }

        let mut modified_input: Option<serde_json::Value> = None;
        let mut system_messages: Vec<String> = Vec::new();

        for task in tasks {
            match task.await {
                Ok(Ok(output)) => {
                    if !output.r#continue {
                        return Ok(HookDecision::Block {
                            reason: output
                                .reason
                                .unwrap_or_else(|| "hook blocked the action".into()),
                        });
                    }
                    if output.modified_input.is_some() && modified_input.is_none() {
                        modified_input = output.modified_input;
                    }
                    if let Some(msg) = output.system_message {
                        system_messages.push(msg);
                    }
                }
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "hook script failed — allowing action to proceed");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "hook task panicked — allowing action to proceed");
                }
            }
        }

        if !system_messages.is_empty() {
            // For PreCompact, inject system messages as context
            if event == HookEvent::PreCompact {
                return Ok(HookDecision::InjectContext {
                    context: system_messages.join("\n"),
                });
            }
        }

        Ok(HookDecision::Allow { modified_input })
    }
}

async fn execute_script_hook(
    hook: &HookDefinition,
    ctx_json: &str,
    timeout_secs: u64,
) -> Result<HookOutput, String> {
    let cmd = hook
        .command
        .as_ref()
        .ok_or_else(|| "hook has no command".to_string())?;

    let mut child = Command::new(cmd);
    child.args(&hook.args);
    child.stdin(std::process::Stdio::piped());
    child.stdout(std::process::Stdio::piped());
    child.stderr(std::process::Stdio::piped());
    // Don't inherit env vars; hooks get clean env
    child.env_remove("PATH"); // They can add if needed
    child.kill_on_drop(true);

    let mut child = child
        .spawn()
        .map_err(|e| format!("hook '{cmd}' failed to start: {e}"))?;

    // Write stdin
    use tokio::io::AsyncWriteExt;
    if let Some(mut stdin) = child.stdin.take() {
        let mut data = ctx_json.as_bytes().to_vec();
        data.push(b'\n');
        let _ = stdin.write_all(&data).await;
        drop(stdin);
    }

    let output = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        child.wait_with_output(),
    )
    .await
    .map_err(|_| format!("hook '{cmd}' timed out after {timeout_secs}s"))?
    .map_err(|e| format!("hook '{cmd}' failed: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "hook '{cmd}' exited with {}: {}",
            output.status,
            stderr.trim()
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Try to parse JSON, fallback to empty (allow)
    let hook_output: HookOutput = serde_json::from_str(&stdout).unwrap_or_default();
    Ok(hook_output)
}

// ── Agent Loop integration helpers ──

impl HookManager {
    /// Build static prompt section listing registered hooks.
    pub fn build_prompt_section(&self) -> Option<String> {
        let total: usize = self.hooks.values().map(|v| v.len()).sum();
        if total == 0 {
            return None;
        }

        let mut section = String::from("# Active Hooks\n\n");
        section.push_str(&format!(
            "{total} hooks registered for the following events:\n"
        ));
        let mut events: Vec<&HookEvent> = self.hooks.keys().collect();
        events.sort_by_key(|e| format!("{e:?}"));
        for event in &events {
            if let Some(list) = self.hooks.get(event) {
                section.push_str(&format!("- **{event:?}**: {} hook(s)\n", list.len()));
            }
        }
        section.push('\n');
        Some(section)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hook_event_parsing() {
        assert_eq!(
            HookEvent::from_str_name("pre-tool-use"),
            Some(HookEvent::PreToolUse)
        );
        assert_eq!(
            HookEvent::from_str_name("PreToolUse"),
            Some(HookEvent::PreToolUse)
        );
        assert_eq!(
            HookEvent::from_str_name("session-start"),
            Some(HookEvent::SessionStart)
        );
        assert_eq!(HookEvent::from_str_name("unknown"), None);
    }

    #[test]
    fn test_load_hook_settings() {
        let settings: HookSettings = serde_json::from_str(
            r#"{
                "hooks": [
                    {
                        "event": "pre-tool-use",
                        "command": "echo",
                        "args": ["blocked"],
                        "matcher": null
                    },
                    {
                        "event": "post-tool-use",
                        "command": "python3",
                        "args": ["format.py"],
                        "matcher": "write"
                    }
                ]
            }"#,
        )
        .unwrap();

        assert_eq!(settings.hooks.len(), 2);
        assert_eq!(settings.hooks[0].event, HookEvent::PreToolUse);
        assert_eq!(settings.hooks[1].event, HookEvent::PostToolUse);
        assert_eq!(settings.hooks[1].matcher.as_deref(), Some("write"));
    }

    #[test]
    fn test_load_from_dir_nonexistent() {
        let mut manager = HookManager::new(HooksConfig::default());
        let count = manager.load_from_dir(std::path::Path::new("/nonexistent/path"));
        assert_eq!(count, 0);
    }

    #[test]
    fn test_build_prompt_section_empty() {
        let manager = HookManager::new(HooksConfig::default());
        assert!(manager.build_prompt_section().is_none());
    }

    #[test]
    fn test_build_prompt_section_with_hooks() {
        let mut manager = HookManager::new(HooksConfig::default());
        let settings: HookSettings = serde_json::from_str(
            r#"{
                "hooks": [
                    {"event": "pre-tool-use", "command": "echo", "args": ["check"]},
                    {"event": "post-tool-use", "command": "format", "args": []}
                ]
            }"#,
        )
        .unwrap();
        manager.load_from_settings(&settings);
        let section = manager.build_prompt_section().unwrap();
        assert!(section.contains("PreToolUse"));
        assert!(section.contains("PostToolUse"));
    }
}
