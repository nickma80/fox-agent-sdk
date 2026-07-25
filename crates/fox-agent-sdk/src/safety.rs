use fox_agent_core::{
    DefaultSafetyPolicy, McpServerKind, McpServerProfile, McpToolDescriptorSnapshot,
    McpTransportKind, PermissionRequest, PermissionResult, RiskLevel, SafetyConfig,
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

    /// Check whether `pattern` matches `tool_name`.
    ///
    /// `*` in the pattern acts as a wildcard (zero or more characters).
    /// Patterns without `*` are treated as exact matches (backward compat).
    fn matches_pattern(pattern: &str, tool_name: &str) -> bool {
        if !pattern.contains('*') {
            return pattern == tool_name;
        }

        // Split pattern by `*` and match segments sequentially.
        // A leading `*` produces an empty first segment.
        let segments: Vec<&str> = pattern.split('*').collect();
        let mut pos = 0usize;

        for (i, seg) in segments.iter().enumerate() {
            if seg.is_empty() {
                // First empty segment means `*` at start; skip.
                // Last empty segment means `*` at end; already matched.
                continue;
            }
            if let Some(found) = tool_name[pos..].find(seg) {
                // First segment must anchor at start (unless preceded by `*`).
                if i == 0 && !pattern.starts_with('*') && found != 0 {
                    return false;
                }
                pos += found + seg.len();
            } else {
                return false;
            }
        }

        // If pattern doesn't end with `*`, we must have consumed the entire
        // tool_name.
        if !pattern.ends_with('*') && pos < tool_name.len() {
            return false;
        }

        true
    }
}

// ── MCP name helpers ──

/// Extract the MCP server name from a sanitised tool name.
///
/// Sanitised format: `mcp__<server>__<tool>` → returns `"<server>"`.
/// Returns `None` if the tool name does not start with `mcp__`.
fn mcp_server_name(tool_name: &str) -> Option<&str> {
    let rest = tool_name.strip_prefix("mcp__")?;
    // Find the next `__` separator
    rest.find("__").map(|idx| &rest[..idx])
}

// ── Permission check ──

impl SafetySystem {
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

    /// Check whether a list of patterns contains a match for `tool_name`.
    fn list_matches(patterns: &[String], tool_name: &str) -> bool {
        patterns.iter().any(|p| Self::matches_pattern(p, tool_name))
    }

    /// Standard permission check.
    ///
    /// Priority (highest first):
    /// 1. Custom hook (skips all rules below)
    /// 2. Denylist (pattern match) → AskUser
    /// 3. Allowlist (pattern match) → Allow (if matched)
    /// 4. Allowlist + not in list → Deny (if allowlist is set)
    /// 5. MCP auto-approve servers → Allow (if tool is from a listed server)
    /// 6. Default policy → Allow / Deny / Confirm
    /// 7. Productive tool confirm → escalate write/edit/bash to AskUser
    pub fn check(&self, tool_name: &str, input: &serde_json::Value) -> PermissionResult {
        self.check_with_mcp_metadata(tool_name, input, None, None)
    }

