//! Memory injection pipeline for the Agent harness.
//!
//! Provides:
//! - `MemoryInjection` / `MemoryInjectionState` — lifecycle for injecting
//!   relevant memories into the system prompt before each turn.
//! - `trigger_recall_for_next_turn()` — background async recall using the
//!   `fox_agent_core::MemoryManager` (with MemoryGraph persistence).

use async_trait::async_trait;
use fox_agent_core::{
    AgentEvent, AgentEventTx, ContentBlock, ExtractedMemory, MemoryConfig, MemoryExtractor,
    MemoryManager as CoreMemoryManager, MemoryRelevanceChecker, MemoryScope, Message, Model,
    RecallMode, Role, format_recall_hits_display_prompt, format_recall_hits_prompt,
    select_recall_hits_for_injection,
};
use futures::StreamExt;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::Instrument;

use std::time::Instant;
use tokio::sync::RwLock;

/// A computed memory injection ready to be inserted into the system prompt.
#[derive(Debug, Clone)]
pub struct MemoryInjection {
    pub prompt: String,
    pub display_prompt: Option<String>,
    pub count: u32,
    pub memory_ids: Vec<String>,
}

/// Events that drive the memory injection state machine.
#[derive(Debug, Clone)]
pub enum MemoryInjectionEvent {
    InjectionComputed { injection: MemoryInjection },
    InjectionConsumed,
    Enabled,
    Disabled,
}

/// Tracks the lifecycle of memory injection across agent turns.
#[derive(Debug, Clone, Default)]
pub struct MemoryInjectionState {
    pub enabled: bool,
    pub pending_injection: Option<MemoryInjection>,
    pub last_injected_at: Option<Instant>,
    pub injection_count: u64,
    pub total_injected_chars: u64,
}

impl MemoryInjectionState {
    pub fn with_enabled(enabled: bool) -> Self {
        Self {
            enabled,
            ..Default::default()
        }
    }

    pub fn apply(
        &mut self,
        event: MemoryInjectionEvent,
    ) -> Option<fox_agent_core::MemoryStateEvent> {
        match event {
            MemoryInjectionEvent::InjectionComputed { injection } => {
                let snapshot = fox_agent_core::MemoryStateEvent::InjectionComputed {
                    count: injection.count,
                    memory_ids: injection.memory_ids.clone(),
                    prompt_chars: injection.prompt.len(),
                };
                self.pending_injection = Some(injection);
                self.injection_count += 1;
                Some(snapshot)
            }
            MemoryInjectionEvent::InjectionConsumed => {
                let injection = self.pending_injection.take()?;
                self.total_injected_chars += injection.prompt.len() as u64;
                self.last_injected_at = Some(Instant::now());
                Some(fox_agent_core::MemoryStateEvent::InjectionConsumed {
                    count: injection.count,
                    memory_ids: injection.memory_ids,
                    prompt_chars: injection.prompt.len(),
                })
            }
            MemoryInjectionEvent::Enabled => {
                self.enabled = true;
                Some(fox_agent_core::MemoryStateEvent::Enabled)
            }
            MemoryInjectionEvent::Disabled => {
                self.enabled = false;
                self.pending_injection = None;
                Some(fox_agent_core::MemoryStateEvent::Disabled)
            }
        }
    }

    pub fn take_pending(&mut self) -> Option<(MemoryInjection, fox_agent_core::MemoryStateEvent)> {
        let injection = self.pending_injection.clone()?;
        let event = self.apply(MemoryInjectionEvent::InjectionConsumed)?;
        Some((injection, event))
    }
}

/// Wraps `fox_agent_core::MemoryManager` with the SDK's injection pipeline.
#[derive(Clone)]
pub struct MemoryManager {
    core: CoreMemoryManager,
    cfg: MemoryConfig,
}

impl MemoryManager {
    pub fn new(cfg: MemoryConfig) -> Self {
        Self {
            core: CoreMemoryManager::new(&cfg),
            cfg,
        }
    }

    /// Access the underlying core MemoryManager.
    pub fn core(&self) -> &CoreMemoryManager {
        &self.core
    }

    /// Set the storage directory for the core MemoryManager.
    pub fn with_storage_dir(mut self, dir: PathBuf) -> Self {
        self.core = self.core.with_storage_dir(dir);
        self
    }

    /// Set the project directory for the core MemoryManager.
    pub fn with_project_dir(mut self, dir: PathBuf) -> Self {
        self.core = self.core.with_project_dir(dir);
        self
    }

