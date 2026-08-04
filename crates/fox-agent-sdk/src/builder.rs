//! Standardized Builder API for Agent and SwarmRuntime assembly.
//!
//! Replaces manual `Provider + Model + Harness + Agent` wiring with
//! a chainable, discoverable builder that provides sensible defaults.

use fox_agent_core::{
    FoxAgentSdkConfig, McpConfig, Model, PermissionDecision, PermissionRequest, PermissionResult,
    PlanningStore, ProviderConfig, SafetyConfig, SessionStore, Skill, Tool, WorkspaceSandbox,
};
use fox_agent_providers::{
    AnthropicCompatibleProvider, DeepSeekProvider, OpenAiCompatibleProvider,
};
use fox_agent_swarm::SwarmCoordinator;
use fox_agent_tools::register_default_tools_with_planning_store_and_skill_registry;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

use crate::agent::{Agent, AuditHandlerFn};
use crate::artifact_tool::ArtifactReadTool;
use crate::governance::GovernanceGuard;
use crate::harness::Harness;
use crate::mcp::{
    McpServerConfig, build_mcp_context_summary, connect_and_discover_tools, effective_profile,
};
use crate::swarm_runtime::SwarmRuntime;

// ── Provider factory ──

fn build_provider(config: ProviderConfig) -> Arc<dyn fox_agent_core::Provider> {
    match config.provider_name.as_str() {
        "openai" => Arc::new(
            OpenAiCompatibleProvider::new(config).expect("failed to construct OpenAI provider"),
        ),
        "anthropic" => Arc::new(
            AnthropicCompatibleProvider::new(config)
                .expect("failed to construct Anthropic provider"),
        ),
        "deepseek" => Arc::new(DeepSeekProvider::new(config)),
        other => {
            panic!("unknown provider name: {other}, expected one of: openai, anthropic, deepseek")
        }
    }
}

// ── AgentBuilder ──

/// Builder for constructing a fully-assembled [`Agent`] with sensible defaults.
///
/// # Minimal example
///
/// ```ignore
/// let mut agent = AgentBuilder::new()
///     .provider_config(ProviderConfig::deepseek(api_key))
///     .model_id("deepseek-v4-flash")
///     .working_dir(".")
///     .with_default_tools()
///     .build()
///     .await?;
///
/// agent.run_once("Say hello").await?;
/// ```
///
/// For testing or custom provider wiring, use [`with_provider`] to inject
/// a pre-built `Arc<dyn Provider>` directly.
pub struct AgentBuilder {
    provider_config: Option<ProviderConfig>,
    provider: Option<Arc<dyn fox_agent_core::Provider>>,
    model_id: Option<String>,
    working_dir: Option<PathBuf>,
    sdk_config: Option<FoxAgentSdkConfig>,
    session_store: Option<Arc<dyn SessionStore>>,
    planning_store: Option<Arc<dyn PlanningStore>>,
    sandbox: Option<WorkspaceSandbox>,
    safety_config: Option<SafetyConfig>,
    default_tools: bool,
    tools: Vec<Arc<dyn Tool>>,
    permission_hook: Option<crate::safety::PermissionHook>,
    /// Optional audit handler — auto-invoked on every user permission decision.
    audit_handler: Option<AuditHandlerFn>,
    system_prompt: Option<String>,
    mcp_servers: Vec<McpServerConfig>,
    mcp_config_override: Option<McpConfig>,
    active_skill: Arc<RwLock<Option<Skill>>>,
}

impl Default for AgentBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentBuilder {
    /// Create a new builder with all fields unset (defaults applied at build).
    pub fn new() -> Self {
        Self {
            provider_config: None,
            provider: None,
            model_id: None,
            working_dir: None,
            sdk_config: None,
            session_store: None,
            planning_store: None,
            sandbox: None,
            safety_config: None,
            default_tools: false,
            tools: Vec::new(),
            permission_hook: None,
            audit_handler: None,
            system_prompt: None,
            mcp_servers: Vec::new(),
            mcp_config_override: None,
            active_skill: Arc::new(RwLock::new(None)),
        }
    }

