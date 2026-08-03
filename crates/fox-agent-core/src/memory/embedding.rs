use crate::config::MemoryConfig;
use anyhow::{Context, Result};
use futures::StreamExt;
use mistralrs::{EmbeddingModelBuilder, EmbeddingRequest, EmbeddingRequestBuilder};
use reqwest::Client;
use serde::Deserialize;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::Mutex;
use std::sync::{Arc, OnceLock, mpsc};
use std::thread;
use std::time::Duration;
use tracing::{info, warn};

const DEFAULT_HF_ENDPOINT: &str = "https://hf-mirror.com/";

/// How long to wait for a single `generate_embeddings()` call before assuming
/// the mistralrs engine is hung and reloading the model.
///
/// CPU inference can be slow for embedding models; 600 s accommodates typical
/// CPU-bound runs without false-positive timeouts.
const EMBED_GEN_TIMEOUT: Duration = Duration::from_secs(600);

pub trait EmbeddingProvider: Send + Sync {
    fn model_name(&self) -> &str;
    fn version(&self) -> &str;
    fn dimension(&self) -> Option<usize>;
    fn embed_batch(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>, String>;

    fn embed_text(&self, input: &str) -> Result<Vec<f32>, String> {
        let mut outputs = self.embed_batch(&[input.to_string()])?;
        outputs
            .pop()
            .ok_or_else(|| "embedding provider returned no vectors".to_string())
    }
}

/// Timeout for the caller side — must exceed [`EMBED_GEN_TIMEOUT`] because
/// the worker may reload the model on the first failure and retry.
const CALLER_TIMEOUT: Duration = Duration::from_secs(900);

#[derive(Debug, Clone)]
pub struct EmbeddingModelDescriptor {
    pub model_id: String,
    pub local_model_dir: Option<PathBuf>,
    pub endpoint: String,
    pub token: Option<String>,
    pub cache_dir: PathBuf,
}

impl EmbeddingModelDescriptor {
    pub fn from_config(config: &MemoryConfig) -> Self {
        use crate::memory::storage::default_storage_dir;
        let storage_root = default_storage_dir();
        let cache_dir = config
            .embedding_cache_dir
            .clone()
            .unwrap_or_else(|| storage_root.join("models"));
        Self {
            model_id: config.embedding_model_id.clone(),
            local_model_dir: config.embedding_model_path.clone(),
            endpoint: config
                .embedding_hf_endpoint
                .clone()
                .unwrap_or_else(|| DEFAULT_HF_ENDPOINT.to_string()),
            token: config.embedding_hf_token.clone(),
            cache_dir,
        }
    }

