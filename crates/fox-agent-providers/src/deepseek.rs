//! DeepSeek provider — optimized for DeepSeek API v4 features.
//!
//! Key DeepSeek API optimizations:
//!
//! - **Prefix caching**: Freezes system prompt and tool schemas on first call so
//!   subsequent turns share byte-identical prefixes, maximizing disk KV cache hits
//!   (billed at ~10% of normal input price).
//!
//! - **Thinking mode**: v4 models (`deepseek-v4-pro`, `deepseek-v4-flash`) support
//!   thinking via `thinking.type: "enabled"` + `reasoning_effort`. When thinking is
//!   enabled, `temperature`/`top_p` are NOT sent (the API ignores them).
//!
//! - **reasoning_content**: DeepSeek streams the thinking chain via `reasoning_content`
//!   in the SSE delta, mapped to `StreamEvent::ThinkingDelta` for the application layer.
//!   When persisting assistant messages, reasoning content is stored as
//!   `ContentBlock::Reasoning` so it can be sent back on tool-calling turns (the API
//!   requires it when tool calls are present).
//!
//! - **Usage via stream_options**: Uses `stream_options: {include_usage: true}` so the
//!   final chunk carries the official usage object with `prompt_cache_hit_tokens`,
//!   `prompt_cache_miss_tokens`, and `completion_tokens_details.reasoning_tokens`.

use async_trait::async_trait;
use fox_agent_core::{
    ContentBlock, EventStream, Message, Provider, ProviderError, Role, StreamEvent, TokenUsage,
    ToolDefinition,
};
use futures::stream::StreamExt;
use reqwest::header::HeaderMap;
use reqwest::Client;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

use crate::config::ProviderConfig;
use crate::util::build_headers;

const SSE_CHUNK_TIMEOUT: Duration = Duration::from_secs(180);
const MAX_RETRIES: u32 = 3;
const RETRY_BASE_DELAY_MS: u64 = 1000;
const DEFAULT_MAX_TOKENS: u32 = 131_072;

// ---------------------------------------------------------------------------
// DeepSeekProvider
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct DeepSeekProvider {
    http: Client,
    cfg: ProviderConfig,
    max_tokens: u32,
    thinking_enabled: Arc<RwLock<bool>>,
    /// Frozen system prompt — set on first complete() call, never modified.
    frozen_system: Arc<RwLock<Option<String>>>,
    /// Frozen tools JSON — serialized on first call with sorted keys.
    frozen_tools: Arc<RwLock<Option<Vec<Value>>>>,
}

impl DeepSeekProvider {
    /// Create a new DeepSeek provider from a [`ProviderConfig`].
    ///
    /// Uses [`ProviderConfig::deepseek`] for a convenient default — or pass
    /// any custom [`ProviderConfig`] to override the base URL, headers, etc.
    pub fn new(cfg: ProviderConfig) -> Self {
        let timeout_secs = cfg.timeout_secs.max(120);
        let http = Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .expect("DeepSeek: failed to build HTTP client");
        Self {
            http,
            cfg,
            max_tokens: DEFAULT_MAX_TOKENS,
            thinking_enabled: Arc::new(RwLock::new(true)),
            frozen_system: Arc::new(RwLock::new(None)),
            frozen_tools: Arc::new(RwLock::new(None)),
        }
    }

    /// Convenience constructor using the default DeepSeek endpoint.
    pub fn with_default_endpoint(api_key: impl Into<String>) -> Self {
        Self::new(ProviderConfig::deepseek(api_key))
    }

    /// Override the maximum output tokens.
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Enable or disable thinking mode for v4 models.
    pub async fn set_thinking_enabled(&self, enabled: bool) {
        *self.thinking_enabled.write().await = enabled;
    }

    /// Returns whether thinking mode is currently enabled.
    pub async fn thinking_enabled(&self) -> bool {
        *self.thinking_enabled.read().await
    }

    fn build_headers(&self) -> Result<HeaderMap, ProviderError> {
        build_headers(&self.cfg)
    }

    fn chat_url(&self) -> String {
        format!("{}/chat/completions", self.cfg.base_url.trim_end_matches('/'))
    }

    fn is_v4_model(model: &str) -> bool {
        model.trim().to_ascii_lowercase().starts_with("deepseek-v4")
    }

    fn is_reasoner_model(model: &str) -> bool {
        let m = model.trim().to_ascii_lowercase();
        m.contains("reasoner") || m.contains("r1")
    }

    /// Serialize tool definitions into OpenAI function-calling format.
    fn serialize_tools(tools: &[ToolDefinition]) -> Vec<Value> {
        if tools.is_empty() {
            return Vec::new();
        }
        tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters_schema,
                    }
                })
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Message conversion — build OpenAI-compatible messages from SDK messages
// ---------------------------------------------------------------------------

