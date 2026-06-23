//! ReplayRunner: deterministic replay of AgentEvent streams for testing.
//!
//! Captures agent event transcripts and replays them with assertions
//! (golden-file testing), useful for regression tests and CI.

use fox_agent_core::{AgentEvent, EnvelopePayload, EventEnvelope};
use fox_agent_swarm::GoldenTranscript;
use std::path::PathBuf;

/// A read-back runner that replays event envelopes and runs verification checks.
pub struct ReplayRunner {
    transcript: GoldenTranscript,
    events: Vec<EventEnvelope>,
}

impl ReplayRunner {
    /// Load a transcript from a JSONL file.
    pub fn from_file(path: &PathBuf) -> std::io::Result<Self> {
        let envelopes = crate::event_recorder::EventRecorder::load_from_file(path)?;
        let events_json: Vec<String> = envelopes
            .iter()
            .map(|e| serde_json::to_string(e).unwrap_or_default())
            .collect();

        Ok(Self {
            transcript: GoldenTranscript {
                session_id: envelopes
                    .first()
                    .map(|e| e.session_id.clone())
                    .unwrap_or_default(),
                events: events_json,
                verification_checks: vec![],
            },
            events: envelopes,
        })
    }

    /// Create a runner from an in-memory transcript.
    pub fn from_transcript(transcript: GoldenTranscript) -> Self {
        let events: Vec<EventEnvelope> = transcript
            .events
            .iter()
            .filter_map(|s| serde_json::from_str(s).ok())
            .collect();
        Self { transcript, events }
    }

    /// Run all verification checks and return failures.
    pub fn verify(&self) -> Vec<String> {
        let mut failures = Vec::new();

        for check in &self.transcript.verification_checks {
            if let Some(ref text) = check.must_contain_text {
                if !self.events.iter().any(|e| envelope_contains_text(e, text)) {
                    failures.push(format!(
                        "Check '{}': no event contains text '{}'",
                        check.description, text
                    ));
                }
            }

            if let Some(ref tool_name) = check.must_have_tool_call {
                if !self
                    .events
                    .iter()
                    .any(|e| has_tool_call(e, tool_name))
                {
                    failures.push(format!(
                        "Check '{}': no tool call '{}' found",
                        check.description, tool_name
                    ));
                }
            }

            if check.must_have_usage {
                if !self.events.iter().any(has_usage_event) {
                    failures.push(format!(
                        "Check '{}': no usage event found",
                        check.description
                    ));
                }
            }
        }

        failures
    }

    /// Run all checks, panicking on failure (for test use).
    pub fn verify_or_panic(&self) {
        let failures = self.verify();
        for f in &failures {
            eprintln!("[REPLAY FAIL] {f}");
        }
        assert!(failures.is_empty(), "replay verification failed:\n{}", failures.join("\n"));
    }

    /// Return all events in order.
    pub fn events(&self) -> &[EventEnvelope] {
        &self.events
    }

    /// Return events filtered by source (e.g. "tool", "provider", "agent").
    pub fn events_by_source(&self, source: &str) -> Vec<&EventEnvelope> {
        self.events.iter().filter(|e| e.source == source).collect()
    }

    /// Count total tokens across all usage events.
    pub fn total_tokens(&self) -> u64 {
        self.events
            .iter()
            .filter_map(|e| match &e.event {
                EnvelopePayload::ModelUsage { usage } => Some(usage.total_tokens as u64),
                _ => None,
            })
            .sum()
    }
}

// ── Helpers ──

fn envelope_contains_text(env: &EventEnvelope, text: &str) -> bool {
    match &env.event {
        EnvelopePayload::ModelTextDelta { text: t }
        | EnvelopePayload::ModelThinkingDelta { text: t } => t.contains(text),
        EnvelopePayload::Error { message, .. } => message.contains(text),
        _ => false,
    }
}

fn has_tool_call(env: &EventEnvelope, tool_name: &str) -> bool {
    matches!(&env.event, EnvelopePayload::ToolCallStart { name, .. } if name == tool_name)
}

fn has_usage_event(env: &EventEnvelope) -> bool {
    matches!(env.event, EnvelopePayload::ModelUsage { .. })
}
