use async_trait::async_trait;
use fox_agent_core::{
    EventStream, Message, Provider, ProviderError, StreamEvent, TokenUsage, ToolDefinition,
};
use futures::{stream, StreamExt};
use reqwest::header::HeaderMap;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

use fox_agent_core::ProviderConfig;
use crate::util::build_headers;

// ── Provider ──

#[derive(Clone)]
pub struct AnthropicCompatibleProvider {
    client: Client,
    cfg: ProviderConfig,
}

impl AnthropicCompatibleProvider {
    pub fn new(cfg: ProviderConfig) -> Result<Self, ProviderError> {
        let client = Client::builder()
            .timeout(Duration::from_secs(cfg.timeout_secs))
            .build()
            .map_err(|err| ProviderError::Message {
                message: format!("failed to build provider client: {err}"),
            })?;
        Ok(Self { client, cfg })
    }

    fn endpoint(&self) -> String {
        format!("{}/messages", self.cfg.base_url.trim_end_matches('/'))
    }

    fn build_headers(&self) -> Result<HeaderMap, ProviderError> {
        build_headers(&self.cfg)
    }

    fn build_payload(
        &self,
        model_id: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
        system_static: &str,
        system_dynamic: &str,
    ) -> AnthropicMessagesRequest {
        let system = [system_static.trim(), system_dynamic.trim()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");

        let messages = messages
            .iter()
            .filter_map(AnthropicMessage::from_sdk_message)
            .collect::<Vec<_>>();

        let tools = if tools.is_empty() {
            None
        } else {
            Some(
                tools
                    .iter()
                    .map(|tool| AnthropicTool {
                        name: tool.name.clone(),
                        description: tool.description.clone(),
                        input_schema: tool.parameters_schema.clone(),
                    })
                    .collect(),
            )
        };

        AnthropicMessagesRequest {
            model: model_id.to_string(),
            system: if system.is_empty() { None } else { Some(system) },
            messages,
            tools,
            max_tokens: 4096,
            stream: self.cfg.use_streaming_api,
        }
    }

    fn response_to_events(
        &self,
        response: AnthropicMessagesResponse,
    ) -> Result<Vec<StreamEvent>, ProviderError> {
        let mut events = Vec::new();
        for block in response.content {
            match block {
                AnthropicResponseContentBlock::Text { text } => {
                    if !text.is_empty() {
                        events.push(StreamEvent::TextDelta { text });
                    }
                }
                AnthropicResponseContentBlock::ToolUse { id, name, input } => {
                    events.push(StreamEvent::ToolUse { id, name, input });
                }
            }
        }
        if let Some(usage) = response.usage.map(TokenUsage::from) {
            events.push(StreamEvent::Usage { usage });
        }
        events.push(StreamEvent::MessageStop { stop_reason: None });
        Ok(events)
    }
}

#[async_trait]
impl Provider for AnthropicCompatibleProvider {
    async fn complete(
        &self,
        model_id: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
        system_static: &str,
        system_dynamic: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream, ProviderError> {
        if self.cfg.use_streaming_api {
            return Err(ProviderError::Message {
                message: "Anthropic streaming is not implemented yet".to_string(),
            });
        }

        let payload = self.build_payload(model_id, messages, tools, system_static, system_dynamic);
        let response = self
            .client
            .post(self.endpoint())
            .headers(self.build_headers()?)
            .json(&payload)
            .send()
            .await
            .map_err(|err| ProviderError::Message {
                message: format!("provider request failed: {err}"),
            })?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::Message {
                message: format!("provider returned {status}: {body}"),
            });
        }

        let response = response
            .json::<AnthropicMessagesResponse>()
            .await
            .map_err(|err| ProviderError::Message {
                message: format!("invalid provider response: {err}"),
            })?;

        Ok(stream::iter(self.response_to_events(response)?.into_iter().map(Ok)).boxed())
    }

    fn name(&self) -> &str {
        &self.cfg.provider_name
    }
}

// ── Request / Response types (Anthropic-specific) ──

/// Anthropic Messages API request.
#[derive(Debug, Clone, Serialize)]
struct AnthropicMessagesRequest {
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<AnthropicTool>>,
    max_tokens: u32,
    stream: bool,
}

/// A single message in an Anthropic API request.
#[derive(Debug, Clone, Serialize)]
struct AnthropicMessage {
    role: String,
    content: Vec<AnthropicRequestContentBlock>,
}

impl AnthropicMessage {
    fn from_sdk_message(message: &Message) -> Option<Self> {
        match message.role {
            fox_agent_core::Role::System => None,
            fox_agent_core::Role::User => Some(Self {
                role: "user".to_string(),
                content: vec![AnthropicRequestContentBlock::Text {
                    text: crate::util::extract_text_content(message),
                }],
            }),
            fox_agent_core::Role::Assistant => Some(Self {
                role: "assistant".to_string(),
                content: vec![AnthropicRequestContentBlock::Text {
                    text: crate::util::extract_text_content(message),
                }],
            }),
            fox_agent_core::Role::Tool => {
                let (tool_use_id, content) = crate::util::extract_tool_result(message);
                Some(Self {
                    role: "user".to_string(),
                    content: vec![AnthropicRequestContentBlock::ToolResult {
                        tool_use_id,
                        content,
                    }],
                })
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
enum AnthropicRequestContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_result")]
    ToolResult { tool_use_id: String, content: String },
}

/// Anthropic tool definition.
#[derive(Debug, Clone, Serialize)]
struct AnthropicTool {
    name: String,
    description: String,
    input_schema: Value,
}

/// Anthropic Messages API response (non-streaming).
#[derive(Debug, Clone, Deserialize)]
struct AnthropicMessagesResponse {
    content: Vec<AnthropicResponseContentBlock>,
    #[serde(default)]
    usage: Option<AnthropicUsage>,
}

#[derive(Debug, Clone, Deserialize)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
}

impl From<AnthropicUsage> for TokenUsage {
    fn from(value: AnthropicUsage) -> Self {
        Self {
            input_tokens: value.input_tokens,
            output_tokens: value.output_tokens,
            total_tokens: value.input_tokens + value.output_tokens,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
enum AnthropicResponseContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse { id: String, name: String, input: Value },
}