    /// Set provider via a pre-built [`Provider`] instance.
    ///
    /// Use this when you need full control over provider construction
    /// (e.g. in tests with [`MockProvider`](fox_agent_providers::MockProvider)).
    /// Takes precedence over [`provider_config`](Self::provider_config) when
    /// both are set.
    pub fn with_provider(mut self, provider: Arc<dyn fox_agent_core::Provider>) -> Self {
        self.provider = Some(provider);
        self
    }

    /// Set the provider configuration and model id to use.
    ///
    /// The builder will construct the appropriate provider (OpenAI / Anthropic
    /// / DeepSeek) based on [`ProviderConfig::provider_name`].
    pub fn provider_config(mut self, config: ProviderConfig) -> Self {
        self.provider_config = Some(config);
        self
    }

    /// Override or set the model id (e.g. `"deepseek-v4-flash"`).
    ///
    /// If not called, falls back to the provider's default.
    pub fn model_id(mut self, id: impl Into<String>) -> Self {
        self.model_id = Some(id.into());
        self
    }

    /// Set the working directory for the agent session.
    ///
    /// If not set, the agent starts without a working directory.
    pub fn working_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.working_dir = Some(dir.into());
        self
    }

    /// Override the full SDK configuration.
    ///
    /// If not called, `FoxAgentSdkConfig::default()` is used.
    pub fn sdk_config(mut self, cfg: FoxAgentSdkConfig) -> Self {
        self.sdk_config = Some(cfg);
        self
    }

    /// Inject a custom session store (overrides config-driven resolution).
    pub fn with_session_store(mut self, store: Arc<dyn SessionStore>) -> Self {
        self.session_store = Some(store);
        self
    }

    /// Inject a custom planning store (overrides config-driven resolution).
    pub fn with_planning_store(mut self, store: Arc<dyn PlanningStore>) -> Self {
        self.planning_store = Some(store);
        self
    }

    /// Attach a workspace sandbox for file-system tool calls.
    pub fn with_sandbox(mut self, sandbox: WorkspaceSandbox) -> Self {
        self.sandbox = Some(sandbox);
        self
    }

    /// Attach a safety/permission configuration.
    pub fn with_safety_policy(mut self, cfg: SafetyConfig) -> Self {
        self.safety_config = Some(cfg);
        self
    }

    /// Register a custom tool from the outside.
    pub fn with_tool(mut self, tool: Arc<dyn Tool>) -> Self {
        self.tools.push(tool);
        self
    }

    /// Request that all built-in default tools be auto-registered.
    pub fn with_default_tools(mut self) -> Self {
        self.default_tools = true;
        self
    }

    /// Set a custom permission hook function.
    ///
    /// When set, the hook is called for every tool invocation and its return
    /// value determines permission.  This is the preferred escape-hatch for
    /// integrating a custom UI approval flow.
    pub fn with_permission_hook(
        mut self,
        hook: impl Fn(&str, &serde_json::Value) -> PermissionResult + Send + Sync + 'static,
    ) -> Self {
        self.permission_hook = Some(Arc::new(hook));
        self
    }

    /// Register an audit handler that is automatically invoked on every user
    /// permission decision (Allow / Deny), eliminating the need for manual
    /// `record_audit` calls in every `RequiresUserDecision` match arm.
    ///
    /// The handler receives:
    /// - `&PermissionRequest` — the original request (contains `policy_source`,
    ///   `tool_name`, `risk_level`, etc.)
    /// - `&PermissionDecision` — the user's choice
    /// - `u64` — the `turn_id` at the time of the decision
    ///
    /// # Example
    ///
    /// ```ignore
    /// let approval = ApprovalManager::new("session-1", SafetyConfig::default());
    /// let audit_path = PathBuf::from("./audit.jsonl");
    ///
    /// let agent = AgentBuilder::new()
    ///     .provider_config(ProviderConfig::deepseek(key))
    ///     .with_default_tools()
    ///     .with_audit_handler(move |req, dec, turn| {
    ///         let result = match dec {
    ///             PermissionDecision::Allow => PermissionResult::Allow,
    ///             PermissionDecision::Deny { reason } =>
    ///                 PermissionResult::Deny { reason: reason.clone() },
    ///         };
    ///         // approval.record_audit(req, &result, turn) is called
    ///         // automatically on every permission decision.
    ///     })
    ///     .build()
    ///     .await?;
    /// ```
    pub fn with_audit_handler(
        mut self,
        handler: impl Fn(&PermissionRequest, &PermissionDecision, u64) + Send + Sync + 'static,
    ) -> Self {
        self.audit_handler = Some(Arc::new(handler));
        self
    }

    /// Override the default system prompt template.
    ///
    /// The provided string replaces the compiled-in coding-oriented system
    /// prompt.  Use this for non-coding agent applications that need a
    /// different persona or domain-specific instructions.
    ///
    /// # Example (customer-support agent)
    ///
    /// ```ignore
    /// AgentBuilder::new()
    ///     .provider_config(ProviderConfig::deepseek(key))
    ///     .with_system_prompt(
    ///         "You are a helpful customer support agent. \
    ///          You can look up orders, process refunds, and answer FAQs."
    ///     )
    ///     .build()
    ///     .await?;
    /// ```
    pub fn with_system_prompt(mut self, template: impl Into<String>) -> Self {
        self.system_prompt = Some(template.into());
        self
    }

    /// Set the path to a global/domain-level AGENTS.md file.
    ///
    /// When set, this file is loaded in addition to the per-project
    /// `<working_dir>/AGENTS.md` and injected into the system prompt's static
    /// (cacheable) section.
    ///
    /// Use this when embedding the SDK in a domain-specific application
    /// (e.g. a coding agent) that ships with its own global instructions.
    ///
    /// # Example (coding agent)
    ///
    /// ```ignore
    /// AgentBuilder::new()
    ///     .provider_config(ProviderConfig::deepseek(key))
    ///     .working_dir(user_project)
    ///     .with_global_agents_md_path(dirs::config_dir().unwrap().join("my-code-agent/AGENTS.md"))
    ///     .with_default_tools()
    ///     .build()
    ///     .await?;
    /// ```
    pub fn with_global_agents_md_path(mut self, path: impl Into<PathBuf>) -> Self {
        let mut cfg = self.sdk_config.unwrap_or_default();
        cfg.global_agents_md_path = Some(path.into());
        self.sdk_config = Some(cfg);
        self
    }

    /// Set the storage directory for all persisted SDK data.
    ///
    /// Data is organised as subdirectories:
    /// - `sessions/` — session snapshots
    /// - `planning/` — planning state (goals, plans, todos)
    /// - `memory/`  — long-term memory graph
    ///
    /// Relative paths are resolved against `working_dir`.
    ///
    /// # Example
    ///
    /// ```ignore
    /// AgentBuilder::new()
    ///     .provider_config(ProviderConfig::deepseek(key))
    ///     .working_dir(".")
    ///     .with_storage_dir(".fox-code")
    ///     .with_default_tools()
    ///     .build()
    ///     .await?;
    /// ```
    pub fn with_storage_dir(mut self, dir: impl Into<std::path::PathBuf>) -> Self {
        let mut cfg = self.sdk_config.unwrap_or_default();
        cfg.storage_dir = dir.into();
        self.sdk_config = Some(cfg);
        self
    }

    /// Add an MCP server configuration.
    ///
    /// The server will be connected at build time and its tools
    /// automatically registered with the agent.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Stdio transport
    /// AgentBuilder::new()
    ///     .provider_config(ProviderConfig::deepseek(key))
    ///     .with_mcp_server(McpServerConfig {
    ///         name: "filesystem".into(),
    ///         transport: McpTransportMode::Stdio {
    ///             command: "npx".into(),
    ///             args: vec!["-y".into(), "@modelcontextprotocol/server-filesystem".into(), "/tmp".into()],
    ///             ..Default::default()
    ///         },
    ///         ..Default::default()
    ///     })
    ///     .build()
    ///     .await?;
    ///
    /// // SSE transport
    /// AgentBuilder::new()
    ///     .with_mcp_server(McpServerConfig {
    ///         name: "remote-tools".into(),
    ///         transport: McpTransportMode::Sse {
    ///             url: "https://mcp.example.com".into(),
    ///             headers: vec![("Authorization".into(), "Bearer token".into())],
    ///             connect_timeout_secs: None, // defaults to 30s
    ///         },
    ///         ..Default::default()
    ///     })
    ///     .build()
    ///     .await?;
    /// ```
    pub fn with_mcp_server(mut self, config: McpServerConfig) -> Self {
        self.mcp_servers.push(config);
        self
    }

    /// Override the global MCP configuration.
    ///
    /// Takes precedence over `McpConfig` set via `FoxAgentSdkConfig.mcp`.
    pub fn with_mcp_config(mut self, cfg: McpConfig) -> Self {
        self.mcp_config_override = Some(cfg);
        self
    }

    // ── build ──

    /// Assemble an [`Agent`] from the accumulated config.
    pub async fn build(self) -> Result<Agent, String> {
        let mut sdk_config = self.sdk_config.unwrap_or_default();
        if let Some(mcp_override) = self.mcp_config_override {
            sdk_config.mcp = mcp_override;
        }

        // ── Inject MCP auto-approve servers into SafetyConfig ──
        {
            let auto_approve: Vec<String> = self
                .mcp_servers
                .iter()
                .filter(|c| c.auto_approve)
                .map(|c| c.name.clone())
                .collect();
            if !auto_approve.is_empty() {
                sdk_config.safety.mcp_auto_approve_servers = Some(auto_approve);
            }
        }
        let budget_timeout = sdk_config.budget.provider_timeout_secs;

        let provider = if let Some(p) = self.provider {
            p
        } else if let Some(mut cfg) = self.provider_config {
            cfg.timeout_secs = budget_timeout;
            build_provider(cfg)
        } else if let Some(ref mut cfg) = sdk_config.provider {
            cfg.timeout_secs = budget_timeout;
            build_provider(cfg.clone())
        } else {
            return Err("provider or provider_config is required. \
                Set FoxAgentSdkConfig.provider or call AgentBuilder::provider_config()."
                .to_string());
        };

        let model_id = self
            .model_id
            .or(sdk_config.default_model.clone())
            .unwrap_or_else(|| "gpt-4o".to_string());

        let model: Arc<dyn Model> = Arc::new(fox_agent_core::DefaultModel::new(provider, model_id));

        let mut harness = if let Some(hook) = self.permission_hook {
            Harness::with_permission_hook(
                sdk_config.clone(),
                self.working_dir.clone(),
                move |name, input| hook(name, input),
            )
        } else {
            Harness::new(sdk_config.clone(), self.working_dir.clone())
        };
        // Assemble the LLM wiki assistant from the model's provider (PRD §6 Phase 6).
        harness.attach_wiki_assistant(model.clone());

        // Override stores if explicitly provided
        if let Some(store) = self.session_store {
            harness.session_store = store;
        }
        if let Some(store) = self.planning_store {
            harness.planning_store = store.clone();
            fox_agent_core::set_default_planning_store(store);
        }

        // Override system prompt if custom template provided
        if let Some(template) = self.system_prompt {
            harness.prompt_builder = harness.prompt_builder.with_system_template(template);
        }

        let mut agent = Agent::new(model, harness, self.active_skill.clone());

        // Wire audit handler if registered
        if let Some(handler) = self.audit_handler {
            agent.set_audit_handler(handler);
        }

        if self.default_tools {
            // Load skills: project (.claude/skills/) + global ({storage_dir}/skills/) + additional
            let working_dir = agent.harness().session_working_dir().cloned();
            let storage_dir = resolve_storage_root_for_skills(&sdk_config, working_dir.as_deref());
            {
                let mut reg = agent.harness().skill_registry.write().await;
                if sdk_config.skills.enabled {
                    let _ = reg.load_from_config(
                        &storage_dir,
                        working_dir.as_deref(),
                        &sdk_config.skills,
                    );
                } else {
                    // When skills are disabled, still load project skills for backward compat
                    let _ = reg.load_from_working_dir(working_dir.as_deref());
                }
            }

            // ── Load hooks ──
            agent.harness().load_hooks(&storage_dir).await;

            // ── Load plugins ──
            let plugins_cfg = &sdk_config.plugins;
            if plugins_cfg.enabled {
                let mut pm = agent.harness().plugin_manager.write().await;
                if let Ok(count) = pm.discover_installed()
                    && count > 0
                {
                    info!(count, "Loaded installed plugins");
                    // Load plugin skills into registry
                    let plugin_skills = pm.active_skills();
                    let plugin_skills_count = plugin_skills.len();
                    if plugin_skills_count > 0 {
                        // Drop pm lock before acquiring skill_registry lock
                        drop(pm);
                        let mut reg = agent.harness().skill_registry.write().await;
                        // Re-acquire pm to read skills
                        let pm2 = agent.harness().plugin_manager.read().await;
                        for skill in pm2.active_skills() {
                            reg.insert(skill);
                        }
                        info!(count = plugin_skills_count, "Loaded plugin skills");
                    }
                }
            }

            // Register default tools including skill tool for on-demand activation
            let planning_store = agent.harness().planning_store.clone();
            let skill_registry = agent.harness().skill_registry.clone();
            let active_skill = agent.active_skill.clone();
            register_default_tools_with_planning_store_and_skill_registry(
                agent.harness().tool_executor(),
                planning_store,
                Some(skill_registry),
                Some(active_skill),
            )
            .await;

            // Replace the default memory tool with one backed by the harness's
            // memory_manager, so `enrich` / `rebuild_index` operate on the same
            // storage as the injection pipeline and the wiki assistant is used
            // (PRD §6 Phase 6). register_tool overwrites by name.
            let memory_tool = Arc::new(fox_agent_tools::MemoryTool::with_manager(
                agent.harness().memory_manager.core().clone(),
            ));
            agent.harness().register_tool(memory_tool).await;
        }

        if sdk_config.artifact_store.enabled {
            let artifact_tool = Arc::new(ArtifactReadTool::new(
                agent.harness().artifact_store.clone(),
            ));
            agent.harness().register_tool(artifact_tool).await;
        }

        // Phase 3: register subagent tool for isolated exploration
        {
            let subagent_tool = Arc::new(crate::subagent::SubagentTool {
                parent_harness: agent.harness.clone(),
                parent_model: agent.model.clone(),
                artifact_store: agent.harness().artifact_store.clone(),
                event_tx: None,
            });
            agent.harness().register_tool(subagent_tool).await;
            agent.subagent_runtime_enabled = true;
        }

        for tool in self.tools {
            agent.harness().register_tool(tool).await;
        }

        // Connect MCP servers and register their tools
        if !self.mcp_servers.is_empty() {
            match connect_and_discover_tools(&self.mcp_servers).await {
                Ok((mcp_tools, mcp_client, descriptors)) => {
                    for tool in mcp_tools {
                        agent.harness().register_tool(tool).await;
                    }

                    // Build MCP resources/prompts context for the system prompt
                    let mcp_ctx = build_mcp_context_summary(&mcp_client).await;
                    if !mcp_ctx.is_empty() {
                        agent.set_mcp_context(mcp_ctx);
                    }

                    let profiles = self
                        .mcp_servers
                        .iter()
                        .map(|cfg| {
                            let profile = effective_profile(cfg);
                            (profile.server_name.clone(), profile)
                        })
                        .collect();
                    agent.set_mcp_runtime_metadata(profiles, descriptors);
                    agent.mcp_client = Some(mcp_client);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to connect MCP servers — continuing without MCP tools");
                }
            }
        }

        if let Some(sandbox) = self.sandbox {
            agent.harness().set_sandbox(sandbox).await;
        }

        // Unified resource governance — timeout, retries, concurrency, budgets
        agent.set_governance(GovernanceGuard::new(sdk_config.budget));

        Ok(agent)
    }

    /// Assemble a [`SwarmRuntime`] from the accumulated config.
    ///
    /// The built runtime reuses the same provider, model, and harness as
    /// a single-agent build, but wraps them in a [`SwarmCoordinator`].
    pub async fn build_swarm_runtime(self) -> Result<SwarmRuntime, String> {
        let agent = self.build().await?;
        let coordinator = Arc::new(SwarmCoordinator::new());
        Ok(SwarmRuntime::new(coordinator, agent.model.clone(), agent.harness.clone()).await)
    }
}

