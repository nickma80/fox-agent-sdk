/// SDK top-level configuration.
#[derive(Debug, Clone)]
pub struct FoxAgentSdkConfig {
    /// Memory system configuration
    pub memory: MemoryConfig,
    /// Context window compaction configuration
    pub compaction: CompactionConfig,
    /// Tool permission safety configuration
    pub safety: SafetyConfig,
    /// Optional root directory for persisted session snapshots.
    pub session_storage_dir: Option<std::path::PathBuf>,
    /// Optional root directory for persisted planning snapshots.
    pub planning_storage_dir: Option<std::path::PathBuf>,
    /// Whether to persist a fresh session snapshot at key lifecycle points.
    pub auto_snapshot: bool,
}

impl Default for FoxAgentSdkConfig {
    fn default() -> Self {
        Self {
            memory: MemoryConfig::default(),
            compaction: CompactionConfig::default(),
            safety: SafetyConfig::default(),
            session_storage_dir: None,
            planning_storage_dir: None,
            auto_snapshot: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoExtractScope {
    Project,
    Global,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContradictionPolicy {
    Ignore,
    Supersede,
    DowngradeConfidence,
    MarkContradictionEdge,
}

/// Memory retrieval and injection configuration.
#[derive(Debug, Clone)]
pub struct MemoryConfig {
    /// Whether memory retrieval is enabled
    pub enabled: bool,
    /// Whether semantic embedding generation and recall are enabled.
    pub embedding_enabled: bool,
    /// Memory storage root directory. None = default (~/.fox-agent/memory/)
    pub storage_dir: Option<std::path::PathBuf>,
    /// Local directory containing a pre-downloaded embedding model.
    /// If not provided, the SDK will use `embedding_model_id` and may download
    /// it from Hugging Face or the configured mirror.
    pub embedding_model_path: Option<std::path::PathBuf>,
    /// Hugging Face repo id for the embedding model.
    pub embedding_model_id: String,
    /// Optional custom Hugging Face endpoint. Supports mirrors such as
    /// `https://hf-mirror.com/`.
    pub embedding_hf_endpoint: Option<String>,
    /// Optional Hugging Face token for gated or rate-limited downloads.
    pub embedding_hf_token: Option<String>,
    /// Optional local cache directory for downloaded embedding models.
    pub embedding_cache_dir: Option<std::path::PathBuf>,
    /// Whether the SDK should attempt to download the model when
    /// `embedding_model_path` is not provided.
    pub auto_download_embedding_model: bool,
    /// Whether to enable local ANN indexing for semantic recall.
    /// When enabled, the SDK builds a local HNSW index file next to the graph
    /// (e.g. `global.ann.bin`) and uses it to narrow candidates before exact scoring.
    pub ann_enabled: bool,
    /// Minimum number of embedded memories required before using ANN.
    pub ann_min_vectors: usize,
    /// Candidate multiplier for ANN search. Actual retrieved candidates are
    /// `limit * ann_candidate_multiplier` (bounded by available vectors).
    pub ann_candidate_multiplier: usize,
    /// Maximum candidate memories retrieved per query (before scoring)
    pub max_candidates: usize,
    /// Maximum results returned after scoring and ranking
    pub max_results: usize,
    /// Maximum characters allowed in the injected memory prompt.
    pub injection_max_chars: usize,
    /// Maximum number of memories injected per category.
    pub injection_max_per_category: usize,
    /// Maximum BFS depth when expanding from initial hits in the memory graph
    pub max_graph_depth: usize,
    /// Enable LLM relevance verification using the agent's provider/model
    pub verify_relevance: bool,
    /// Model ID to use for relevance verification. None = use agent's default model
    pub verify_model: Option<String>,
    /// Enable automatic memory extraction from conversation transcripts
    pub auto_extract: bool,
    /// Scope to store auto-extracted memories into.
    pub auto_extract_scope: AutoExtractScope,
    /// How many recent messages should be used to build the ingestion transcript.
    pub auto_extract_message_window: usize,
    /// Max number of extracted memories to process per turn.
    pub auto_extract_max_items_per_turn: usize,
    /// Similarity threshold for duplicate detection.
    pub dedupe_similarity_threshold: f32,
    /// Similarity threshold used when grouping memories into clusters.
    pub cluster_similarity_threshold: f32,
    /// Minimum number of members required to keep a generated cluster.
    pub cluster_min_members: usize,
    /// Policy to apply when contradiction is detected.
    pub contradiction_policy: ContradictionPolicy,
    /// Confidence decay applied when contradiction policy is downgrade.
    pub contradiction_confidence_decay: f32,
    /// Optional retention window for memories based on `updated_at`.
    pub retention_days: Option<u64>,
    /// Optional per-scope maximum number of memories retained on disk.
    pub memory_size_limit: Option<usize>,
    /// Automatically rebuild embeddings when the configured model/version changes.
    pub rebuild_on_model_change: bool,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            embedding_enabled: true,
            storage_dir: None,
            embedding_model_path: None,
            embedding_model_id: "Qwen/Qwen3-Embedding-0.6B".to_string(),
            embedding_hf_endpoint: None,
            embedding_hf_token: None,
            embedding_cache_dir: None,
            auto_download_embedding_model: false,
            ann_enabled: false,
            ann_min_vectors: 256,
            ann_candidate_multiplier: 8,
            max_candidates: 30,
            max_results: 10,
            injection_max_chars: 1_500,
            injection_max_per_category: 3,
            max_graph_depth: 2,
            verify_relevance: false,
            verify_model: None,
            auto_extract: false,
            auto_extract_scope: AutoExtractScope::Project,
            auto_extract_message_window: 6,
            auto_extract_max_items_per_turn: 4,
            dedupe_similarity_threshold: 0.92,
            cluster_similarity_threshold: 0.9,
            cluster_min_members: 2,
            contradiction_policy: ContradictionPolicy::MarkContradictionEdge,
            contradiction_confidence_decay: 0.2,
            retention_days: None,
            memory_size_limit: None,
            rebuild_on_model_change: false,
        }
    }
}

/// Context compaction configuration.
#[derive(Debug, Clone)]
pub struct CompactionConfig {
    /// Whether automatic compaction is enabled
    pub enabled: bool,
    /// Approximate character budget before compaction is triggered
    pub token_budget: usize,
    /// Number of most recent messages to preserve during compaction
    pub preserve_recent_messages: usize,
    /// Maximum turn count before compaction is triggered (fallback)
    pub max_turns_before_compaction: usize,
    /// Character threshold for `ContextLimitApproaching` trigger
    /// (fraction of token_budget; e.g. 0.85 = 85% of budget).
    /// When the current context exceeds this, the agent may preemptively compact.
    pub context_limit_threshold: f64,
    /// Maximum number of compaction operations allowed
    /// (prevents infinite compaction loops).
    pub max_compaction_count: u32,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            token_budget: 12_000,
            preserve_recent_messages: 6,
            max_turns_before_compaction: 12,
            context_limit_threshold: 0.85,
            max_compaction_count: 10,
        }
    }
}

