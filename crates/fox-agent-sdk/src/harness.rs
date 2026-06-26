use fox_agent_core::{
    ContextInfo, FoxAgentSdkConfig, InterruptManager, InjectedInterrupt, MemoryStateEvent,
    PermissionResult, PlanningStore, SessionStore, SkillInfo, SkillRegistry, SplitPrompt, Tool, ToolContext,
    ToolDefinition, ToolError, ToolOutput, WorkspaceSandbox, FilePlanningStore, FileSessionStore,
    InMemoryPlanningStore, InMemorySessionStore, set_default_planning_store,
};
use fox_agent_tools::ToolExecutor;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info};

use crate::compaction::CompactionManager;
use crate::memory::{MemoryInjection, MemoryInjectionState, MemoryManager};
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
}

impl Harness {
    pub fn new(cfg: FoxAgentSdkConfig, working_dir: Option<PathBuf>) -> Self {
        let memory_cfg = cfg.memory.clone();
        let compaction_cfg = cfg.compaction.clone();
        let safety_cfg = cfg.safety.clone();
        let session_store = resolve_session_store(&cfg, working_dir.as_deref());
        let planning_store = resolve_planning_store(&cfg, working_dir.as_deref());
        set_default_planning_store(planning_store.clone());
        let memory_state = Arc::new(RwLock::new(MemoryInjectionState::with_enabled(memory_cfg.enabled)));
        let session = SessionState::new(working_dir);
        info!(
            session_id = %session.id,
            memory_enabled = memory_cfg.enabled,
            compaction_enabled = compaction_cfg.enabled,
            "Harness created"
        );
        let version = env!("CARGO_PKG_VERSION").to_string();
        let git_hash = std::env::var("FOX_AGENT_GIT_HASH").unwrap_or_else(|_| "unknown".to_string());
        Self {
            cfg,
            session_state: session,
            tool_executor: ToolExecutor::new(),
            memory_state,
            memory_manager: MemoryManager::new(memory_cfg),
            compaction_manager: Arc::new(RwLock::new(CompactionManager::new(compaction_cfg))),
            safety_system: SafetySystem::new(safety_cfg),
            prompt_builder: PromptBuilder::new(version, git_hash),
            planning_store,
            session_store,
            skill_registry: Arc::new(RwLock::new(SkillRegistry::default())),
            interrupt_manager: Arc::new(RwLock::new(InterruptManager::default())),
        }
    }

    pub fn with_permission_hook(
        cfg: FoxAgentSdkConfig,
        working_dir: Option<PathBuf>,
        hook: impl Fn(&str, &serde_json::Value) -> PermissionResult + Send + Sync + 'static,
    ) -> Self {
        let memory_cfg = cfg.memory.clone();
        let compaction_cfg = cfg.compaction.clone();
        let safety_cfg = cfg.safety.clone();
        let session_store = resolve_session_store(&cfg, working_dir.as_deref());
        let planning_store = resolve_planning_store(&cfg, working_dir.as_deref());
        set_default_planning_store(planning_store.clone());
        let memory_state = Arc::new(RwLock::new(MemoryInjectionState::with_enabled(memory_cfg.enabled)));
        let session = SessionState::new(working_dir);
        info!(
            session_id = %session.id,
            memory_enabled = memory_cfg.enabled,
            "Harness created with custom permission hook"
        );
        let version = env!("CARGO_PKG_VERSION").to_string();
        let git_hash = std::env::var("FOX_AGENT_GIT_HASH").unwrap_or_else(|_| "unknown".to_string());
        Self {
            cfg,
            session_state: session,
            tool_executor: ToolExecutor::new(),
            memory_state,
            memory_manager: MemoryManager::new(memory_cfg),
            compaction_manager: Arc::new(RwLock::new(CompactionManager::new(compaction_cfg))),
            safety_system: SafetySystem::with_permission_hook(safety_cfg, hook),
            prompt_builder: PromptBuilder::new(version, git_hash),
            planning_store,
            session_store,
            skill_registry: Arc::new(RwLock::new(SkillRegistry::default())),
            interrupt_manager: Arc::new(RwLock::new(InterruptManager::default())),
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
}

fn resolve_session_store(
    cfg: &FoxAgentSdkConfig,
    working_dir: Option<&std::path::Path>,
) -> Arc<dyn SessionStore> {
    if let Some(dir) = &cfg.session_storage_dir {
        return Arc::new(FileSessionStore::new(dir.clone()));
    }
    if let Some(dir) = working_dir {
        return Arc::new(FileSessionStore::new(
            dir.join(".fox-agent-sdk").join("sessions"),
        ));
    }
    Arc::new(InMemorySessionStore::default())
}

fn resolve_planning_store(
    cfg: &FoxAgentSdkConfig,
    working_dir: Option<&std::path::Path>,
) -> Arc<dyn PlanningStore> {
    if let Some(dir) = &cfg.planning_storage_dir {
        return Arc::new(FilePlanningStore::new(dir.clone()));
    }
    if let Some(dir) = working_dir {
        return Arc::new(FilePlanningStore::new(
            dir.join(".fox-agent-sdk").join("planning"),
        ));
    }
    Arc::new(InMemoryPlanningStore::default())
}
