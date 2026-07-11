/// SDK top-level configuration.
use serde::{Deserialize, Serialize};
use crate::SkillsConfig;

// ── Provider config ──

/// Authentication configuration for an LLM provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AuthConfig {
    /// No authentication
    None,
    /// HTTP Bearer token (Authorization: Bearer <token>)
    BearerToken(String),
    /// Custom API-key header (e.g. x-api-key: <value>)
    ApiKeyHeader { header_name: String, value: String },
}

/// Configuration for an LLM provider backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Short name identifying this provider (e.g. "openai", "deepseek")
    pub provider_name: String,
    /// Base URL for the provider's API (e.g. https://api.openai.com/v1)
    pub base_url: String,
    /// Authentication method
    pub auth: AuthConfig,
    /// Request timeout in seconds
    #[serde(default)]
    pub timeout_secs: u64,
    /// Additional HTTP headers sent with every request
    #[serde(default)]
    pub default_headers: Vec<(String, String)>,
    /// Whether to use SSE streaming for responses
    #[serde(default = "default_true")]
    pub use_streaming_api: bool,
}

impl ProviderConfig {
    pub fn openai(api_key: impl Into<String>) -> Self {
        Self {
            provider_name: "openai".to_string(),
            base_url: "https://api.openai.com/v1".to_string(),
            auth: AuthConfig::BearerToken(api_key.into()),
            timeout_secs: 60,
            default_headers: Vec::new(),
            use_streaming_api: true,
        }
    }

    pub fn anthropic(api_key: impl Into<String>) -> Self {
        Self {
            provider_name: "anthropic".to_string(),
            base_url: "https://api.anthropic.com/v1".to_string(),
            auth: AuthConfig::ApiKeyHeader {
                header_name: "x-api-key".to_string(),
                value: api_key.into(),
            },
            timeout_secs: 60,
            default_headers: vec![("anthropic-version".to_string(), "2023-06-01".to_string())],
            use_streaming_api: false,
        }
    }

    /// DeepSeek Chat API configuration.
    pub fn deepseek(api_key: impl Into<String>) -> Self {
        Self {
            provider_name: "deepseek".to_string(),
            base_url: "https://api.deepseek.com/".to_string(),
            auth: AuthConfig::BearerToken(api_key.into()),
            timeout_secs: 120,
            default_headers: Vec::new(),
            use_streaming_api: true,
        }
    }

    /// Build a [`reqwest::Client`] from this provider config, applying the
    /// global proxy if one is provided.
    ///
    /// Call this in the builder when constructing provider instances.
    pub fn build_http_client(&self, proxy: Option<&ProxyConfig>) -> Result<reqwest::Client, String> {
        let mut builder = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(self.timeout_secs.max(60)));
        if let Some(proxy_cfg) = proxy {
            builder = builder.proxy(proxy_cfg.to_reqwest_proxy()?);
        }
        builder
            .build()
            .map_err(|e| format!("failed to build HTTP client for provider '{}': {e}", self.provider_name))
    }
}

