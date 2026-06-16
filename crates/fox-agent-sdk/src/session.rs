//! Session state management with Reducer pattern.
//!
//! All state changes flow through `SessionEvent` → `SessionState::apply()` → `SessionChange`,
//! ensuring traceable, testable state transitions.

use fox_agent_core::Message;
use std::path::PathBuf;

/// Immutable session identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionId(String);

impl SessionId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Session status lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    Active,
    Paused,
    Closed,
    Crashed,
}

/// A snapshot of environment variables captured at a point in time.
#[derive(Debug, Clone)]
pub struct EnvSnapshot {
    pub key: String,
    pub value: String,
}

/// Events that drive the session state machine.
#[derive(Debug, Clone)]
pub enum SessionEvent {
    /// Set working directory (project root)
    SetWorkingDir(Option<PathBuf>),
    /// Set model identifier
    SetModel(String),
    /// Set provider key
    SetProviderKey(String),
    /// Update session title
    SetTitle(Option<String>),
    /// Mark session as closed
    MarkClosed,
    /// Mark session as crashed
    MarkCrashed,
    /// Add an environment snapshot
    AddEnvSnapshot(EnvSnapshot),
}

/// A record of a state change (for external observers / telemetry).
#[derive(Debug, Clone)]
pub struct SessionChange {
    pub event: SessionEvent,
    pub session_id: String,
}

/// Session state — all mutations go through `apply(SessionEvent)`.
#[derive(Debug, Clone)]
pub struct SessionState {
    pub id: String,
    pub parent_id: Option<String>,
    pub title: Option<String>,
    pub model: Option<String>,
    pub provider_key: Option<String>,
    pub status: SessionStatus,
    pub working_dir: Option<PathBuf>,
    pub messages: Vec<Message>,
    pub env_snapshots: Vec<EnvSnapshot>,
}

impl SessionState {
    /// Create a new session.
    pub fn new(working_dir: Option<PathBuf>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            parent_id: None,
            title: None,
            model: None,
            provider_key: None,
            status: SessionStatus::Active,
            working_dir,
            messages: Vec::new(),
            env_snapshots: Vec::new(),
        }
    }

    /// Create a child session (for subagents/swarm).
    pub fn new_child(parent_id: &str, working_dir: Option<PathBuf>) -> Self {
        Self {
            parent_id: Some(parent_id.to_string()),
            ..Self::new(working_dir)
        }
    }

    /// Apply a session event and return the resulting change record.
    pub fn apply(&mut self, event: SessionEvent) -> SessionChange {
        let change = SessionChange {
            event: event.clone(),
            session_id: self.id.clone(),
        };
        match event {
            SessionEvent::SetWorkingDir(dir) => {
                self.working_dir = dir;
            }
            SessionEvent::SetModel(model) => {
                self.model = Some(model);
            }
            SessionEvent::SetProviderKey(key) => {
                self.provider_key = Some(key);
            }
            SessionEvent::SetTitle(title) => {
                self.title = title;
            }
            SessionEvent::MarkClosed => {
                self.status = SessionStatus::Closed;
            }
            SessionEvent::MarkCrashed => {
                self.status = SessionStatus::Crashed;
            }
            SessionEvent::AddEnvSnapshot(snapshot) => {
                // Replace existing entry for the same key
                if let Some(existing) = self.env_snapshots.iter_mut().find(|e| e.key == snapshot.key) {
                    existing.value = snapshot.value;
                } else {
                    self.env_snapshots.push(snapshot);
                }
            }
        }
        change
    }

    /// Total number of messages in this session.
    pub fn message_count(&self) -> usize {
        self.messages.len()
    }
}
