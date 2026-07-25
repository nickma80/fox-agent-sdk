//! Unified routing policy engine and governance metrics (Phase 4).
//!
//! The routing engine consolidates all factors that influence how a tool
//! result is handled — context pressure, result size, tool type, MCP profile —
//! into a single [`ToolResultRouting`] decision.
//!
//! Governance metrics track aggregate usage patterns across all sessions
//! to give operators visibility into storage pressure, sub-agent usage,
//! and compaction frequency.

use fox_agent_core::{
    ArtifactStoreConfig, McpServerKind, McpServerProfile, McpToolDescriptorSnapshot,
    RoutingPolicyConfig, ToolResultRouting,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tracing::{debug, info, trace, warn};

// ── Routing Policy Engine ──

/// Inputs to the routing decision.
pub struct RoutingInput<'a> {
    pub tool_name: &'a str,
    pub raw_output_text: &'a str,
    pub truncated_by_context_guard: bool,
    /// 0.0–1.0: how full the context window is.
    pub context_pressure: f64,
    pub mcp_profile: Option<&'a McpServerProfile>,
    pub mcp_descriptor: Option<&'a McpToolDescriptorSnapshot>,
    /// Number of consecutive tool-heavy exploration turns.
    pub consecutive_exploration_turns: u32,
}

impl<'a> RoutingInput<'a> {
    /// Create a minimal routing input for non-MCP tools.
    pub fn simple(tool_name: &'a str, raw_output_text: &'a str) -> Self {
        Self {
            tool_name,
            raw_output_text,
            truncated_by_context_guard: false,
            context_pressure: 0.0,
            mcp_profile: None,
            mcp_descriptor: None,
            consecutive_exploration_turns: 0,
        }
    }
}

/// Unified routing policy engine.
#[derive(Clone)]
pub struct RoutingPolicyEngine {
    cfg: RoutingPolicyConfig,
}

impl RoutingPolicyEngine {
    pub fn new(cfg: RoutingPolicyConfig) -> Self {
        Self { cfg }
    }

    /// Determine how a tool result should be handled.
    pub fn decide(&self, input: &RoutingInput<'_>, artifact_cfg: &ArtifactStoreConfig) -> ToolResultRouting {
        // Fast-path: context guard already truncated — always externalize
        if input.truncated_by_context_guard {
            debug!(
                tool = %input.tool_name,
                output_len = input.raw_output_text.len(),
                "routing: Externalize (context guard truncated)"
            );
            return ToolResultRouting::Externalize;
        }

        // Delegate to the existing externalize decision for MCP-specific logic,
        // then decide full routing.
        let externalize: bool = should_externalize(
            artifact_cfg,
            input,
        );

        // Context pressure escalates the decision
        let pressure = input.context_pressure;
        let threshold = self.cfg.context_pressure_threshold;

        if pressure >= threshold && self.cfg.enabled {
            // High pressure: try to delegate first, then externalize
            if self.is_delegate_candidate(input.tool_name) {
                debug!(
                    tool = %input.tool_name,
                    pressure = pressure,
                    threshold = threshold,
                    "routing: DelegateToSubagent (high context pressure)"
                );
                return ToolResultRouting::DelegateToSubagent;
            }
            if externalize {
                debug!(
                    tool = %input.tool_name,
                    pressure = pressure,
                    "routing: Externalize (high context pressure)"
                );
                return ToolResultRouting::Externalize;
            }
            debug!(
                tool = %input.tool_name,
                pressure = pressure,
                "routing: Externalize (forced under pressure)"
            );
            return ToolResultRouting::Externalize; // force externalize under pressure
        }

        // Normal pressure: use size-based thresholds
        let chars = input.raw_output_text.len();

        if self.cfg.enabled
            && self.is_delegate_candidate(input.tool_name)
            && chars > self.cfg.local_delegate_threshold_chars
        {
            debug!(
                tool = %input.tool_name,
                chars = chars,
                threshold = self.cfg.local_delegate_threshold_chars,
                "routing: DelegateToSubagent (large output)"
            );
            return ToolResultRouting::DelegateToSubagent;
        }

        if externalize && chars > self.cfg.local_externalize_threshold_chars {
            debug!(
                tool = %input.tool_name,
                chars = chars,
                threshold = self.cfg.local_externalize_threshold_chars,
                "routing: Externalize (large output)"
            );
            return ToolResultRouting::Externalize;
        }

        // Size-based externalization for ANY tool exceeding the threshold,
        // even if should_externalize() returned false (e.g. custom tools).
        if self.cfg.enabled && chars > self.cfg.local_externalize_threshold_chars {
            debug!(
                tool = %input.tool_name,
                chars = chars,
                threshold = self.cfg.local_externalize_threshold_chars,
                "routing: Externalize (size threshold)"
            );
            return ToolResultRouting::Externalize;
        }

        if externalize {
            debug!(
                tool = %input.tool_name,
                chars = chars,
                "routing: SummarizeOnly"
            );
            return ToolResultRouting::SummarizeOnly;
        }

        trace!(
            tool = %input.tool_name,
            chars = chars,
            "routing: Inline"
        );
        ToolResultRouting::Inline
    }