// ── SDK config ──

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FoxAgentSdkConfig {
    /// LLM provider configuration.
    ///
    /// When `Some`, `AgentBuilder` auto-creates a provider from this config and
    /// uses the `default_model` as the model id.  Ignored when
    /// `AgentBuilder::provider_config()` or `AgentBuilder::with_provider()` is
    /// called explicitly.
    pub provider: Option<ProviderConfig>,

    /// Default model id (e.g. `"deepseek-reasoner"`, `"gpt-4o"`).
    ///
    /// Ignored when `AgentBuilder::model_id()` is called explicitly.
    /// Falls back to `"gpt-4o"` when neither is set (backward-compatible).
    pub default_model: Option<String>,

    /// Memory system configuration
    pub memory: MemoryConfig,
    /// Context window compaction configuration
    pub compaction: CompactionConfig,
    /// Tool permission safety configuration
    pub safety: SafetyConfig,

    /// Unified storage root directory for all persisted SDK data.
    ///
    /// Application code must set this explicitly.  Data is organised as
    /// subdirectories under the root:
    /// - `sessions/` — session snapshots
    /// - `planning/` — planning state (goals, plans, todos)
    /// - `memory/`  — long-term memory graph
    ///
    /// # Examples
    ///
    /// ```ignore
    /// // store data next to the working tree
    /// storage_dir: working_dir.join(".fox-code"),
    ///
    /// // store data in the user's config directory
    /// storage_dir: dirs::data_dir().unwrap().join("fox-code"),
    /// ```
    pub storage_dir: std::path::PathBuf,

    /// Whether to persist a fresh session snapshot at key lifecycle points.
    pub auto_snapshot: bool,

    /// Skills system configuration (Claude Code compatible).
    pub skills: SkillsConfig,

    /// Budget governance configuration.
    pub budget: BudgetConfig,

    /// MCP integration configuration.
    pub mcp: McpConfig,

    /// Hooks system configuration (Claude Code compatible).
    ///
    /// When enabled, the SDK executes user-defined scripts at key lifecycle
    /// events (PreToolUse, PostToolUse, PreCompact, etc.).
    pub hooks: Option<HooksConfig>,

    /// Plugin system configuration.
    ///
    /// When enabled, the SDK can install and manage plugins from configured
    /// marketplaces.
    pub plugins: Option<PluginsConfig>,

    /// Proxy configuration for all outbound HTTP connections.
    ///
    /// When set, all HTTP clients (providers, MCP transports, tool calls,
    /// marketplace refreshes) are routed through the specified proxy.
    /// Supports HTTP, HTTPS, and SOCKS5 proxies.
    pub proxy: Option<ProxyConfig>,

    /// Optional path to a global AGENTS.md file for domain-level instructions.
    ///
    /// When set, the SDK loads this file (in addition to the per-project
    /// `<working_dir>/AGENTS.md`) and injects it into the static system prompt.
    /// When `None` (default), the SDK falls back to `$FOX_AGENT_DIR/AGENTS.md` or
    /// `~/.fox-agent/AGENTS.md`.
    ///
    /// Set this when embedding the SDK in a domain-specific application
    /// (e.g. a coding agent, a trading agent) that ships with its own
    /// global domain instructions.
    pub global_agents_md_path: Option<std::path::PathBuf>,
}

impl Default for FoxAgentSdkConfig {
    fn default() -> Self {
        Self {
            provider: None,
            default_model: None,
            memory: MemoryConfig::default(),
            compaction: CompactionConfig::default(),
            safety: SafetyConfig::default(),
            storage_dir: std::path::PathBuf::from(".fox-agent-sdk"),
            auto_snapshot: true,
            skills: SkillsConfig::default(),
            budget: BudgetConfig::default(),
            mcp: McpConfig::default(),
            hooks: None,
            plugins: None,
            proxy: None,
            global_agents_md_path: None,
        }
    }
}

// ── Hooks & Plugins stubs (fully implemented as Phase 2/3) ──

/// Hooks system configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HooksConfig {
    /// Enable hooks.
    pub enabled: bool,
    /// Hook execution timeout in seconds.
    #[serde(default = "default_hooks_timeout")]
    pub timeout_secs: u64,
    /// Max concurrent hooks per event.
    #[serde(default = "default_hooks_max_concurrent")]
    pub max_concurrent: usize,
    /// Additional hook directories.
    #[serde(default)]
    pub additional_directories: Vec<std::path::PathBuf>,
    /// Load global hooks from `{storage_dir}/hooks/`.
    #[serde(default = "default_true")]
    pub load_global: bool,
}

fn default_hooks_timeout() -> u64 { 30 }
fn default_hooks_max_concurrent() -> usize { 5 }
fn default_true() -> bool { true }

impl Default for HooksConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            timeout_secs: 30,
            max_concurrent: 5,
            additional_directories: Vec::new(),
            load_global: true,
        }
    }
}

/// Plugin system configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginsConfig {
    /// Enable plugin system.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Auto-update check interval in hours (0 = disabled).
    #[serde(default)]
    pub auto_update_hours: u64,
    /// Plugin names to auto-install on startup.
    #[serde(default)]
    pub preinstall: Vec<String>,
    /// Configured marketplaces.
    #[serde(default)]
    pub marketplaces: Vec<MarketplaceConfig>,
}

