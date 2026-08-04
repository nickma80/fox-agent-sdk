//! Token report: aggregates TokenUsage across an entire task execution.
//!
//! Used by the evaluation harness to track token efficiency per benchmark case.

use crate::provider::TokenUsage;
use serde::{Deserialize, Serialize};

/// Aggregated token consumption for a task execution.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenReport {
    /// Total input tokens across all API calls.
    pub total_input: u64,

    /// Total output tokens across all API calls.
    pub total_output: u64,

    /// Cache read tokens (prefix caching hits).
    pub cache_read: u64,

    /// Cache write tokens (prefix caching writes).
    pub cache_write: u64,

    /// Number of tool call round-trips.
    pub tool_calls: u64,

    /// Number of compaction events triggered.
    pub compactions: u64,

    /// Number of model API calls made.
    pub api_calls: u64,
}

impl TokenReport {
    /// Record a single TokenUsage event.
    pub fn record_usage(&mut self, usage: &TokenUsage) {
        self.total_input += usage.input_tokens as u64;
        self.total_output += usage.output_tokens as u64;
        self.cache_read += usage.cache_read_input_tokens.unwrap_or(0) as u64;
        self.cache_write += usage.cache_creation_input_tokens.unwrap_or(0) as u64;
        self.api_calls += 1;
    }

    /// Record a tool call.
    pub fn record_tool_call(&mut self) {
        self.tool_calls += 1;
    }

    /// Record a compaction event.
    pub fn record_compaction(&mut self) {
        self.compactions += 1;
    }

    /// Total tokens consumed (input + output).
    pub fn total_tokens(&self) -> u64 {
        self.total_input + self.total_output
    }

    /// Cache hit ratio (0.0–1.0).
    pub fn cache_hit_ratio(&self) -> f64 {
        if self.total_input == 0 {
            return 0.0;
        }
        self.cache_read as f64 / self.total_input as f64
    }

    /// Merge another report into this one.
    pub fn merge(&mut self, other: &TokenReport) {
        self.total_input += other.total_input;
        self.total_output += other.total_output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
        self.tool_calls += other.tool_calls;
        self.compactions += other.compactions;
        self.api_calls += other.api_calls;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_usage() {
        let mut report = TokenReport::default();
        let usage = TokenUsage {
            input_tokens: 100,
            output_tokens: 50,
            total_tokens: 150,
            cache_read_input_tokens: Some(80),
            cache_creation_input_tokens: Some(20),
        };
        report.record_usage(&usage);
        assert_eq!(report.total_input, 100);
        assert_eq!(report.total_output, 50);
        assert_eq!(report.cache_read, 80);
        assert_eq!(report.api_calls, 1);
    }

    #[test]
    fn test_cache_hit_ratio() {
        let report = TokenReport {
            total_input: 1000,
            cache_read: 400,
            ..Default::default()
        };
        assert!((report.cache_hit_ratio() - 0.4).abs() < 0.001);
    }

    #[test]
    fn test_merge() {
        let mut a = TokenReport {
            total_input: 10,
            total_output: 5,
            ..Default::default()
        };
        let b = TokenReport {
            total_input: 20,
            total_output: 10,
            ..Default::default()
        };
        a.merge(&b);
        assert_eq!(a.total_input, 30);
        assert_eq!(a.total_output, 15);
    }
}
