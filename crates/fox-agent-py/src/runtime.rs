//! Global tokio runtime singleton bridged to Python's asyncio event loop.
//!
//! All async operations in fox-agent-py go through this runtime.
//! The runtime is initialized lazily on first use and reused for the
//! lifetime of the Python process.

use std::sync::OnceLock;
use tokio::runtime::Runtime;

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

/// Get (or initialize) the global tokio multi-thread runtime.
///
/// Returns a reference to the runtime if initialization succeeded,
/// or None if a tokio runtime could not be created.
pub fn get_runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| {
        Runtime::new().expect("failed to create tokio runtime for fox-agent-py")
    })
}

/// Run a future on the global tokio runtime, returning the result directly.
///
/// This is used by `#[pyfunction]` async methods — PyO3 + pyo3-asyncio
/// will call `get_runtime().block_on(...)` instead of this function.
/// Use this only for non-async Python code that needs to call into Rust async.
#[allow(dead_code)]
pub fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    get_runtime().block_on(fut)
}