    /// Set the session ID for Session-scoped memory isolation.
    pub fn with_session_id(mut self, id: impl Into<String>) -> Self {
        self.core = self.core.with_session_id(id);
        self
    }

    /// Store a memory entry via the core MemoryManager (project scope).
    pub async fn add_memory(&self, content: impl Into<String>) -> String {
        let entry = fox_agent_core::MemoryEntry::new(fox_agent_core::MemoryCategory::Fact, content);
        self.core
            .remember_project(entry)
            .unwrap_or_else(|_| "".to_string())
    }

    /// Background recall: searches the core MemoryManager for memories
    /// relevant to the most recent user message, then stores the result
    /// in `memory_state` as a pending injection.
    pub fn trigger_recall_for_next_turn(
        &self,
        messages: Vec<Message>,
        memory_state: Arc<RwLock<MemoryInjectionState>>,
    ) {
        if !self.cfg.enabled {
            return;
        }
        let core = self.core.clone();
        let cfg = self.cfg.clone();
        tokio::spawn(
            async move {
                // Extract query from the most recent user message
                let query = messages
                    .iter()
                    .rev()
                    .find_map(|m| {
                        if m.role != Role::User {
                            return None;
                        }
                        m.content.iter().find_map(|b| match b {
                            ContentBlock::Text { text } => Some(text.clone()),
                            _ => None,
                        })
                    })
                    .unwrap_or_default();

                if query.is_empty() {
                    return;
                }

                let results = match core.recall_detailed(
                    Some(&query),
                    cfg.max_results,
                    if core.semantic_enabled() {
                        RecallMode::Cascade
                    } else {
                        RecallMode::Keyword
                    },
                    MemoryScope::All,
                ) {
                    Ok(r) => r,
                    Err(_) => return,
                };

                if results.is_empty() {
                    return;
                }

                let selected = select_recall_hits_for_injection(
                    &results,
                    cfg.injection_max_chars,
                    cfg.injection_max_per_category,
                );
                if selected.is_empty() {
                    return;
                }
                let Some(prompt) = format_recall_hits_prompt(
                    &selected,
                    cfg.injection_max_chars,
                    cfg.injection_max_per_category,
                ) else {
                    return;
                };
                let display_prompt = format_recall_hits_display_prompt(
                    &selected,
                    cfg.injection_max_chars,
                    cfg.injection_max_per_category,
                );
                let selected_ids = selected
                    .iter()
                    .map(|hit| hit.entry.id.clone())
                    .collect::<Vec<_>>();
                let injection = MemoryInjection {
                    prompt: format!("{prompt}\n"),
                    display_prompt,
                    count: selected.len() as u32,
                    memory_ids: selected_ids,
                };

                let mut state = memory_state.write().await;
                let _ = state.apply(MemoryInjectionEvent::InjectionComputed { injection });
            }
            .in_current_span(),
        );
    }

    pub fn trigger_ingestion_for_turn(
        &self,
        messages: Vec<Message>,
        model: Arc<dyn Model>,
        event_tx: AgentEventTx,
    ) {
        if !self.cfg.enabled || !self.cfg.auto_extract {
            return;
        }
        let transcript =
            build_ingestion_transcript(&messages, self.cfg.auto_extract_message_window);
        if transcript.trim().is_empty() {
            return;
        }
        let core = self.core.clone();
        let cfg = self.cfg.clone();
        tokio::spawn(
            async move {
                let worker = model_for_memory_tasks(model, cfg.verify_model.clone());
                let extractor = ModelBackedExtractor {
                    model: worker.clone(),
                };
                let checker = ModelBackedRelevanceChecker { model: worker };
                let checker_ref: Option<&dyn MemoryRelevanceChecker> = Some(&checker);
                let report = match core
                    .ingest_transcript(&transcript, &extractor, checker_ref)
                    .await
                {
                    Ok(report) => report,
                    Err(_) => return,
                };
                if report.created_ids.is_empty()
                    && report.reinforced_ids.is_empty()
                    && report.skipped_duplicates == 0
                    && report.skipped_irrelevant == 0
                {
                    return;
                }
                let _ = event_tx
                    .send(AgentEvent::MemoryStateChanged {
                        event: fox_agent_core::MemoryStateEvent::IngestionCompleted {
                            created_ids: report.created_ids,
                            reinforced_ids: report.reinforced_ids,
                            contradiction_ids: report.contradiction_ids,
                            skipped: report.skipped_duplicates + report.skipped_irrelevant,
                        },
                    })
                    .await;
            }
            .in_current_span(),
        );
    }
}