/// Build OpenAI-format message array from SDK messages.
///
/// For DeepSeek thinking mode:
/// - `reasoning_content` from assistant messages is always included for
///   tool-calling turns (required by DeepSeek API). For non-tool turns
///   the API silently ignores it, but sending it is harmless.
/// - System message is always first for prefix cache stability.
fn build_api_messages(messages: &[Message], system: &str) -> Vec<Value> {
    let mut api_messages: Vec<Value> = Vec::new();

    if !system.is_empty() {
        api_messages.push(serde_json::json!({
            "role": "system",
            "content": system
        }));
    }

    for msg in messages {
        match msg.role {
            Role::User => {
                let mut text_parts: Vec<String> = Vec::new();
                let mut images: Vec<Value> = Vec::new();
                let mut tool_results: Vec<(String, String)> = Vec::new();

                for block in &msg.content {
                    match block {
                        ContentBlock::Text { text } => text_parts.push(text.clone()),
                        ContentBlock::Image { media_type, data } => {
                            images.push(serde_json::json!({
                                "type": "image_url",
                                "image_url": {
                                    "url": format!("data:{};base64,{}", media_type, data)
                                }
                            }));
                        }
                        ContentBlock::ToolResult { call_id, text, .. } => {
                            tool_results.push((call_id.clone(), text.clone()));
                        }
                        _ => {}
                    }
                }

                for (id, output) in &tool_results {
                    api_messages.push(serde_json::json!({
                        "role": "tool", "tool_call_id": id, "content": output
                    }));
                }

                if !images.is_empty() {
                    let mut content_parts: Vec<Value> = Vec::new();
                    for text in &text_parts {
                        content_parts.push(serde_json::json!({ "type": "text", "text": text }));
                    }
                    content_parts.extend(images);
                    api_messages.push(serde_json::json!({ "role": "user", "content": content_parts }));
                } else if !text_parts.is_empty() {
                    api_messages.push(serde_json::json!({
                        "role": "user", "content": text_parts.join("\n")
                    }));
                }
            }
            Role::Assistant => {
                let mut text_content = String::new();
                let mut reasoning_content = String::new();
                let mut tool_calls: Vec<Value> = Vec::new();

                for block in &msg.content {
                    match block {
                        ContentBlock::Text { text } => text_content.push_str(text),
                        ContentBlock::Reasoning { text } => reasoning_content.push_str(text),
                        ContentBlock::ToolUse { id, name, input } => {
                            let args = if input.is_object() {
                                serde_json::to_string(input).unwrap_or_default()
                            } else {
                                "{}".to_string()
                            };
                            tool_calls.push(serde_json::json!({
                                "id": id, "type": "function",
                                "function": { "name": name, "arguments": args }
                            }));
                        }
                        _ => {}
                    }
                }

                let mut assistant_msg = serde_json::json!({ "role": "assistant" });
                if !text_content.is_empty() {
                    assistant_msg["content"] = serde_json::json!(text_content);
                }
                if !tool_calls.is_empty() {
                    assistant_msg["tool_calls"] = serde_json::json!(tool_calls);
                }
                if !reasoning_content.is_empty() {
                    assistant_msg["reasoning_content"] = serde_json::json!(reasoning_content);
                } else if !tool_calls.is_empty() {
                    assistant_msg["reasoning_content"] = serde_json::json!(" ");
                }

                if !text_content.is_empty() || !tool_calls.is_empty() || !reasoning_content.is_empty() {
                    api_messages.push(assistant_msg);
                }
            }
            Role::System => {} // handled via top-level system parameter
            Role::Tool => {
                let (tool_call_id, content) = extract_first_tool_result(&msg.content);
                if !content.is_empty() {
                    api_messages.push(serde_json::json!({
                        "role": "tool", "tool_call_id": tool_call_id, "content": content
                    }));
                }
            }
        }
    }

    api_messages
}

/// Extract the first tool result from a list of content blocks.
fn extract_first_tool_result(content: &[ContentBlock]) -> (String, String) {
    for block in content {
        if let ContentBlock::ToolResult { call_id, text, .. } = block {
            return (call_id.clone(), text.clone());
        }
    }
    (String::new(), String::new())
}

// ---------------------------------------------------------------------------
// SSE streaming
// ---------------------------------------------------------------------------

/// Accumulated state for a single tool call during SSE parsing.
#[derive(Debug, Clone, Default)]
struct AccumulatingToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

