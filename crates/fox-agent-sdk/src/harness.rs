use fox_agent_core::{
    ContextInfo, FoxAgentSdkConfig, HooksConfig, InterruptManager, InjectedInterrupt, MemoryStateEvent,
    PermissionResult, PlanningStore, SessionStore, SkillInfo, SkillRegistry, SplitPrompt, Tool, ToolContext,
    ToolDefinition, ToolError, ToolOutput, WorkspaceSandbox, FilePlanningStore, FileSessionStore,
    set_default_planning_store,
};
use fox_agent_tools::ToolExecutor;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info};

use crate::compaction::CompactionManager;
use crate::hooks::{HookContext, HookDecision, HookEvent, HookManager};
use crate::memory::{MemoryInjection, MemoryInjectionState, MemoryManager};
use crate::plugin::PluginManager;
use crate::prompt_builder::PromptBuilder;
use crate::safety::SafetySystem;
use crate::session::SessionState;

#[derive(Clone)]
pub struct Harness {
    pub cfg: FoxAgentSdkConfig,
    pub session_state: SessionState,
    tool_executor: ToolExecutor,
    pub memory_state: Arc<RwLock<MemoryInjectionState>>,
    pub memory_manager: MemoryManager,
    pub compaction_manager: Arc<RwLock<CompactionManager>>,
    pub safety_system: SafetySystem,
    pub prompt_builder: PromptBuilder,
    pub planning_store: Arc<dyn PlanningStore>,
    pub session_store: Arc<dyn SessionStore>,
    pub skill_registry: Arc<RwLock<SkillRegistry>>,
    pub interrupt_manager: Arc<RwLock<InterruptManager>>,
    pub hook_manager: Arc<RwLock<HookManager>>,
    pub plugin_manager: Arc<RwLock<PluginManager>>,
}

impl Harness {
    pub fn new(cfg: FoxAgentSdkConfig, working_dir: Option<PathBuf>) -> Self {
        let memory_cfg = cfg.memory.clone();
        let memory_enabled = memory_cfg.enabled;
        let compaction_cfg = cfg.compaction.clone();
        let safety_cfg = cfg.safety.clone();
        let session_store = resolve_session_store(&cfg, working_dir.as_deref());
        let planning_store = resolve_planning_store(&cfg, working_dir.as_deref());
        set_default_planning_store(planning_store.clone());
        let storage_root = resolve_storage_root(&cfg, working_dir.as_deref());
        let memory_manager = MemoryManager::new(memory_cfg.clone())
            .with_storage_dir(storage_root.join("memory"));
        let memory_state = Arc::new(RwLock::new(MemoryInjectionState::with_enabled(memory_enabled)));
        let session = SessionState::new(working_dir);
        info!(
            session_id = %session.id,
            memory_enabled = memory_enabled,
            compaction_enabled = compaction_cfg.enabled,
            "Harness created"
        );
        let version = env!("CARGO_PKG_VERSION").to_string();
        let git_hash = std::env::var("FOX_AGENT_GIT_HASH").unwrap_or_else(|_| "unknown".to_string());
        let mut prompt_builder = PromptBuilder::new(version, git_hash);
        if let Some(ref path) = cfg.global_agents_md_path {
            prompt_builder = prompt_builder.with_global_agents_md_path(path.clone());
        }
        let plugin_marketplaces = cfg.plugins.as_ref().map(|p| p.marketplaces.clone()).unwrap_or_default();
        let plugin_dir_path = storage_root.join("plugins");
        Self {
            cfg,
            session_state: session,
            tool_executor: ToolExecutor::new(),
            memory_state,
            memory_manager,
            compaction_manager: Arc::new(RwLock::new(CompactionManager::new(compaction_cfg))),
            safety_system: SafetySystem::new(safety_cfg),
            prompt_builder,
            planning_store,
            session_store,
            skill_registry: Arc::new(RwLock::new(SkillRegistry::default())),
            interrupt_manager: Arc::new(RwLock::new(InterruptManager::default())),
            hook_manager: Arc::new(RwLock::new(HookManager::new(
                HooksConfig::default(),
            ))),
            plugin_manager: Arc::new(RwLock::new(PluginManager::new(
                plugin_dir_path,
                plugin_marketplaces,
            ))),
        }
    }

