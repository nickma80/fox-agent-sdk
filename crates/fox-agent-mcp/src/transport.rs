//! Transport layer — stdio subprocess and SSE HTTP transports.

use crate::types::{McpRequest, McpResponse};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, Notify};

// ── Transport trait ──

/// The transport abstraction for MCP communication.
///
/// Every server connection uses exactly one transport mode.
#[async_trait]
pub trait McpTransport: Send + Sync {
    /// Send a JSON‑RPC request and wait for the matching response.
    async fn send(&self, request: &McpRequest) -> Result<McpResponse, TransportError>;

    /// Start the transport (connect / spawn process).
    async fn start(&self) -> Result<(), TransportError>;

    /// Graceful shutdown.
    async fn shutdown(&self) -> Result<(), TransportError>;
}

// ── Errors ──

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("process exited: {0}")]
    ProcessExited(String),
    #[error("timeout: {0}")]
    Timeout(String),
    #[error("codec: {0}")]
    Codec(#[from] crate::json_rpc::CodecError),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("not started")]
    NotStarted,
}

// ── Stdio transport ──

/// Default grace period (ms) after spawning the child before checking if it
/// exited immediately.  Catches fast-fail scenarios (missing binary, bad args,
/// Python import errors) *before* the caller sends the first request.
///
/// 5 seconds covers typical `uvx` / `npx` package‑install overhead.
const DEFAULT_STARTUP_GRACE_MS: u64 = 5_000;

/// Configuration for a stdio‑based MCP server (local subprocess).
#[derive(Clone)]
pub struct StdioTransportConfig {
    pub command: String,
    pub args: Vec<String>,
    pub env: Option<HashMap<String, String>>,
    pub cwd: Option<String>,
    pub request_timeout_ms: u64,
    /// How long to wait after spawn before verifying the child is still alive.
    /// Set to 0 to disable the startup health‑check.
    pub startup_grace_ms: u64,
}

impl StdioTransportConfig {
    /// Create a config with sensible defaults for `startup_grace_ms`.
    pub fn new(
        command: impl Into<String>,
        args: Vec<String>,
        request_timeout_ms: u64,
    ) -> Self {
        Self {
            command: command.into(),
            args,
            env: None,
            cwd: None,
            request_timeout_ms,
            startup_grace_ms: DEFAULT_STARTUP_GRACE_MS,
        }
    }
}

/// One in-flight request sent to the stdio I/O task: the serialized request
/// JSON paired with a oneshot channel to deliver the matching response.
type PendingRequest = (String, tokio::sync::oneshot::Sender<Result<McpResponse, TransportError>>);

// ── Helper: build a human‑readable diagnostic from captured output buffers ──

fn build_diagnostic(label: &str, stdout: &str, stderr: &str) -> String {
    let mut parts = vec![label.to_string()];
    if !stdout.is_empty() {
        let lines: Vec<&str> = stdout.lines().take(20).collect();
        parts.push(format!("stdout:\n{}", lines.join("\n")));
    }
    if !stderr.is_empty() {
        let lines: Vec<&str> = stderr.lines().take(20).collect();
        parts.push(format!("stderr:\n{}", lines.join("\n")));
    }
    if parts.len() == 1 {
        parts[0].clone()
    } else {
        parts.join("\n")
    }
}

/// Stdio transport — spawns a child process and communicates via stdin / stdout.
///
/// # Lifecycle
///
/// 1. `start()` spawns the child, waits a short grace period, and checks
///    whether it exited immediately.  If so, returns `ProcessExited` right
///    away with the captured stdout + stderr.
/// 2. A background I/O task multiplexes outgoing requests (written to stdin)
///    and incoming JSON‑RPC responses (read line‑by‑line from stdout).
/// 3. When stdout reaches EOF the I/O task drains pending requests with a
///    `ProcessExited` error that includes the child's exit code and any
///    diagnostic output.
pub struct StdioTransport {
    config: StdioTransportConfig,
    /// Channel to send requests to the I/O task.
    sender: tokio::sync::Mutex<Option<mpsc::Sender<PendingRequest>>>,
    /// Handle to the child process.
    child: Arc<tokio::sync::Mutex<Option<Child>>>,
    /// Signalled when the stderr‑capture task finishes (child closed stderr).
    stderr_done: Arc<Notify>,
}

