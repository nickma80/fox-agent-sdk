// Re-export public API from peer crates (facade).
pub use fox_agent_core::*;
pub use fox_agent_providers::*;
pub use fox_agent_swarm::*;
pub use fox_agent_tools::*;

// Internal modules (dependency order: infra → business → orchestration).
mod agent;
mod approval_manager;
mod artifact_store;
mod artifact_tool;
mod builder;
mod compaction;
pub mod eval;
mod event_recorder;
mod governance;
mod harness;
mod hooks;
mod mcp;
mod memory;
mod noise;
mod plugin;
mod prompt_builder;
mod replay_runner;
mod routing;
mod safety;
mod scrub;
mod session;
mod subagent;
mod swarm_runtime;

pub use artifact_tool::*;
pub use compaction::*;
pub use memory::{MemoryInjection, MemoryInjectionEvent, MemoryInjectionState};
pub use safety::*;
pub use session::*;

pub use agent::*;
pub use approval_manager::*;
pub use builder::*;
pub use event_recorder::*;
pub use governance::*;
pub use harness::*;
pub use hooks::*;
pub use mcp::*;
pub use plugin::*;
pub use replay_runner::*;
pub use scrub::*;
pub use swarm_runtime::*;

#[cfg(test)]
mod tests;
