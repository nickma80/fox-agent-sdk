//! Transport layer — stdio subprocess and SSE HTTP transports.

use crate::types::{McpRequest, McpResponse};
use async_trait::async_trait;
use std::collections::HashMap;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

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

/// Configuration for a stdio‑based MCP server (local subprocess).
#[derive(Clone)]
pub struct StdioTransportConfig {
    pub command: String,
    pub args: Vec<String>,
    pub env: Option<HashMap<String, String>>,
    pub cwd: Option<String>,
    pub request_timeout_ms: u64,
}

/// Stdio transport — spawns a child process and communicates via stdin / stdout.
///
/// Internally uses a background task that reads lines from stdout and routes
/// them to the correct pending request via the `pending` map (keyed by request
/// id).
pub struct StdioTransport {
    config: StdioTransportConfig,
    /// Channel to send requests to the I/O task. Each message is
    /// `(request_json, response_tx)`.
    sender: tokio::sync::Mutex<
        Option<mpsc::Sender<(String, tokio::sync::oneshot::Sender<Result<McpResponse, TransportError>>)>>,
    >,
    /// Handle to the child process (kept alive until shutdown).
    child: tokio::sync::Mutex<Option<Child>>,
}

impl StdioTransport {
    pub fn new(config: StdioTransportConfig) -> Self {
        Self {
            config,
            sender: tokio::sync::Mutex::new(None),
            child: tokio::sync::Mutex::new(None),
        }
    }
}

#[async_trait]
impl McpTransport for StdioTransport {
    async fn start(&self) -> Result<(), TransportError> {
        let mut cmd = Command::new(&self.config.command);
        cmd.args(&self.config.args);
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

        let mut child = cmd.spawn()?;
        let stdout = child.stdout.take().ok_or_else(|| {
            TransportError::Protocol("child process has no stdout".into())
        })?;
        let stdin = child.stdin.take().ok_or_else(|| {
            TransportError::Protocol("child process has no stdin".into())
        })?;

        *self.child.lock().await = Some(child);

        // Spawn reader + writer task.
        let (tx, mut rx) = mpsc::channel::<(
            String,
            tokio::sync::oneshot::Sender<Result<McpResponse, TransportError>>,
        )>(32);
        *self.sender.lock().await = Some(tx);

        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut writer = stdin;
            let mut pending: HashMap<String, tokio::sync::oneshot::Sender<Result<McpResponse, TransportError>>> = HashMap::new();
            let mut line_buf = String::new();

            loop {
                tokio::select! {
                    // --- Incoming request to send ---
                    msg = rx.recv() => {
                        match msg {
                            Some((json, reply)) => {
                                // Extract id from the JSON for routing
                                let req_id: String = if let Ok(v) = serde_json::from_str::<serde_json::Value>(&json) {
                                    v["id"].as_str().unwrap_or("").to_string()
                                } else { String::new() };

                                if let Err(e) = writer.write_all(json.as_bytes()).await {
                                    let _ = reply.send(Err(TransportError::Io(e)));
                                    continue;
                                }
                                if let Err(e) = writer.flush().await {
                                    let _ = reply.send(Err(TransportError::Io(e)));
                                    continue;
                                }
                                if !req_id.is_empty() {
                                    pending.insert(req_id, reply);
                                }
                            }
                            None => break, // channel closed
                        }
                    }

                    // --- Incoming response line ---
                    line_res = reader.read_line(&mut line_buf) => {
                        match line_res {
                            Ok(0) => {
                                // EOF — process ended
                                break;
                            }
                            Ok(_) => {
                                let trimmed = line_buf.trim().to_string();
                                line_buf.clear();

                                // Parse and route
                                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&trimmed) {
                                    if let Some(id) = val.get("id").and_then(|v| v.as_str()) {
                                        if let Some(reply) = pending.remove(id) {
                                            match serde_json::from_value::<McpResponse>(val) {
                                                Ok(resp) => { let _ = reply.send(Ok(resp)); }
                                                Err(e) => {
                                                    let _ = reply.send(Err(TransportError::Codec(
                                                        crate::json_rpc::CodecError::Parse(e)
                                                    )));
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                // Drain pending with error
                                for (_, reply) in pending.drain() {
                                    let _ = reply.send(Err(TransportError::Io(
                                        std::io::Error::new(std::io::ErrorKind::BrokenPipe, format!("read error: {e}"))
                                    )));
                                }
                                break;
                            }
                        }
                    }
                }
            }

            // Drain remaining pending
            for (_, reply) in pending.drain() {
                let _ = reply.send(Err(TransportError::ProcessExited("process ended".into())));
            }
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