    pub fn check_with_mcp_metadata(
        &self,
        tool_name: &str,
        input: &serde_json::Value,
        profile: Option<&McpServerProfile>,
        descriptor: Option<&McpToolDescriptorSnapshot>,
    ) -> PermissionResult {
        // If a custom permission hook is registered, delegate to it
        if let Some(ref hook) = self.inner.custom_hook {
            return hook(tool_name, input);
        }

        // Rule 1: denylist (highest priority — overrides everything except custom hook)
        if let Some(ref denylist) = self.inner.cfg.tool_denylist {
            if Self::list_matches(denylist, tool_name) {
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

        // Rule 2: allowlist — if set, only matching tools pass
        if let Some(ref allowlist) = self.inner.cfg.tool_allowlist {
            if Self::list_matches(allowlist, tool_name) {
                return self.apply_productive_tool_confirm(tool_name, input, PermissionResult::Allow);
            }
            // Allowlist is set but tool didn't match — deny
            return PermissionResult::Deny {
                reason: format!(
                    "tool `{tool_name}` is not in the allowlist and has been denied"
                ),
            };
        }

        // Rule 3: MCP auto-approve — auto-allow tools from listed MCP servers
        if let Some(ref servers) = self.inner.cfg.mcp_auto_approve_servers {
            if let Some(server) = mcp_server_name(tool_name)
                && servers.iter().any(|s| s == server)
            {
                return self.apply_productive_tool_confirm(tool_name, input, PermissionResult::Allow);
            }
        }

        // Rule 4: MCP profile-aware gating
        if let Some(result) = self.apply_mcp_profile_confirm(tool_name, profile, descriptor) {
            return result;
        }

        // Rule 5: default policy
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

        // Rule 6: productive tool confirmation
        self.apply_productive_tool_confirm(tool_name, input, result)
    }

    fn apply_mcp_profile_confirm(
        &self,
        tool_name: &str,
        profile: Option<&McpServerProfile>,
        descriptor: Option<&McpToolDescriptorSnapshot>,
    ) -> Option<PermissionResult> {
        if !tool_name.starts_with("mcp__") {
            return None;
        }
        let profile = profile?;

        let descriptor_summary = descriptor
            .map(|d| {
                if d.description.is_empty() {
                    d.original_name.clone()
                } else {
                    format!("{} ({})", d.description, d.original_name)
                }
            })
            .unwrap_or_else(|| tool_name.to_string());
        let original_name = descriptor
            .map(|d| d.original_name.as_str())
            .unwrap_or(tool_name);

        let destructive_keywords = [
            "write",
            "delete",
            "remove",
            "create",
            "rename",
            "move",
            "click",
            "submit",
            "type",
            "fill",
            "upload",
            "press",
            "execute",
            "run",
            "post",
            "patch",
            "put",
        ];
        let interactive_browser_keywords = [
            "click",
            "submit",
            "type",
            "fill",
            "upload",
            "press",
            "select",
            "drag",
        ];
        let readonly_keywords = [
            "read",
            "list",
            "search",
            "find",
            "fetch",
            "get",
            "snapshot",
            "screenshot",
            "inspect",
            "extract",
        ];
        let descriptor_lower = descriptor_summary.to_lowercase();
        let looks_destructive = destructive_keywords
            .iter()
            .any(|kw| descriptor_lower.contains(kw));
        let looks_browser_interactive = interactive_browser_keywords
            .iter()
            .any(|kw| descriptor_lower.contains(kw));
        let looks_readonly = readonly_keywords
            .iter()
            .any(|kw| descriptor_lower.contains(kw));
        let capability_is_mutating = profile.capability_tags.iter().any(|tag| {
            matches!(
                tag.as_str(),
                "write" | "mutate" | "delete" | "shell" | "execute" | "browser-interactive"
            )
        });
        let capability_is_remote = profile.capability_tags.iter().any(|tag| {
            matches!(tag.as_str(), "network" | "remote" | "http")
        });

        let ask = |risk_level: RiskLevel, policy_source: &str, prompt: String| {
            Some(PermissionResult::AskUser {
                request: PermissionRequest::new(tool_name, prompt).with_risk(
                    risk_level,
                    policy_source,
                    descriptor_summary.clone(),
                ),
            })
        };

        match profile.kind {
            McpServerKind::ReadOnly
                if profile.transport == McpTransportKind::Sse || capability_is_remote =>
            {
                ask(
                    RiskLevel::Medium,
                    "mcp-profile:readonly-remote",
                    format!(
                        "Remote read-only MCP tool `{original_name}` requires your confirmation"
                    ),
                )
            }
            McpServerKind::Shell => ask(
                if looks_destructive || capability_is_mutating {
                    RiskLevel::Critical
                } else {
                    RiskLevel::High
                },
                if looks_destructive || capability_is_mutating {
                    "mcp-profile:shell-destructive"
                } else {
                    "mcp-profile:shell"
                },
                format!("MCP shell tool `{original_name}` requires your confirmation"),
            ),
            McpServerKind::Browser if looks_browser_interactive || capability_is_mutating => ask(
                RiskLevel::Critical,
                "mcp-profile:browser-interactive",
                format!(
                    "Interactive MCP browser tool `{original_name}` requires your confirmation"
                ),
            ),
            McpServerKind::Browser => ask(
                RiskLevel::High,
                if looks_readonly {
                    "mcp-profile:browser-read"
                } else {
                    "mcp-profile:browser"
                },
                format!(
                    "MCP browser tool `{original_name}` may interact with remote pages and requires your confirmation"
                ),
            ),
            McpServerKind::ExternalApi if looks_destructive || capability_is_mutating => ask(
                RiskLevel::Critical,
                "mcp-profile:external-api-write",
                format!(
                    "Mutating MCP external API tool `{original_name}` requires your confirmation"
                ),
            ),
            McpServerKind::ExternalApi => ask(
                RiskLevel::High,
                if looks_readonly {
                    "mcp-profile:external-api-read"
                } else {
                    "mcp-profile:external-api"
                },
                format!(
                    "MCP external API tool `{original_name}` performs remote calls and requires your confirmation"
                ),
            ),
            McpServerKind::Filesystem if looks_destructive => ask(
                RiskLevel::High,
                "mcp-profile:filesystem-write",
                format!("MCP filesystem tool `{original_name}` looks mutating and requires your confirmation"),
            ),
            McpServerKind::Unknown if profile.transport == McpTransportKind::Sse && looks_destructive => ask(
                RiskLevel::Critical,
                "mcp-profile:sse-unknown-write",
                format!(
                    "Remote MCP tool `{original_name}` uses SSE transport and looks mutating, so it requires your confirmation"
                ),
            ),
            McpServerKind::Unknown if profile.transport == McpTransportKind::Sse => ask(
                RiskLevel::High,
                "mcp-profile:sse-unknown",
                format!("Remote MCP tool `{original_name}` uses SSE transport and requires your confirmation"),
            ),
            // Unknown MCP servers via Stdio still need confirmation — we don't
            // know what they can do, so treat them conservatively.
            McpServerKind::Unknown if looks_destructive || capability_is_mutating => ask(
                RiskLevel::High,
                "mcp-profile:unknown-destructive",
                format!(
                    "Unrecognised MCP tool `{original_name}` looks mutating and requires your confirmation"
                ),
            ),
            McpServerKind::Unknown => ask(
                RiskLevel::Medium,
                "mcp-profile:unknown",
                format!("Unrecognised MCP tool `{original_name}` requires your confirmation"),
            ),
            _ => None,
        }
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
        assert!(matches!(system.check("artifact_read", &serde_json::json!({})), PermissionResult::Allow));
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
    fn test_mcp_external_api_requires_confirmation() {
        let cfg = SafetyConfig {
            default_policy: DefaultSafetyPolicy::Allow,
            ..Default::default()
        };
        let system = SafetySystem::new(cfg);
        let profile = McpServerProfile {
            server_name: "remote_api".to_string(),
            kind: McpServerKind::ExternalApi,
            transport: McpTransportKind::Sse,
            auto_approve: false,
            allowed_tools: Vec::new(),
            capability_tags: Vec::new(),
        };
        let descriptor = McpToolDescriptorSnapshot {
            server_name: "remote_api".to_string(),
            tool_name: "mcp__remote_api__search".to_string(),
            original_name: "mcp://remote_api/search".to_string(),
            description: "Search remote API".to_string(),
            input_schema: serde_json::json!({}),
            output_hint: None,
        };
        let result = system.check_with_mcp_metadata(
            "mcp__remote_api__search",
            &serde_json::json!({}),
            Some(&profile),
            Some(&descriptor),
        );
        assert!(matches!(result, PermissionResult::AskUser { .. }));
    }

    #[test]
    fn test_mcp_external_api_read_uses_read_policy_source() {
        let cfg = SafetyConfig {
            default_policy: DefaultSafetyPolicy::Allow,
            ..Default::default()
        };
        let system = SafetySystem::new(cfg);
        let profile = McpServerProfile {
            server_name: "remote_api".to_string(),
            kind: McpServerKind::ExternalApi,
            transport: McpTransportKind::Sse,
            auto_approve: false,
            allowed_tools: Vec::new(),
            capability_tags: vec!["remote".to_string()],
        };
        let descriptor = McpToolDescriptorSnapshot {
            server_name: "remote_api".to_string(),
            tool_name: "mcp__remote_api__list_orders".to_string(),
            original_name: "mcp://remote_api/list_orders".to_string(),
            description: "List orders from remote API".to_string(),
            input_schema: serde_json::json!({}),
            output_hint: None,
        };
        let result = system.check_with_mcp_metadata(
            "mcp__remote_api__list_orders",
            &serde_json::json!({}),
            Some(&profile),
            Some(&descriptor),
        );
        match result {
            PermissionResult::AskUser { request } => {
                assert_eq!(request.risk_level, RiskLevel::High);
                assert_eq!(request.policy_source, "mcp-profile:external-api-read");
            }
            other => panic!("expected AskUser, got {other:?}"),
        }
    }

    #[test]
    fn test_mcp_readonly_sse_requires_confirmation() {
        let cfg = SafetyConfig {
            default_policy: DefaultSafetyPolicy::Allow,
            ..Default::default()
        };
        let system = SafetySystem::new(cfg);
        let profile = McpServerProfile {
            server_name: "docs".to_string(),
            kind: McpServerKind::ReadOnly,
            transport: McpTransportKind::Sse,
            auto_approve: false,
            allowed_tools: Vec::new(),
            capability_tags: vec!["remote".to_string()],
        };
        let descriptor = McpToolDescriptorSnapshot {
            server_name: "docs".to_string(),
            tool_name: "mcp__docs__search".to_string(),
            original_name: "mcp://docs/search".to_string(),
            description: "Search remote docs".to_string(),
            input_schema: serde_json::json!({}),
            output_hint: None,
        };
        let result = system.check_with_mcp_metadata(
            "mcp__docs__search",
            &serde_json::json!({}),
            Some(&profile),
            Some(&descriptor),
        );
        match result {
            PermissionResult::AskUser { request } => {
                assert_eq!(request.risk_level, RiskLevel::Medium);
                assert_eq!(request.policy_source, "mcp-profile:readonly-remote");
            }
            other => panic!("expected AskUser, got {other:?}"),
        }
    }

    #[test]
    fn test_mcp_unknown_stdio_requires_confirmation() {
        let cfg = SafetyConfig {
            default_policy: DefaultSafetyPolicy::Allow,
            ..Default::default()
        };
        let system = SafetySystem::new(cfg);
        let profile = McpServerProfile {
            server_name: "unknown_srv".to_string(),
            kind: McpServerKind::Unknown,
            transport: McpTransportKind::Stdio,
            auto_approve: false,
            allowed_tools: Vec::new(),
            capability_tags: Vec::new(),
        };
        let descriptor = McpToolDescriptorSnapshot {
            server_name: "unknown_srv".to_string(),
            tool_name: "mcp__unknown_srv__list".to_string(),
            original_name: "mcp://unknown_srv/list".to_string(),
            description: "List something".to_string(),
            input_schema: serde_json::json!({}),
            output_hint: None,
        };
        let result = system.check_with_mcp_metadata(
            "mcp__unknown_srv__list",
            &serde_json::json!({}),
            Some(&profile),
            Some(&descriptor),
        );
        match result {
            PermissionResult::AskUser { request } => {
                assert_eq!(request.risk_level, RiskLevel::Medium);
                assert_eq!(request.policy_source, "mcp-profile:unknown");
            }
            other => panic!("expected AskUser for unknown stdio MCP, got {other:?}"),
        }
    }

    #[test]
    fn test_mcp_unknown_stdio_destructive_requires_high_risk() {
        let cfg = SafetyConfig {
            default_policy: DefaultSafetyPolicy::Allow,
            ..Default::default()
        };
        let system = SafetySystem::new(cfg);
        let profile = McpServerProfile {
            server_name: "unknown_srv".to_string(),
            kind: McpServerKind::Unknown,
            transport: McpTransportKind::Stdio,
            auto_approve: false,
            allowed_tools: Vec::new(),
            capability_tags: vec!["mutate".to_string()],
        };
        let descriptor = McpToolDescriptorSnapshot {
            server_name: "unknown_srv".to_string(),
            tool_name: "mcp__unknown_srv__delete".to_string(),
            original_name: "mcp://unknown_srv/delete".to_string(),
            description: "Delete items".to_string(),
            input_schema: serde_json::json!({}),
            output_hint: None,
        };
        let result = system.check_with_mcp_metadata(
            "mcp__unknown_srv__delete",
            &serde_json::json!({}),
            Some(&profile),
            Some(&descriptor),
        );
        match result {
            PermissionResult::AskUser { request } => {
                assert_eq!(request.risk_level, RiskLevel::High);
                assert_eq!(request.policy_source, "mcp-profile:unknown-destructive");
            }
            other => panic!("expected AskUser (High) for unknown destructive MCP, got {other:?}"),
        }
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

    // ── Pattern matching tests ──

    #[test]
    fn test_matches_exact() {
        assert!(SafetySystem::matches_pattern("read", "read"));
        assert!(!SafetySystem::matches_pattern("read", "write"));
    }

    #[test]
    fn test_matches_prefix_wildcard() {
        assert!(SafetySystem::matches_pattern("mcp__*", "mcp__akshare__get_news_data"));
        assert!(SafetySystem::matches_pattern("mcp__*", "mcp__filesystem__read"));
        assert!(!SafetySystem::matches_pattern("mcp__*", "stock_data"));
    }

    #[test]
    fn test_matches_suffix_wildcard() {
        assert!(SafetySystem::matches_pattern("*__get_news_data", "mcp__akshare__get_news_data"));
        assert!(!SafetySystem::matches_pattern("*__get_news_data", "mcp__akshare__get_hist_data"));
    }

    #[test]
    fn test_matches_middle_wildcard() {
        assert!(SafetySystem::matches_pattern("mcp__*__get_news_data", "mcp__akshare__get_news_data"));
        assert!(SafetySystem::matches_pattern("mcp__*__get_news_data", "mcp__filesystem__get_news_data"));
        assert!(!SafetySystem::matches_pattern("mcp__*__get_news_data", "mcp__akshare__get_hist_data"));
    }

    #[test]
    fn test_matches_server_specific() {
        assert!(SafetySystem::matches_pattern("mcp__akshare__*", "mcp__akshare__get_news_data"));
        assert!(SafetySystem::matches_pattern("mcp__akshare__*", "mcp__akshare__get_hist_data"));
        assert!(!SafetySystem::matches_pattern("mcp__akshare__*", "mcp__filesystem__read"));
        assert!(!SafetySystem::matches_pattern("mcp__akshare__*", "stock_data"));
    }

    // ── MCP server name extraction tests ──

    #[test]
    fn test_mcp_server_name_akshare() {
        assert_eq!(mcp_server_name("mcp__akshare__get_news_data"), Some("akshare"));
    }

    #[test]
    fn test_mcp_server_name_filesystem() {
        assert_eq!(mcp_server_name("mcp__filesystem__read"), Some("filesystem"));
    }

    #[test]
    fn test_mcp_server_name_none_for_non_mcp_tool() {
        assert_eq!(mcp_server_name("stock_data"), None);
        assert_eq!(mcp_server_name("read"), None);
    }

    // ── Allowlist/denylist with wildcards ──

    #[test]
    fn test_allowlist_with_wildcard_allows_mcp_subset() {
        let cfg = SafetyConfig {
            tool_allowlist: Some(vec![
                "read".to_string(),
                "mcp__akshare__*".to_string(),
            ]),
            default_policy: DefaultSafetyPolicy::Deny,
            ..Default::default()
        };
        let system = SafetySystem::new(cfg);
        assert!(matches!(system.check("read", &serde_json::json!({})), PermissionResult::Allow));
        assert!(matches!(
            system.check("mcp__akshare__get_news_data", &serde_json::json!({})),
            PermissionResult::Allow
        ));
        assert!(matches!(
            system.check("mcp__filesystem__read", &serde_json::json!({})),
            PermissionResult::Deny { .. }
        ));
        assert!(matches!(
            system.check("write", &serde_json::json!({})),
            PermissionResult::Deny { .. }
        ));
    }

    #[test]
    fn test_denylist_with_wildcard_blocks_mcp_subset() {
        let cfg = SafetyConfig {
            tool_denylist: Some(vec!["mcp__akshare__*".to_string()]),
            default_policy: DefaultSafetyPolicy::Allow,
            ..Default::default()
        };
        let system = SafetySystem::new(cfg);
        assert!(matches!(
            system.check("mcp__akshare__get_news_data", &serde_json::json!({})),
            PermissionResult::AskUser { .. }
        ));
        assert!(matches!(
            system.check("mcp__filesystem__read", &serde_json::json!({})),
            PermissionResult::Allow
        ));
    }

    #[test]
    fn test_denylist_overrides_allowlist_wildcard() {
        let cfg = SafetyConfig {
            tool_allowlist: Some(vec!["mcp__*".to_string()]),
            tool_denylist: Some(vec!["mcp__akshare__*".to_string()]),
            default_policy: DefaultSafetyPolicy::Deny,
            ..Default::default()
        };
        let system = SafetySystem::new(cfg);
        // denylist has priority — akshare tools require confirmation
        assert!(matches!(
            system.check("mcp__akshare__get_news_data", &serde_json::json!({})),
            PermissionResult::AskUser { .. }
        ));
        // filesystem is in allowlist but not denylist — allowed
        assert!(matches!(
            system.check("mcp__filesystem__read", &serde_json::json!({})),
            PermissionResult::Allow
        ));
    }

    // ── MCP auto-approve server tests ──

    #[test]
    fn test_mcp_auto_approve_allows_listed_server() {
        let cfg = SafetyConfig {
            default_policy: DefaultSafetyPolicy::Deny,
            mcp_auto_approve_servers: Some(vec!["akshare".to_string()]),
            ..Default::default()
        };
        let system = SafetySystem::new(cfg);
        assert!(matches!(
            system.check("mcp__akshare__get_news_data", &serde_json::json!({})),
            PermissionResult::Allow
        ));
        assert!(matches!(
            system.check("mcp__akshare__get_hist_data", &serde_json::json!({})),
            PermissionResult::Allow
        ));
    }

    #[test]
    fn test_mcp_auto_approve_denies_unlisted_server() {
        let cfg = SafetyConfig {
            default_policy: DefaultSafetyPolicy::Deny,
            mcp_auto_approve_servers: Some(vec!["akshare".to_string()]),
            ..Default::default()
        };
        let system = SafetySystem::new(cfg);
        assert!(matches!(
            system.check("mcp__filesystem__read", &serde_json::json!({})),
            PermissionResult::Deny { .. }
        ));
        assert!(matches!(
            system.check("stock_data", &serde_json::json!({})),
            PermissionResult::Deny { .. }
        ));
    }

    #[test]
    fn test_mcp_auto_approve_with_strict_default_deny() {
        let cfg = SafetyConfig {
            default_policy: DefaultSafetyPolicy::Deny,
            mcp_auto_approve_servers: Some(vec![
                "akshare".to_string(),
                "filesystem".to_string(),
            ]),
            ..Default::default()
        };
        let system = SafetySystem::new(cfg);
        // Auto-approved servers: allow
        assert!(matches!(
            system.check("mcp__akshare__get_news_data", &serde_json::json!({})),
            PermissionResult::Allow
        ));
        assert!(matches!(
            system.check("mcp__filesystem__read", &serde_json::json!({})),
            PermissionResult::Allow
        ));
        // Non-MCP tools: denied by default
        assert!(matches!(
            system.check("stock_data", &serde_json::json!({})),
            PermissionResult::Deny { .. }
        ));
    }

    #[test]
    fn test_mcp_auto_approve_allowlist_takes_priority() {
        // When both allowlist and mcp_auto_approve are set, allowlist takes
        // priority (checked first).  So if mcp_auto_approve lists "akshare"
        // but allowlist doesn't match, it's denied.
        let cfg = SafetyConfig {
            tool_allowlist: Some(vec!["stock_*".to_string()]),
            default_policy: DefaultSafetyPolicy::Deny,
            mcp_auto_approve_servers: Some(vec!["akshare".to_string()]),
            ..Default::default()
        };
        let system = SafetySystem::new(cfg);
        // allowlist takes priority → akshare tools NOT matched by allowlist → denied
        assert!(matches!(
            system.check("mcp__akshare__get_news_data", &serde_json::json!({})),
            PermissionResult::Deny { .. }
        ));
        // custom tool matched by allowlist → allowed
        assert!(matches!(
            system.check("stock_data", &serde_json::json!({})),
            PermissionResult::Allow
        ));
    }

    #[test]
    fn test_mcp_auto_approve_denylist_overrides() {
        let cfg = SafetyConfig {
            tool_denylist: Some(vec!["mcp__akshare__get_hist_data".to_string()]),
            default_policy: DefaultSafetyPolicy::Deny,
            mcp_auto_approve_servers: Some(vec!["akshare".to_string()]),
            ..Default::default()
        };
        let system = SafetySystem::new(cfg);
        // denylist takes priority over mcp_auto_approve
        assert!(matches!(
            system.check("mcp__akshare__get_hist_data", &serde_json::json!({})),
            PermissionResult::AskUser { .. }
        ));
        // other akshare tools still auto-approved
        assert!(matches!(
            system.check("mcp__akshare__get_news_data", &serde_json::json!({})),
            PermissionResult::Allow
        ));
    }
}