/// Default policy for tools not in allowlist or denylist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultSafetyPolicy {
    /// Require user confirmation
    Confirm,
    /// Auto-allow without asking user
    Allow,
    /// Auto-deny without asking user
    Deny,
}

/// Safety/permission configuration with allowlist/denylist support.
#[derive(Debug, Clone)]
pub struct SafetyConfig {
    /// Tool allowlist. If set, only tools in this list are automatically allowed.
    /// None means allowlist mode is disabled.
    pub tool_allowlist: Option<Vec<String>>,

    /// Tool denylist. Tools in this list always require user confirmation,
    /// regardless of other settings.
    pub tool_denylist: Option<Vec<String>>,

    /// Default policy for tools not covered by allowlist or denylist.
    pub default_policy: DefaultSafetyPolicy,
}

impl Default for SafetyConfig {
    fn default() -> Self {
        Self {
            tool_allowlist: None,
            tool_denylist: None,
            default_policy: DefaultSafetyPolicy::Allow,
        }
    }
}

/// File system operation type for sandbox validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxOperation {
    /// Read operation (e.g. read, glob, grep, ls)
    Read,
    /// Write operation (e.g. write, edit)
    Write,
    /// Execute operation (e.g. bash)
    Execute,
}

impl std::fmt::Display for SandboxOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SandboxOperation::Read => write!(f, "read"),
            SandboxOperation::Write => write!(f, "write"),
            SandboxOperation::Execute => write!(f, "execute"),
        }
    }
}