impl Default for PluginsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_update_hours: 0,
            preinstall: Vec::new(),
            marketplaces: Vec::new(),
        }
    }
}

/// A plugin marketplace source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplaceConfig {
    pub name: String,
    #[serde(default)]
    pub url: String,
    pub source: String,               // "GitHub" | "Git" | "Http" | "Local"
    #[serde(default)]
    pub auto_update_hours: u64,        // 0 = disabled
    #[serde(default)]
    pub owner: Option<String>,         // GitHub: owner
    #[serde(default)]
    pub repo: Option<String>,          // GitHub: repo name
    #[serde(default)]
    pub branch: Option<String>,        // Git branch
    #[serde(default)]
    pub path: Option<std::path::PathBuf>, // Local path
}

// ── Proxy config ──

/// Outbound HTTP proxy configuration.
///
/// Applies to all HTTP clients in the SDK: providers, MCP transports,
/// tool calls (webfetch, websearch), and marketplace refreshes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    /// Proxy URL, e.g. `http://127.0.0.1:7890`, `socks5://127.0.0.1:1080`.
    pub url: String,
    /// Optional basic auth: `username` portion.
    #[serde(default)]
    pub username: Option<String>,
    /// Optional basic auth: `password` portion.
    #[serde(default)]
    pub password: Option<String>,
    /// Host patterns to bypass the proxy for (e.g. `localhost`, `*.internal`).
    #[serde(default)]
    pub no_proxy: Vec<String>,
}

impl ProxyConfig {
    /// Build a [`reqwest::Proxy`] from this configuration.
    pub fn to_reqwest_proxy(&self) -> Result<reqwest::Proxy, String> {
        let mut proxy = reqwest::Proxy::all(&self.url)
            .map_err(|e| format!("invalid proxy URL '{}': {e}", self.url))?;
        if let (Some(u), Some(p)) = (&self.username, &self.password) {
            proxy = proxy.basic_auth(u, p);
        }
        if !self.no_proxy.is_empty() {
            let no_proxy_str = self.no_proxy.join(",");
            proxy = proxy.no_proxy(reqwest::NoProxy::from_string(&no_proxy_str));
        }
        Ok(proxy)
    }
}

/// Error returned by [`FoxAgentSdkConfig::load_from_file`].
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config file {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse config file {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: toml::de::Error,
    },
}

