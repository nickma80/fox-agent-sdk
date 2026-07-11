use fox_agent_core::{
    DefaultSafetyPolicy, PermissionRequest, PermissionResult, RiskLevel, SafetyConfig,
};
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone)]
pub struct SafetySystem {
    inner: Arc<SafetySystemInner>,
}

struct SafetySystemInner {
    cfg: SafetyConfig,
    custom_hook: Option<Arc<dyn Fn(&str, &serde_json::Value) -> PermissionResult + Send + Sync>>,
}

impl SafetySystem {
    pub fn new(cfg: SafetyConfig) -> Self {
        Self {
            inner: Arc::new(SafetySystemInner {
                cfg,
                custom_hook: None,
            }),
        }
    }

    pub fn with_permission_hook(
        cfg: SafetyConfig,
        hook: impl Fn(&str, &serde_json::Value) -> PermissionResult + Send + Sync + 'static,
    ) -> Self {
        Self {
            inner: Arc::new(SafetySystemInner {
                cfg,
                custom_hook: Some(Arc::new(hook)),
            }),
        }
    }

    /// Check if a bash command is read-only (e.g. ls, grep, cat).
    fn is_readonly_bash(input: &Value) -> bool {
        let cmd = input.get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let cmd_trimmed = cmd.trim();

        // Dangerous patterns — if present, the command can modify the system
        let dangerous_patterns = [
            "rm ", "mv ", "cp ", "dd ", ">", ">>", "chmod", "chown",
            "kill", "shutdown", "reboot", "sudo", "su ",
            "git commit", "git push", "git merge", "git rebase",
            "cargo publish", "cargo build", "cargo test", "cargo run",
            "make ", "cmake", "npm install", "pip install", "apt ",
            "sed ", "awk ", "perl ", "python ", "node ", "ruby ",
            "curl ", "wget ", "ssh ", "scp ", "rsync",
        ];

        for pattern in &dangerous_patterns {
            if cmd_trimmed.contains(pattern) {
                return false;
            }
        }

        // Known read-only prefixes
        let readonly_prefixes = [
            "ls", "cat", "head", "tail", "grep", "find", "wc", "du", "df",
            "file", "stat", "echo", "printf", "date", "uname", "whoami",
            "which", "type", "env", "printenv", "pwd", "id", "ps",
            "git log", "git show", "git diff", "git status", "git branch",
            "cargo check", "cargo doc", "rustc --version",
            "python --version", "node --version",
        ];

        for prefix in &readonly_prefixes {
            if cmd_trimmed.starts_with(prefix) {
                return true;
            }
        }

        false
    }

    /// Standard permission check (no intent analysis).
    pub fn check(&self, tool_name: &str, input: &serde_json::Value) -> PermissionResult {
        // If a custom permission hook is registered, delegate to it
        if let Some(ref hook) = self.inner.custom_hook {
            return hook(tool_name, input);
        }

        // Priority:
        // 1. Denylist → AskUser
        // 2. Allowlist + not in list → Deny
        // 3. Allowlist + in list → Allow
        // 4. No allowlist → default_policy
        // 5. productive_tool_confirm → escalate write/edit/bash to AskUser

        // Rule 1: denylist
        if let Some(ref denylist) = self.inner.cfg.tool_denylist {
            if denylist.iter().any(|d| d == tool_name) {
                return PermissionResult::AskUser {
                    request: PermissionRequest::new(
                        tool_name,
                        format!("tool `{tool_name}` is in the denylist and requires your confirmation"),
                    ).with_risk(
                        RiskLevel::High,
                        "denylist",
                        tool_name.to_string(),
                    ),
                };
            }
        }

        // Rules 2 & 3: allowlist
        if let Some(ref allowlist) = self.inner.cfg.tool_allowlist {
            if allowlist.iter().any(|a| a == tool_name) {
                let result = PermissionResult::Allow;
                return self.apply_productive_tool_confirm(tool_name, input, result);
            }
            return PermissionResult::Deny {
                reason: format!(
                    "tool `{tool_name}` is not in the allowlist and has been denied"
                ),
            };
        }

        // Rule 4: default policy
        let result = match self.inner.cfg.default_policy {
            DefaultSafetyPolicy::Allow => PermissionResult::Allow,
            DefaultSafetyPolicy::Deny => PermissionResult::Deny {
                reason: format!("tool `{tool_name}` has been denied by default policy"),
            },
            DefaultSafetyPolicy::Confirm => PermissionResult::AskUser {
                request: PermissionRequest::new(
                    tool_name,
                    format!("tool `{tool_name}` requires your confirmation"),
                ).with_risk(
                    RiskLevel::Medium,
                    "default:confirm",
                    tool_name.to_string(),
                ),
            },
        };

        // Rule 5: productive tool confirmation
        self.apply_productive_tool_confirm(tool_name, input, result)
    }