    fn is_delegate_candidate(&self, tool_name: &str) -> bool {
        self.cfg.delegate_candidate_tools.iter().any(|pattern| {
            if !pattern.contains('*') {
                return pattern == tool_name;
            }
            // Simple wildcard matching
            let pattern = pattern.replace('*', "");
            tool_name.contains(&pattern)
        })
    }
}

// ── Externalize decision helper (consolidated from agent.rs) ──

fn should_externalize(
    artifact_cfg: &ArtifactStoreConfig,
    input: &RoutingInput<'_>,
) -> bool {
    let tool_name = input.tool_name;
    let raw_output_text = input.raw_output_text;

    // Parse MCP tool name
    let mcp_info = parse_mcp_tool_name(tool_name);
    if let Some((server_name, _mcp_tool_name)) = &mcp_info {
        let profile = input.mcp_profile;
        let descriptor = input.mcp_descriptor;

        let is_html_payload = raw_output_text.to_lowercase().contains("<html");
        let descriptor_text = descriptor
            .map(|d| {
                format!(
                    "{} {}",
                    d.description.to_lowercase(),
                    d.original_name.to_lowercase()
                )
            })
            .unwrap_or_default();
        let noisy_keywords = [
            "search", "read", "find", "list", "grep", "glob", "ls", "fetch",
            "html", "navigate", "screenshot", "browse",
        ];
        let noisy_descriptor = noisy_keywords
            .iter()
            .any(|kw| descriptor_text.contains(kw));

        // SSE remote: externalize medium+ outputs
        if matches!(profile.map(|p| p.transport), Some(fox_agent_core::McpTransportKind::Sse)) {
            if raw_output_text.len() > 5_000 && noisy_descriptor {
                return true;
            }
            if raw_output_text.len() > 20_000 {
                return true;
            }
        }

        // Browser: never store full HTML inline by default
        if matches!(profile.map(|p| p.kind), Some(McpServerKind::Browser))
            && !artifact_cfg.mcp_browser_store_full_html
            && is_html_payload
        {
            return true;
        }

        // Filesystem: externalize large reads
        if matches!(profile.map(|p| p.kind), Some(McpServerKind::Filesystem))
            && raw_output_text.len() > 5_000
            && noisy_descriptor
        {
            return true;
        }

        // External API: always externalize (they can be very large)
        if matches!(profile.map(|p| p.kind), Some(McpServerKind::ExternalApi))
            && raw_output_text.len() > 2_000
        {
            return true;
        }

        // Unknown MCP: conservative — externalize large outputs
        if matches!(profile.map(|p| p.kind), Some(McpServerKind::Unknown) | None)
            && raw_output_text.len() > 5_000
        {
            return true;
        }

        // Shell: externalize large outputs
        if matches!(profile.map(|p| p.kind), Some(McpServerKind::Shell))
            && raw_output_text.len() > 10_000
        {
            return true;
        }
    }

    // Non-MCP tool: externalize if over threshold
    raw_output_text.len() > 8_000
}

