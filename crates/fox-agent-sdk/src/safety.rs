use fox_agent_core::{
    DefaultSafetyPolicy, PermissionRequest, PermissionResult, SafetyConfig,
};
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

    pub fn check(&self, tool_name: &str, input: &serde_json::Value) -> PermissionResult {
        // If a custom permission hook is registered, delegate to it
        if let Some(ref hook) = self.inner.custom_hook {
            return hook(tool_name, input);
        }

        // Built-in allowlist/denylist logic:
        //
        // Priority:
        // 1. If tool is in denylist → always AskUser (require confirmation)
        // 2. If allowlist is configured and tool is NOT in allowlist → Deny
        // 3. If allowlist is configured and tool IS in allowlist → Allow
        // 4. If allowlist is NOT configured → follow default_policy

        // Rule 1: denylist check first
        if let Some(ref denylist) = self.inner.cfg.tool_denylist {
            if denylist.iter().any(|d| d == tool_name) {
                return PermissionResult::AskUser {
                    request: PermissionRequest::new(
                        tool_name,
                        format!("tool `{tool_name}` is in the denylist and requires your confirmation"),
                    ).with_risk(
                        fox_agent_core::RiskLevel::High,
                        "denylist",
                        tool_name.to_string(),
                    ),
                };
            }
        }

        // Rules 2 & 3: allowlist check
        if let Some(ref allowlist) = self.inner.cfg.tool_allowlist {
            if allowlist.iter().any(|a| a == tool_name) {
                // Rule 3: explicitly in allowlist → Allow
                return PermissionResult::Allow;
            }
            // Rule 2: allowlist configured but tool not found → Deny
            return PermissionResult::Deny {
                reason: format!(
                    "tool `{tool_name}` is not in the allowlist and has been denied"
                ),
            };
        }

        // Rule 4: no allowlist configured → follow default_policy
        match self.inner.cfg.default_policy {
            DefaultSafetyPolicy::Allow => PermissionResult::Allow,
            DefaultSafetyPolicy::Deny => PermissionResult::Deny {
                reason: format!("tool `{tool_name}` has been denied by default policy"),
            },
            DefaultSafetyPolicy::Confirm => PermissionResult::AskUser {
                request: PermissionRequest::new(
                    tool_name,
                    format!("tool `{tool_name}` requires your confirmation"),
                ).with_risk(
                    fox_agent_core::RiskLevel::Medium,
                    "default:confirm",
                    tool_name.to_string(),
                ),
            },
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
            tool_allowlist: None,
            default_policy: DefaultSafetyPolicy::Allow,
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
            tool_denylist: None,
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
            tool_denylist: None,
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
            tool_allowlist: None,
            tool_denylist: None,
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
            tool_allowlist: None,
            tool_denylist: None,
            default_policy: DefaultSafetyPolicy::Deny,
            ..Default::default()
        };
        let system = SafetySystem::new(cfg);
        let result = system.check("read", &serde_json::json!({}));
        assert!(matches!(result, PermissionResult::Deny { .. }));
    }

    #[test]
    fn test_allowlist_overrides_denylist_check() {
        // A tool in both allowlist and denylist: denylist takes priority
        let cfg = SafetyConfig {
            tool_allowlist: Some(vec!["bash".to_string()]),
            tool_denylist: Some(vec!["bash".to_string()]),
            default_policy: DefaultSafetyPolicy::Allow,
            ..Default::default()
        };
        let system = SafetySystem::new(cfg);
        let result = system.check("bash", &serde_json::json!({}));
        assert!(matches!(result, PermissionResult::AskUser { .. }));
    }
}
