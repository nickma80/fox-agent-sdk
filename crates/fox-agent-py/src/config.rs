//! Python bindings for SDK configuration types.
//!
//! Maps Rust config types to Python classes with automatic
//! TOML serialization support.

use fox_agent_core::{
    AuthConfig as RustAuthConfig, DefaultSafetyPolicy, FoxAgentSdkConfig,
    ProviderConfig as RustProviderConfig, SafetyConfig,
};
use pyo3::prelude::*;

/// Python binding for ProviderConfig.
///
/// ```python
/// cfg = ProviderConfig.deepseek("sk-xxx")
/// cfg = ProviderConfig.openai("sk-xxx")
/// cfg = ProviderConfig.anthropic("sk-xxx")
/// ```
#[pyclass(name = "ProviderConfig", module = "fox_agent_sdk._core")]
#[derive(Clone)]
pub struct PyProviderConfig {
    inner: RustProviderConfig,
}

#[pymethods]
impl PyProviderConfig {
    /// Create a DeepSeek provider configuration.
    #[staticmethod]
    fn deepseek(api_key: String) -> Self {
        Self {
            inner: RustProviderConfig::deepseek(api_key),
        }
    }

    /// Create an OpenAI provider configuration.
    #[staticmethod]
    fn openai(api_key: String) -> Self {
        Self {
            inner: RustProviderConfig::openai(api_key),
        }
    }

    /// Create an Anthropic provider configuration.
    #[staticmethod]
    fn anthropic(api_key: String) -> Self {
        Self {
            inner: RustProviderConfig::anthropic(api_key),
        }
    }

    /// Create a custom provider configuration.
    #[staticmethod]
    #[pyo3(signature = (provider_name, base_url, api_key, *, timeout_secs = 120))]
    fn custom(provider_name: String, base_url: String, api_key: String, timeout_secs: u64) -> Self {
        Self {
            inner: RustProviderConfig {
                provider_name,
                base_url,
                auth: RustAuthConfig::BearerToken(api_key),
                timeout_secs,
                default_headers: vec![],
                use_streaming_api: true,
            },
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "ProviderConfig(provider={}, base_url={})",
            self.inner.provider_name, self.inner.base_url
        )
    }
}

impl PyProviderConfig {
    pub fn into_inner(self) -> RustProviderConfig {
        self.inner
    }
}

/// Python binding for SafetyConfig.
#[pyclass(name = "SafetyConfig", module = "fox_agent_sdk._core")]
#[derive(Clone)]
pub struct PySafetyConfig {
    inner: SafetyConfig,
}

#[pymethods]
impl PySafetyConfig {
    /// Create with the "allow all" default safety policy.
    #[new]
    #[pyo3(signature = (default_policy = "allow", productive_tool_confirm = false))]
    fn new(default_policy: &str, productive_tool_confirm: bool) -> PyResult<Self> {
        let policy = match default_policy {
            "allow" => DefaultSafetyPolicy::Allow,
            "confirm" => DefaultSafetyPolicy::Confirm,
            "deny" => DefaultSafetyPolicy::Deny,
            other => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "unknown default_policy '{}', expected 'allow', 'confirm', or 'deny'",
                    other
                )));
            }
        };

        Ok(Self {
            inner: SafetyConfig {
                default_policy: policy,
                productive_tool_confirm,
                ..Default::default()
            },
        })
    }

    fn __repr__(&self) -> String {
        format!(
            "SafetyConfig(policy={:?}, productive_tool_confirm={})",
            self.inner.default_policy, self.inner.productive_tool_confirm
        )
    }
}

impl PySafetyConfig {
    pub fn into_inner(self) -> SafetyConfig {
        self.inner
    }
}

/// Python binding for SdkConfig (FoxAgentSdkConfig).
///
/// Can be loaded from a TOML file.
#[pyclass(name = "SdkConfig", module = "fox_agent_sdk._core")]
#[derive(Clone)]
pub struct PySdkConfig {
    inner: FoxAgentSdkConfig,
}

#[pymethods]
impl PySdkConfig {
    /// Load configuration from an agent.toml file.
    #[staticmethod]
    fn from_file(path: String) -> PyResult<Self> {
        let cfg = FoxAgentSdkConfig::load_from_file(&path).map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!(
                "failed to load config from '{}': {}",
                path, e
            ))
        })?;
        Ok(Self { inner: cfg })
    }

    fn __repr__(&self) -> String {
        format!(
            "SdkConfig(model={:?}, storage_dir={:?})",
            self.inner.default_model, self.inner.storage_dir
        )
    }
}

impl PySdkConfig {
    pub fn into_inner(self) -> FoxAgentSdkConfig {
        self.inner
    }
}
