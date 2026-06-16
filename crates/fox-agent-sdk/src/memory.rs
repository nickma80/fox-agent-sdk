//! Memory injection pipeline for the Agent harness.
//!
//! Provides:
//! - `MemoryInjection` / `MemoryInjectionState` — lifecycle for injecting
//!   relevant memories into the system prompt before each turn.
//! - `trigger_recall_for_next_turn()` — background async recall using the
//!   `fox_agent_core::MemoryManager` (with MemoryGraph persistence).

use fox_agent_core::{
    ContentBlock, MemoryManager as CoreMemoryManager, MemoryScope, Message, RecallMode, Role,
    MemoryConfig,
};

use std::sync::Arc;
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

    pub fn apply(&mut self, event: MemoryInjectionEvent) -> Option<fox_agent_core::MemoryStateEvent> {
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
                let Some(injection) = self.pending_injection.take() else {
                    return None;
                };
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

    /// Store a memory entry via the core MemoryManager (project scope).
    pub async fn add_memory(&self, content: impl Into<String>) -> String {
        let entry = fox_agent_core::MemoryEntry::new(
            fox_agent_core::MemoryCategory::Fact,
            content,
        );
        self.core.remember_project(entry).unwrap_or_else(|_| "".to_string())
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
        tokio::spawn(async move {
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

            let results = match core.recall(
                Some(&query),
                cfg.max_results,
                RecallMode::Keyword,
                MemoryScope::All,
            ) {
                Ok(r) => r,
                Err(_) => return,
            };

            if results.is_empty() {
                return;
            }

            let prompt = results
                .iter()
                .map(|(e, _)| format!("- {}", e.content))
                .collect::<Vec<_>>()
                .join("\n");
            let injection = MemoryInjection {
                prompt: format!("Relevant memories:\n{prompt}\n"),
                display_prompt: None,
                count: results.len() as u32,
                memory_ids: results.iter().map(|(e, _)| e.id.clone()).collect(),
            };

            let mut state = memory_state.write().await;
            let _ = state.apply(MemoryInjectionEvent::InjectionComputed { injection });
        });
    }
}