fn build_ingestion_transcript(messages: &[Message], window: usize) -> String {
    let start = messages.len().saturating_sub(window.max(1));
    messages[start..]
        .iter()
        .filter_map(|message| {
            let role = match message.role {
                Role::User => "User",
                Role::Assistant => "Assistant",
                Role::Tool => "Tool",
                Role::System => return None,
            };
            let text = message
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    ContentBlock::Reasoning { .. } => None,
                    ContentBlock::ToolUse { name, .. } => Some(name.as_str()),
                    ContentBlock::ToolResult { text, .. } => Some(text.as_str()),
                    ContentBlock::Image { .. } => None,
                    ContentBlock::NarrativeSummary { .. } => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            if text.trim().is_empty() {
                None
            } else {
                Some(format!("{role}: {}", text.trim()))
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn model_for_memory_tasks(model: Arc<dyn Model>, override_model: Option<String>) -> Arc<dyn Model> {
    let fork = model.fork();
    if let Some(model_id) = override_model {
        let _ = fork.set_model(&model_id);
    }
    fork
}

struct ModelBackedExtractor {
    model: Arc<dyn Model>,
}

struct ModelBackedRelevanceChecker {
    model: Arc<dyn Model>,
}

#[async_trait]
impl MemoryExtractor for ModelBackedExtractor {
    async fn extract(
        &self,
        transcript: &str,
        existing: &[String],
    ) -> Result<Vec<ExtractedMemory>, String> {
        let mut system = String::from(
            r#"You are a memory extraction assistant. Extract important NEW learnings from the conversation that should be remembered for future sessions.

Categories (use EXACTLY one of these):
- fact
- preference
- correction
- entity

For each memory, output in this exact format (one per line):
CATEGORY|CONTENT|TRUST

Where TRUST is high/medium/low. Output ONLY lines in that format."#,
        );
        if !existing.is_empty() {
            system.push_str("\n\nAlready known memories:\n");
            for mem in existing.iter().take(60) {
                system.push_str("- ");
                system.push_str(mem);
                system.push('\n');
            }
        }
        let response = run_memory_prompt(self.model.clone(), &system, transcript).await?;
        Ok(response
            .lines()
            .filter(|line| line.contains('|'))
            .filter_map(|line| {
                let parts: Vec<&str> = line.split('|').collect();
                if parts.len() < 3 {
                    return None;
                }
                Some(ExtractedMemory {
                    category: parts[0].trim().to_lowercase(),
                    content: parts[1].trim().to_string(),
                    trust: parts[2].trim().to_lowercase(),
                })
            })
            .collect())
    }
}

#[async_trait]
impl MemoryRelevanceChecker for ModelBackedRelevanceChecker {
    async fn check_relevance(&self, memory: &str, context: &str) -> Result<(bool, String), String> {
        let system = "You decide whether an extracted memory is relevant and grounded in the given transcript. Reply with exactly:\nRELEVANT: yes/no\nREASON: <brief reason>";
        let prompt = format!("## Candidate Memory\n{memory}\n\n## Transcript\n{context}");
        let response = run_memory_prompt(self.model.clone(), system, &prompt).await?;
        let relevant = response
            .lines()
            .find(|line| line.to_ascii_lowercase().starts_with("relevant:"))
            .map(|line| line.to_ascii_lowercase().contains("yes"))
            .unwrap_or(false);
        let reason = response
            .lines()
            .find(|line| line.to_ascii_lowercase().starts_with("reason:"))
            .map(|line| line["reason:".len()..].trim().to_string())
            .unwrap_or_else(|| response.trim().to_string());
        Ok((relevant, reason))
    }

    async fn check_contradiction(&self, new: &str, existing: &str) -> Result<bool, String> {
        let system = "You are a contradiction detector. Given existing information and new information, reply with exactly YES or NO depending on whether the new information directly contradicts the existing information.";
        let prompt = format!("## Existing\n{existing}\n\n## New\n{new}");
        let response = run_memory_prompt(self.model.clone(), system, &prompt).await?;
        Ok(response.trim().to_ascii_uppercase().starts_with("YES"))
    }
}

async fn run_memory_prompt(
    model: Arc<dyn Model>,
    system: &str,
    user_message: &str,
) -> Result<String, String> {
    let messages = vec![Message::user(user_message)];
    let mut stream = model
        .complete(&messages, &[], system, "", None)
        .await
        .map_err(|e| e.to_string())?;
    let mut output = String::new();
    while let Some(event) = stream.next().await {
        if let fox_agent_core::StreamEvent::TextDelta { text } = event.map_err(|e| e.to_string())? {
            output.push_str(&text);
        }
    }
    Ok(output)
}
