use async_stream::try_stream;
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
pub struct OpenAiCompatibleProvider {
    client: Client,
    cfg: ProviderConfig,
}

impl OpenAiCompatibleProvider {
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
        format!("{}/chat/completions", self.cfg.base_url.trim_end_matches('/'))
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
    ) -> ChatCompletionRequest {
        let mut payload_messages = Vec::new();
        let system_prompt = [system_static.trim(), system_dynamic.trim()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");

        if !system_prompt.is_empty() {
            payload_messages.push(ChatCompletionMessage {
                role: "system".to_string(),
                content: Some(system_prompt),
                tool_call_id: None,
            });
        }

        payload_messages.extend(messages.iter().map(ChatCompletionMessage::from));

        let tools = if tools.is_empty() {
            None
        } else {
            Some(
                tools
                    .iter()
                    .map(|tool| ChatCompletionTool {
                        tool_type: "function".to_string(),
                        function: ChatCompletionFunction {
                            name: tool.name.clone(),
                            description: tool.description.clone(),
                            parameters: tool.parameters_schema.clone(),
                        },
                    })
                    .collect(),
            )
        };

        ChatCompletionRequest {
            model: model_id.to_string(),
            messages: payload_messages,
            tools,
            stream: self.cfg.use_streaming_api,
        }
    }

    fn response_to_events(
        &self,
        response: ChatCompletionResponse,
    ) -> Result<Vec<StreamEvent>, ProviderError> {
        let mut events = Vec::new();
        let Some(choice) = response.choices.into_iter().next() else {
            return Err(ProviderError::Message {
                message: "provider returned no choices".to_string(),
            });
        };

        if let Some(tool_calls) = choice.message.tool_calls {
            for call in tool_calls {
                let input = serde_json::from_str(&call.function.arguments).map_err(|err| {
                    ProviderError::Message {
                        message: format!("invalid tool arguments JSON: {err}"),
                    }
                })?;
                events.push(StreamEvent::ToolUse {
                    id: call.id,
                    name: call.function.name,
                    input,
                });
            }
        }

        if let Some(content) = choice.message.content {
            if !content.is_empty() {
                events.push(StreamEvent::TextDelta { text: content });
            }
        }

        if let Some(usage) = response.usage.map(TokenUsage::from) {
            events.push(StreamEvent::Usage { usage });
        }
        events.push(StreamEvent::MessageStop { stop_reason: None });
        Ok(events)
    }

    fn stream_response_to_events(&self, response: reqwest::Response) -> EventStream {
        parse_openai_stream(response)
    }
}

#[async_trait]
impl Provider for OpenAiCompatibleProvider {
    async fn complete(
        &self,
        model_id: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
        system_static: &str,
        system_dynamic: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream, ProviderError> {
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

        if self.cfg.use_streaming_api {
            return Ok(self.stream_response_to_events(response));
        }

        let response = response
            .json::<ChatCompletionResponse>()
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

// ── Request / Response types (OpenAI-specific) ──

/// OpenAI Chat Completion request payload.
#[derive(Debug, Clone, Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<ChatCompletionMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ChatCompletionTool>>,
    stream: bool,
}

/// A single message in an OpenAI Chat Completion request.
#[derive(Debug, Clone, Serialize)]
struct ChatCompletionMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

impl From<&Message> for ChatCompletionMessage {
    fn from(message: &Message) -> Self {
        match message.role {
            fox_agent_core::Role::System => Self {
                role: "system".to_string(),
                content: Some(crate::util::extract_text_content(message)),
                tool_call_id: None,
            },
            fox_agent_core::Role::User => Self {
                role: "user".to_string(),
                content: Some(crate::util::extract_text_content(message)),
                tool_call_id: None,
            },
            fox_agent_core::Role::Assistant => Self {
                role: "assistant".to_string(),
                content: Some(crate::util::extract_text_content(message)),
                tool_call_id: None,
            },
            fox_agent_core::Role::Tool => {
                let (tool_call_id, content) = crate::util::extract_tool_result(message);
                Self {
                    role: "tool".to_string(),
                    content: Some(content),
                    tool_call_id: Some(tool_call_id),
                }
            }
        }
    }
}

/// OpenAI tool definition wrapper.
#[derive(Debug, Clone, Serialize)]
struct ChatCompletionTool {
    #[serde(rename = "type")]
    tool_type: String,
    function: ChatCompletionFunction,
}

/// OpenAI function metadata within a tool definition.
#[derive(Debug, Clone, Serialize)]
struct ChatCompletionFunction {
    name: String,
    description: String,
    parameters: Value,
}

/// OpenAI Chat Completion response (non-streaming).
#[derive(Debug, Clone, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatCompletionChoice>,
    #[serde(default)]
    usage: Option<OpenAiUsage>,
}

#[derive(Debug, Clone, Deserialize)]
struct ChatCompletionChoice {
    message: ChatCompletionResponseMessage,
}

#[derive(Debug, Clone, Deserialize)]
struct ChatCompletionResponseMessage {
    content: Option<String>,
    tool_calls: Option<Vec<ChatCompletionToolCall>>,
}

#[derive(Debug, Clone, Deserialize)]
struct ChatCompletionToolCall {
    id: String,
    function: ChatCompletionToolCallFunction,
}

#[derive(Debug, Clone, Deserialize)]
struct ChatCompletionToolCallFunction {
    name: String,
    arguments: String,
}

/// Token usage reported by OpenAI.
#[derive(Debug, Clone, Deserialize)]
struct OpenAiUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
    #[serde(default)]
    total_tokens: u32,
}

