//! Standardized Builder API for Agent and SwarmRuntime assembly.
//!
//! Replaces manual `Provider + Model + Harness + Agent` wiring with
//! a chainable, discoverable builder that provides sensible defaults.

use fox_agent_core::{
    FoxAgentSdkConfig, Model, PermissionResult, PlanningStore, SafetyConfig, SessionStore,
    WorkspaceSandbox, Tool,
};
use fox_agent_providers::{AnthropicCompatibleProvider, DeepSeekProvider, OpenAiCompatibleProvider, ProviderConfig};
use fox_agent_swarm::SwarmCoordinator;
use std::path::PathBuf;
use std::sync::Arc;

use crate::agent::Agent;
use crate::harness::Harness;
use crate::swarm_runtime::SwarmRuntime;

// ── Provider factory ──

fn build_provider(config: ProviderConfig) -> Arc<dyn fox_agent_core::Provider> {
    match config.provider_name.as_str() {
        "openai" => Arc::new(OpenAiCompatibleProvider::new(config)
            .expect("failed to construct OpenAI provider")),
        "anthropic" => Arc::new(AnthropicCompatibleProvider::new(config)
            .expect("failed to construct Anthropic provider")),
        "deepseek" => Arc::new(DeepSeekProvider::new(config)),
        other => panic!("unknown provider name: {other}, expected one of: openai, anthropic, deepseek"),
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
    permission_hook:
        Option<Arc<dyn Fn(&str, &serde_json::Value) -> PermissionResult + Send + Sync>>,
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

    // ── build ──

    /// Assemble an [`Agent`] from the accumulated config.
    pub async fn build(self) -> Result<Agent, String> {
        let provider = if let Some(p) = self.provider {
            p
        } else if let Some(cfg) = self.provider_config {
            build_provider(cfg)
        } else {
            return Err("provider or provider_config is required".to_string());
        };

        let model_id = self
            .model_id
            .unwrap_or_else(|| "gpt-4o".to_string());

        let model: Arc<dyn Model> = Arc::new(fox_agent_core::DefaultModel::new(provider, model_id));

        let sdk_config = self.sdk_config.unwrap_or_default();

        let mut harness = if let Some(hook) = self.permission_hook {
            Harness::with_permission_hook(
                sdk_config.clone(),
                self.working_dir.clone(),
                move |name, input| hook(name, input),
            )
        } else {
            Harness::new(sdk_config.clone(), self.working_dir.clone())
        };

        // Override stores if explicitly provided
        if let Some(store) = self.session_store {
            harness.session_store = store;
        }
        if let Some(store) = self.planning_store {
            harness.planning_store = store.clone();
            fox_agent_core::set_default_planning_store(store);
        }

        let agent = Agent::new(model, harness);

        if self.default_tools {
            agent.harness().register_default_tools().await;
        }

        for tool in self.tools {
            agent.harness().register_tool(tool).await;
        }

        if let Some(sandbox) = self.sandbox {
            agent.harness().set_sandbox(sandbox).await;
        }

        Ok(agent)
    }

    /// Assemble a [`SwarmRuntime`] from the accumulated config.
    ///
    /// The built runtime reuses the same provider, model, and harness as
    /// a single-agent build, but wraps them in a [`SwarmCoordinator`].
    pub async fn build_swarm_runtime(self) -> Result<SwarmRuntime, String> {
        let agent = self.build().await?;
        let coordinator = Arc::new(SwarmCoordinator::new());
        Ok(SwarmRuntime::new(
            coordinator,
            agent.model.clone(),
            agent.harness.clone(),
        ))
    }
}

// ── SwarmRuntimeBuilder ──

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

    // ── build ──

    /// Assemble the full [`SwarmRuntime`].
    pub async fn build(self) -> Result<SwarmRuntime, String> {
        let coordinator = self
            .coordinator
            .unwrap_or_else(|| Arc::new(SwarmCoordinator::new()));

        let agent = self.inner.build().await?;
        Ok(SwarmRuntime::new(
            coordinator,
            agent.model.clone(),
            agent.harness.clone(),
        ))
    }
}