    fn resolved_model_dir(&self) -> PathBuf {
        if let Some(local) = &self.local_model_dir {
            return local.clone();
        }
        self.cache_dir.join(sanitize_repo_id(&self.model_id))
    }
}

#[derive(Clone)]
pub struct MistralEmbeddingProvider {
    model_name: String,
    version: String,
    dimension: Arc<OnceLock<usize>>,
    tx: mpsc::Sender<WorkerRequest>,
}

enum WorkerRequest {
    Embed {
        inputs: Vec<String>,
        response_tx: mpsc::Sender<Result<Vec<Vec<f32>>, String>>,
    },
}

impl MistralEmbeddingProvider {
    pub fn from_config(config: &MemoryConfig) -> Result<Option<Self>, String> {
        if !config.enabled {
            return Ok(None);
        }
        if config.embedding_model_path.is_none() && !config.auto_download_embedding_model {
            return Ok(None);
        }
        let descriptor = EmbeddingModelDescriptor::from_config(config);
        let dimension = Arc::new(OnceLock::new());
        let (tx, rx) = mpsc::channel();
        let dimension_for_thread = Arc::clone(&dimension);
        let model_name = descriptor.model_id.clone();
        let version = derive_embedding_version(&descriptor);
        thread::Builder::new()
            .name("fox-memory-embed".to_string())
            .spawn(move || worker_loop(descriptor, rx, dimension_for_thread))
            .map_err(|e| format!("failed to spawn embedding worker: {e}"))?;
        Ok(Some(Self {
            model_name,
            version,
            dimension,
            tx,
        }))
    }
}

impl EmbeddingProvider for MistralEmbeddingProvider {
    fn model_name(&self) -> &str {
        &self.model_name
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn dimension(&self) -> Option<usize> {
        self.dimension.get().copied()
    }

    fn embed_batch(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>, String> {
        if inputs.is_empty() {
            return Ok(Vec::new());
        }
        let (response_tx, response_rx) = mpsc::channel();
        self.tx
            .send(WorkerRequest::Embed {
                inputs: inputs.to_vec(),
                response_tx,
            })
            .map_err(|e| format!("failed to send embedding request: {e}"))?;
        response_rx
            .recv_timeout(CALLER_TIMEOUT)
            .map_err(|e| format!("embedding request timed out ({CALLER_TIMEOUT:?}): {e}"))?
    }
}

// ── Worker thread ──────────────────────────────────────────────────────────

fn worker_loop(
    descriptor: EmbeddingModelDescriptor,
    rx: mpsc::Receiver<WorkerRequest>,
    dimension: Arc<OnceLock<usize>>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(err) => {
            let message = format!("failed to initialize embedding runtime: {err}");
            while let Ok(WorkerRequest::Embed { response_tx, .. }) = rx.recv() {
                let _ = response_tx.send(Err(message.clone()));
            }
            return;
        }
    };

    // Load the initial model.
    let mut model = match load_embedding_model(&runtime, &descriptor) {
        Ok(m) => m,
        Err(err) => {
            while let Ok(WorkerRequest::Embed { response_tx, .. }) = rx.recv() {
                let _ = response_tx.send(Err(err.clone()));
            }
            return;
        }
    };

    while let Ok(request) = rx.recv() {
        match request {
            WorkerRequest::Embed {
                inputs,
                response_tx,
            } => match process_embed_request(&runtime, &descriptor, &mut model, &inputs) {
                Ok(vectors) => {
                    if let Some(first) = vectors.first() {
                        let _ = dimension.set(first.len());
                    }
                    if response_tx.send(Ok(vectors)).is_err() {
                        warn!("embedding result dropped: receiver timed out");
                    }
                }
                Err(e) => {
                    if response_tx.send(Err(e)).is_err() {
                        warn!("embedding error dropped: receiver timed out");
                    }
                }
            },
        }
    }
}

/// Process a single embedding request, with timeout and model-reload on failure.
///
/// `mistralrs` engines can crash; `generate_embeddings()` may hang indefinitely
/// on a dead engine instead of returning an error.  We guard against this with
/// a per-call timeout, and reload the model from scratch on any failure.
fn process_embed_request(
    runtime: &tokio::runtime::Runtime,
    descriptor: &EmbeddingModelDescriptor,
    model: &mut mistralrs::Model,
    inputs: &[String],
) -> Result<Vec<Vec<f32>>, String> {
    let request = build_request(inputs);

    // 1) First attempt with timeout
    let result = runtime.block_on(async {
        tokio::time::timeout(EMBED_GEN_TIMEOUT, model.generate_embeddings(request))
            .await
            .map_err(|_elapsed| {
                anyhow::anyhow!("embedding model inference timed out after {EMBED_GEN_TIMEOUT:?}")
            })?
            .map_err(|e| anyhow::anyhow!(e.to_string()))
    });

    match result {
        Ok(vectors) => return Ok(vectors),
        Err(e) => {
            warn!(
                error = %e,
                "embedding generation failed — reloading model"
            );
        }
    }

    // 2) Reload model
    *model = load_embedding_model(runtime, descriptor)?;

    // 3) Retry
    let retry_request = build_request(inputs);
    let result = runtime.block_on(async {
        tokio::time::timeout(EMBED_GEN_TIMEOUT, model.generate_embeddings(retry_request))
            .await
            .map_err(|_elapsed| {
                anyhow::anyhow!("embedding model inference timed out after {EMBED_GEN_TIMEOUT:?}")
            })?
            .map_err(|e| anyhow::anyhow!(e.to_string()))
    });

    match result {
        Ok(vectors) => Ok(vectors),
        Err(e) => Err(format!("embedding failed after model reload: {e}")),
    }
}

fn load_embedding_model(
    runtime: &tokio::runtime::Runtime,
    descriptor: &EmbeddingModelDescriptor,
) -> Result<mistralrs::Model, String> {
    runtime.block_on(async {
        let model_dir = prepare_model_dir(descriptor)
            .await
            .map_err(|e| e.to_string())?;
        tracing::info!(
            path = %model_dir.display(),
            model = %descriptor.model_id,
            "Loading embedding model"
        );
        EmbeddingModelBuilder::new(model_dir.to_string_lossy().to_string())
            .build()
            .await
            .map_err(|e| format!("failed to build embedding model: {e}"))
    })
}

// ── HTTP helpers ────────────────────────────────────────────────────────────

fn build_request(inputs: &[String]) -> EmbeddingRequestBuilder {
    let mut builder = EmbeddingRequest::builder();
    for input in inputs {
        builder = builder.add_prompt(input.clone());
    }
    builder.with_truncate_sequence(true)
}

async fn prepare_model_dir(descriptor: &EmbeddingModelDescriptor) -> Result<PathBuf> {
    let model_dir = descriptor.resolved_model_dir();
    if let Some(local) = &descriptor.local_model_dir {
        // User specified a local model directory — create it if missing
        // and let download_repo_snapshot populate it.
        if !local.exists() {
            info!("creating embedding model directory: {}", local.display());
            fs::create_dir_all(local)
                .with_context(|| format!("failed to create model directory {}", local.display()))?;
        }
        // If the directory exists but has no snapshot yet, download the model.
        if !has_model_snapshot(local) {
            info!(
                model_id = %descriptor.model_id,
                dir = %local.display(),
                "downloading embedding model to configured directory"
            );
            download_repo_snapshot(descriptor, local).await?;
        }
        return Ok(local.clone());
    }
    if has_model_snapshot(&model_dir) {
        return Ok(model_dir);
    }
    download_repo_snapshot(descriptor, &model_dir).await?;
    Ok(model_dir)
}

fn has_model_snapshot(model_dir: &Path) -> bool {
    model_dir.join("config.json").exists()
}

async fn download_repo_snapshot(
    descriptor: &EmbeddingModelDescriptor,
    model_dir: &Path,
) -> Result<()> {
    fs::create_dir_all(model_dir)
        .with_context(|| format!("failed to create model cache dir {}", model_dir.display()))?;
    let client = Client::builder()
        .build()
        .context("failed to create reqwest client")?;
    let files = list_repo_files(&client, descriptor).await?;
    if files.is_empty() {
        anyhow::bail!("model repo {} returned no files", descriptor.model_id);
    }
    for file in files {
        download_repo_file(&client, descriptor, model_dir, &file).await?;
    }
    Ok(())
}

async fn list_repo_files(
    client: &Client,
    descriptor: &EmbeddingModelDescriptor,
) -> Result<Vec<String>> {
    let url = format!(
        "{}/api/models/{}",
        descriptor.endpoint.trim_end_matches('/'),
        descriptor.model_id
    );
    let mut req = client.get(url);
    if let Some(token) = &descriptor.token {
        req = req.bearer_auth(token);
    }
    let response = req.send().await.context("failed to query model metadata")?;
    let response = response
        .error_for_status()
        .context("model metadata request returned error")?;
    let payload: HfModelApiResponse = response
        .json()
        .await
        .context("failed to decode model metadata response")?;
    Ok(payload
        .siblings
        .into_iter()
        .filter_map(|file| file.rfilename)
        .filter(|name| !name.ends_with(".md") && !name.ends_with(".png"))
        .collect())
}

async fn download_repo_file(
    client: &Client,
    descriptor: &EmbeddingModelDescriptor,
    model_dir: &Path,
    file: &str,
) -> Result<()> {
    let target_path = model_dir.join(file);
    if target_path.exists() {
        return Ok(());
    }
    if let Some(parent) = target_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let url = format!(
        "{}/{}/resolve/main/{}",
        descriptor.endpoint.trim_end_matches('/'),
        descriptor.model_id,
        file
    );
    info!(model = %descriptor.model_id, file = %file, "Downloading embedding model file");
    let mut req = client.get(url);
    if let Some(token) = &descriptor.token {
        req = req.bearer_auth(token);
    }
    let response = req
        .send()
        .await
        .with_context(|| format!("failed to download {file}"))?
        .error_for_status()
        .with_context(|| format!("download returned error for {file}"))?;
    let mut file_out = tokio::fs::File::create(&target_path)
        .await
        .with_context(|| format!("failed to create {}", target_path.display()))?;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.with_context(|| format!("failed to read download chunk for {file}"))?;
        tokio::io::AsyncWriteExt::write_all(&mut file_out, &chunk)
            .await
            .with_context(|| format!("failed to write chunk to {}", target_path.display()))?;
    }
    tokio::io::AsyncWriteExt::flush(&mut file_out)
        .await
        .with_context(|| format!("failed to flush {}", target_path.display()))?;
    Ok(())
}

fn sanitize_repo_id(repo_id: &str) -> String {
    repo_id.replace('/', "--")
}

fn derive_embedding_version(descriptor: &EmbeddingModelDescriptor) -> String {
    let model_key = sanitize_repo_id(&descriptor.model_id);
    let model_dir = descriptor.resolved_model_dir();
    if model_dir.exists()
        && let Ok(fingerprint) = fingerprint_model_dir(&model_dir)
    {
        return format!("{model_key}@{fingerprint}");
    }
    if let Some(local_dir) = &descriptor.local_model_dir {
        return format!(
            "local:{model_key}@{}",
            stable_hash(&local_dir.to_string_lossy())
        );
    }
    format!(
        "remote:{model_key}@{}",
        stable_hash(descriptor.endpoint.trim_end_matches('/'))
    )
}

fn fingerprint_model_dir(model_dir: &Path) -> Result<String> {
    let mut entries = Vec::new();
    collect_model_file_metadata(model_dir, model_dir, &mut entries)?;
    entries.sort();

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for (relative_path, file_len, modified_at) in entries {
        relative_path.hash(&mut hasher);
        file_len.hash(&mut hasher);
        modified_at.hash(&mut hasher);
    }
    Ok(format!("{:016x}", hasher.finish()))
}

fn collect_model_file_metadata(
    root: &Path,
    current: &Path,
    entries: &mut Vec<(String, u64, u64)>,
) -> Result<()> {
    for entry in fs::read_dir(current)
        .with_context(|| format!("failed to read model dir {}", current.display()))?
    {
        let entry = entry.with_context(|| format!("failed to inspect {}", current.display()))?;
        let path = entry.path();
        let metadata = entry
            .metadata()
            .with_context(|| format!("failed to read metadata for {}", path.display()))?;
        if metadata.is_dir() {
            collect_model_file_metadata(root, &path, entries)?;
            continue;
        }

        let relative_path = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let modified_at = metadata
            .modified()
            .ok()
            .and_then(|ts| ts.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        entries.push((relative_path, metadata.len(), modified_at));
    }
    Ok(())
}

fn stable_hash(value: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[derive(Debug, Deserialize)]
struct HfModelApiResponse {
    #[serde(default)]
    siblings: Vec<HfRepoFile>,
}

#[derive(Debug, Deserialize)]
struct HfRepoFile {
    #[serde(default)]
    rfilename: Option<String>,
}

pub fn create_embedding_provider(config: &MemoryConfig) -> Option<Arc<dyn EmbeddingProvider>> {
    match MistralEmbeddingProvider::from_config(config) {
        Ok(provider) => provider.map(|provider| Arc::new(provider) as Arc<dyn EmbeddingProvider>),
        Err(err) => {
            warn!(error = %err, "Failed to initialize embedding provider; semantic recall disabled");
            None
        }
    }
}

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct FixedEmbeddingProvider {
    model_name: String,
    version: String,
    dimension: Arc<Mutex<Option<usize>>>,
    values: Arc<dyn Fn(&[String]) -> Vec<Vec<f32>> + Send + Sync>,
}

#[cfg(test)]
impl FixedEmbeddingProvider {
    pub(crate) fn new(
        model_name: &str,
        values: impl Fn(&[String]) -> Vec<Vec<f32>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            model_name: model_name.to_string(),
            version: "test".to_string(),
            dimension: Arc::new(Mutex::new(None)),
            values: Arc::new(values),
        }
    }
}

#[cfg(test)]
impl EmbeddingProvider for FixedEmbeddingProvider {
    fn model_name(&self) -> &str {
        &self.model_name
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn dimension(&self) -> Option<usize> {
        self.dimension.lock().ok().and_then(|guard| *guard)
    }

    fn embed_batch(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>, String> {
        let vectors = (self.values)(inputs);
        if let Some(first) = vectors.first()
            && let Ok(mut guard) = self.dimension.lock()
        {
            *guard = Some(first.len());
        }
        Ok(vectors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_embedding_version_uses_configured_model_identity() {
        let temp = std::env::temp_dir().join(format!("fox-embed-version-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp).unwrap();
        std::fs::write(temp.join("config.json"), "{}").unwrap();

        let mut cfg = MemoryConfig::default();
        cfg.embedding_model_id = "BAAI/bge-small-en-v1.5".to_string();
        cfg.embedding_model_path = Some(temp.clone());

        let descriptor = EmbeddingModelDescriptor::from_config(&cfg);
        let version = derive_embedding_version(&descriptor);

        assert!(version.contains("BAAI--bge-small-en-v1.5"));
        assert!(!version.contains("qwen3-embedding-0.6b"));

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn derive_embedding_version_changes_when_local_snapshot_changes() {
        let temp =
            std::env::temp_dir().join(format!("fox-embed-fingerprint-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp).unwrap();
        std::fs::write(temp.join("config.json"), "{\"name\":\"v1\"}").unwrap();

        let mut cfg = MemoryConfig::default();
        cfg.embedding_model_id = "Qwen/Qwen3-Embedding-0.6B".to_string();
        cfg.embedding_model_path = Some(temp.clone());

        let descriptor = EmbeddingModelDescriptor::from_config(&cfg);
        let version_before = derive_embedding_version(&descriptor);

        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(temp.join("config.json"), "{\"name\":\"v2-updated\"}").unwrap();

        let version_after = derive_embedding_version(&descriptor);
        assert_ne!(version_before, version_after);

        let _ = std::fs::remove_dir_all(temp);
    }
}