// ── MCP name parsing ──

fn parse_mcp_tool_name(tool_name: &str) -> Option<(String, String)> {
    let rest = tool_name.strip_prefix("mcp__")?;
    let idx = rest.find("__")?;
    Some((rest[..idx].to_string(), rest[idx + 2..].to_string()))
}

// ── Governance Metrics ──

/// Aggregate metrics collected across all sessions for governance oversight.
///
/// All counters use atomic operations so they can be updated from multiple
/// turn loops without additional synchronisation.
#[derive(Clone)]
pub struct GovernanceMetrics {
    inner: Arc<MetricsInner>,
}

struct MetricsInner {
    // Artifact metrics
    artifact_write_count: AtomicU64,
    artifact_write_bytes: AtomicU64,
    artifact_read_count: AtomicU64,
    artifact_gc_deleted: AtomicU64,
    artifact_gc_bytes_freed: AtomicU64,
    // Routing metrics
    inline_count: AtomicU64,
    summarize_only_count: AtomicU64,
    externalize_count: AtomicU64,
    delegate_count: AtomicU64,
    // Sub-agent metrics
    subagent_task_count: AtomicU64,
    subagent_success_count: AtomicU64,
    subagent_timeout_count: AtomicU64,
    subagent_error_count: AtomicU64,
    // Compaction metrics
    compaction_trigger_count: AtomicU64,
    // MCP metrics
    mcp_call_count: AtomicU64,
}