impl StdioTransport {
    pub fn new(config: StdioTransportConfig) -> Self {
        Self {
            config,
            sender: tokio::sync::Mutex::new(None),
            child: Arc::new(tokio::sync::Mutex::new(None)),
            stderr_done: Arc::new(Notify::new()),
        }
    }
}

#[async_trait]
impl McpTransport for StdioTransport {
    async fn start(&self) -> Result<(), TransportError> {
        // ── Build the command ──
        let mut cmd = Command::new(&self.config.command);
        cmd.args(&self.config.args);

        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x08000000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        if let Some(ref cwd) = self.config.cwd {
            cmd.current_dir(cwd);
        }
        if let Some(ref env) = self.config.env {
            for (k, v) in env {
                cmd.env(k, v);
            }
        }
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| {
            TransportError::ProcessExited(format!(
                "failed to spawn '{}': {e}",
                self.config.command,
            ))
        })?;

        let child_id = child.id().unwrap_or(0);

        // ── Startup health‑check: wait and see if the child died immediately ──
        if self.config.startup_grace_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(self.config.startup_grace_ms))
                .await;

            match child.try_wait() {
                Ok(Some(status)) => {
                    // Child exited during the grace period — collect any
                    // buffered stdout / stderr for diagnostics.
                    let mut stdout_str = String::new();
                    let mut stderr_str = String::new();
                    if let Some(ref mut out) = child.stdout {
                        use tokio::io::AsyncReadExt;
                        let mut buf = vec![0u8; 8192];
                        loop {
                            match out.read(&mut buf).await {
                                Ok(0) | Err(_) => break,
                                Ok(n) => {
                                    if let Ok(s) = std::str::from_utf8(&buf[..n]) {
                                        stdout_str.push_str(s);
                                    }
                                }
                            }
                        }
                    }
                    if let Some(ref mut err) = child.stderr {
                        use tokio::io::AsyncReadExt;
                        let mut buf = vec![0u8; 8192];
                        loop {
                            match err.read(&mut buf).await {
                                Ok(0) | Err(_) => break,
                                Ok(n) => {
                                    if let Ok(s) = std::str::from_utf8(&buf[..n]) {
                                        stderr_str.push_str(s);
                                    }
                                }
                            }
                        }
                    }
                    let detail = build_diagnostic(
                        &format!(
                            "process '{}' (pid {}) exited immediately with {status}",
                            self.config.command, child_id,
                        ),
                        &stdout_str,
                        &stderr_str,
                    );
                    return Err(TransportError::ProcessExited(detail));
                }
                Ok(None) => {
                    // Still alive — good.
                }
                Err(e) => {
                    return Err(TransportError::Io(e));
                }
            }
        }

        // ── Take the pipes ──
        let stdout = child.stdout.take().ok_or_else(|| {
            TransportError::Protocol("child process has no stdout".into())
        })?;
        let stdin = child.stdin.take().ok_or_else(|| {
            TransportError::Protocol("child process has no stdin".into())
        })?;
        let stderr = child.stderr.take();

        // Shared buffers for diagnostic output.
        let stdout_buf: Arc<std::sync::Mutex<String>> =
            Arc::new(std::sync::Mutex::new(String::new()));
        let stderr_buf: Arc<std::sync::Mutex<String>> =
            Arc::new(std::sync::Mutex::new(String::new()));

        // ── Stderr capture task ──
        let stderr_done = self.stderr_done.clone();
        if let Some(stderr) = stderr {
            let stderr_buf = stderr_buf.clone();
            let server = self.config.command.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                let mut buf = String::new();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "mcp::stderr", server = %server, pid = child_id, "{line}");
                    buf.push_str(&line);
                    buf.push('\n');
                }
                if let Ok(mut guard) = stderr_buf.lock() {
                    *guard = buf;
                }
                stderr_done.notify_one();
            });
        } else {
            // No stderr pipe — mark as "done" immediately so EOF handling
            // doesn't wait forever.
            stderr_done.notify_one();
        }

        // ── Channel and I/O task ──
        *self.child.lock().await = Some(child);

        let (tx, mut rx) = mpsc::channel::<PendingRequest>(32);
        *self.sender.lock().await = Some(tx);

        let child_handle = self.child.clone();
        let stderr_done2 = self.stderr_done.clone();
        let stderr_buf2 = stderr_buf.clone();
        let stdout_buf2 = stdout_buf.clone();
        let cmd_label = self.config.command.clone();
        tokio::spawn(async move {
            io_task(
                stdout,
                stdin,
                &mut rx,
                child_handle,
                stderr_done2,
                stdout_buf2,
                stderr_buf2,
                &cmd_label,
            )
            .await;
        });

        Ok(())
    }

    async fn send(&self, request: &McpRequest) -> Result<McpResponse, TransportError> {
        let json = crate::json_rpc::serialize_request(request)?;
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();

        {
            let sender_guard = self.sender.lock().await;
            let sender = sender_guard.as_ref().ok_or(TransportError::NotStarted)?;
            sender.send((json, reply_tx)).await.map_err(|_| {
                TransportError::ProcessExited("transport channel closed".into())
            })?;
        }

        match tokio::time::timeout(
            std::time::Duration::from_millis(self.config.request_timeout_ms),
            reply_rx,
        )
        .await
        {
            Ok(Ok(resp)) => resp,
            Ok(Err(_recv_err)) => Err(TransportError::ProcessExited(
                "response channel closed".into(),
            )),
            Err(_elapsed) => Err(TransportError::Timeout("request timed out".into())),
        }
    }

    async fn shutdown(&self) -> Result<(), TransportError> {
        // Close sender channel to stop the I/O task.
        *self.sender.lock().await = None;

        // Kill the child process.
        if let Some(mut child) = self.child.lock().await.take() {
            let _ = child.kill().await;
        }
        Ok(())
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        // Best-effort: we can't do async in drop.
        // The sender channel drop will cause the I/O task to exit.
    }
}