impl FoxAgentSdkConfig {
    /// Load configuration from a TOML file, returning a [`FoxAgentSdkConfig`].
    ///
    /// Fields not present in the file fall back to their [`Default`] values.
    ///
    /// Paths starting with `~` are expanded to the user's home directory.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let cfg = FoxAgentSdkConfig::load_from_file("agent.toml")?;
    /// ```
    pub fn load_from_file<P: AsRef<std::path::Path>>(path: P) -> Result<FoxAgentSdkConfig, ConfigError> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path)
            .map_err(|e| ConfigError::Io { path: path.display().to_string(), source: e })?;
        let mut cfg: FoxAgentSdkConfig = toml::from_str(&content)
            .map_err(|e| ConfigError::Parse { path: path.display().to_string(), source: e })?;
        
        // Expand ~ in paths to user's home directory
        cfg.expand_home_paths();
        
        Ok(cfg)
    }

    /// Build a [`reqwest::Client`] with the configured proxy settings.
    ///
    /// Returns a plain client builder (with no proxy) when `self.proxy` is `None`.
    pub fn build_reqwest_client(&self) -> Result<reqwest::Client, String> {
        let mut builder = reqwest::Client::builder();
        if let Some(ref proxy_cfg) = self.proxy {
            builder = builder.proxy(proxy_cfg.to_reqwest_proxy()?);
        }
        builder
            .build()
            .map_err(|e| format!("failed to build reqwest client: {e}"))
    }
    
    /// Expand paths starting with `~` to the user's home directory.
    fn expand_home_paths(&mut self) {
        if let Some(home) = dirs::home_dir() {
            // Helper: replace ~/ with home dir
            let expand = |s: &str| -> std::path::PathBuf {
                let rest = s.strip_prefix("~").unwrap_or(s);
                // Strip leading / or \ so Path::join works correctly on all platforms
                let rest = rest.trim_start_matches('/').trim_start_matches('\\');
                home.join(rest)
            };

            if let Some(storage_str) = self.storage_dir.to_str() {
                if storage_str.starts_with("~") {
                    self.storage_dir = expand(storage_str);
                }
            }
            
            if let Some(ref path) = self.global_agents_md_path {
                if let Some(path_str) = path.to_str() {
                    if path_str.starts_with("~") {
                        self.global_agents_md_path = Some(expand(path_str));
                    }
                }
            }
            
            if let Some(ref path) = self.memory.embedding_model_path {
                if let Some(path_str) = path.to_str() {
                    if path_str.starts_with("~") {
                        self.memory.embedding_model_path = Some(expand(path_str));
                    }
                }
            }

            // Also expand memory.embedding_cache_dir
            if let Some(ref path) = self.memory.embedding_cache_dir {
                if let Some(path_str) = path.to_str() {
                    if path_str.starts_with("~") {
                        self.memory.embedding_cache_dir = Some(expand(path_str));
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AutoExtractScope {
    /// Store auto-extracted memories in the session-local scope first.
    /// Combine with `auto_promote_enabled` to let frequently-reinforced
    /// session memories graduate to the project scope automatically.
    Session,
    Project,
    Global,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContradictionPolicy {
    Ignore,
    Supersede,
    DowngradeConfidence,
    MarkContradictionEdge,
}

/// Memory retrieval and injection configuration.
///
/// The actual storage directory is inherited from the parent
/// `FoxAgentSdkConfig.storage_dir` (→ `{storage_dir}/memory/`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryConfig {
    /// Whether memory retrieval is enabled
    pub enabled: bool,
    /// Whether semantic embedding generation and recall are enabled.
    pub embedding_enabled: bool,
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
    /// Enable automatic promotion of frequently-reinforced session memories
    /// to a longer-lived scope (project/global).
    pub auto_promote_enabled: bool,
    /// Strength (reinforcement count) threshold at which a session memory is
    /// automatically promoted. A memory starts at strength 1 and gains +1 per
    /// reinforcement, so a threshold of 3 promotes after 2 reinforcements.
    pub auto_promote_strength_threshold: u32,
    /// Target scope for auto-promotion (must be Project or Global).
    pub auto_promote_target: AutoExtractScope,
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
            auto_promote_enabled: false,
            auto_promote_strength_threshold: 3,
            auto_promote_target: AutoExtractScope::Project,
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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
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
    /// Minimum number of turns between consecutive compactions.
    /// Prevents compaction from firing every turn when the agent
    /// reads large files that immediately fill the budget again.
    pub min_compaction_gap_turns: u32,
    /// Whether to use an LLM to produce a structured narrative summary
    /// of the compacted messages. When enabled, the summarizer asks the
    /// LLM to output a JSON NarrativeRecord (user intent → actions →
    /// findings → decisions). Falls back to mechanical truncation if the
    /// LLM call fails. The resulting narratives are stored in MemoryGraph
    /// and injected as "## Session History" on subsequent turns.
    pub llm_summary_enabled: bool,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            // 3,200,000 chars ≈ 800K tokens (80% of DeepSeek's 1M context).
            token_budget: 3_200_000,
            preserve_recent_messages: 80,
            max_turns_before_compaction: 500,
            context_limit_threshold: 0.90,
            max_compaction_count: 10,
            min_compaction_gap_turns: 20,
            llm_summary_enabled: true,
        }
    }
}

/// Default policy for tools not in allowlist or denylist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DefaultSafetyPolicy {
    /// Require user confirmation
    Confirm,
    /// Auto-allow without asking user
    Allow,
    /// Auto-deny without asking user
    Deny,
}

/// Safety/permission configuration with allowlist/denylist support.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SafetyConfig {
    /// Tool allowlist. If set, only tools in this list are automatically allowed.
    /// None means allowlist mode is disabled.
    pub tool_allowlist: Option<Vec<String>>,

    /// Tool denylist. Tools in this list always require user confirmation,
    /// regardless of other settings.
    pub tool_denylist: Option<Vec<String>>,

    /// Default policy for tools not covered by allowlist or denylist.
    pub default_policy: DefaultSafetyPolicy,

    /// Approval cache configuration.
    pub approval_cache: crate::event::ApprovalCacheConfig,

    /// When enabled, productive tools (write, edit, non-readonly bash) always
    /// require user confirmation — regardless of the user's message content.
    /// This is a simple, reliable gate: modification tools are dangerous by
    /// nature and should be confirmed. Read-only bash commands (ls, grep, cat,
    /// etc.) are still allowed automatically.
    pub productive_tool_confirm: bool,
}

impl Default for SafetyConfig {
    fn default() -> Self {
        Self {
            tool_allowlist: None,
            tool_denylist: None,
            default_policy: DefaultSafetyPolicy::Allow,
            approval_cache: crate::event::ApprovalCacheConfig::default(),
            productive_tool_confirm: true,
        }
    }
}

/// File system operation type for sandbox validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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

// ── Budget governance ──

/// Budget configuration for governing agent resource consumption.
///
/// When any limit is exceeded, the agent returns a structured error
/// instead of silently continuing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BudgetConfig {
    /// Maximum total tokens (input + output) per session.
    /// When exceeded, `ErrorKind::BudgetExceeded` is returned.
    #[serde(default)]
    pub token_budget: Option<u64>,

    /// Maximum estimated cost in USD cents per session.
    /// When exceeded, `ErrorKind::BudgetExceeded` is returned.
    #[serde(default)]
    pub cost_budget_cents: Option<u64>,

    /// Provider request timeout in seconds.
    /// When exceeded, the HTTP request is aborted.
    #[serde(default = "default_provider_timeout_secs")]
    pub provider_timeout_secs: u64,

    /// Number of retries on transient provider errors.
    #[serde(default = "default_provider_retries")]
    pub provider_retries: u32,

    /// Tool execution timeout in seconds per invocation.
    /// When exceeded, the tool is killed and `ToolError::Timeout` is returned.
    #[serde(default = "default_tool_timeout_secs")]
    pub tool_timeout_secs: u64,

    /// Maximum parallel tool invocations allowed.
    #[serde(default)]
    pub tool_concurrency_limit: usize,

    /// Maximum number of turns per session (0 = unlimited).
    #[serde(default)]
    pub max_turns: u64,
}