// ── Helpers ──

fn resolve_storage_root_for_skills(
    cfg: &FoxAgentSdkConfig,
    working_dir: Option<&std::path::Path>,
) -> std::path::PathBuf {
    if cfg.storage_dir.is_relative()
        && let Some(dir) = working_dir
    {
        return dir.join(&cfg.storage_dir);
    }
    cfg.storage_dir.clone()
}

// ── SwarmRuntimeBuilder ────

/// Builder for constructing a [`SwarmRuntime`].
///
/// Same API surface as [`AgentBuilder`] with the addition of optional
/// coordinator pre-configuration.
pub struct SwarmRuntimeBuilder {
    inner: AgentBuilder,
    coordinator: Option<Arc<SwarmCoordinator>>,
}

impl Default for SwarmRuntimeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl SwarmRuntimeBuilder {
    /// Create a new builder with all fields unset.
    pub fn new() -> Self {
        Self {
            inner: AgentBuilder::new(),
            coordinator: None,
        }
    }

    /// Pre-built coordinator. If not provided, a default one is created.
    pub fn coordinator(mut self, c: Arc<SwarmCoordinator>) -> Self {
        self.coordinator = Some(c);
        self
    }

    // ── Delegated methods ──

    pub fn with_provider(mut self, provider: Arc<dyn fox_agent_core::Provider>) -> Self {
        self.inner = self.inner.with_provider(provider);
        self
    }

