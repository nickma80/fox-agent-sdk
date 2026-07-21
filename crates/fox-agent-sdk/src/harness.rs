use fox_agent_core::{
    ContextInfo, FoxAgentSdkConfig, HooksConfig, InterruptManager, InjectedInterrupt, MemoryStateEvent,
    PermissionResult, PlanningStore, SessionStore, SkillInfo, SkillRegistry, SplitPrompt, Tool, ToolContext,
    ToolDefinition, ToolError, ToolOutput, WorkspaceSandbox, FilePlanningStore, FileSessionStore,
    set_default_planning_store, Role,
};
use fox_agent_tools::ToolExecutor;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tracing::{debug, info};

use crate::compaction::CompactionManager;
use crate::hooks::{HookContext, HookDecision, HookEvent, HookManager};
use crate::memory::{MemoryInjection, MemoryInjectionState, MemoryManager};
use crate::plugin::PluginManager;
use crate::prompt_builder::PromptBuilder;
use crate::safety::SafetySystem;
use crate::session::SessionState;

/// Cache key for read tool deduplication: (file_path, offset, limit).
type ReadCacheKey = (String, usize, usize);

/// Cached read result with timestamp.
struct CachedRead {
    text: String,
    at: Instant,
}

#[derive(Clone)]
pub struct Harness {
    pub cfg: FoxAgentSdkConfig,
    /// Mutable conversation state, behind interior mutability so turn-driving
    /// methods can take `&self` (letting callers hold a read lock during a
    /// turn instead of an exclusive write lock). Cloning a `Harness` shares
    /// this `Arc`; use [`Harness::fork_session_state`] when an independent
    /// session is required (e.g. swarm workers).
    pub session_state: Arc<RwLock<SessionState>>,
    /// Immutable session id, hoisted for cheap synchronous access without
    /// locking `session_state`.
    session_id: String,
    /// Immutable working directory, hoisted for cheap synchronous access.
    session_working_dir: Option<PathBuf>,
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
    /// The user's first message in this session, captured for compaction
    /// pinning (global session context). Never updated after first capture.
    pub first_user_message: Arc<RwLock<Option<String>>>,
    /// The user's most recent message, updated on every new user input.
    /// Used by Intent Guard, Intent Anchor prompt injection, and Drift Detection
    /// to keep the agent focused on the CURRENT task (not the original one).
    pub latest_user_message: Arc<RwLock<Option<String>>>,
    /// Simple read-tool cache keyed by (file_path, offset, limit) to avoid
    /// re-reading the same file segment within a 60s window.
    read_cache: Arc<RwLock<HashMap<ReadCacheKey, CachedRead>>>,
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
        let session = SessionState::new(working_dir);
        let memory_manager = MemoryManager::new(memory_cfg.clone())
            .with_storage_dir(storage_root.join("memory"))
            .with_session_id(session.id.clone());
        let memory_state = Arc::new(RwLock::new(MemoryInjectionState::with_enabled(memory_enabled)));
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
        let plugin_proxy = cfg.proxy.clone();
        Self {
            cfg,
            session_id: session.id.clone(),
            session_working_dir: session.working_dir.clone(),
            session_state: Arc::new(RwLock::new(session)),
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
            ).with_proxy(plugin_proxy))),
            first_user_message: Arc::new(RwLock::new(None)),
            latest_user_message: Arc::new(RwLock::new(None)),
            read_cache: Arc::new(RwLock::new(HashMap::new())),
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
        let session = SessionState::new(working_dir);
        let memory_manager = MemoryManager::new(memory_cfg.clone())
            .with_storage_dir(storage_root.join("memory"))
            .with_session_id(session.id.clone());
        let memory_state = Arc::new(RwLock::new(MemoryInjectionState::with_enabled(memory_enabled)));
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
        let plugin_proxy = cfg.proxy.clone();
        Self {
            cfg,
            session_id: session.id.clone(),
            session_working_dir: session.working_dir.clone(),
            session_state: Arc::new(RwLock::new(session)),
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
            ).with_proxy(plugin_proxy))),
            first_user_message: Arc::new(RwLock::new(None)),
            latest_user_message: Arc::new(RwLock::new(None)),
            read_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn register_tool(&self, tool: Arc<dyn Tool>) {
        info!(name = %tool.name(), "Registering tool");
        self.tool_executor.register_tool(tool).await;
    }

    // ── Session state accessors ──
    //
    // `session_state` is behind an `Arc<RwLock<..>>` so turn-driving methods
    // can take `&self`. These helpers keep call sites concise and centralise
    // the locking discipline (never hold a guard across an `.await` on the
    // model, except the take/write-back pattern in `maybe_compact_messages`).

    /// Immutable session id (no locking).
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Immutable working directory (no locking).
    pub fn session_working_dir(&self) -> Option<&PathBuf> {
        self.session_working_dir.as_ref()
    }

    /// Append a message to the session (both working context and full transcript).
    ///
    /// On every **real** user message (not system-injected interrupts),
    /// captures it as the `latest_user_message` and (once) `first_user_message`.
    ///
    /// System-injected messages starting with "Interrupt: " are skipped so
    /// drift-detection and intent-anchor prompt slots always reference the
    /// user's actual task, not stale injected reminders.
    pub async fn push_message(&self, msg: fox_agent_core::Message) {
        if msg.role == Role::User {
            let text: String = msg.content.iter().filter_map(|b| match b {
                fox_agent_core::ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            }).collect::<Vec<_>>().join(" ");
            if !text.is_empty() && !text.starts_with("Interrupt: ") {
                *self.latest_user_message.write().await = Some(text.clone());
                let mut first = self.first_user_message.write().await;
                if first.is_none() {
                    *first = Some(text);
                }
            }
        }
        self.session_state.write().await.push_message(msg);
    }

    /// Get the latest user message (current task), if any.
    /// Used by Intent Guard, Intent Anchor prompt, and Drift Detection.
    pub async fn latest_user_message_text(&self) -> Option<String> {
        self.latest_user_message.read().await.clone()
    }

    /// Get the first user message (session context), if any.
    /// Used by compaction pinning for global session continuity.
    pub async fn first_user_message_text(&self) -> Option<String> {
        self.first_user_message.read().await.clone()
    }

    /// Deprecated alias — returns the latest user message.
    /// Kept for backward compatibility with existing callers.
    pub async fn intent_anchor_text(&self) -> Option<String> {
        self.latest_user_message_text().await
    }

    /// Scan session messages to repopulate `first_user_message` and
    /// `latest_user_message`. Called after session restore so Intent Guard
    /// and Intent Anchor work correctly on restored sessions.
    pub async fn repopulate_user_message_tracking(&self) {
        let messages = self.session_state.read().await.messages.clone();
        Self::scan_user_messages(&messages, &self.first_user_message, &self.latest_user_message);
    }

    /// Sync version: repopulate from an existing slice of messages.
    /// Uses `try_write` since there's no contention at restore time.
    pub fn repopulate_user_message_tracking_sync(&self, messages: &[fox_agent_core::Message]) {
        Self::scan_user_messages(messages, &self.first_user_message, &self.latest_user_message);
    }

    fn scan_user_messages(
        messages: &[fox_agent_core::Message],
        first_out: &Arc<RwLock<Option<String>>>,
        latest_out: &Arc<RwLock<Option<String>>>,
    ) {
        let mut first: Option<String> = None;
        let mut latest: Option<String> = None;
        for msg in messages {
            if msg.role == Role::User {
                let text: String = msg.content.iter().filter_map(|b| match b {
                    fox_agent_core::ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                }).collect::<Vec<_>>().join(" ");
                if !text.is_empty() && !text.starts_with("Interrupt: ") {
                    if first.is_none() {
                        first = Some(text.clone());
                    }
                    latest = Some(text);
                }
            }
        }
        if let Some(t) = first {
            *first_out.try_write().expect("first_user_message lock uncontended at restore time") = Some(t);
        }
        if let Some(t) = latest {
            *latest_out.try_write().expect("latest_user_message lock uncontended at restore time") = Some(t);
        }
    }

    /// Clone the current working-context messages.
    pub async fn session_messages(&self) -> Vec<fox_agent_core::Message> {
        self.session_state.read().await.messages.clone()
    }

    /// Clone the full un-compacted transcript (for restore / display).
    pub async fn full_messages(&self) -> Vec<fox_agent_core::Message> {
        self.session_state.read().await.full_messages.clone()
    }

    /// Read-lock the session state (for building a persistence snapshot etc.).
    pub async fn session_state_read(
        &self,
    ) -> tokio::sync::RwLockReadGuard<'_, SessionState> {
        self.session_state.read().await
    }

    /// Replace the session state (used when restoring from a snapshot).
    /// Rebuilds the `Arc` so external clones are not affected, and keeps the
    /// hoisted id/working_dir and memory manager session id in sync. Takes
    /// `&mut self` because the owner has exclusive access during restore.
    pub fn reset_session_state(&mut self, state: SessionState) {
        self.session_id = state.id.clone();
        self.session_working_dir = state.working_dir.clone();
        self.memory_manager = self.memory_manager.clone().with_session_id(state.id.clone());
        self.session_state = Arc::new(RwLock::new(state));
    }

    /// Create an independent copy of this harness whose `session_state` is a
    /// fresh, separate `Arc` (not shared with `self`). Used by swarm workers
    /// so their conversation does not pollute the parent session.
    pub async fn fork_session_state(&self) -> Harness {
        let cloned_state = self.session_state.read().await.clone();
        let mut forked = self.clone();
        forked.session_state = Arc::new(RwLock::new(cloned_state));
        forked
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

    /// Execute a tool with read-cache deduplication for the `read` tool.
    ///
    /// If the same (file_path, offset, limit) was read within the last 60s,
    /// returns the cached result with a `[CACHED]` prefix to save I/O and
    /// avoid re-filling the context window with duplicate content.
    pub async fn execute_tool_with_cache(&self, name: &str, input: serde_json::Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        if name == "read" {
            let file_path = input.get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let offset = input.get("offset")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;
            let limit = input.get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(300) as usize;
            let key: ReadCacheKey = (file_path, offset, limit);

            // Check cache
            {
                let cache = self.read_cache.read().await;
                if let Some(cached) = cache.get(&key) {
                    if cached.at.elapsed() < std::time::Duration::from_secs(60) {
                        debug!(
                            file = %key.0,
                            offset = key.1,
                            limit = key.2,
                            age_secs = cached.at.elapsed().as_secs(),
                            "Read cache hit"
                        );
                        return Ok(ToolOutput {
                            text: format!(
                                "[CACHED — read {}s ago]\n{}",
                                cached.at.elapsed().as_secs(),
                                cached.text
                            ),
                            is_error: false,
                            json: None,
                        });
                    }
                }
            }

            // Execute
            let output = self.tool_executor.execute_tool(name, input, ctx).await?;

            // Store in cache (only if successful and not too large)
            if !output.is_error && output.text.len() < 100_000 {
                self.read_cache.write().await.insert(
                    key,
                    CachedRead {
                        text: output.text.clone(),
                        at: Instant::now(),
                    },
                );
            }
            Ok(output)
        } else {
            self.tool_executor.execute_tool(name, input, ctx).await
        }
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
        let intent_anchor = self.intent_anchor_text().await;
        let narrative_prompt = self.memory_manager.core().build_narrative_prompt(20);
        self.prompt_builder.build_split(
            &self.session_id,
            &self.planning_store,
            self.session_working_dir.as_deref(),
            &skills,
            memory_prompt,
            active_skill,
            intent_anchor.as_deref(),
            narrative_prompt.as_deref(),
        )
    }

    pub async fn maybe_compact_messages<F>(
        &self,
        summarizer: F,
        mode: crate::compaction::CompactionMode,
        turn_start: u64,
        turn_end: u64,
    ) -> Option<(fox_agent_core::CompactionEvent, Vec<fox_agent_core::NarrativeRecord>)>
    where
        F: FnOnce(Vec<fox_agent_core::Message>) -> crate::compaction::SummarizerFuture,
    {
        // Take the message vec out under a short write guard so the summarizer
        // (an async LLM call) does not run while holding the session lock,
        // then write the compacted result back.
        let mut messages = {
            let mut ss = self.session_state.write().await;
            std::mem::take(&mut ss.messages)
        };
        let (event, narratives) = self
            .compaction_manager
            .write()
            .await
            .maybe_compact(&mut messages, summarizer, mode, turn_start, turn_end)
            .await
            .unzip();
        {
            let mut ss = self.session_state.write().await;
            ss.messages = messages;
        }
        event.map(|e| (e, narratives.unwrap_or_default()))
    }

    pub async fn take_memory_injection_for_prompt(&self) -> Option<(MemoryInjection, MemoryStateEvent)> {
        self.memory_state.write().await.take_pending()
    }

    pub async fn trigger_memory_for_next_turn(&self) {
        let messages = self.session_state.read().await.messages.clone();
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

    /// Clear the graceful-shutdown flag so a new user turn can proceed.
    pub async fn clear_graceful_shutdown(&self) {
        self.interrupt_manager.write().await.clear_graceful_shutdown();
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
            self.session_working_dir.as_deref(),
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
        let session_id = self.session_id.clone();
        let working_dir = self.session_working_dir
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
        let session_id = self.session_id.clone();
        let working_dir = self.session_working_dir
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
