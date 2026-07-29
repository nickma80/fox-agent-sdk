//! Python bindings for the Swarm multi-agent system.
//!
//! Exposes [`SwarmCoordinator`], [`SwarmSupervisor`] and supporting
//! types so Python users can coordinate multiple worker agents.

use fox_agent_sdk::{
    AgentReport as RustAgentReport, RetryPolicy as RustRetryPolicy,
    SwarmCoordinator as RustSwarmCoordinator,
    SwarmSupervisor as RustSwarmSupervisor,
    SwarmSummaryReport as RustSwarmSummaryReport,
    WorkerHandle as RustWorkerHandle, WorkerStatus as RustWorkerStatus,
};
use pyo3::prelude::*;
use std::sync::Arc;
use std::time::Duration;

// ── PyWorkerStatus ──

/// The status of a swarm worker.
#[pyclass(name = "WorkerStatus", module = "fox_agent_sdk._core")]
#[derive(Clone)]
pub struct PyWorkerStatus {
    #[allow(dead_code)]
    inner: RustWorkerStatus,
}

#[pymethods]
impl PyWorkerStatus {
    #[classattr]
    const READY: &'static str = "ready";

    #[classattr]
    const RUNNING: &'static str = "running";

    #[classattr]
    const BLOCKED: &'static str = "blocked";

    #[classattr]
    const COMPLETED: &'static str = "completed";

    #[classattr]
    const FAILED: &'static str = "failed";

    #[classattr]
    const TIMED_OUT: &'static str = "timed_out";

    fn __repr__(&self) -> String {
        format!("WorkerStatus({:?})", self.inner)
    }
}

// ── PyWorkerHandle ──

/// A handle to a worker registered in the swarm.
#[pyclass(name = "WorkerHandle", module = "fox_agent_sdk._core")]
#[derive(Clone)]
pub struct PyWorkerHandle {
    inner: RustWorkerHandle,
}

#[pymethods]
impl PyWorkerHandle {
    #[getter]
    fn worker_id(&self) -> &str {
        &self.inner.worker_id
    }

    #[getter]
    fn prompt(&self) -> &str {
        &self.inner.prompt
    }

    #[getter]
    fn status(&self) -> String {
        format!("{:?}", self.inner.status).to_lowercase()
    }

    #[getter]
    fn started_at_secs(&self) -> Option<u64> {
        self.inner.started_at_secs
    }

    fn __repr__(&self) -> String {
        format!(
            "WorkerHandle(id='{}', status={})",
            self.inner.worker_id,
            format!("{:?}", self.inner.status).to_lowercase()
        )
    }
}

impl From<RustWorkerHandle> for PyWorkerHandle {
    fn from(h: RustWorkerHandle) -> Self {
        Self { inner: h }
    }
}

// ── PyAgentReport ──

/// A report submitted by a worker upon task completion.
#[pyclass(name = "AgentReport", module = "fox_agent_sdk._core")]
#[derive(Clone)]
pub struct PyAgentReport {
    inner: RustAgentReport,
}

#[pymethods]
impl PyAgentReport {
    #[getter]
    fn worker_id(&self) -> &str {
        &self.inner.worker_id
    }

    #[getter]
    fn task_id(&self) -> Option<&str> {
        self.inner.task_id.as_deref()
    }

    #[getter]
    fn status(&self) -> String {
        format!("{:?}", self.inner.status).to_lowercase()
    }

    #[getter]
    fn summary(&self) -> &str {
        &self.inner.summary
    }

    fn __repr__(&self) -> String {
        format!(
            "AgentReport(worker='{}', status={})",
            self.inner.worker_id,
            format!("{:?}", self.inner.status).to_lowercase()
        )
    }
}

impl From<RustAgentReport> for PyAgentReport {
    fn from(r: RustAgentReport) -> Self {
        Self { inner: r }
    }
}

// ── PySwarmSummaryReport ──

/// A summary report aggregating all workers in the swarm.
#[pyclass(name = "SwarmSummaryReport", module = "fox_agent_sdk._core")]
#[derive(Clone)]
pub struct PySwarmSummaryReport {
    inner: RustSwarmSummaryReport,
}

#[pymethods]
impl PySwarmSummaryReport {
    #[getter]
    fn total_workers(&self) -> u32 {
        self.inner.total_workers
    }

    #[getter]
    fn completed(&self) -> u32 {
        self.inner.completed
    }

    #[getter]
    fn failed(&self) -> u32 {
        self.inner.failed
    }

    #[getter]
    fn timed_out(&self) -> u32 {
        self.inner.timed_out
    }

    #[getter]
    fn tasks_reassigned(&self) -> u32 {
        self.inner.tasks_reassigned
    }