    pub fn provider_config(mut self, config: ProviderConfig) -> Self {
        self.inner = self.inner.provider_config(config);
        self
    }

    pub fn model_id(mut self, id: impl Into<String>) -> Self {
        self.inner = self.inner.model_id(id);
        self
    }

    pub fn working_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.inner = self.inner.working_dir(dir);
        self
    }

    pub fn sdk_config(mut self, cfg: FoxAgentSdkConfig) -> Self {
        self.inner = self.inner.sdk_config(cfg);
        self
    }

    pub fn with_session_store(mut self, store: Arc<dyn SessionStore>) -> Self {
        self.inner = self.inner.with_session_store(store);
        self
    }

    pub fn with_planning_store(mut self, store: Arc<dyn PlanningStore>) -> Self {
        self.inner = self.inner.with_planning_store(store);
        self
    }

    pub fn with_sandbox(mut self, sandbox: WorkspaceSandbox) -> Self {
        self.inner = self.inner.with_sandbox(sandbox);
        self
    }

    pub fn with_safety_policy(mut self, cfg: SafetyConfig) -> Self {
        self.inner = self.inner.with_safety_policy(cfg);
        self
    }

    pub fn with_tool(mut self, tool: Arc<dyn Tool>) -> Self {
        self.inner = self.inner.with_tool(tool);
        self
    }

    pub fn with_default_tools(mut self) -> Self {
        self.inner = self.inner.with_default_tools();
        self
    }

    pub fn with_permission_hook(
        mut self,
        hook: impl Fn(&str, &serde_json::Value) -> PermissionResult + Send + Sync + 'static,
    ) -> Self {
        self.inner = self.inner.with_permission_hook(hook);
        self
    }

    pub fn with_audit_handler(
        mut self,
        handler: impl Fn(&PermissionRequest, &PermissionDecision, u64) + Send + Sync + 'static,
    ) -> Self {
        self.inner = self.inner.with_audit_handler(handler);
        self
    }

    // ── build ──

    /// Assemble the full [`SwarmRuntime`].
    pub async fn build(self) -> Result<SwarmRuntime, String> {
        let coordinator = self
            .coordinator
            .unwrap_or_else(|| Arc::new(SwarmCoordinator::new()));

        let agent = self.inner.build().await?;
        Ok(SwarmRuntime::new(coordinator, agent.model.clone(), agent.harness.clone()).await)
    }
}
