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
mod agent;
mod swarm_runtime;

pub use session::*;
pub use memory::{MemoryInjection, MemoryInjectionEvent, MemoryInjectionState};
pub use safety::*;
pub use compaction::*;
pub use prompt_builder::*;
pub use harness::*;
pub use agent::*;
pub use swarm_runtime::*;

#[cfg(test)]
mod tests;