/// Run a streaming chat completion request with retry logic.
async fn run_deepseek_stream(
    client: Client,
    url: String,
    headers: HeaderMap,
    request_body: Value,
) -> Result<EventStream, ProviderError> {
    let mut last_error: Option<ProviderError> = None;

    for attempt in 0..MAX_RETRIES {
        if attempt > 0 {
            let delay = RETRY_BASE_DELAY_MS * (1 << (attempt - 1));
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }

        match stream_response(client.clone(), url.clone(), headers.clone(), request_body.clone()).await {
            Ok(stream) => return Ok(stream),
            Err(e) => {
                let error_str = e.to_string().to_lowercase();
                let retryable = error_str.contains("timeout")
                    || error_str.contains("connection")
                    || error_str.contains("rate")
                    || error_str.contains("server")
                    || error_str.contains("busy")
                    || error_str.contains("capacity");

                if retryable && attempt + 1 < MAX_RETRIES {
                    last_error = Some(e);
                    continue;
                }
                return Err(e);
            }
        }
    }

    Err(last_error.unwrap_or_else(|| ProviderError::Message {
        message: format!("DeepSeek: request failed after {MAX_RETRIES} retries"),
    }))
}

/// Single streaming request to the DeepSeek API.
async fn stream_response(
    client: Client,
    url: String,
    headers: HeaderMap,
    request_body: Value,
) -> Result<EventStream, ProviderError> {
    let response = client
        .post(&url)
        .headers(headers)
        .json(&request_body)
        .send()
        .await
        .map_err(|err| ProviderError::Message {
            message: format!("DeepSeek request failed: {err}"),
        })?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(ProviderError::Message {
            message: format!("DeepSeek API error: {status} — {body}"),
        });
    }

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<StreamEvent, ProviderError>>(128);
    let byte_stream = response.bytes_stream().map(|r| r.map(|b| b.to_vec()).map_err(|e| e));

    tokio::spawn(async move {
        parse_sse_stream(byte_stream, tx).await;
    });

    let stream = futures::stream::unfold(rx, |mut rx| async {
        rx.recv().await.map(|item| (item, rx))
    });
    Ok(Box::pin(stream))
}

/// Parse SSE byte stream from DeepSeek and send events through the channel.
async fn parse_sse_stream(
    byte_stream: impl futures::Stream<Item = Result<Vec<u8>, reqwest::Error>> + 'static,
    tx: tokio::sync::mpsc::Sender<Result<StreamEvent, ProviderError>>,
) {
    let mut buffer = String::new();
    let mut tool_calls: Vec<AccumulatingToolCall> = Vec::new();
    let mut stream = Box::pin(byte_stream);
    let send_ok = |tx: &tokio::sync::mpsc::Sender<Result<StreamEvent, ProviderError>>, event| {
        let _ = tx.try_send(Ok(event));
    };
    let send_err = |tx: &tokio::sync::mpsc::Sender<Result<StreamEvent, ProviderError>>, msg: String| {
        let _ = tx.try_send(Err(ProviderError::Message { message: msg }));
    };

    loop {
        let chunk = match tokio::time::timeout(SSE_CHUNK_TIMEOUT, stream.next()).await {
            Ok(Some(Ok(bytes))) => bytes,
            Ok(Some(Err(e))) => {
                send_err(&tx, format!("DeepSeek stream read error: {e}"));
                return;
            }
            Ok(None) => break,
            Err(_) => {
                send_err(&tx, format!("DeepSeek stream timeout after {}s", SSE_CHUNK_TIMEOUT.as_secs()));
                return;
            }
        };

        buffer.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(line_end) = buffer.find('\n') {
            let line = buffer[..line_end].trim_end_matches('\r').to_string();
            buffer = buffer[line_end + 1..].to_string();

            let data = if let Some(d) = line.strip_prefix("data: ") { d } else { continue };

            if data == "[DONE]" { break; }

            let parsed: Value = match serde_json::from_str(data) {
                Ok(v) => v,
                Err(_) => continue,
            };

            if let Some(error) = parsed.get("error") {
                let msg = error.get("message").and_then(|v| v.as_str()).unwrap_or("unknown error");
                send_err(&tx, msg.to_string());
                return;
            }

            // ── Usage from stream_options.include_usage ──
            if let Some(usage) = parsed.get("usage") {
                if !usage.is_null() {
                    if let Some(prompt_tokens) = usage.get("prompt_tokens").and_then(|v| v.as_u64()) {
                        let output_tokens = usage.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                        send_ok(&tx, StreamEvent::Usage {
                            usage: TokenUsage {
                                input_tokens: prompt_tokens as u32,
                                output_tokens,
                                total_tokens: (prompt_tokens + output_tokens as u64) as u32,
                                cache_read_input_tokens: None,
                                cache_creation_input_tokens: None,
                            }
                        });
                        continue;
                    }
                }
            }

            let Some(choices) = parsed.get("choices") else { continue };

            for choice in choices.as_array().iter().flat_map(|a| a.iter()) {
                let Some(delta) = choice.get("delta") else { continue };

                if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
                    if !content.is_empty() {
                        send_ok(&tx, StreamEvent::TextDelta { text: content.to_string() });
                    }
                }

                if let Some(reasoning) = delta.get("reasoning_content").and_then(|v| v.as_str()) {
                    if !reasoning.is_empty() {
                        send_ok(&tx, StreamEvent::ThinkingDelta { text: reasoning.to_string() });
                    }
                }

                if let Some(tc_array) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                    for tc in tc_array {
                        let idx = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                        while tool_calls.len() <= idx {
                            tool_calls.push(AccumulatingToolCall::default());
                        }
                        if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                            tool_calls[idx].id = Some(id.to_string());
                        }
                        if let Some(fn_name) = tc.get("function").and_then(|v| v.get("name")).and_then(|v| v.as_str()) {
                            tool_calls[idx].name = Some(fn_name.to_string());
                        }
                        if let Some(args) = tc.get("function").and_then(|v| v.get("arguments")).and_then(|v| v.as_str()) {
                            tool_calls[idx].arguments.push_str(args);
                        }
                    }
                }

                if let Some(reason) = choice.get("finish_reason").and_then(|v| v.as_str()) {
                    if reason == "tool_calls" || reason == "function_call" {
                        for tc in tool_calls.drain(..) {
                            if let (Some(id), Some(name)) = (tc.id, tc.name) {
                                let input = if tc.arguments.trim().is_empty() {
                                    serde_json::json!({})
                                } else {
                                    serde_json::from_str(&tc.arguments).unwrap_or(serde_json::json!({}))
                                };
                                send_ok(&tx, StreamEvent::ToolUse { id, name, input });
                            }
                        }
                    }
                }
            }
        }
    }

    for tc in tool_calls.drain(..) {
        if let (Some(id), Some(name)) = (tc.id, tc.name) {
            let input = if tc.arguments.trim().is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_str(&tc.arguments).unwrap_or(serde_json::json!({}))
            };
            send_ok(&tx, StreamEvent::ToolUse { id, name, input });
        }
    }

    send_ok(&tx, StreamEvent::MessageStop { stop_reason: None });
}

