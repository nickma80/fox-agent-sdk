// Re-export public API from peer crates (facade).
pub use fox_agent_core::*;
pub use fox_agent_providers::*;
pub use fox_agent_tools::*;
pub use fox_agent_swarm::*;

// Internal modules (dependency order: infra → business → orchestration).
mod session;
mod memory;
mod safety;
mod compaction;
mod prompt_builder;
mod harness;
mod builder;
mod event_recorder;
mod approval_manager;
mod governance;
mod replay_runner;
mod scrub;
mod agent;
mod mcp;
mod hooks;
mod plugin;
mod swarm_runtime;

pub use session::*;
pub use memory::{MemoryInjection, MemoryInjectionEvent, MemoryInjectionState};
pub use safety::*;
pub use compaction::*;

pub use harness::*;
pub use builder::*;
pub use event_recorder::*;
pub use approval_manager::*;
pub use governance::*;
pub use replay_runner::*;
pub use scrub::*;
pub use agent::*;
pub use mcp::*;
pub use swarm_runtime::*;
pub use hooks::*;
pub use plugin::*;

#[cfg(test)]
mod tests;
