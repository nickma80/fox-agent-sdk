use async_trait::async_trait;
use futures::stream;
use futures::StreamExt;
use fox_agent_core::{EventStream, Message, Provider, ProviderError, StreamEvent, ToolDefinition};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct MockProvider {
    name: String,
    scripts: Arc<Mutex<VecDeque<Vec<StreamEvent>>>>,
}

impl MockProvider {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            scripts: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    pub fn push_script(&self, events: Vec<StreamEvent>) {
        let Ok(mut guard) = self.scripts.lock() else { return };
        guard.push_back(events);
    }
}

#[async_trait]
impl Provider for MockProvider {
    async fn complete(
        &self,
        _model_id: &str,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system_static: &str,
        _system_dynamic: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream, ProviderError> {
        let events = {
            let mut guard = self.scripts.lock().map_err(|_| ProviderError::Message {
                message: "mock provider script lock poisoned".to_string(),
            })?;
            guard.pop_front().ok_or_else(|| ProviderError::Message {
                message: "mock provider has no more scripted responses".to_string(),
            })?
        };

        Ok(stream::iter(events.into_iter().map(Ok)).boxed())
    }

    fn name(&self) -> &str {
        &self.name
    }
}