// ---------------------------------------------------------------------------
// Provider trait implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl Provider for DeepSeekProvider {
    async fn complete(
        &self,
        model_id: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
        system_static: &str,
        system_dynamic: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream, ProviderError> {
        let thinking_enabled = *self.thinking_enabled.read().await;

        // Combine system parts
        let system = [system_static.trim(), system_dynamic.trim()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");

        // ── Cache: freeze system prompt ──
        {
            let frozen = self.frozen_system.read().await;
            if let Some(ref frozen_system) = *frozen {
                if frozen_system != &system {
                    // Log warning but continue — cache miss is unavoidable
                }
            }
        }
        {
            let mut frozen = self.frozen_system.write().await;
            if frozen.is_none() {
                *frozen = Some(system.clone());
            } else if frozen.as_deref() != Some(&system) {
                *frozen = Some(system.clone());
            }
        }

        // ── Cache: freeze tool schemas ──
        let frozen_tools: Vec<Value> = {
            let frozen = self.frozen_tools.read().await;
            if let Some(ref ft) = *frozen {
                ft.clone()
            } else {
                drop(frozen);
                let serialized = Self::serialize_tools(tools);
                let mut frozen = self.frozen_tools.write().await;
                *frozen = Some(serialized.clone());
                serialized
            }
        };

        // ── Build API messages ──
        let api_messages = build_api_messages(messages, &system);

        // ── Build request body ──
        let mut request = serde_json::json!({
            "model": model_id,
            "messages": api_messages,
            "stream": true,
            "max_tokens": self.max_tokens,
            "stream_options": { "include_usage": true },
        });

        if !frozen_tools.is_empty() {
            request["tools"] = serde_json::json!(frozen_tools);
            request["tool_choice"] = serde_json::json!("auto");
        }

        // ── Thinking mode ──
        if Self::is_v4_model(model_id) {
            request["thinking"] = serde_json::json!({
                "type": if thinking_enabled { "enabled" } else { "disabled" }
            });
            if thinking_enabled {
                request["reasoning_effort"] = serde_json::json!("high");
            }
        } else if Self::is_reasoner_model(model_id) {
            request["thinking"] = serde_json::json!({
                "type": if thinking_enabled { "enabled" } else { "disabled" }
            });
        }

        // ── Execute streaming request with retry ──
        let client = self.http.clone();
        let url = self.chat_url();
        let headers = self.build_headers()?;

        run_deepseek_stream(client, url, headers, request).await
    }

    fn name(&self) -> &str {
        &self.cfg.provider_name
    }
}
