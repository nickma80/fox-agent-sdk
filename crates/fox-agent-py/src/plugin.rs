//! Python bindings for the Plugin system.
//!
//! Plugins extend the agent with additional skills, tools, and
//! configuration. The PluginManager handles discovery, installation,
//! and marketplace integration.

use fox_agent_sdk::{MarketplaceConfig, PluginManifest, PluginsConfig};
use pyo3::prelude::*;

// ── PyPluginsConfig ──

/// Configuration for the plugins system.
#[pyclass(name = "PluginsConfig", module = "fox_agent_sdk._core")]
#[derive(Clone)]
pub struct PyPluginsConfig {
    inner: PluginsConfig,
}

#[pymethods]
impl PyPluginsConfig {
    #[new]
    #[pyo3(signature = (enabled=true, auto_update_hours=0))]
    fn new(enabled: bool, auto_update_hours: u64) -> Self {
        Self {
            inner: PluginsConfig {
                enabled,
                auto_update_hours,
                preinstall: vec![],
                marketplaces: vec![],
            },
        }
    }

    /// Add a plugin name to preinstall on startup.
    fn add_preinstall(&mut self, name: String) {
        self.inner.preinstall.push(name);
    }

    /// Add a marketplace for plugin discovery.
    #[pyo3(signature = (name, url, source, owner=None, repo=None))]
    fn add_marketplace(
        &mut self,
        name: String,
        url: String,
        source: String,
        owner: Option<String>,
        repo: Option<String>,
    ) {
        self.inner.marketplaces.push(MarketplaceConfig {
            name,
            url,
            source,
            auto_update_hours: 0,
            owner,
            repo,
            branch: None,
            path: None,
        });
    }

    fn __repr__(&self) -> String {
        format!(
            "PluginsConfig(enabled={}, preinstall={:?})",
            self.inner.enabled, self.inner.preinstall
        )
    }
}

// ── PyPluginManifest ──

/// Metadata for an installed plugin (read-only view).
#[pyclass(name = "PluginManifest", module = "fox_agent_sdk._core")]
#[derive(Clone)]
pub struct PyPluginManifest {
    #[pyo3(get)]
    name: String,
    #[pyo3(get)]
    version: Option<String>,
    #[pyo3(get)]
    description: Option<String>,
    #[pyo3(get)]
    author: Option<String>,
    #[pyo3(get)]
    repository: Option<String>,
}

impl PyPluginManifest {
    pub(crate) fn from_manifest(m: &PluginManifest) -> Self {
        Self {
            name: m.name.clone(),
            version: m.version.clone(),
            description: m.description.clone(),
            author: m.author.clone(),
            repository: m.repository.clone(),
        }
    }
}

#[pymethods]
impl PyPluginManifest {
    fn __repr__(&self) -> String {
        format!(
            "PluginManifest(name='{}', version={:?})",
            self.name, self.version
        )
    }
}
