//! OTel GenAI export pipeline.
//!
//! The SDK exports observability data **only** via OTLP/HTTP following the
//! OTel GenAI semantic conventions ([semantic-conventions-genai]) plus
//! `fox_agent.*` extensions. No dedicated platform exporters (fox-smith /
//! Langfuse) are bundled — any OTel backend is a valid sink.
//!
//! The pipeline is config-gated (`[observability].enabled = false` by
//! default); when disabled it is never constructed and the agent incurs zero
//! overhead. Event timestamps are captured locally in nanoseconds on receipt,
//! so span timing does not depend on the envelope's second-granularity clock.
//!
//! [semantic-conventions-genai]: https://github.com/open-telemetry/semantic-conventions-genai

mod mapper;
mod otlp;
mod queue;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fox_agent_core::{AgentEvent, OtelGenAiConfig};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use mapper::SpanMapper;
pub use mapper::{EvaluationRecord, ExportIdentity, MapperConfig};
pub use otlp::{OtValue, OtlpSpan, OtlpSpanKind, OtlpStatusCode};

/// OTLP `scope.name` identifying this SDK in the exported traces.
pub const SDK_SCOPE_NAME: &str = "fox-agent-sdk";
/// OTLP `scope.version` — the compiled SDK version.
pub const SDK_SCOPE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Current wall-clock time in Unix nanoseconds.
pub fn now_unix_nano() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Batch OTel GenAI exporter.
///
/// Consumes an `AgentEvent` stream (typically via [`fanout`]) and exports
/// completed spans to `POST {endpoint}/v1/traces`. Failed exports are queued
/// on disk (`[observability].queue_dir`) and replayed on the next successful
/// flush.
pub struct OtelGenAiExporter {
    config: Arc<OtelGenAiConfig>,
    client: reqwest::Client,
    endpoint: String,
    mapper: Mutex<SpanMapper>,
    buffer: Mutex<Vec<OtlpSpan>>,
    queue_dir: Option<PathBuf>,
}

impl OtelGenAiExporter {
    /// Create an exporter from `[observability]` config and the agent's
    /// runtime identity. Errors when disabled or when `endpoint` is unset.
    pub fn new(config: OtelGenAiConfig, identity: ExportIdentity) -> Result<Arc<Self>, String> {
        if !config.enabled {
            return Err(
                "observability export is disabled (set [observability].enabled = true)".into(),
            );
        }
        let endpoint = match config.endpoint.as_deref() {
            Some(e) if !e.trim().is_empty() => normalize_endpoint(e),
            _ => {
                return Err(
                    "observability.endpoint must be set when observability.enabled = true".into(),
                );
            }
        };
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.export_timeout_secs.max(1)))
            .build()
            .map_err(|e| format!("failed to build otel http client: {e}"))?;
        let mapper = SpanMapper::new(
            identity,
            MapperConfig {
                agent_name: config.agent_name.clone(),
                agent_id: config.agent_id.clone(),
                capture_content: config.capture_content,
                max_content_chars: config.max_content_chars,
            },
        );
        Ok(Arc::new(Self {
            queue_dir: config.queue_dir.clone(),
            endpoint,
            client,
            config: Arc::new(config),
            mapper: Mutex::new(mapper),
            buffer: Mutex::new(Vec::new()),
        }))
    }

    /// Create an exporter from a full `FoxAgentSdkConfig` (reads the
    /// `[observability]` block) plus the agent's runtime identity.
    pub fn from_sdk_config(
        sdk_config: &fox_agent_core::FoxAgentSdkConfig,
        identity: ExportIdentity,
    ) -> Result<Arc<Self>, String> {
        Self::new(sdk_config.observability.clone(), identity)
    }

    /// Spawn the background export loop. The task drains `rx`, batches spans,
    /// and flushes on `batch_size` / `flush_interval_ms`. Returns the task
    /// handle; the final partial batch is flushed when the channel closes.
    pub fn spawn(self: Arc<Self>, mut rx: mpsc::Receiver<AgentEvent>) -> JoinHandle<()> {
        tokio::spawn(async move {
            let flush_interval = Duration::from_millis(self.config.flush_interval_ms.max(100));
            let mut interval = tokio::time::interval(flush_interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    ev = rx.recv() => match ev {
                        Some(ev) => {
                            self.record_event(&ev);
                            if self.buffered_len() >= self.config.batch_size.max(1) {
                                self.flush_batch().await;
                            }
                        }
                        None => break,
                    },
                    _ = interval.tick() => {
                        self.flush_batch().await;
                    }
                }
            }
            self.flush_batch().await;
        })
    }

    /// Push an evaluation score; it becomes a `gen_ai.evaluation.result`
    /// event on the most relevant open span (turn span by default).
    pub fn push_evaluation(&self, record: EvaluationRecord) {
        let ts = now_unix_nano();
        if let Ok(mut mapper) = self.mapper.lock() {
            mapper.push_evaluation(record, ts);
        }
    }

    /// Record one event into the mapper and buffer any completed spans.
    fn record_event(&self, ev: &AgentEvent) {
        let ts = now_unix_nano();
        let spans = {
            let mut mapper = match self.mapper.lock() {
                Ok(m) => m,
                Err(poisoned) => poisoned.into_inner(),
            };
            mapper.on_event(ev, ts)
        };
        if !spans.is_empty()
            && let Ok(mut buffer) = self.buffer.lock()
        {
            buffer.extend(spans);
        }
    }

    fn buffered_len(&self) -> usize {
        self.buffer.lock().map(|b| b.len()).unwrap_or(0)
    }

    /// Drain the buffer and POST it. On failure the request body is queued to
    /// disk for later replay. Queued bodies are replayed first.
    pub async fn flush_batch(&self) {
        self.replay_queue().await;
        let spans = {
            let mut buffer = match self.buffer.lock() {
                Ok(b) => b,
                Err(poisoned) => poisoned.into_inner(),
            };
            if buffer.is_empty() {
                return;
            }
            std::mem::take(&mut *buffer)
        };
        let body = otlp::export_request(
            &self.config.service_name,
            SDK_SCOPE_NAME,
            SDK_SCOPE_VERSION,
            &spans,
        );
        if let Err(e) = self.post_body(&body).await {
            tracing::warn!(
                error = %e,
                queued = spans.len(),
                "otel export failed; request queued for retry"
            );
            if let Some(dir) = &self.queue_dir {
                queue::append_request(dir, &self.config.agent_name, &body);
            }
        }
    }

    /// Replay any queued request bodies; clears the queue on success.
    async fn replay_queue(&self) {
        let Some(dir) = &self.queue_dir else {
            return;
        };
        let bodies = queue::read_requests(dir);
        if bodies.is_empty() {
            return;
        }
        let mut ok = true;
        for body in &bodies {
            if let Err(e) = self.post_body(body).await {
                tracing::warn!(error = %e, "otel queue replay failed; keeping queue");
                ok = false;
                break;
            }
        }
        if ok {
            queue::clear(dir);
        }
    }

    async fn post_body(&self, body: &serde_json::Value) -> Result<(), String> {
        let mut req = self.client.post(&self.endpoint);
        for (key, value) in &self.config.headers {
            req = req.header(key, value);
        }
        let resp = req
            .json(body)
            .send()
            .await
            .map_err(|e| format!("otel endpoint request failed: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("otel endpoint returned {}", resp.status()));
        }
        Ok(())
    }
}