    /// All worker reports as a list of [`AgentReport`] objects.
    #[getter]
    fn worker_reports(&self) -> Vec<PyAgentReport> {
        self.inner
            .worker_reports
            .iter()
            .map(|r| PyAgentReport::from(r.clone()))
            .collect()
    }

    /// True if all workers have reached a terminal state.
    fn all_terminal(&self) -> bool {
        self.inner.all_terminal()
    }

    /// Human-readable summary string.
    fn format(&self) -> String {
        self.inner.format()
    }

    fn __repr__(&self) -> String {
        format!(
            "SwarmSummaryReport(total={}, completed={}, failed={}, timed_out={})",
            self.inner.total_workers,
            self.inner.completed,
            self.inner.failed,
            self.inner.timed_out
        )
    }
}

impl From<RustSwarmSummaryReport> for PySwarmSummaryReport {
    fn from(r: RustSwarmSummaryReport) -> Self {
        Self { inner: r }
    }
}

// ── PyRetryPolicy ──

/// Retry policy for swarm worker tasks.
#[pyclass(name = "RetryPolicy", module = "fox_agent_sdk._core")]
#[derive(Clone)]
pub struct PyRetryPolicy {
    inner: RustRetryPolicy,
}

#[pymethods]
impl PyRetryPolicy {
    #[new]
    #[pyo3(signature = (
        max_retries = 3,
        retry_delay_secs = 2,
        reassign_on_exhausted = true,
        task_timeout_secs = 300,
        health_check_interval_secs = 5
    ))]
    fn new(
        max_retries: u32,
        retry_delay_secs: u64,
        reassign_on_exhausted: bool,
        task_timeout_secs: u64,
        health_check_interval_secs: u64,
    ) -> Self {
        Self {
            inner: RustRetryPolicy {
                max_retries,
                retry_delay_secs,
                reassign_on_exhausted,
                task_timeout_secs,
                health_check_interval_secs,
            },
        }
    }

    #[getter]
    fn max_retries(&self) -> u32 {
        self.inner.max_retries
    }

    #[getter]
    fn retry_delay_secs(&self) -> u64 {
        self.inner.retry_delay_secs
    }

    #[getter]
    fn reassign_on_exhausted(&self) -> bool {
        self.inner.reassign_on_exhausted
    }

    #[getter]
    fn task_timeout_secs(&self) -> u64 {
        self.inner.task_timeout_secs
    }

    #[getter]
    fn health_check_interval_secs(&self) -> u64 {
        self.inner.health_check_interval_secs
    }

    fn __repr__(&self) -> String {
        format!(
            "RetryPolicy(max_retries={}, timeout={}s)",
            self.inner.max_retries, self.inner.task_timeout_secs
        )
    }
}

impl PyRetryPolicy {
    pub(crate) fn into_inner(self) -> RustRetryPolicy {
        self.inner
    }
}

// ── Helpers ──

fn pynone(py: Python<'_>) -> Py<PyAny> {
    py.None()
}

// ── PySwarmCoordinator ──

/// Central coordinator for swarm multi-agent orchestration.
///
/// Manages the shared plan, worker registry, task assignments,
/// and inter-worker messaging.
#[pyclass(name = "SwarmCoordinator", module = "fox_agent_sdk._core")]
pub struct PySwarmCoordinator {
    inner: Arc<RustSwarmCoordinator>,
}

#[pymethods]
impl PySwarmCoordinator {
    /// Create a new empty swarm coordinator.
    #[new]
    fn new() -> Self {
        Self {
            inner: Arc::new(RustSwarmCoordinator::new()),
        }
    }