fn default_provider_timeout_secs() -> u64 {
    120
}
fn default_provider_retries() -> u32 {
    2
}
fn default_tool_timeout_secs() -> u64 {
    60
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            token_budget: None,
            cost_budget_cents: None,
            provider_timeout_secs: default_provider_timeout_secs(),
            provider_retries: default_provider_retries(),
            tool_timeout_secs: default_tool_timeout_secs(),
            tool_concurrency_limit: 8,
            max_turns: 0,
        }
    }
}

/// Accumulated metrics for an agent session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MetricsSnapshot {
    pub total_tokens: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub estimated_cost_cents: u64,
    pub tool_calls: u64,
    pub tool_success_count: u64,
    pub tool_error_count: u64,
    pub compaction_count: u64,
    pub turns_completed: u64,
    pub total_latency_ms: u64,
    pub token_usage_history: Vec<TokenUsageEntry>,
}

/// A single token usage record with timing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsageEntry {
    pub timestamp: u64,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub latency_ms: u64,
    pub cost_cents: u64,
}

impl MetricsSnapshot {
    /// Record a model usage event.
    pub fn record(&mut self, usage: &crate::provider::TokenUsage, latency_ms: u64, cost_cents: u64) {
        self.total_input_tokens += usage.input_tokens as u64;
        self.total_output_tokens += usage.output_tokens as u64;
        self.total_tokens = self.total_input_tokens + self.total_output_tokens;
        self.estimated_cost_cents += cost_cents;
        self.tool_calls += 1; // increment tool call count per usage record
        self.total_latency_ms += latency_ms;
        self.token_usage_history.push(TokenUsageEntry {
            timestamp: crate::planning::now_secs(),
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            latency_ms,
            cost_cents,
        });
    }

