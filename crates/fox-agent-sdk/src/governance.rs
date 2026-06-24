//! Governance: budget enforcement, metrics collection, and cost tracking.
//!
//! Integrates [`BudgetConfig`] and [`MetricsSnapshot`] into the Agent
//! turn loop, stopping execution when token or cost budgets are exceeded.

use fox_agent_core::{BudgetConfig, MetricsSnapshot, TokenUsage};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

/// Runtime governor that enforces budget limits and collects metrics.
#[derive(Clone)]
pub struct GovernanceGuard {
    config: BudgetConfig,
    metrics: Arc<RwLock<MetricsSnapshot>>,
    /// Hooks called after each model response with usage + latency.
    metrics_hooks: Arc<RwLock<Vec<Arc<dyn Fn(&MetricsSnapshot) + Send + Sync>>>>,
    /// Number of turns completed this session.
    turns: Arc<RwLock<u64>>,
    /// Latency tracker for a single turn.
    turn_start: Arc<RwLock<Option<Instant>>>,
}

impl GovernanceGuard {
    /// Create a new guard with the given budget config.
    pub fn new(config: BudgetConfig) -> Self {
        Self {
            config,
            metrics: Arc::new(RwLock::new(MetricsSnapshot::default())),
            metrics_hooks: Arc::new(RwLock::new(Vec::new())),
            turns: Arc::new(RwLock::new(0)),
            turn_start: Arc::new(RwLock::new(None)),
        }
    }

    /// Register a callback to be invoked after each usage record.
    pub async fn add_metrics_hook(
        &self,
        hook: impl Fn(&MetricsSnapshot) + Send + Sync + 'static,
    ) {
        self.metrics_hooks.write().await.push(Arc::new(hook));
    }

    /// Signal the start of a turn.
    pub async fn turn_begin(&self) {
        *self.turn_start.write().await = Some(Instant::now());
    }

    /// Signal the end of a turn. Returns an error if budget was exceeded.
    pub async fn turn_end(&self) -> Result<(), String> {
        let mut t = self.turns.write().await;
        *t += 1;
        let turns = *t;
        drop(t);
        // Clear latency tracker
        *self.turn_start.write().await = None;
        // Check max turns
        if self.config.max_turns > 0 && turns > self.config.max_turns {
            return Err(format!(
                "max turns exceeded: {}/{}",
                turns, self.config.max_turns
            ));
        }
        // Check budgets
        let metrics = self.metrics.read().await;
        if let Some(msg) = metrics.exceeds_budget(&self.config) {
            return Err(msg);
        }
        Ok(())
    }

    /// Record model usage (called after each model response).
    pub async fn record_usage(
        &self,
        usage: &TokenUsage,
        provider_latency_ms: u64,
        cost_cents: u64,
    ) -> Result<(), String> {
        {
            let mut m = self.metrics.write().await;
            m.record(usage, provider_latency_ms, cost_cents);
        }
        // Fire hooks
        let hooks = self.metrics_hooks.read().await;
        let metrics = self.metrics.read().await;
        for hook in hooks.iter() {
            hook(&metrics);
        }
        drop(hooks);
        // Check budget after recording
        if let Some(msg) = metrics.exceeds_budget(&self.config) {
            return Err(msg);
        }
        Ok(())
    }

    /// Return a snapshot of current metrics.
    pub async fn snapshot(&self) -> MetricsSnapshot {
        self.metrics.read().await.clone()
    }

    /// Record a tool execution success.
    pub async fn record_tool_success(&self) {
        self.metrics.write().await.record_tool_success();
    }

    /// Record a tool execution error.
    pub async fn record_tool_error(&self) {
        self.metrics.write().await.record_tool_error();
    }

    /// Record a compaction event.
    pub async fn record_compaction(&self) {
        self.metrics.write().await.record_compaction();
    }

    /// Get the budget config.
    pub fn budget(&self) -> &BudgetConfig {
        &self.config
    }

    /// Check whether a turn is currently active.
    pub async fn is_turn_active(&self) -> bool {
        self.turn_start.read().await.is_some()
    }
}

