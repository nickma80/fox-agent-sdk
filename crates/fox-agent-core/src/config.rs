/// SDK top-level configuration.
#[derive(Debug, Clone)]
pub struct FoxAgentSdkConfig {
    /// Memory system configuration
    pub memory: MemoryConfig,
    /// Context window compaction configuration
    pub compaction: CompactionConfig,
    /// Tool permission safety configuration
    pub safety: SafetyConfig,
}

impl Default for FoxAgentSdkConfig {
    fn default() -> Self {
        Self {
            memory: MemoryConfig::default(),
            compaction: CompactionConfig::default(),
            safety: SafetyConfig::default(),
        }
    }
}

/// Memory retrieval and injection configuration.
#[derive(Debug, Clone)]
pub struct MemoryConfig {
    /// Whether memory retrieval is enabled
    pub enabled: bool,
    /// Memory storage root directory. None = default (~/.fox-agent/memory/)
    pub storage_dir: Option<std::path::PathBuf>,
    /// Path to ONNX embedding model. None = embedding disabled (keyword-only search)
    pub embedding_model_path: Option<std::path::PathBuf>,
    /// Maximum candidate memories retrieved per query (before scoring)
    pub max_candidates: usize,
    /// Maximum results returned after scoring and ranking
    pub max_results: usize,
    /// Maximum BFS depth when expanding from initial hits in the memory graph
    pub max_graph_depth: usize,
    /// Enable LLM relevance verification using the agent's provider/model
    pub verify_relevance: bool,
    /// Model ID to use for relevance verification. None = use agent's default model
    pub verify_model: Option<String>,
    /// Enable automatic memory extraction from conversation transcripts
    pub auto_extract: bool,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            storage_dir: None,
            embedding_model_path: None,
            max_candidates: 30,
            max_results: 10,
            max_graph_depth: 2,
            verify_relevance: false,
            verify_model: None,
            auto_extract: false,
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