    /// Record a tool success.
    pub fn record_tool_success(&mut self) {
        self.tool_success_count += 1;
    }

    /// Record a tool error.
    pub fn record_tool_error(&mut self) {
        self.tool_error_count += 1;
    }

    /// Record a compaction event.
    pub fn record_compaction(&mut self) {
        self.compaction_count += 1;
    }

    /// Computed tool error rate (0.0 - 1.0), or 0.0 if no tool calls.
    pub fn tool_error_rate(&self) -> f64 {
        let total = self.tool_success_count + self.tool_error_count;
        if total == 0 {
            return 0.0;
        }
        self.tool_error_count as f64 / total as f64
    }

    /// Check whether the accumulated metrics exceed the given budget.
    pub fn exceeds_budget(&self, budget: &BudgetConfig) -> Option<String> {
        if let Some(limit) = budget.token_budget {
            if self.total_tokens > limit {
                return Some(format!(
                    "token budget exceeded: {}/{} tokens",
                    self.total_tokens, limit
                ));
            }
        }
        if let Some(limit) = budget.cost_budget_cents {
            if self.estimated_cost_cents > limit {
                return Some(format!(
                    "cost budget exceeded: {}/{} cents",
                    self.estimated_cost_cents, limit
                ));
            }
        }
        None
    }
}

// ── MCP Configuration ──

/// Configuration for the Model Context Protocol integration.
///
/// When `enabled` is true, connected MCP servers contribute their tool
/// definitions to the agent's tool list at build time.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct McpConfig {
    /// Global MCP enable/disable switch.
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Connection timeout in seconds.
    #[serde(default = "default_mcp_connect_timeout")]
    pub connect_timeout_secs: u64,

    /// Per-tool-call timeout in seconds.
    #[serde(default = "default_mcp_tool_timeout")]
    pub tool_timeout_secs: u64,

    /// Maximum concurrent MCP tool calls.
    #[serde(default)]
    pub max_concurrent_tools: usize,

    /// Whether to automatically refresh tool lists (tools/list_changed).
    #[serde(default)]
    pub auto_refresh_tools: bool,

    /// Refresh interval in seconds (0 = disabled).
    #[serde(default)]
    pub tool_refresh_interval_secs: u64,

    /// Maximum reconnect attempts after SSE disconnect.
    #[serde(default)]
    pub max_reconnect_attempts: u32,

    /// Reconnect backoff in milliseconds.
    #[serde(default = "default_reconnect_backoff")]
    pub reconnect_backoff_ms: u64,

    /// Default risk level assigned to MCP server tools.
    #[serde(default)]
    pub default_risk_level: McpRiskLevel,

    /// Whether to expose resources to the system prompt.
    #[serde(default)]
    pub expose_resources: bool,

    /// Max resources to inject per turn.
    #[serde(default = "default_max_resources")]
    pub max_resources_per_injection: usize,
}

fn default_enabled() -> bool {
    false
}
fn default_mcp_connect_timeout() -> u64 {
    30
}
fn default_mcp_tool_timeout() -> u64 {
    60
}
fn default_reconnect_backoff() -> u64 {
    1000
}
fn default_max_resources() -> usize {
    5
}

/// Risk level assigned to MCP server tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum McpRiskLevel {
    /// Safe read-only operations
    #[default]
    Low,
    /// Read + limited write
    Medium,
    /// Arbitrary write or shell invocation
    High,
    /// Network access or destructive operations
    Critical,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            connect_timeout_secs: default_mcp_connect_timeout(),
            tool_timeout_secs: default_mcp_tool_timeout(),
            max_concurrent_tools: 4,
            auto_refresh_tools: false,
            tool_refresh_interval_secs: 0,
            max_reconnect_attempts: 3,
            reconnect_backoff_ms: default_reconnect_backoff(),
            default_risk_level: McpRiskLevel::High,
            expose_resources: false,
            max_resources_per_injection: default_max_resources(),
        }
    }
}