impl GovernanceMetrics {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(MetricsInner {
                artifact_write_count: AtomicU64::new(0),
                artifact_write_bytes: AtomicU64::new(0),
                artifact_read_count: AtomicU64::new(0),
                artifact_gc_deleted: AtomicU64::new(0),
                artifact_gc_bytes_freed: AtomicU64::new(0),
                inline_count: AtomicU64::new(0),
                summarize_only_count: AtomicU64::new(0),
                externalize_count: AtomicU64::new(0),
                delegate_count: AtomicU64::new(0),
                subagent_task_count: AtomicU64::new(0),
                subagent_success_count: AtomicU64::new(0),
                subagent_timeout_count: AtomicU64::new(0),
                subagent_error_count: AtomicU64::new(0),
                compaction_trigger_count: AtomicU64::new(0),
                mcp_call_count: AtomicU64::new(0),
            }),
        }
    }

    // ── Atomic updates ──

    pub fn record_artifact_write(&self, bytes: u64) {
        self.inner.artifact_write_count.fetch_add(1, Ordering::Relaxed);
        self.inner.artifact_write_bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn record_artifact_read(&self) {
        self.inner.artifact_read_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_artifact_gc(&self, deleted: u64, bytes_freed: u64) {
        self.inner.artifact_gc_deleted.fetch_add(deleted, Ordering::Relaxed);
        self.inner.artifact_gc_bytes_freed.fetch_add(bytes_freed, Ordering::Relaxed);
    }

    pub fn record_routing(&self, routing: ToolResultRouting) {
        match routing {
            ToolResultRouting::Inline => { self.inner.inline_count.fetch_add(1, Ordering::Relaxed); }
            ToolResultRouting::SummarizeOnly => { self.inner.summarize_only_count.fetch_add(1, Ordering::Relaxed); }
            ToolResultRouting::Externalize => { self.inner.externalize_count.fetch_add(1, Ordering::Relaxed); }
            ToolResultRouting::DelegateToSubagent => { self.inner.delegate_count.fetch_add(1, Ordering::Relaxed); }
        }
    }

    pub fn record_subagent_success(&self) {
        self.inner.subagent_task_count.fetch_add(1, Ordering::Relaxed);
        self.inner.subagent_success_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_subagent_timeout(&self) {
        self.inner.subagent_task_count.fetch_add(1, Ordering::Relaxed);
        self.inner.subagent_timeout_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_subagent_error(&self) {
        self.inner.subagent_task_count.fetch_add(1, Ordering::Relaxed);
        self.inner.subagent_error_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_compaction(&self) {
        self.inner.compaction_trigger_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_mcp_call(&self) {
        self.inner.mcp_call_count.fetch_add(1, Ordering::Relaxed);
    }

    // ── Snapshot reading ──

    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            artifact_write_count: self.inner.artifact_write_count.load(Ordering::Relaxed),
            artifact_write_bytes: self.inner.artifact_write_bytes.load(Ordering::Relaxed),
            artifact_read_count: self.inner.artifact_read_count.load(Ordering::Relaxed),
            artifact_gc_deleted: self.inner.artifact_gc_deleted.load(Ordering::Relaxed),
            artifact_gc_bytes_freed: self.inner.artifact_gc_bytes_freed.load(Ordering::Relaxed),
            inline_count: self.inner.inline_count.load(Ordering::Relaxed),
            summarize_only_count: self.inner.summarize_only_count.load(Ordering::Relaxed),
            externalize_count: self.inner.externalize_count.load(Ordering::Relaxed),
            delegate_count: self.inner.delegate_count.load(Ordering::Relaxed),
            subagent_task_count: self.inner.subagent_task_count.load(Ordering::Relaxed),
            subagent_success_count: self.inner.subagent_success_count.load(Ordering::Relaxed),
            subagent_timeout_count: self.inner.subagent_timeout_count.load(Ordering::Relaxed),
            subagent_error_count: self.inner.subagent_error_count.load(Ordering::Relaxed),
            compaction_trigger_count: self.inner.compaction_trigger_count.load(Ordering::Relaxed),
            mcp_call_count: self.inner.mcp_call_count.load(Ordering::Relaxed),
        }
    }
}

impl Default for GovernanceMetrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Point-in-time snapshot of governance metrics.
#[derive(Debug, Clone, Default)]
pub struct MetricsSnapshot {
    pub artifact_write_count: u64,
    pub artifact_write_bytes: u64,
    pub artifact_read_count: u64,
    pub artifact_gc_deleted: u64,
    pub artifact_gc_bytes_freed: u64,
    pub inline_count: u64,
    pub summarize_only_count: u64,
    pub externalize_count: u64,
    pub delegate_count: u64,
    pub subagent_task_count: u64,
    pub subagent_success_count: u64,
    pub subagent_timeout_count: u64,
    pub subagent_error_count: u64,
    pub compaction_trigger_count: u64,
    pub mcp_call_count: u64,
}

impl MetricsSnapshot {
    /// Format a human-readable summary for operator dashboards.
    pub fn format_summary(&self) -> String {
        let total_routing = self.inline_count
            + self.summarize_only_count
            + self.externalize_count
            + self.delegate_count;
        let pct = |n: u64| -> f64 {
            if total_routing == 0 { 0.0 } else { (n as f64) / (total_routing as f64) * 100.0 }
        };
        format!(
            "Governance Metrics:\n\
             Routing: {total} total — inline {i} ({ip:.0}%), summarize {s} ({sp:.0}%), \
             externalize {e} ({ep:.0}%), delegate {d} ({dp:.0}%)\n\
             Artifacts: {aw} writes ({ab} bytes), {ar} reads, gc: {gd} deleted ({gf} bytes freed)\n\
             Sub-agents: {st} tasks — {ss} success, {sto} timeout, {se} error\n\
             Compaction: {comp} triggers, MCP: {mcp} calls",
            total = total_routing,
            i = self.inline_count, ip = pct(self.inline_count),
            s = self.summarize_only_count, sp = pct(self.summarize_only_count),
            e = self.externalize_count, ep = pct(self.externalize_count),
            d = self.delegate_count, dp = pct(self.delegate_count),
            aw = self.artifact_write_count, ab = self.artifact_write_bytes,
            ar = self.artifact_read_count,
            gd = self.artifact_gc_deleted, gf = self.artifact_gc_bytes_freed,
            st = self.subagent_task_count, ss = self.subagent_success_count,
            sto = self.subagent_timeout_count, se = self.subagent_error_count,
            comp = self.compaction_trigger_count, mcp = self.mcp_call_count,
        )
    }
}
