//! Traits and implementations for memory relevance verification and extraction.
//!
//! Replaces babycode's Sidecar (Haiku model) with the main agent's
//! [`Provider`] trait, so memory operations use the same provider/model
//! as the primary agent.

use async_trait::async_trait;

/// A memory extracted from a conversation transcript.
#[derive(Debug, Clone)]
pub struct ExtractedMemory {
    pub category: String,
    pub content: String,
    pub trust: String,
}

/// Trait for verifying memory relevance against a context.
#[async_trait]
pub trait MemoryRelevanceChecker: Send + Sync {
    /// Check if a stored memory is relevant to the current context.
    /// Returns `(is_relevant, explanation)`.
    async fn check_relevance(&self, memory: &str, context: &str) -> Result<(bool, String), String>;

    /// Check if new information contradicts existing information.
    async fn check_contradiction(&self, new: &str, existing: &str) -> Result<bool, String>;
}

/// Trait for extracting memories from conversation transcripts.
#[async_trait]
pub trait MemoryExtractor: Send + Sync {
    /// Extract new memories from a transcript, avoiding duplicates vs `existing`.
    async fn extract(&self, transcript: &str, existing: &[String]) -> Result<Vec<ExtractedMemory>, String>;
}

// ── Default provider-based implementation ──

use crate::message::Message;
use crate::provider::Provider;
use futures::StreamExt;
use std::sync::Arc;

/// Default implementation using a Provider + model_id for LLM calls.
pub struct ProviderRelevanceChecker {
    provider: Arc<dyn Provider>,
    model_id: String,
}

impl ProviderRelevanceChecker {
    pub fn new(provider: Arc<dyn Provider>, model_id: impl Into<String>) -> Self {
        Self { provider, model_id: model_id.into() }
    }
}

#[async_trait]
impl MemoryRelevanceChecker for ProviderRelevanceChecker {
    async fn check_relevance(&self, memory_content: &str, context: &str) -> Result<(bool, String), String> {
        let system = r#"You are a memory relevance checker. Your job is to determine if a stored memory is relevant to the current context.

Respond in this exact format:
RELEVANT: yes/no
REASON: <brief explanation>

Be conservative — only say "yes" if the memory would actually be useful for the current task."#;

        let prompt = format!(
            "## Stored Memory\n{memory_content}\n\n## Current Context\n{context}\n\nIs this memory relevant to the current context?"
        );

        let response = call_provider(&*self.provider, &self.model_id, system, &prompt).await?;

        let mut is_relevant = false;
        for line in response.lines() {
            let l = line.trim();
            if l.len() >= 9 && l[..9].eq_ignore_ascii_case("relevant:") {
                let v = l[9..].trim();
                is_relevant = v.eq_ignore_ascii_case("yes") || v.starts_with("yes");
                break;
            }
        }
        let reason = response.lines()
            .find(|l| l.to_lowercase().starts_with("reason:"))
            .map(|l| l.trim_start_matches(|c: char| !c.is_alphabetic()).trim().to_string())
            .unwrap_or_else(|| response.trim().to_string());

        Ok((is_relevant, reason))
    }

    async fn check_contradiction(&self, new_content: &str, existing_content: &str) -> Result<bool, String> {
        let system = "You are a contradiction detector. Given two statements, determine if the new information directly contradicts the existing information. Reply with exactly YES or NO.";
        let prompt = format!(
            "## Existing Information\n{existing_content}\n\n## New Information\n{new_content}\n\nDoes the new information contradict the existing information?"
        );
        let response = call_provider(&*self.provider, &self.model_id, system, &prompt).await?;
        Ok(response.trim().to_uppercase().starts_with("YES"))
    }
}

/// Default implementation using a Provider + model_id for extraction.
pub struct ProviderExtractor {
    provider: Arc<dyn Provider>,
    model_id: String,
}

impl ProviderExtractor {
    pub fn new(provider: Arc<dyn Provider>, model_id: impl Into<String>) -> Self {
        Self { provider, model_id: model_id.into() }
    }
}

#[async_trait]
impl MemoryExtractor for ProviderExtractor {
    async fn extract(&self, transcript: &str, existing: &[String]) -> Result<Vec<ExtractedMemory>, String> {
        let mut system = String::from(
            r#"You are a memory extraction assistant. Extract important NEW learnings from the conversation that should be remembered for future sessions.

Categories (use EXACTLY one of these):
- fact: Technical facts about the codebase, architecture, patterns, dependencies, tools, environment
- preference: User preferences, workflow habits, UX expectations, coding style, conventions
- correction: Mistakes that were corrected, bugs found and fixed, wrong assumptions
- entity: Named entities worth tracking - people, projects, services, repos, teams

Categorization rules:
- If it describes what the USER WANTS or HOW THEY LIKE THINGS, it is "preference", not "fact"
- If it describes a BUG FIX or MISTAKE, it is "correction", not "fact"

Quality bar: Only extract information that would ACTUALLY BE USEFUL if recalled in a future session on a different topic.

For each memory, output in this exact format (one per line):
CATEGORY|CONTENT|TRUST

Where CATEGORY is fact/preference/correction/entity, CONTENT is a concise statement (1-2 sentences), and TRUST is high/medium/low.

Output ONLY the formatted lines, no other text. If no NEW memories worth extracting, output nothing."#,
        );

        if !existing.is_empty() {
            system.push_str("\n\nAlready known (do NOT re-extract these or close paraphrases):\n");
            for mem in existing.iter().take(80) {
                let truncated = if mem.len() > 150 { &mem[..150] } else { mem.as_str() };
                system.push_str("- ");
                system.push_str(truncated);
                system.push('\n');
            }
        }

        let response = call_provider(&*self.provider, &self.model_id, &system, transcript).await?;

        let memories = response.lines()
            .filter(|l| l.contains('|'))
            .filter_map(|line| {
                let parts: Vec<&str> = line.split('|').collect();
                if parts.len() >= 3 {
                    Some(ExtractedMemory {
                        category: parts[0].trim().to_lowercase(),
                        content: parts[1].trim().to_string(),
                        trust: parts[2].trim().to_lowercase(),
                    })
                } else {
                    None
                }
            })
            .collect();

        Ok(memories)
    }
}

async fn call_provider(provider: &dyn Provider, model_id: &str, system: &str, user_message: &str) -> Result<String, String> {
    let msg = Message::user(user_message);
    let mut stream = provider
        .complete(model_id, &[msg], &[], system, "", None)
        .await
        .map_err(|e| format!("provider call failed: {e}"))?;

    let mut out = String::new();
    while let Some(event) = stream.next().await {
        match event.map_err(|e| format!("stream error: {e}"))? {
            crate::provider::StreamEvent::TextDelta { text } => out.push_str(&text),
            _ => {}
        }
    }
    Ok(out)
}