// ── I/O task (extracted for readability) ──

#[allow(clippy::too_many_arguments)]
async fn io_task(
    stdout: tokio::process::ChildStdout,
    mut stdin: tokio::process::ChildStdin,
    rx: &mut mpsc::Receiver<PendingRequest>,
    child_handle: Arc<tokio::sync::Mutex<Option<Child>>>,
    stderr_done: Arc<Notify>,
    stdout_buf: Arc<std::sync::Mutex<String>>,
    stderr_buf: Arc<std::sync::Mutex<String>>,
    cmd_label: &str,
) {
    let mut reader = BufReader::new(stdout);
    let mut pending: HashMap<
        String,
        tokio::sync::oneshot::Sender<Result<McpResponse, TransportError>>,
    > = HashMap::new();
    let mut line_buf = String::new();
    let mut saw_json_rpc = false;

    /// Flush the current `line_buf` content into the shared stdout buffer
    /// (used for early stdout that isn't JSON‑RPC).
    fn flush_stdout_buf(buf: &Arc<std::sync::Mutex<String>>, extra: &str) {
        if let Ok(mut guard) = buf.lock() {
            if !guard.is_empty() {
                guard.push('\n');
            }
            guard.push_str(extra);
        }
    }

    loop {
        tokio::select! {
            // --- Incoming request to send ---
            msg = rx.recv() => {
                match msg {
                    Some((json, reply)) => {
                        // Extract id for routing.  JSON‑RPC ids may be a
                        // number or a string, so we key on the id value's
                        // canonical JSON form (e.g. "1" or "\"abc\"").
                        let req_id: String = match serde_json::from_str::<serde_json::Value>(&json) {
                            Ok(v) if !v["id"].is_null() => v["id"].to_string(),
                            _ => String::new(),
                        };

                        // MCP stdio: newline‑delimited JSON.  Without the
                        // trailing `\n` the child's stdin reader blocks.
                        if let Err(e) = stdin.write_all(json.as_bytes()).await {
                            let _ = reply.send(Err(TransportError::Io(e)));
                            continue;
                        }
                        if let Err(e) = stdin.write_all(b"\n").await {
                            let _ = reply.send(Err(TransportError::Io(e)));
                            continue;
                        }
                        if let Err(e) = stdin.flush().await {
                            let _ = reply.send(Err(TransportError::Io(e)));
                            continue;
                        }
                        if !req_id.is_empty() {
                            pending.insert(req_id, reply);
                        }
                    }
                    None => {
                        // Channel closed — graceful shutdown.
                        let detail = build_diagnostic(
                            "transport channel closed",
                            &stdout_buf.lock().map(|s| s.clone()).unwrap_or_default(),
                            &stderr_buf.lock().map(|s| s.clone()).unwrap_or_default(),
                        );
                        drain_pending(&mut pending, detail);
                        break;
                    }
                }
            }

            // --- Incoming response line ---
            line_res = reader.read_line(&mut line_buf) => {
                match line_res {
                    Ok(0) => {
                        // EOF — child closed its stdout.
                        //
                        // Wait briefly for stderr capture and exit status to
                        // settle (especially on Windows where pipe closure ≠
                        // process exit).
                        let _ = tokio::time::timeout(
                            std::time::Duration::from_secs(2),
                            stderr_done.notified(),
                        ).await;

                        let status = {
                            let mut guard = child_handle.lock().await;
                            match guard.as_mut() {
                                Some(child) => child.try_wait().ok().flatten(),
                                None => None,
                            }
                        };

                        let label = match status {
                            Some(s) if s.success() && !saw_json_rpc => format!(
                                "process '{}' exited with {s} before sending JSON-RPC response — \
                                 the wrapper may not have properly forwarded stdout \
                                 (common with Python tool runners on Windows)",
                                cmd_label,
                            ),
                            Some(s) if s.success() => format!(
                                "process '{}' exited cleanly with {s} but stdout closed \
                                 while waiting for JSON-RPC response",
                                cmd_label,
                            ),
                            Some(s) => format!(
                                "process '{}' exited with {s}",
                                cmd_label,
                            ),
                            None if !saw_json_rpc => format!(
                                "process '{}' stdout closed before any JSON-RPC response — \
                                 likely a pipe inheritance issue on Windows; the wrapper \
                                 started a subprocess but did not forward stdin/stdout",
                                cmd_label,
                            ),
                            None => format!(
                                "process '{}' stdout closed (child may still be alive)",
                                cmd_label,
                            ),
                        };
                        let detail = build_diagnostic(
                            &label,
                            &stdout_buf.lock().map(|s| s.clone()).unwrap_or_default(),
                            &stderr_buf.lock().map(|s| s.clone()).unwrap_or_default(),
                        );
                        drain_pending(&mut pending, detail);
                        break;
                    }
                    Ok(_) => {
                        let trimmed = line_buf.trim().to_string();
                        line_buf.clear();

                        // Accumulate non‑JSON‑RPC stdout in the shared buffer
                        // for diagnostics (some servers print fatal errors or
                        // startup banners to stdout).
                        if !saw_json_rpc {
                            if serde_json::from_str::<serde_json::Value>(&trimmed).is_ok() {
                                saw_json_rpc = true;
                            } else {
                                flush_stdout_buf(&stdout_buf, &trimmed);
                            }
                        }

                        // Parse and route the response to the waiting sender.
                        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&trimmed)
                            && !val["id"].is_null()
                            && let Some(reply) = pending.remove(&val["id"].to_string())
                        {
                            match serde_json::from_value::<McpResponse>(val) {
                                Ok(resp) => { let _ = reply.send(Ok(resp)); }
                                Err(e) => {
                                    let _ = reply.send(Err(TransportError::Codec(
                                        crate::json_rpc::CodecError::Parse(e)
                                    )));
                                }
                            }
                        }
                        // Notifications (no id) and unmatched ids are silently
                        // ignored — they don't have a pending sender.
                    }
                    Err(e) => {
                        let detail = build_diagnostic(
                            &format!("stdout read error: {e}"),
                            &stdout_buf.lock().map(|s| s.clone()).unwrap_or_default(),
                            &stderr_buf.lock().map(|s| s.clone()).unwrap_or_default(),
                        );
                        drain_pending(&mut pending, detail);
                        break;
                    }
                }
            }
        }
    }
}