    /// Register a new worker in the swarm.
    ///
    /// Returns a [`WorkerHandle`] representing the registered worker.
    fn spawn<'py>(
        &self,
        worker_id: String,
        prompt: String,
        py: Python<'py>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let coordinator = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py::<_, Py<PyAny>>(py, async move {
            let handle = coordinator.spawn(worker_id, prompt).await;
            Python::with_gil(|py| {
                let obj = PyWorkerHandle::from(handle).into_pyobject(py).unwrap();
                Ok(obj.into_any().unbind())
            })
        })
    }

    /// List all registered workers.
    fn list_workers<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let coordinator = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py::<_, Py<PyAny>>(py, async move {
            let workers = coordinator.list_workers().await;
            Python::with_gil(|py| {
                let list = pyo3::types::PyList::empty(py);
                for w in workers {
                    list.append(PyWorkerHandle::from(w).into_pyobject(py).unwrap())?;
                }
                Ok(list.into_any().unbind())
            })
        })
    }

    /// Wait until at least `expected_count` workers have registered,
    /// or `timeout_secs` elapses.
    fn await_members<'py>(
        &self,
        expected_count: usize,
        timeout_secs: u64,
        py: Python<'py>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let coordinator = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py::<_, Py<PyAny>>(py, async move {
            let result = coordinator
                .await_members(expected_count, Duration::from_secs(timeout_secs))
                .await;
            Python::with_gil(|py| match result {
                Some(workers) => {
                    let list = pyo3::types::PyList::empty(py);
                    for w in workers {
                        list.append(PyWorkerHandle::from(w).into_pyobject(py).unwrap())?;
                    }
                    Ok(list.into_any().unbind())
                }
                None => Ok(pynone(py)),
            })
        })
    }

    /// Assign the next runnable (unblocked) task to a worker.
    ///
    /// Returns the task description string, or `None` if no task is available.
    fn assign_next_task<'py>(
        &self,
        worker_id: String,
        py: Python<'py>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let coordinator = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py::<_, Py<PyAny>>(py, async move {
            let item = coordinator.assign_next_runnable_task(&worker_id).await;
            Python::with_gil(|py| match item {
                Some(plan_item) => {
                    Ok(plan_item.content.into_pyobject(py).unwrap().into_any().unbind())
                }
                None => Ok(pynone(py)),
            })
        })
    }

    /// Record a task completion report from a worker.
    fn report_completion<'py>(
        &self,
        worker_id: String,
        task_id: String,
        summary: String,
        py: Python<'py>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let coordinator = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py::<_, Py<PyAny>>(py, async move {
            let report = coordinator.report_completion(&worker_id, &task_id, summary).await;
            Python::with_gil(|py| match report {
                Some(r) => {
                    let obj = PyAgentReport::from(r).into_pyobject(py).unwrap();
                    Ok(obj.into_any().unbind())
                }
                None => Ok(pynone(py)),
            })
        })
    }

    /// Get all completion reports so far.
    fn reports<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let coordinator = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py::<_, Py<PyAny>>(py, async move {
            let reports = coordinator.reports().await;
            Python::with_gil(|py| {
                let list = pyo3::types::PyList::empty(py);
                for r in reports {
                    list.append(PyAgentReport::from(r).into_pyobject(py).unwrap())?;
                }
                Ok(list.into_any().unbind())
            })
        })
    }

    /// Broadcast a message from one worker to all others.
    fn broadcast<'py>(
        &self,
        from_worker_id: String,
        content: String,
        py: Python<'py>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let coordinator = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py::<_, Py<PyAny>>(py, async move {
            let _msgs = coordinator.broadcast(&from_worker_id, content).await;
            Python::with_gil(|py| Ok(pynone(py)))
        })
    }

    fn __repr__(&self) -> String {
        "SwarmCoordinator()".to_string()
    }
}

// ── PySwarmSupervisor ──

/// Supervisor that monitors worker health, handles retries,
/// and generates summary reports.
#[pyclass(name = "SwarmSupervisor", module = "fox_agent_sdk._core")]
pub struct PySwarmSupervisor {
    inner: Arc<RustSwarmSupervisor>,
}

#[pymethods]
impl PySwarmSupervisor {
    /// Create a new supervisor with the given coordinator and retry policy.
    #[new]
    fn new(coordinator: &PySwarmCoordinator, policy: PyRetryPolicy) -> Self {
        Self {
            inner: Arc::new(RustSwarmSupervisor::new(
                coordinator.inner.clone(),
                policy.into_inner(),
            )),
        }
    }

    /// Create a supervisor with default retry policy.
    #[staticmethod]
    fn with_defaults(coordinator: &PySwarmCoordinator) -> Self {
        Self {
            inner: Arc::new(RustSwarmSupervisor::with_defaults(
                coordinator.inner.clone(),
            )),
        }
    }

    /// Run the background health-check loop.
    ///
    /// Returns when all workers have reached a terminal state.
    fn run_health_loop<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let supervisor = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py::<_, Py<PyAny>>(py, async move {
            supervisor.run_health_loop().await;
            Python::with_gil(|py| Ok(pynone(py)))
        })
    }

    /// Generate a summary report from the current coordinator state.
    fn generate_summary<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let supervisor = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py::<_, Py<PyAny>>(py, async move {
            let report = supervisor.generate_summary().await;
            Python::with_gil(|py| {
                let obj = PySwarmSummaryReport::from(report).into_pyobject(py).unwrap();
                Ok(obj.into_any().unbind())
            })
        })
    }

    /// Block until all workers complete, then return the summary.
    fn await_completion<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let supervisor = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py::<_, Py<PyAny>>(py, async move {
            let report = supervisor.await_completion().await;
            Python::with_gil(|py| {
                let obj = PySwarmSummaryReport::from(report).into_pyobject(py).unwrap();
                Ok(obj.into_any().unbind())
            })
        })
    }

    fn __repr__(&self) -> String {
        "SwarmSupervisor()".to_string()
    }
}
