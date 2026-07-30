//! ApprovalManager: permission cache and audit trail.
//!
//! The manager sits between the SafetySystem and the user, providing
//! three layers of caching (this-turn, this-session, this-workspace)
//! and a structured audit log. Waiting for a user permission decision
//! never times out — a pending request stays pending until the user
//! explicitly allows or denies it.

use fox_agent_core::{
    ApprovalCacheEntry, ApprovalScope, PermissionAuditEntry, PermissionRequest, PermissionResult,
    SafetyConfig,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::event_recorder::EventRecorder;

/// Manages approval caching, timeout, and audit.
pub struct ApprovalManager {
    /// Turn-level cache (cleared at end of turn)
    turn_cache: Mutex<HashMap<String, ApprovalCacheEntry>>,
    /// Session-level cache (persists across turns)
    session_cache: Mutex<HashMap<String, ApprovalCacheEntry>>,
    /// Workspace-level cache (persists across session restarts)
    workspace_cache: Mutex<HashMap<String, ApprovalCacheEntry>>,
    /// Audit trail
    audit_log: Mutex<Vec<PermissionAuditEntry>>,
    /// Configuration
    config: SafetyConfig,
    /// Session id for audit entries
    session_id: String,
    /// Current turn id
    turn_id: Mutex<u64>,
    /// Optional event recorder for audit export
    recorder: Option<Arc<EventRecorder>>,
}

impl ApprovalManager {
    /// Create a new manager.
    pub fn new(session_id: impl Into<String>, config: SafetyConfig) -> Self {
        Self {
            turn_cache: Mutex::new(HashMap::new()),
            session_cache: Mutex::new(HashMap::new()),
            workspace_cache: Mutex::new(HashMap::new()),
            audit_log: Mutex::new(Vec::new()),
            config,
            session_id: session_id.into(),
            turn_id: Mutex::new(1),
            recorder: None,
        }
    }

    /// Set an event recorder for audit trail export.
    pub fn set_recorder(&mut self, recorder: Arc<EventRecorder>) {
        self.recorder = Some(recorder);
    }

    /// Advance to a new turn (clears turn cache, increments turn_id).
    pub async fn next_turn(&self) {
        self.turn_cache.lock().await.clear();
        let mut t = self.turn_id.lock().await;
        *t += 1;
    }

    /// Check whether a permission request is already cached.
    ///
    /// Returns `Some(PermissionResult)` if a cached entry matches,
    /// otherwise `None` (caller must perform full safety check).
    pub async fn check_cache(&self, tool_name: &str) -> Option<PermissionResult> {
        let cache_config = &self.config.approval_cache;
        if !cache_config.enabled {
            return None;
        }

        let now = fox_agent_core::now_secs();

        // Check turn cache
        {
            let cache = self.turn_cache.lock().await;
            if let Some(entry) = cache.get(tool_name)
                && !self.is_expired(entry, now)
            {
                return Some(entry.decision.clone());
            }
        }
        // Check session cache
        {
            let cache = self.session_cache.lock().await;
            if let Some(entry) = cache.get(tool_name)
                && !self.is_expired(entry, now)
            {
                return Some(entry.decision.clone());
            }
        }
        // Check workspace cache
        {
            let cache = self.workspace_cache.lock().await;
            if let Some(entry) = cache.get(tool_name)
                && !self.is_expired(entry, now)
            {
                return Some(entry.decision.clone());
            }
        }
        None
    }

    /// Store a decision in the cache according to its scope.
    pub async fn cache_decision(
        &self,
        tool_name: &str,
        decision: &PermissionResult,
        scope: ApprovalScope,
    ) {
        let now = fox_agent_core::now_secs();
        let ttl = self.config.approval_cache.ttl_secs;
        let expires_at = if ttl > 0 { Some(now + ttl) } else { None };

        let entry = ApprovalCacheEntry {
            tool_name: tool_name.to_string(),
            workspace_key: None,
            decision: decision.clone(),
            scope,
            expires_at,
            created_at: now,
        };

        match scope {
            ApprovalScope::ThisTurn => {
                self.insert_or_update(&self.turn_cache, tool_name, entry)
                    .await;
            }
            ApprovalScope::ThisSession => {
                self.insert_or_update(&self.session_cache, tool_name, entry)
                    .await;
            }
            ApprovalScope::ThisWorkspace => {
                self.insert_or_update(&self.workspace_cache, tool_name, entry)
                    .await;
            }
        }
    }

    /// Record a permission decision in the audit trail.
    pub async fn record_audit(
        &self,
        request: &PermissionRequest,
        decision: &PermissionResult,
        latency_ms: u64,
    ) {
        let entry = PermissionAuditEntry {
            timestamp: fox_agent_core::now_secs(),
            session_id: self.session_id.clone(),
            turn_id: *self.turn_id.lock().await,
            tool_name: request.tool_name.clone(),
            input: serde_json::Value::Null, // filled by caller if needed
            decision: decision.clone(),
            request_id: request.request_id.clone(),
            latency_ms,
        };
        self.audit_log.lock().await.push(entry);
    }

    /// Return a copy of the audit trail.
    pub async fn audit_log(&self) -> Vec<PermissionAuditEntry> {
        self.audit_log.lock().await.clone()
    }

    /// Export the audit trail to a JSONL file.
    pub async fn export_audit(&self, path: &std::path::PathBuf) -> std::io::Result<()> {
        let log = self.audit_log.lock().await;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut f = std::fs::File::create(path)?;
        for entry in log.iter() {
            let line = serde_json::to_string(entry)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
            let _ = std::io::Write::write_fmt(&mut f, format_args!("{line}\n"));
        }
        Ok(())
    }

    // ── Private helpers ──

    fn is_expired(&self, entry: &ApprovalCacheEntry, now: u64) -> bool {
        if let Some(expires) = entry.expires_at {
            now >= expires
        } else {
            false
        }
    }

    async fn insert_or_update(
        &self,
        cache: &Mutex<HashMap<String, ApprovalCacheEntry>>,
        tool_name: &str,
        entry: ApprovalCacheEntry,
    ) {
        let mut map = cache.lock().await;
        if map.len() >= self.config.approval_cache.max_entries {
            // Evict the oldest entry
            if let Some(oldest_key) = map
                .iter()
                .min_by_key(|(_, v)| v.created_at)
                .map(|(k, _)| k.clone())
            {
                map.remove(&oldest_key);
            }
        }
        map.insert(tool_name.to_string(), entry);
    }
}
