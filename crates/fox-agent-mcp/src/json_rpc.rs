//! JSON‑RPC 2.0 wire format helpers.
//!
//! MCP uses a newline-delimited JSON stream over stdio / SSE.
//! Each message is a single JSON object terminated by `\n`.

use crate::types::{McpNotification, McpRequest, McpResponse};
use serde_json::Value;

/// Maximum line length before we refuse to parse (4 MiB).
const MAX_LINE_LEN: usize = 4 * 1024 * 1024;

/// Errors that can occur while encoding / decoding MCP messages.
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("parse: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("line too large: {0} bytes (max {MAX_LINE_LEN})")]
    LineTooLarge(usize),
    #[error("unknown message type")]
    UnknownMessageType,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Deserialize one JSON‑RPC line into either a response or notification.
pub fn parse_line(line: &str) -> Result<McpMessage, CodecError> {
    let line = line.trim();
    if line.is_empty() {
        return Err(CodecError::UnknownMessageType);
    }
    if line.len() > MAX_LINE_LEN {
        return Err(CodecError::LineTooLarge(line.len()));
    }

    let val: Value = serde_json::from_str(line)?;

    // Distinguish response (has "id" and either "result" or "error") from
    // notification (has "method" without "id").
    if val.get("id").is_some() && (val.get("result").is_some() || val.get("error").is_some()) {
        Ok(McpMessage::Response(serde_json::from_value(val)?))
    } else if val.get("method").is_some() {
        Ok(McpMessage::Notification(serde_json::from_value(val)?))
    } else {
        // Must be a response even if neither result nor error present (e.g. void).
        match serde_json::from_value::<McpResponse>(val.clone()) {
            Ok(r) => Ok(McpMessage::Response(r)),
            Err(_) => Err(CodecError::UnknownMessageType),
        }
    }
}

/// Serialize a request to a JSON line (with trailing newline).
pub fn serialize_request(req: &McpRequest) -> Result<String, CodecError> {
    let mut json = serde_json::to_string(req)?;
    json.push('\n');
    Ok(json)
}

/// An MCP message — either a response or a server-initiated notification.
#[derive(Debug, Clone)]
pub enum McpMessage {
    Response(McpResponse),
    Notification(McpNotification),
}