/// Calculate estimated cost in cents for a token usage.
///
/// Prices are approximate per-model defaults. For production use,
/// inject pricing via a custom `metrics_hook`.
pub fn estimate_cost_cents(model_id: &str, usage: &TokenUsage) -> u64 {
    let (input_price_per_1m, output_price_per_1m) = match model_id {
        id if id.contains("deepseek") => (140, 280),     // DeepSeek v3 pricing
        id if id.contains("claude") => (3000, 15000),     // Claude Sonnet
        id if id.contains("gpt-4o") => (2500, 10000),     // GPT-4o
        _ => (500, 1500),                                  // default conservative
    };

    let input_cost = usage.input_tokens as u64 * input_price_per_1m / 1_000_000;
    let output_cost = usage.output_tokens as u64 * output_price_per_1m / 1_000_000;
    input_cost + output_cost
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_cost_for_known_models() {
        let usage = TokenUsage {
            input_tokens: 1000,
            output_tokens: 500,
            total_tokens: 1500,
            cache_read_input_tokens: Some(0),
            cache_creation_input_tokens: Some(0),
        };
        let cost = estimate_cost_cents("deepseek-v4-flash", &usage);
        // 1000*140/1M = 0.14, 500*280/1M = 0.14 → 0 (integer cents)
        assert_eq!(cost, 0);

        let big_usage = TokenUsage {
            input_tokens: 100_000,
            output_tokens: 50_000,
            total_tokens: 150_000,
            cache_read_input_tokens: Some(0),
            cache_creation_input_tokens: Some(0),
        };
        let cost = estimate_cost_cents("deepseek-v4-flash", &big_usage);
        // 100000*140/1M=14, 50000*280/1M=14 → 28 cents
        assert!(cost >= 14);
    }

    #[tokio::test]
    async fn budget_enforcement_stops_on_token_exceeded() {
        let config = BudgetConfig {
            token_budget: Some(1000),
            ..BudgetConfig::default()
        };
        let guard = GovernanceGuard::new(config);

        let usage = TokenUsage {
            input_tokens: 800,
            output_tokens: 300,
            total_tokens: 1100,
            cache_read_input_tokens: Some(0),
            cache_creation_input_tokens: Some(0),
        };
        let result = guard.record_usage(&usage, 100, 0).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("token budget exceeded"));
    }

    #[tokio::test]
    async fn budget_enforcement_passes_when_under_limit() {
        let config = BudgetConfig::default();
        let guard = GovernanceGuard::new(config);

        let usage = TokenUsage {
            input_tokens: 100,
            output_tokens: 50,
            total_tokens: 150,
            cache_read_input_tokens: Some(0),
            cache_creation_input_tokens: Some(0),
        };
        let result = guard.record_usage(&usage, 50, 0).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn metrics_snapshot_accumulates_correctly() {
        let config = BudgetConfig::default();
        let guard = GovernanceGuard::new(config);

        guard.record_usage(&TokenUsage { input_tokens:100, output_tokens:50, total_tokens:150, cache_read_input_tokens:Some(0), cache_creation_input_tokens:Some(0) }, 100, 5).await.unwrap();
        guard.record_usage(&TokenUsage { input_tokens:200, output_tokens:100, total_tokens:300, cache_read_input_tokens:Some(0), cache_creation_input_tokens:Some(0) }, 200, 3).await.unwrap();

        let snap = guard.snapshot().await;
        assert_eq!(snap.total_input_tokens, 300);
        assert_eq!(snap.total_output_tokens, 150);
        assert_eq!(snap.total_tokens, 450);
        assert_eq!(snap.estimated_cost_cents, 8);
        assert_eq!(snap.total_latency_ms, 300);
    }

    #[tokio::test]
    async fn max_turns_enforcement() {
        let config = BudgetConfig { max_turns: 2, ..BudgetConfig::default() };
        let guard = GovernanceGuard::new(config);

        guard.turn_begin().await;
        assert!(guard.turn_end().await.is_ok()); // turn 1

        guard.turn_begin().await;
        assert!(guard.turn_end().await.is_ok()); // turn 2

        guard.turn_begin().await;
        assert!(guard.turn_end().await.is_err()); // turn 3 exceeds
    }

    #[tokio::test]
    async fn metrics_hook_fires_on_usage() {
        let config = BudgetConfig::default();
        let guard = GovernanceGuard::new(config);
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let c = counter.clone();
        guard.add_metrics_hook(move |_| { c.fetch_add(1, std::sync::atomic::Ordering::SeqCst); }).await;

        guard.record_usage(&TokenUsage { input_tokens:10, output_tokens:5, total_tokens:15, cache_read_input_tokens:Some(0), cache_creation_input_tokens:Some(0) }, 10, 0).await.unwrap();
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
