/// Authentication configuration for a provider.
#[derive(Debug, Clone)]
pub enum AuthConfig {
    /// No authentication
    None,
    /// HTTP Bearer token (Authorization: Bearer <token>)
    BearerToken(String),
    /// Custom API-key header (e.g. x-api-key: <value>)
    ApiKeyHeader { header_name: String, value: String },
}

/// Configuration for an LLM provider backend.
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    /// Short name identifying this provider (e.g. "openai")
    pub provider_name: String,
    /// Base URL for the provider's API (e.g. https://api.openai.com/v1)
    pub base_url: String,
    /// Authentication method
    pub auth: AuthConfig,
    /// Request timeout in seconds
    pub timeout_secs: u64,
    /// Additional HTTP headers sent with every request
    pub default_headers: Vec<(String, String)>,
    /// Whether to use SSE streaming for responses
    pub use_streaming_api: bool,
}

impl ProviderConfig {
    pub fn openai(api_key: impl Into<String>) -> Self {
        Self {
            provider_name: "openai".to_string(),
            base_url: "https://api.openai.com/v1".to_string(),
            auth: AuthConfig::BearerToken(api_key.into()),
            timeout_secs: 60,
            default_headers: Vec::new(),
            use_streaming_api: true,
        }
    }

    pub fn anthropic(api_key: impl Into<String>) -> Self {
        Self {
            provider_name: "anthropic".to_string(),
            base_url: "https://api.anthropic.com/v1".to_string(),
            auth: AuthConfig::ApiKeyHeader {
                header_name: "x-api-key".to_string(),
                value: api_key.into(),
            },
            timeout_secs: 60,
            default_headers: vec![("anthropic-version".to_string(), "2023-06-01".to_string())],
            use_streaming_api: false,
        }
    }

    /// DeepSeek Chat API configuration.
    pub fn deepseek(api_key: impl Into<String>) -> Self {
        Self {
            provider_name: "deepseek".to_string(),
            base_url: "https://api.deepseek.com/".to_string(),
            auth: AuthConfig::BearerToken(api_key.into()),
            timeout_secs: 120,
            default_headers: Vec::new(),
            use_streaming_api: true,
        }
    }
}