    pub fn with_permission_hook(
        cfg: FoxAgentSdkConfig,
        working_dir: Option<PathBuf>,
        hook: impl Fn(&str, &serde_json::Value) -> PermissionResult + Send + Sync + 'static,
    ) -> Self {
        let memory_cfg = cfg.memory.clone();
        let memory_enabled = memory_cfg.enabled;
        let compaction_cfg = cfg.compaction.clone();
        let safety_cfg = cfg.safety.clone();
        let session_store = resolve_session_store(&cfg, working_dir.as_deref());
        let planning_store = resolve_planning_store(&cfg, working_dir.as_deref());
        set_default_planning_store(planning_store.clone());
        let storage_root = resolve_storage_root(&cfg, working_dir.as_deref());
        let memory_manager = MemoryManager::new(memory_cfg.clone())
            .with_storage_dir(storage_root.join("memory"));
        let memory_state = Arc::new(RwLock::new(MemoryInjectionState::with_enabled(memory_enabled)));
        let session = SessionState::new(working_dir);
        info!(
            session_id = %session.id,
            memory_enabled = memory_enabled,
            "Harness created with custom permission hook"
        );
        let version = env!("CARGO_PKG_VERSION").to_string();
        let git_hash = std::env::var("FOX_AGENT_GIT_HASH").unwrap_or_else(|_| "unknown".to_string());
        let mut prompt_builder = PromptBuilder::new(version, git_hash);
        if let Some(ref path) = cfg.global_agents_md_path {
            prompt_builder = prompt_builder.with_global_agents_md_path(path.clone());
        }
        let plugin_marketplaces = cfg.plugins.as_ref().map(|p| p.marketplaces.clone()).unwrap_or_default();
        let plugin_dir_path = storage_root.join("plugins");
        Self {
            cfg,
            session_state: session,
            tool_executor: ToolExecutor::new(),
            memory_state,
            memory_manager,
            compaction_manager: Arc::new(RwLock::new(CompactionManager::new(compaction_cfg))),
            safety_system: SafetySystem::with_permission_hook(safety_cfg, hook),
            prompt_builder,
            planning_store,
            session_store,
            skill_registry: Arc::new(RwLock::new(SkillRegistry::default())),
            interrupt_manager: Arc::new(RwLock::new(InterruptManager::default())),
            hook_manager: Arc::new(RwLock::new(HookManager::new(
                HooksConfig::default(),
            ))),
            plugin_manager: Arc::new(RwLock::new(PluginManager::new(
                plugin_dir_path,
                plugin_marketplaces,
            ))),
        }
    }

    pub async fn register_tool(&self, tool: Arc<dyn Tool>) {
        info!(name = %tool.name(), "Registering tool");
        self.tool_executor.register_tool(tool).await;
    }

    pub async fn register_default_tools(&self) {
        info!("Registering all default tools");
        fox_agent_tools::register_default_tools_with_planning_store(
            &self.tool_executor,
            self.planning_store.clone(),
        )
        .await;
    }

    pub fn tool_executor(&self) -> &ToolExecutor {
        &self.tool_executor
    }

    /// Set a workspace sandbox on the tool executor.
    /// All subsequent tool calls will be validated against this sandbox.
    pub async fn set_sandbox(&self, sandbox: WorkspaceSandbox) {
        self.tool_executor.set_sandbox(Some(sandbox)).await;
    }

    pub async fn tool_definitions(&self) -> Vec<ToolDefinition> {
        self.tool_executor.tool_definitions().await
    }

    pub async fn execute_tool(&self, name: &str, input: serde_json::Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        debug!(tool = %name, "Executing tool via harness");
        self.tool_executor.execute_tool(name, input, ctx).await
    }

    pub async fn check_tool_permission(&self, tool_name: &str, input: &serde_json::Value) -> PermissionResult {
        self.safety_system.check(tool_name, input)
    }

    pub async fn build_system_prompt_split(&self, memory_prompt: Option<&str>, active_skill: Option<&str>) -> (SplitPrompt, ContextInfo) {
        // Collect skill metadata for the static skills list
        let skills = {
            let reg = self.skill_registry.read().await;
            reg.list().into_iter().map(|s| SkillInfo { name: s.name, description: s.description }).collect::<Vec<_>>()
        };
        self.prompt_builder.build_split(
            &self.session_state.id,
            &self.planning_store,
            self.session_state.working_dir.as_deref(),
            &skills,
            memory_prompt,
            active_skill,
        )
    }

    pub async fn maybe_compact_messages(&mut self) -> Option<fox_agent_core::CompactionEvent> {
        self.compaction_manager.write().await.maybe_compact(&mut self.session_state.messages)
    }

    pub async fn take_memory_injection_for_prompt(&self) -> Option<(MemoryInjection, MemoryStateEvent)> {
        self.memory_state.write().await.take_pending()
    }

    pub fn trigger_memory_for_next_turn(&self) {
        let messages = self.session_state.messages.clone();
        self.memory_manager.trigger_recall_for_next_turn(messages, self.memory_state.clone());
    }