/// Drain all pending requests with the given error detail.
fn drain_pending(
    pending: &mut HashMap<
        String,
        tokio::sync::oneshot::Sender<Result<McpResponse, TransportError>>,
    >,
    detail: String,
) {
    for (_, reply) in pending.drain() {
        let _ = reply.send(Err(TransportError::ProcessExited(detail.clone())));
    }
}

// ── SSE transport ──

/// Configuration for an SSE‑based MCP server (HTTP long‑poll).
#[derive(Clone)]
pub struct SseTransportConfig {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub connect_timeout_secs: u64,
    pub request_timeout_secs: u64,
}

/// Streamable HTTP transport — connects to a remote MCP server via HTTP POST.
///
/// Implements the MCP "Streamable HTTP" transport:
/// - Sends `Accept: application/json, text/event-stream` so servers that reply
///   with either a plain JSON body or an SSE stream are both accepted.
/// - Parses SSE-framed responses (`event:` / `data:` lines) as well as plain
///   JSON bodies.
/// - Captures the `Mcp-Session-Id` response header on initialize and echoes it
///   back on every subsequent request for session continuity.
/// - Treats `202 Accepted` / empty bodies (e.g. for notifications) as success.
pub struct SseTransport {
    config: SseTransportConfig,
    client: tokio::sync::Mutex<Option<reqwest::Client>>,
    session_id: tokio::sync::Mutex<Option<String>>,
}