    /// When `productive_tool_confirm` is enabled, write/edit/non-readonly-bash
    /// always require user confirmation — regardless of what the user's message
    /// says. This is simpler and more reliable than guessing the user's intent
    /// from keywords.
    fn apply_productive_tool_confirm(
        &self,
        tool_name: &str,
        input: &Value,
        fallback: PermissionResult,
    ) -> PermissionResult {
        if !self.inner.cfg.productive_tool_confirm {
            return fallback;
        }

        let is_productive = match tool_name {
            "write" | "edit" => true,
            "bash" => !Self::is_readonly_bash(input),
            _ => false,
        };

        if !is_productive {
            return fallback;
        }

        PermissionResult::AskUser {
            request: PermissionRequest::new(
                tool_name,
                format!(
                    "`{tool_name}` is a modification tool and requires your confirmation"
                ),
            ).with_risk(
                RiskLevel::High,
                "productive-tool-confirm",
                tool_name.to_string(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fox_agent_core::DefaultSafetyPolicy;

    #[test]
    fn test_denylist_overrides_default_allow() {
        let cfg = SafetyConfig {
            tool_denylist: Some(vec!["bash".to_string()]),
            ..Default::default()
        };
        let system = SafetySystem::new(cfg);
        let result = system.check("bash", &serde_json::json!({}));
        assert!(matches!(result, PermissionResult::AskUser { .. }));
    }

    #[test]
    fn test_allowlist_allows_known_tools() {
        let cfg = SafetyConfig {
            tool_allowlist: Some(vec!["read".to_string(), "grep".to_string()]),
            default_policy: DefaultSafetyPolicy::Deny,
            ..Default::default()
        };
        let system = SafetySystem::new(cfg);
        let result = system.check("read", &serde_json::json!({}));
        assert!(matches!(result, PermissionResult::Allow));
    }

    #[test]
    fn test_allowlist_denies_unknown_tools() {
        let cfg = SafetyConfig {
            tool_allowlist: Some(vec!["read".to_string(), "grep".to_string()]),
            default_policy: DefaultSafetyPolicy::Deny,
            ..Default::default()
        };
        let system = SafetySystem::new(cfg);
        let result = system.check("write", &serde_json::json!({}));
        assert!(matches!(result, PermissionResult::Deny { .. }));
    }

    #[test]
    fn test_default_policy_confirm() {
        let cfg = SafetyConfig {
            default_policy: DefaultSafetyPolicy::Confirm,
            ..Default::default()
        };
        let system = SafetySystem::new(cfg);
        let result = system.check("read", &serde_json::json!({}));
        assert!(matches!(result, PermissionResult::AskUser { .. }));
    }

    #[test]
    fn test_default_policy_deny() {
        let cfg = SafetyConfig {
            default_policy: DefaultSafetyPolicy::Deny,
            ..Default::default()
        };
        let system = SafetySystem::new(cfg);
        let result = system.check("read", &serde_json::json!({}));
        assert!(matches!(result, PermissionResult::Deny { .. }));
    }

    #[test]
    fn test_allowlist_overrides_denylist_check() {
        let cfg = SafetyConfig {
            tool_allowlist: Some(vec!["bash".to_string()]),
            tool_denylist: Some(vec!["bash".to_string()]),
            ..Default::default()
        };
        let system = SafetySystem::new(cfg);
        let result = system.check("bash", &serde_json::json!({}));
        assert!(matches!(result, PermissionResult::AskUser { .. }));
    }

    #[test]
    fn test_productive_tool_confirm_escalates_write() {
        let cfg = SafetyConfig {
            default_policy: DefaultSafetyPolicy::Allow,
            productive_tool_confirm: true,
            ..Default::default()
        };
        let system = SafetySystem::new(cfg);
        let result = system.check("write", &serde_json::json!({"file_path": "test.rs"}));
        assert!(
            matches!(result, PermissionResult::AskUser { .. }),
            "write should require confirmation when productive_tool_confirm is on"
        );
    }

    #[test]
    fn test_productive_tool_confirm_escalates_dangerous_bash() {
        let cfg = SafetyConfig {
            default_policy: DefaultSafetyPolicy::Allow,
            productive_tool_confirm: true,
            ..Default::default()
        };
        let system = SafetySystem::new(cfg);
        let result = system.check("bash", &serde_json::json!({"command": "cargo build"}));
        assert!(
            matches!(result, PermissionResult::AskUser { .. }),
            "dangerous bash should require confirmation"
        );
    }

    #[test]
    fn test_productive_tool_confirm_allows_readonly_bash() {
        let cfg = SafetyConfig {
            default_policy: DefaultSafetyPolicy::Allow,
            productive_tool_confirm: true,
            ..Default::default()
        };
        let system = SafetySystem::new(cfg);
        let result = system.check("bash", &serde_json::json!({"command": "ls -la"}));
        assert!(
            matches!(result, PermissionResult::Allow),
            "read-only bash should be allowed even with productive_tool_confirm"
        );
    }

    #[test]
    fn test_productive_tool_confirm_allows_read_tools() {
        let cfg = SafetyConfig {
            default_policy: DefaultSafetyPolicy::Allow,
            productive_tool_confirm: true,
            ..Default::default()
        };
        let system = SafetySystem::new(cfg);
        assert!(matches!(system.check("read", &serde_json::json!({})), PermissionResult::Allow));
        assert!(matches!(system.check("grep", &serde_json::json!({})), PermissionResult::Allow));
    }

    #[test]
    fn test_productive_tool_confirm_disabled() {
        let cfg = SafetyConfig {
            default_policy: DefaultSafetyPolicy::Allow,
            productive_tool_confirm: false,
            ..Default::default()
        };
        let system = SafetySystem::new(cfg);
        let result = system.check("write", &serde_json::json!({"file_path": "test.rs"}));
        assert!(matches!(result, PermissionResult::Allow));
    }

    #[test]
    fn test_is_readonly_bash() {
        assert!(SafetySystem::is_readonly_bash(&serde_json::json!({"command": "ls -la"})));
        assert!(SafetySystem::is_readonly_bash(&serde_json::json!({"command": "cat file.txt"})));
        assert!(SafetySystem::is_readonly_bash(&serde_json::json!({"command": "grep -r pattern ."})));
        assert!(!SafetySystem::is_readonly_bash(&serde_json::json!({"command": "rm -rf /tmp/foo"})));
        assert!(!SafetySystem::is_readonly_bash(&serde_json::json!({"command": "git commit -m 'msg'"})));
        assert!(!SafetySystem::is_readonly_bash(&serde_json::json!({"command": "cargo build"})));
    }
}