    pub async fn queue_soft_interrupt(&self, content: impl Into<String>, urgent: bool) {
        debug!("Queuing soft interrupt: urgent={urgent}");
        self.interrupt_manager.write().await.queue_soft_interrupt(content, urgent);
    }

    pub async fn request_graceful_shutdown(&self) {
        info!("Graceful shutdown requested");
        self.interrupt_manager.write().await.request_graceful_shutdown();
    }

    pub async fn take_pending_interrupts(&self) -> Vec<InjectedInterrupt> {
        self.interrupt_manager.write().await.take_pending_interrupts()
    }

    pub async fn is_graceful_shutdown_requested(&self) -> bool {
        self.interrupt_manager.read().await.is_graceful_shutdown_requested()
    }

    // ── Hook integration ──

    /// Load all hooks from project + global directories.
    pub async fn load_hooks(
        &self,
        storage_dir: &std::path::Path,
    ) -> usize {
        let config = self.cfg.hooks.clone().unwrap_or_default();
        if !config.enabled {
            return 0;
        }
        let mut hm = self.hook_manager.write().await;
        hm.load_all(
            storage_dir,
            self.session_state.working_dir.as_deref(),
            &config,
        )
    }

    /// Run PreToolUse hooks before a tool is executed.
    ///
    /// Returns `(allowed, block_reason, modified_input)`.
    pub async fn run_pre_tool_hooks(
        &self,
        tool_name: &str,
        input: &serde_json::Value,
    ) -> (bool, Option<String>, Option<serde_json::Value>) {
        let session_id = self.session_state.id.clone();
        let working_dir = self.session_state.working_dir
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let hm = self.hook_manager.read().await;
        let ctx = HookContext {
            session_id: &session_id,
            event: "pre-tool-use",
            working_dir: &working_dir,
            tool_name: Some(tool_name),
            tool_input: Some(input.clone()),
            tool_output: None,
            hook_event_name: "PreToolUse",
        };
        match hm.execute(HookEvent::PreToolUse, ctx).await {
            Ok(HookDecision::Allow { modified_input }) => (true, None, modified_input),
            Ok(HookDecision::Block { reason }) => (false, Some(reason), None),
            Ok(HookDecision::InjectContext { .. }) => (true, None, None),
            Err(e) => {
                tracing::warn!(error = %e, "PreToolUse hook error — allowing");
                (true, None, None)
            }
        }
    }

    /// Run PostToolUse hooks after a tool is executed.
    ///
    /// Returns `(allowed, block_reason)`.
    pub async fn run_post_tool_hooks(
        &self,
        tool_name: &str,
        tool_output_text: &str,
    ) -> (bool, Option<String>) {
        let session_id = self.session_state.id.clone();
        let working_dir = self.session_state.working_dir
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let hm = self.hook_manager.read().await;
        let ctx = HookContext {
            session_id: &session_id,
            event: "post-tool-use",
            working_dir: &working_dir,
            tool_name: Some(tool_name),
            tool_input: None,
            tool_output: Some(tool_output_text.to_string()),
            hook_event_name: "PostToolUse",
        };
        match hm.execute(HookEvent::PostToolUse, ctx).await {
            Ok(HookDecision::Allow { .. }) => (true, None),
            Ok(HookDecision::Block { reason }) => (false, Some(reason)),
            Ok(HookDecision::InjectContext { .. }) => (true, None),
            Err(e) => {
                tracing::warn!(error = %e, "PostToolUse hook error — allowing");
                (true, None)
            }
        }
    }

    /// Get hook prompt section for system prompt injection.
    pub async fn build_hook_prompt_section(&self) -> Option<String> {
        let hm = self.hook_manager.read().await;
        hm.build_prompt_section()
    }
}

fn resolve_storage_root(
    cfg: &FoxAgentSdkConfig,
    working_dir: Option<&std::path::Path>,
) -> std::path::PathBuf {
    // Resolve relative paths against working_dir
    if cfg.storage_dir.is_relative() {
        if let Some(dir) = working_dir {
            return dir.join(&cfg.storage_dir);
        }
    }
    cfg.storage_dir.clone()
}

fn resolve_session_store(
    cfg: &FoxAgentSdkConfig,
    working_dir: Option<&std::path::Path>,
) -> Arc<dyn SessionStore> {
    Arc::new(FileSessionStore::new(resolve_storage_root(cfg, working_dir).join("sessions")))
}

fn resolve_planning_store(
    cfg: &FoxAgentSdkConfig,
    working_dir: Option<&std::path::Path>,
) -> Arc<dyn PlanningStore> {
    Arc::new(FilePlanningStore::new(resolve_storage_root(cfg, working_dir).join("planning")))
}