impl SseTransport {
    pub fn new(config: SseTransportConfig) -> Self {
        Self {
            config,
            client: tokio::sync::Mutex::new(None),
            session_id: tokio::sync::Mutex::new(None),
        }
    }
}

/// Extract the first JSON-RPC response from an SSE-framed body.
///
/// SSE frames are separated by blank lines; `data:` lines within a frame are
/// concatenated. We parse each frame's data as JSON and return the first
/// message that looks like a response: it carries an `id` and is not a
/// request/notification (no `method`). This correctly handles responses whose
/// `result` is `null`.
fn parse_sse_response(body: &str) -> Option<McpResponse> {
    fn try_frame(data: &str) -> Option<McpResponse> {
        let value: serde_json::Value = serde_json::from_str(data).ok()?;
        let obj = value.as_object()?;
        // A response has an id and no method (methods denote requests/notifications).
        if obj.contains_key("method") || !obj.contains_key("id") {
            return None;
        }
        serde_json::from_value(value).ok()
    }

    let mut data_buf = String::new();
    for line in body.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            // End of a frame — try to parse accumulated data.
            if !data_buf.is_empty() {
                if let Some(resp) = try_frame(&data_buf) {
                    return Some(resp);
                }
                data_buf.clear();
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            if !data_buf.is_empty() {
                data_buf.push('\n');
            }
            data_buf.push_str(rest.trim_start());
        }
        // Ignore `event:`, `id:`, `retry:`, comment (`:`) lines.
    }
    // Trailing frame without a terminating blank line.
    if !data_buf.is_empty() {
        return try_frame(&data_buf);
    }
    None
}

