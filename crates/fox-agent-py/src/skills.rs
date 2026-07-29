//! Skill system bindings for Claude Code-compatible skill management.
//!
//! Exposes Skill, SkillRegistry, and skill loading to Python developers.

use fox_agent_core::{Skill, SkillRegistry};
use pyo3::prelude::*;
use std::sync::Arc;

// ── PySkill ──

// ── PySkill ──

/// A Claude Code-compatible skill (YAML frontmatter + Markdown body).
///
/// Skills are loaded from `.claude/skills/` directories and provide
/// specialized prompts and tool restrictions for specific tasks.
#[pyclass(name = "Skill", module = "fox_agent_sdk._core")]
#[derive(Clone)]
pub struct PySkill {
    inner: Skill,
}

#[pymethods]
impl PySkill {
    #[getter]
    fn name(&self) -> &str {
        &self.inner.name
    }

    #[getter]
    fn description(&self) -> &str {
        &self.inner.description
    }

    #[getter]
    fn prompt(&self) -> &str {
        &self.inner.prompt
    }

    #[getter]
    fn allowed_tools(&self) -> Vec<String> {
        self.inner.allowed_tools.clone()
    }

    #[getter]
    fn model(&self) -> Option<&str> {
        self.inner.model.as_deref()
    }

    #[getter]
    fn version(&self) -> Option<&str> {
        self.inner.version.as_deref()
    }

    #[getter]
    fn disable_model_invocation(&self) -> bool {
        self.inner.disable_model_invocation
    }

    fn __repr__(&self) -> String {
        format!(
            "Skill(name='{}', description='{}')",
            self.inner.name, self.inner.description
        )
    }
}

impl PySkill {
    pub fn from_inner(skill: Skill) -> Self {
        Self { inner: skill }
    }
}

// ── PySkillRegistry ──

/// Registry of loaded skills, accessible via `agent.skill_registry`.
///
/// Provides list/get access. Skills are automatically loaded from
/// `.claude/skills/` when `with_default_tools()` is used.
#[pyclass(name = "SkillRegistry", module = "fox_agent_sdk._core")]
#[derive(Clone)]
pub struct PySkillRegistry {
    inner: Arc<tokio::sync::RwLock<SkillRegistry>>,
}

#[pymethods]
impl PySkillRegistry {
    /// List all currently loaded skills.
    fn list(&self, py: Python<'_>) -> Vec<Py<PySkill>> {
        let reg = self.inner.blocking_read();
        reg.list()
            .into_iter()
            .map(|s| PySkill::from_inner(s).into_pyobject(py).unwrap().unbind())
            .collect()
    }

    /// Get a skill by name, or None if not found.
    fn get(&self, name: &str) -> Option<PySkill> {
        let reg = self.inner.blocking_read();
        reg.get(name).cloned().map(PySkill::from_inner)
    }

    fn __repr__(&self) -> String {
        let reg = self.inner.blocking_read();
        format!("SkillRegistry(skills={})", reg.list().len())
    }
}

impl PySkillRegistry {
    pub fn new(inner: Arc<tokio::sync::RwLock<SkillRegistry>>) -> Self {
        Self { inner }
    }
}