impl From<OpenAiUsage> for TokenUsage {
    fn from(value: OpenAiUsage) -> Self {
        Self {
            input_tokens: value.prompt_tokens,
            output_tokens: value.completion_tokens,
            total_tokens: value.total_tokens,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
        }
    }
}

// ── SSE streaming types ──

/// A single chunk in an OpenAI SSE streaming response.
#[derive(Debug, Clone, Deserialize)]
struct ChatCompletionChunk {
    choices: Vec<ChatCompletionChunkChoice>,
}

#[derive(Debug, Clone, Deserialize)]
struct ChatCompletionChunkChoice {
    delta: ChatCompletionChunkDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct ChatCompletionChunkDelta {
    content: Option<String>,
    tool_calls: Option<Vec<ChatCompletionChunkToolCall>>,
}

#[derive(Debug, Clone, Deserialize)]
struct ChatCompletionChunkToolCall {
    index: usize,
    id: Option<String>,
    function: Option<ChatCompletionChunkToolCallFunction>,
}

#[derive(Debug, Clone, Deserialize)]
struct ChatCompletionChunkToolCallFunction {
    name: Option<String>,
    arguments: Option<String>,
}

// ── SSE streaming parser ──

#[derive(Debug, Clone, Default)]
struct PartialToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

#[derive(Debug, Clone, Default)]
struct ToolCallAccumulator(Vec<PartialToolCall>);

impl ToolCallAccumulator {
    /// Apply streaming tool-call chunks, returning `ToolInputDelta` events for
    /// any argument fragments seen (for progress display).
    fn apply_chunks(&mut self, chunks: Vec<ChatCompletionChunkToolCall>) -> Vec<StreamEvent> {
        let mut deltas = Vec::new();
        for chunk in chunks {
            let index = chunk.index;
            if self.0.len() <= index {
                self.0.resize_with(index + 1, PartialToolCall::default);
            }
            let state = &mut self.0[index];
            if let Some(id) = chunk.id {
                state.id = Some(id);
            }
            if let Some(function) = chunk.function {
                if let Some(name) = function.name {
                    state.name = Some(name);
                }
                if let Some(arguments) = function.arguments {
                    if !arguments.is_empty() {
                        state.arguments.push_str(&arguments);
                        deltas.push(StreamEvent::ToolInputDelta {
                            index,
                            id: state.id.clone(),
                            name: state.name.clone(),
                            delta: arguments,
                        });
                    }
                }
            }
        }
        deltas
    }

    fn flush_as_events(&mut self) -> Result<Vec<StreamEvent>, ProviderError> {
        let calls = std::mem::take(&mut self.0);
        let mut events = Vec::new();
        for call in calls {
            if call.id.is_none() && call.name.is_none() && call.arguments.is_empty() {
                continue;
            }
            let id = call.id.ok_or_else(|| ProviderError::Message {
                message: "streaming tool call missing id".to_string(),
            })?;
            let name = call.name.ok_or_else(|| ProviderError::Message {
                message: "streaming tool call missing function name".to_string(),
            })?;
            let input = if call.arguments.trim().is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_str(&call.arguments).map_err(|err| ProviderError::Message {
                    message: format!("invalid streaming tool arguments JSON: {err}"),
                })?
            };
            events.push(StreamEvent::ToolUse { id, name, input });
        }
        Ok(events)
    }
}

fn parse_openai_stream(response: reqwest::Response) -> EventStream {
    let byte_stream = response.bytes_stream();

    Box::pin(try_stream! {
        let mut buffer = String::new();
        let mut tool_calls = ToolCallAccumulator::default();
        futures::pin_mut!(byte_stream);

        while let Some(chunk) = byte_stream.next().await {
            let chunk = chunk.map_err(|err| ProviderError::Message {
                message: format!("failed to read streaming response: {err}"),
            })?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(idx) = buffer.find('\n') {
                let mut line = buffer.drain(..=idx).collect::<String>();
                while line.ends_with(['\n', '\r']) {
                    line.pop();
                }
                if line.is_empty() { continue; }
                if !line.starts_with("data: ") { continue; }

                let data = &line["data: ".len()..];
                if data == "[DONE]" {
                    for event in tool_calls.flush_as_events()? {
                        yield event;
                    }
                    yield StreamEvent::MessageStop { stop_reason: None };
                    return;
                }

                let event: ChatCompletionChunk = serde_json::from_str(data).map_err(|err| {
                    ProviderError::Message {
                        message: format!("invalid streaming payload: {err}"),
                    }
                })?;

                let Some(choice) = event.choices.into_iter().next() else { continue };

                if let Some(text) = choice.delta.content {
                    if !text.is_empty() {
                        yield StreamEvent::TextDelta { text };
                    }
                }

                if let Some(chunks) = choice.delta.tool_calls {
                    for delta_event in tool_calls.apply_chunks(chunks) {
                        yield delta_event;
                    }
                }

                if matches!(choice.finish_reason.as_deref(), Some("tool_calls")) {
                    for event in tool_calls.flush_as_events()? {
                        yield event;
                    }
                }
            }
        }

        for event in tool_calls.flush_as_events()? {
            yield event;
        }
        yield StreamEvent::MessageStop { stop_reason: None };
    })
}