#[async_trait]
impl McpTransport for SseTransport {
    async fn start(&self) -> Result<(), TransportError> {
        let mut headers = reqwest::header::HeaderMap::new();
        for (k, v) in &self.config.headers {
            headers.insert(
                reqwest::header::HeaderName::from_bytes(k.as_bytes())
                    .map_err(|e| TransportError::Protocol(format!("invalid header name '{k}': {e}")))?,
                reqwest::header::HeaderValue::from_str(v)
                    .map_err(|e| TransportError::Protocol(format!("invalid header value '{v}': {e}")))?,
            );
        }
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        );
        // Streamable HTTP requires accepting both JSON and SSE.
        headers.insert(
            reqwest::header::ACCEPT,
            reqwest::header::HeaderValue::from_static("application/json, text/event-stream"),
        );

        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(self.config.connect_timeout_secs))
            .default_headers(headers)
            .build()
            .map_err(|e| TransportError::Protocol(format!("failed to build http client: {e}")))?;

        // No ping handshake — connectivity is verified by the `initialize`
        // request that the client issues immediately after start().
        *self.client.lock().await = Some(client);
        Ok(())
    }

    async fn send(&self, request: &McpRequest) -> Result<McpResponse, TransportError> {
        let json = serde_json::to_string(request).map_err(crate::json_rpc::CodecError::from)?;

        let client = {
            let guard = self.client.lock().await;
            guard.as_ref().ok_or(TransportError::NotStarted)?.clone()
        };

        let mut req = client.post(&self.config.url).body(json);
        // Echo the session id captured from a prior response, if any.
        if let Some(sid) = self.session_id.lock().await.as_ref() {
            req = req.header("Mcp-Session-Id", sid);
        }

        let resp = tokio::time::timeout(
            std::time::Duration::from_secs(self.config.request_timeout_secs),
            req.send(),
        )
        .await
        .map_err(|_| TransportError::Timeout("request timed out".into()))?
        .map_err(|e| TransportError::Protocol(format!("POST failed: {e}")))?;

        let status = resp.status();

        // Capture / refresh the MCP session id.
        if let Some(sid) = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
        {
            *self.session_id.lock().await = Some(sid);
        }

        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(TransportError::Protocol(format!(
                "POST returned {status}: {body_text}"
            )));
        }

        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        // 202 Accepted / 204 No Content are used for notifications — no body.
        if status == reqwest::StatusCode::ACCEPTED || status == reqwest::StatusCode::NO_CONTENT {
            return Ok(McpResponse {
                jsonrpc: "2.0".into(),
                id: request.id.clone(),
                result: Some(serde_json::Value::Null),
                error: None,
            });
        }

        let body = resp.text().await.map_err(|e| {
            TransportError::Protocol(format!("failed to read response body: {e}"))
        })?;

        if body.trim().is_empty() {
            // Empty body (e.g. accepted notification) — synthesize an ok response.
            return Ok(McpResponse {
                jsonrpc: "2.0".into(),
                id: request.id.clone(),
                result: Some(serde_json::Value::Null),
                error: None,
            });
        }

        if content_type.contains("text/event-stream") {
            parse_sse_response(&body).ok_or_else(|| {
                TransportError::Protocol(format!(
                    "no JSON-RPC response found in SSE stream: {body}"
                ))
            })
        } else {
            crate::json_rpc::deserialize_response(&body).map_err(TransportError::Codec)
        }
    }

    async fn shutdown(&self) -> Result<(), TransportError> {
        *self.client.lock().await = None;
        *self.session_id.lock().await = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sse_framed_response() {
        let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\n\n";
        let resp = parse_sse_response(body).expect("should parse SSE frame");
        assert_eq!(resp.id, serde_json::json!(1));
        assert_eq!(resp.result, Some(serde_json::json!({"ok": true})));
    }

    #[test]
    fn parses_multiline_data_frame() {
        // Two data: lines within one frame are concatenated with newlines.
        let body = "data: {\"jsonrpc\":\"2.0\",\"id\":2,\ndata: \"result\":{\"v\":1}}\n\n";
        let resp = parse_sse_response(body).expect("should parse multiline data");
        assert_eq!(resp.id, serde_json::json!(2));
    }

    #[test]
    fn skips_frames_without_response() {
        // First frame is a comment/heartbeat, second carries the response.
        let body = ": keep-alive\n\ndata: {\"jsonrpc\":\"2.0\",\"id\":3,\"result\":null}\n\n";
        let resp = parse_sse_response(body).expect("should skip to response frame");
        assert_eq!(resp.id, serde_json::json!(3));
    }

    #[test]
    fn returns_none_when_no_response() {
        let body = "event: ping\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notify\"}\n\n";
        assert!(parse_sse_response(body).is_none());
    }
}

