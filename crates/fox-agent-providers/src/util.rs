use fox_agent_core::{Message, ProviderError};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};

use fox_agent_core::{AuthConfig, ProviderConfig};

/// Build HTTP headers from a provider configuration (auth + defaults).
pub fn build_headers(cfg: &ProviderConfig) -> Result<HeaderMap, ProviderError> {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

    match &cfg.auth {
        AuthConfig::None => {}
        AuthConfig::BearerToken(token) => {
            let value = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|err| {
                ProviderError::Message {
                    message: format!("invalid authorization header: {err}"),
                }
            })?;
            headers.insert(AUTHORIZATION, value);
        }
        AuthConfig::ApiKeyHeader { header_name, value } => {
            let name = HeaderName::from_bytes(header_name.as_bytes()).map_err(|err| {
                ProviderError::Message {
                    message: format!("invalid header name `{header_name}`: {err}"),
                }
            })?;
            let value = HeaderValue::from_str(value).map_err(|err| ProviderError::Message {
                message: format!("invalid header value for `{header_name}`: {err}"),
            })?;
            headers.insert(name, value);
        }
    }

    for (key, value) in &cfg.default_headers {
        let name = HeaderName::from_bytes(key.as_bytes()).map_err(|err| ProviderError::Message {
            message: format!("invalid default header name `{key}`: {err}"),
        })?;
        let value = HeaderValue::from_str(value).map_err(|err| ProviderError::Message {
            message: format!("invalid default header value for `{key}`: {err}"),
        })?;
        headers.insert(name, value);
    }

    Ok(headers)
}

/// Concatenate all text content of a message into a single string.
pub fn extract_text_content(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            fox_agent_core::ContentBlock::Text { text } => Some(text.as_str()),
            fox_agent_core::ContentBlock::Reasoning { text } => Some(text.as_str()),
            fox_agent_core::ContentBlock::ToolResult { text, .. } => Some(text.as_str()),
            fox_agent_core::ContentBlock::Image { data, .. } => Some(data.as_str()),
            fox_agent_core::ContentBlock::ToolUse { .. } => None,
            fox_agent_core::ContentBlock::NarrativeSummary { text } => Some(text.as_str()),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Extract the first tool result (call_id + text) from a Tool-role message.
pub fn extract_tool_result(message: &Message) -> (String, String) {
    let mut tool_call_id = String::new();
    let mut content = String::new();
    for block in &message.content {
        if let fox_agent_core::ContentBlock::ToolResult { call_id, text, .. } = block {
            tool_call_id = call_id.clone();
            content = text.clone();
            break;
        }
    }
    (tool_call_id, content)
}