/// Fan a single `mpsc::Receiver<AgentEvent>` out to two independent
/// receivers (e.g. UI display + OTel export) without touching the agent's
/// emit path. The forward task stops when both subscribers are gone.
pub fn fanout(
    rx: mpsc::Receiver<AgentEvent>,
) -> (mpsc::Receiver<AgentEvent>, mpsc::Receiver<AgentEvent>) {
    let (a_tx, a_rx) = mpsc::channel(256);
    let (b_tx, b_rx) = mpsc::channel(256);
    tokio::spawn(async move {
        let mut rx = rx;
        while let Some(ev) = rx.recv().await {
            let a_ok = a_tx.send(ev.clone()).await.is_ok();
            let b_ok = b_tx.send(ev).await.is_ok();
            if !a_ok && !b_ok {
                break;
            }
        }
    });
    (a_rx, b_rx)
}

/// Normalize an endpoint: strip trailing slashes and append `/v1/traces`
/// unless the URL already targets it.
fn normalize_endpoint(raw: &str) -> String {
    let trimmed = raw.trim_end_matches('/');
    if trimmed.ends_with("/v1/traces") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/v1/traces")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_normalization() {
        assert_eq!(
            normalize_endpoint("http://127.0.0.1:4318"),
            "http://127.0.0.1:4318/v1/traces"
        );
        assert_eq!(
            normalize_endpoint("http://127.0.0.1:4318/"),
            "http://127.0.0.1:4318/v1/traces"
        );
        assert_eq!(
            normalize_endpoint("http://h/v1/traces"),
            "http://h/v1/traces"
        );
    }

    #[test]
    fn exporter_requires_endpoint_when_enabled() {
        let cfg = OtelGenAiConfig {
            enabled: true,
            endpoint: None,
            ..Default::default()
        };
        let err = OtelGenAiExporter::new(
            cfg,
            ExportIdentity {
                provider_name: "deepseek".into(),
                model_id: "deepseek-chat".into(),
                session_id: "sess".into(),
            },
        )
        .err()
        .expect("construction should fail without an endpoint");
        assert!(err.contains("endpoint"));
    }

    #[tokio::test]
    async fn disabled_config_never_constructs_exporter() {
        let cfg = OtelGenAiConfig::default(); // enabled = false
        assert!(
            OtelGenAiExporter::new(
                cfg,
                ExportIdentity {
                    provider_name: "deepseek".into(),
                    model_id: "deepseek-chat".into(),
                    session_id: "sess".into(),
                },
            )
            .is_err()
        );
    }
}
