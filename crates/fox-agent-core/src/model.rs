use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::message::Message;
use crate::provider::{EventStream, Provider, ProviderError};
use crate::tool::ToolDefinition;

/// A named model route (provider name + model id).
#[derive(Debug, Clone)]
pub struct ModelRoute {
    /// Provider name (e.g. "openai")
    pub provider_name: String,
    /// Model identifier (e.g. "gpt-4o")
    pub model_id: String,
}

/// Mutable runtime state of a model instance (e.g. provider session id).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelRuntimeState {
    /// Opaque session id used to resume a prior provider conversation
    pub resume_session_id: Option<String>,
}

/// Events that modify ModelRuntimeState.
#[derive(Debug, Clone)]
pub enum ModelStateEvent {
    /// Set or clear the provider resume session id
    SetResumeSessionId(Option<String>),
}

/// Trait that wraps a Provider with model selection and routing logic.
#[async_trait]
pub trait Model: Send + Sync {
    /// Send messages and get a streaming response via the underlying provider.
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        system_static: &str,
        system_dynamic: &str,
        resume_session_id: Option<&str>,
    ) -> Result<EventStream, ProviderError>;

    /// Provider name.
    fn provider_name(&self) -> &str;
    /// Currently selected model id.
    fn model_id(&self) -> String;
    /// Switch to a different model.
    fn set_model(&self, model: &str) -> Result<(), ProviderError>;
    /// Available models as human-readable strings.
    fn available_models_display(&self) -> Vec<String>;
    /// Available model routes.
    fn model_routes(&self) -> Vec<ModelRoute>;
    /// Fork a new model handle sharing the same provider.
    fn fork(&self) -> Arc<dyn Model>;
    /// Snapshot the current runtime state.
    fn runtime_state(&self) -> ModelRuntimeState;
    /// Apply a state event.
    fn apply_state_event(&self, event: ModelStateEvent);

    /// Access the underlying provider, when directly exposed.
    ///
    /// Returns `None` for composite/wrapped models that route through another
    /// abstraction — the memory wiki assistant (`ProviderBackedWikiAssistant`)
    /// is only assembled from a directly exposed provider.
    fn provider(&self) -> Option<Arc<dyn Provider>> {
        None
    }
}

/// Default implementation of Model that wraps a single Provider.
#[derive(Clone)]
pub struct DefaultModel {
    /// The underlying LLM provider
    provider: Arc<dyn Provider>,
    /// Currently selected model id (thread-safe)
    model_id: Arc<std::sync::RwLock<String>>,
    /// Mutable runtime state (thread-safe)
    runtime_state: Arc<std::sync::RwLock<ModelRuntimeState>>,
}

impl DefaultModel {
    pub fn new(provider: Arc<dyn Provider>, model_id: impl Into<String>) -> Self {
        Self {
            model_id: Arc::new(std::sync::RwLock::new(model_id.into())),
            runtime_state: Arc::new(std::sync::RwLock::new(ModelRuntimeState::default())),
            provider,
        }
    }
}

#[async_trait]
impl Model for DefaultModel {
    fn provider(&self) -> Option<Arc<dyn Provider>> {
        Some(self.provider.clone())
    }

    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        system_static: &str,
        system_dynamic: &str,
        resume_session_id: Option<&str>,
    ) -> Result<EventStream, ProviderError> {
        let model_id = self.model_id();
        self.provider
            .complete(
                &model_id,
                messages,
                tools,
                system_static,
                system_dynamic,
                resume_session_id,
            )
            .await
    }

    fn provider_name(&self) -> &str {
        self.provider.name()
    }

    fn model_id(&self) -> String {
        self.model_id
            .read()
            .map(|s| s.clone())
            .unwrap_or_else(|_| "unknown".to_string())
    }

    fn set_model(&self, model: &str) -> Result<(), ProviderError> {
        let mut guard = self.model_id.write().map_err(|_| ProviderError::Message {
            message: "model id lock poisoned".to_string(),
        })?;
        *guard = model.to_string();
        Ok(())
    }

    fn available_models_display(&self) -> Vec<String> {
        vec![self.model_id()]
    }

    fn model_routes(&self) -> Vec<ModelRoute> {
        vec![ModelRoute {
            provider_name: self.provider.name().to_string(),
            model_id: self.model_id(),
        }]
    }

    fn fork(&self) -> Arc<dyn Model> {
        Arc::new(Self {
            provider: self.provider.clone(),
            model_id: self.model_id.clone(),
            runtime_state: self.runtime_state.clone(),
        })
    }

    fn runtime_state(&self) -> ModelRuntimeState {
        self.runtime_state
            .read()
            .map(|s| s.clone())
            .unwrap_or_default()
    }

    fn apply_state_event(&self, event: ModelStateEvent) {
        let runtime_state = self.runtime_state.clone();
        let _ = std::thread::spawn(move || {
            let Ok(mut state) = runtime_state.write() else {
                return;
            };
            match event {
                ModelStateEvent::SetResumeSessionId(id) => state.resume_session_id = id,
            }
        });
    }
}
