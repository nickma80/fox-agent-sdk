//! Async generator for streaming Agent events to Python.
//!
//! [`PyEventStream`] implements Python's async iterator protocol
//! (`__aiter__` / `__anext__`) so that Python code can write:
//!
//! ```python
//! async for event in agent.run("hello"):
//!     print(event)
//! ```
//!
//! Each call to `__anext__` bridges a Rust [`tokio`] future onto
//! Python's asyncio event loop via [`pyo3_async_runtimes::tokio::future_into_py`].

use fox_agent_core::AgentEvent;
use pyo3::prelude::*;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// A Python async iterator that yields Agent events as dicts.
///
/// Created by [`super::agent::PyAgent::run`] and [`super::agent::PyAgent::resume`].
/// The underlying [`mpsc::Receiver`] is polled on the tokio runtime each
/// time Python's event loop calls `__anext__`.
#[pyclass(name = "EventStream", module = "fox_agent_sdk._core")]
pub struct PyEventStream {
    rx: Arc<tokio::sync::Mutex<mpsc::Receiver<AgentEvent>>>,
    exhausted: Arc<Mutex<bool>>,
}

impl PyEventStream {
    /// Wrap a receiver into a Python async iterator.
    pub fn new(rx: mpsc::Receiver<AgentEvent>) -> Self {
        Self {
            rx: Arc::new(tokio::sync::Mutex::new(rx)),
            exhausted: Arc::new(Mutex::new(false)),
        }
    }
}

#[pymethods]
impl PyEventStream {
    /// Return self as the async iterator.
    fn __aiter__(this: PyRef<'_, Self>) -> PyRef<'_, Self> {
        this
    }

    /// Return the next event as a Python awaitable.
    ///
    /// On the Python side this becomes:
    ///
    /// ```python
    /// event = await stream.__anext__()  # -> dict | raises StopAsyncIteration
    /// ```
    fn __anext__<'py>(this: PyRef<'py, Self>, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let rx = this.rx.clone();
        let exhausted = this.exhausted.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            // Fast-path: if we already exhausted the stream, raise immediately.
            if *exhausted.lock().unwrap() {
                return Err(pyo3::exceptions::PyStopAsyncIteration::new_err(""));
            }

            loop {
                let maybe_event = {
                    let mut guard = rx.lock().await;
                    guard.recv().await
                };

                match maybe_event {
                    Some(event) => {
                        // Convert to Python dict, skipping internal events.
                        let py_dict =
                            Python::with_gil(|py| crate::types::agent_event_to_py(py, &event));
                        if let Some(dict) = py_dict {
                            return Ok(dict);
                        }
                        // Internal event (Compaction, SubagentTaskStarted, etc.) —
                        // skip and poll the next event.
                    }
                    None => {
                        // Channel closed — agent turn finished.
                        *exhausted.lock().unwrap() = true;
                        return Err(pyo3::exceptions::PyStopAsyncIteration::new_err(""));
                    }
                }
            }
        })
    }
}