/// Sandbox error returned when a path access violates sandbox constraints.
#[derive(Debug, Clone, thiserror::Error)]
pub enum SandboxError {
    #[error("access denied: cannot {operation} path `{path}` outside sandbox root `{root}`")]
    AccessDenied {
        path: std::path::PathBuf,
        operation: SandboxOperation,
        root: std::path::PathBuf,
    },
    #[error("path resolution error: {message}")]
    PathResolution {
        message: String,
    },
}

/// Workspace sandbox that constrains tool file system access to a root directory.
///
/// When configured on `ToolExecutor`, all file path operations from tools
/// are validated against this sandbox before execution.
#[derive(Debug, Clone)]
pub struct WorkspaceSandbox {
    /// Allowed root directory. All file path operations are limited to this directory
    /// and its subdirectories.
    pub root_dir: std::path::PathBuf,

    /// Whether reading files outside root_dir is allowed (default false)
    pub allow_read_outside: bool,

    /// Whether writing files outside root_dir is allowed (default false)
    pub allow_write_outside: bool,

    /// Whether executing commands outside root_dir is allowed (default false)
    pub allow_exec_outside: bool,
}

impl WorkspaceSandbox {
    /// Create a new sandbox with the given root directory.
    pub fn new(root_dir: impl Into<std::path::PathBuf>) -> Self {
        Self {
            root_dir: root_dir.into(),
            allow_read_outside: false,
            allow_write_outside: false,
            allow_exec_outside: false,
        }
    }

    /// Set whether reading outside the sandbox is allowed.
    pub fn with_read_outside(mut self, allow: bool) -> Self {
        self.allow_read_outside = allow;
        self
    }

    /// Set whether writing outside the sandbox is allowed.
    pub fn with_write_outside(mut self, allow: bool) -> Self {
        self.allow_write_outside = allow;
        self
    }

    /// Set whether executing outside the sandbox is allowed.
    pub fn with_exec_outside(mut self, allow: bool) -> Self {
        self.allow_exec_outside = allow;
        self
    }

    /// Validate a resolved path against this sandbox.
    ///
    /// If the path is within `root_dir`, it is always allowed.
    /// Otherwise, the outcome depends on the operation type and the
    /// corresponding `allow_*_outside` flag.
    pub fn validate_path(
        &self,
        path: &std::path::Path,
        operation: SandboxOperation,
    ) -> Result<std::path::PathBuf, SandboxError> {
        // Canonicalize the path. If it doesn't exist yet (e.g. a new file to write),
        // resolve it relative to cwd or the sandbox root.
        let canonical = if path.exists() {
            path.canonicalize().map_err(|e| SandboxError::PathResolution {
                message: format!("failed to canonicalize `{}`: {e}", path.display()),
            })?
        } else {
            // For non-existent paths, use the parent's canonical path if it exists,
            // or just use the path as-is
            if let Some(parent) = path.parent() {
                if parent.exists() {
                    let parent_canonical = parent.canonicalize().map_err(|e| {
                        SandboxError::PathResolution {
                            message: format!(
                                "failed to canonicalize parent of `{}`: {e}",
                                path.display()
                            ),
                        }
                    })?;
                    parent_canonical.join(
                        path.file_name()
                            .unwrap_or_else(|| std::ffi::OsStr::new("")),
                    )
                } else {
                    path.to_path_buf()
                }
            } else {
                path.to_path_buf()
            }
        };

        if canonical.starts_with(&self.root_dir) {
            return Ok(canonical);
        }

        match operation {
            SandboxOperation::Read if self.allow_read_outside => Ok(canonical),
            SandboxOperation::Write if self.allow_write_outside => Ok(canonical),
            SandboxOperation::Execute if self.allow_exec_outside => Ok(canonical),
            _ => Err(SandboxError::AccessDenied {
                path: canonical,
                operation,
                root: self.root_dir.clone(),
            }),
        }
    }
}
