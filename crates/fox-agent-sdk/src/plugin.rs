use fox_agent_core::{MarketplaceConfig, ProxyConfig, SkillSource, Skill};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::info;

// ── Plugin manifest ──

/// Parsed from `plugin.json` in the plugin root directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_author_compat")]
    pub author: Option<String>,
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub min_sdk_version: Option<String>,
    #[serde(default)]
    pub entry: Option<PluginEntry>,

    /// Dependencies: plugin-name → version constraint
    #[serde(default)]
    pub dependencies: HashMap<String, String>,
}

/// `author` field can be either:
/// - a flat string: `"author": "Jesse Vincent"`
/// - an object: `"author": { "name": "Jesse Vincent", "email": "..." }`
fn deserialize_author_compat<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let val: Option<serde_json::Value> = Option::deserialize(deserializer)?;
    match val {
        None => Ok(None),
        Some(serde_json::Value::String(s)) => Ok(Some(s)),
        Some(serde_json::Value::Object(map)) => {
            Ok(Some(map.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string()))
        }
        _ => Ok(None),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginEntry {
    #[serde(default)]
    pub skills: Vec<String>,
}

/// An installed plugin and its metadata.
#[derive(Debug, Clone)]
pub struct InstalledPlugin {
    pub manifest: PluginManifest,
    pub path: PathBuf,
    pub source: PluginSource,
}

#[derive(Debug, Clone)]
pub enum PluginSource {
    GitHub { owner: String, repo: String, branch: Option<String> },
    Git { url: String, branch: Option<String> },
    Http { url: String },
    Local { path: PathBuf },
}

// ── PluginManager ──

/// A plugin entry as listed in a marketplace index.
///
/// Matches the real Claude Code `.claude-plugin/marketplace.json` format
/// while also accepting legacy flat `"source": "github"` strings from older
/// cached indexes.
///
/// Real format:
/// ```json
/// {
///   "name": "my-plugin",
///   "description": "...",
///   "source": { "source": "url", "url": "https://github.com/...git", "sha": "..." },
///   "homepage": "https://..."
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplacePluginEntry {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub homepage: Option<String>,

    /// Accepts two formats:
    /// - Claude Code nested: `{ source, url, path ?, ref ?, sha ? }`
    /// - Legacy flat string: `"github"` / `"git"` / `"http"` / `"local"`
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_source_compat")]
    pub source: Option<MarketplacePluginSource>,

    // ── Legacy / flat fields (for custom marketplaces) ──
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

/// Deserialize `source` field that can be either a nested object or a flat string.
fn deserialize_source_compat<'de, D>(deserializer: D) -> Result<Option<MarketplacePluginSource>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;
    let val: Option<serde_json::Value> = Option::deserialize(deserializer)?;
    match val {
        None => Ok(None),
        Some(serde_json::Value::String(s)) => {
            // Flat string — could be:
            // - a relative path: "./plugins/frontend-design" (bundled plugin)
            // - a source type tag: "github", "git", "http", "local" (legacy)
            let (source_type, url) = if s.starts_with("./") || s.starts_with("../") {
                // Bundled plugin in marketplace repo
                (String::new(), s)
            } else {
                // Legacy source type string
                (s, String::new())
            };
            Ok(Some(MarketplacePluginSource {
                source: source_type,
                url,
                path: None,
                r#ref: None,
                sha: None,
            }))
        }
        Some(obj) => {
            let nested: MarketplacePluginSource = serde_json::from_value(obj)
                .map_err(de::Error::custom)?;
            Ok(Some(nested))
        }
    }
}

/// Check if a marketplace entry refers to a bundled plugin
/// (relative path like `./plugins/frontend-design` inside the marketplace repo).
fn is_bundled_source(entry: &MarketplacePluginEntry) -> bool {
    entry.source.as_ref().map_or(false, |s| {
        s.url.starts_with("./") || s.url.starts_with("../")
    })
}

/// Find the plugin manifest file for a cloned plugin directory.
///
/// Supports two conventions:
/// 1. `plugin.json` at root (our own convention)
/// 2. `.claude-plugin/plugin.json` (Claude Code convention — used by superpowers, etc.)
fn find_plugin_manifest(plugin_dir: &Path) -> Option<PathBuf> {
    let root_manifest = plugin_dir.join("plugin.json");
    if root_manifest.exists() {
        return Some(root_manifest);
    }
    let claude_manifest = plugin_dir.join(".claude-plugin").join("plugin.json");
    if claude_manifest.exists() {
        return Some(claude_manifest);
    }
    None
}

/// Nested source descriptor found in Claude Code `.claude-plugin/marketplace.json`.
///
/// ```json
/// { "source": "url", "url": "https://github.com/owner/repo.git", "sha": "abc123" }
/// { "source": "git-subdir", "url": "owner/repo", "path": "plugins/my-plugin", "ref": "main", "sha": "..." }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplacePluginSource {
    /// Source type: `"url"`, `"github"`, or `"git-subdir"`.
    #[serde(default)]
    pub source: String,
    /// Git clone URL or shorthand (e.g. `"owner/repo"` for git-subdir).
    #[serde(default)]
    pub url: String,
    /// Subdirectory path within the repo (used by `git-subdir`).
    #[serde(default)]
    pub path: Option<String>,
    /// Branch or tag ref (used by `git-subdir`).
    #[serde(default)]
    pub r#ref: Option<String>,
    /// Pinned commit SHA.
    #[serde(default)]
    pub sha: Option<String>,
}

/// Deserialize the plugins array with per-entry error tolerance — individual
/// entries that fail to parse are logged and skipped rather than failing the
/// entire index.
fn deserialize_plugins_with_skip<'de, D>(deserializer: D) -> Result<Vec<MarketplacePluginEntry>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw: Vec<serde_json::Value> = Vec::deserialize(deserializer)?;
    let mut plugins = Vec::with_capacity(raw.len());
    for (i, val) in raw.iter().enumerate() {
        match serde_json::from_value::<MarketplacePluginEntry>(val.clone()) {
            Ok(entry) => {
                if entry.name.is_empty() {
                    tracing::warn!(index = i, "marketplace plugin entry missing 'name' — skipping");
                    continue;
                }
                plugins.push(entry);
            }
            Err(e) => {
                let name = val.get("name").and_then(|n| n.as_str()).unwrap_or("<unknown>");
                tracing::warn!(index = i, %name, error = %e, "failed to parse marketplace plugin entry — skipping");
            }
        }
    }
    Ok(plugins)
}

/// Marketplace index format (downloaded from marketplace server)
/// or read from `.claude-plugin/marketplace.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplaceIndex {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_plugins_with_skip")]
    pub plugins: Vec<MarketplacePluginEntry>,
}

pub struct PluginManager {
    installed: HashMap<String, InstalledPlugin>,
    marketplaces: Vec<MarketplaceConfig>,
    plugin_dir: PathBuf,
    proxy: Option<ProxyConfig>,
}

impl PluginManager {
    pub fn new(plugin_dir: PathBuf, marketplaces: Vec<MarketplaceConfig>) -> Self {
        Self {
            installed: HashMap::new(),
            marketplaces,
            plugin_dir,
            proxy: None,
        }
    }

    /// Set the proxy configuration for HTTP marketplace refreshes.
    pub fn with_proxy(mut self, proxy: Option<ProxyConfig>) -> Self {
        self.proxy = proxy;
        self
    }

    /// Discover already-installed plugins in `{plugin_dir}/`.
    pub fn discover_installed(&mut self) -> Result<usize, String> {
        let dir = &self.plugin_dir;
        if !dir.exists() {
            std::fs::create_dir_all(dir).map_err(|e| {
                format!("failed to create plugin dir `{}`: {e}", dir.display())
            })?;
            return Ok(0);
        }
        let mut count = 0;
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let manifest_path = find_plugin_manifest(&path);
                let Some(manifest_path) = manifest_path else {
                    continue;
                };
                match std::fs::read_to_string(&manifest_path) {
                    Ok(content) => match serde_json::from_str::<PluginManifest>(&content) {
                        Ok(manifest) => {
                            self.installed.insert(
                                manifest.name.clone(),
                                InstalledPlugin {
                                    path: path.clone(),
                                    manifest,
                                    source: PluginSource::Local { path: path.clone() },
                                },
                            );
                            count += 1;
                        }
                        Err(e) => {
                            tracing::warn!(
                                path = %manifest_path.display(),
                                error = %e,
                                "failed to parse plugin.json — skipping"
                            );
                        }
                    },
                    Err(e) => {
                        tracing::warn!(
                            path = %manifest_path.display(),
                            error = %e,
                            "failed to read plugin.json — skipping"
                        );
                    }
                }
            }
        }
        Ok(count)
    }

    /// Collect all skills from installed plugins.
    ///
    /// Supports two conventions:
    /// 1. `entry.skills` — explicit skill directories in `plugin.json`
    /// 2. No `entry` — scans plugin root + `skills/` for `.md` files (Claude Code convention)
    pub fn active_skills(&self) -> Vec<Skill> {
        let mut skills = Vec::new();
        for plugin in self.installed.values() {
            if let Some(ref entry) = plugin.manifest.entry {
                for skills_dir in &entry.skills {
                    let dir = plugin.path.join(skills_dir);
                    if dir.exists() {
                        collect_skills_from_dir(&dir, &plugin.manifest.name, &mut skills);
                    }
                }
            } else {
                // Claude Code convention: scan plugin root and skills/ for .md files
                collect_skills_from_dir(&plugin.path, &plugin.manifest.name, &mut skills);
                let skills_dir = plugin.path.join("skills");
                if skills_dir.exists() {
                    collect_skills_from_dir(&skills_dir, &plugin.manifest.name, &mut skills);
                }
            }
        }
        skills
    }

    /// Collect AGENTS.md content from installed plugins.
    pub fn active_agents_md(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        for plugin in self.installed.values() {
            let agents_md = plugin.path.join("AGENTS.md");
            if agents_md.exists() {
                paths.push(agents_md);
            }
        }
        paths
    }

    // ── Marketplace ──

    /// Refresh marketplace index.
    ///
    /// - **GitHub / Git sources**: shallow-clones the repo into
    ///   `{plugin_dir}/marketplaces/{name}/`, then scans `plugins/*/plugin.json`
    ///   (Claude Code directory-based marketplace format).
    /// - **HTTP sources**: fetches the URL and expects a JSON [`MarketplaceIndex`].
    ///
    /// The resulting index is cached to `{plugin_dir}/marketplaces/{name}.json`.
    pub async fn refresh_marketplace(&self, name: &str) -> Result<MarketplaceIndex, String> {
        let mc = self
            .marketplaces
            .iter()
            .find(|m| m.name == name)
            .ok_or_else(|| format!("marketplace '{name}' not found"))?;

        let index = match mc.source.as_str() {
            "GitHub" | "Git" => self.refresh_git_marketplace(mc).await?,
            "Http" => self.refresh_http_marketplace(mc).await?,
            "Local" => self.refresh_local_marketplace(mc)?,
            other => return Err(format!("unsupported marketplace source: {other}")),
        };

        // Cache the index to disk
        let cache_dir = self.plugin_dir.join("marketplaces");
        std::fs::create_dir_all(&cache_dir).map_err(|e| {
            format!("failed to create marketplace cache dir: {e}")
        })?;
        let cache_path = cache_dir.join(format!("{name}.json"));
        let cache_json = serde_json::to_string_pretty(&index)
            .map_err(|e| format!("failed to serialize marketplace index: {e}"))?;
        std::fs::write(&cache_path, &cache_json).map_err(|e| {
            format!("failed to cache marketplace index: {e}")
        })?;

        info!(
            name = %index.name,
            plugins = index.plugins.len(),
            "Marketplace refreshed successfully"
        );
        Ok(index)
    }

    /// Clone a GitHub/Git marketplace repo and scan for plugins.
    ///
    /// Precedence:
    /// 1. `.claude-plugin/marketplace.json` — Claude Code standard index
    /// 2. `plugins/*/plugin.json` — directory-based fallback
    async fn refresh_git_marketplace(&self, mc: &MarketplaceConfig) -> Result<MarketplaceIndex, String> {
        let (git_url, branch) = match mc.source.as_str() {
            "GitHub" => {
                let owner = mc.owner.as_deref().unwrap_or("");
                let repo = mc.repo.as_deref().unwrap_or("");
                let url = format!("https://github.com/{owner}/{repo}.git");
                let branch = mc.branch.as_deref().unwrap_or("main");
                (url, branch)
            }
            "Git" => {
                let branch = mc.branch.as_deref().unwrap_or("main");
                (mc.url.clone(), branch)
            }
            _ => unreachable!(),
        };

        let repo_dir = self.plugin_dir.join("marketplaces").join(&mc.name);

        if repo_dir.exists() {
            // Already cloned — do a fast `git fetch --depth 1` update
            info!(dir = %repo_dir.display(), "Marketplace repo exists, fetching updates");
            match git_fetch_update(&repo_dir, self.proxy.as_ref()) {
                Ok(()) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "git fetch failed, re-cloning");
                    let _ = std::fs::remove_dir_all(&repo_dir);
                    clone_git_repo(&git_url, branch, &repo_dir, self.proxy.as_ref())?;
                }
            }
        } else {
            info!(%git_url, branch, "Cloning marketplace repo");
            clone_git_repo(&git_url, branch, &repo_dir, self.proxy.as_ref())?;
        }

        // ── Prefer .claude-plugin/marketplace.json (Claude Code standard) ──
        let manifest_path = repo_dir.join(".claude-plugin").join("marketplace.json");
        if manifest_path.exists() {
            info!(path = %manifest_path.display(), "Found Claude Code marketplace manifest");
            let content = std::fs::read_to_string(&manifest_path)
                .map_err(|e| format!("failed to read {}: {e}", manifest_path.display()))?;
            let mut index: MarketplaceIndex = serde_json::from_str(&content)
                .map_err(|e| format!("invalid .claude-plugin/marketplace.json: {e}"))?;
            index.name = mc.name.clone();
            return Ok(index);
        }

        // ── Fallback: scan plugins/ directory ──
        let plugins_dir = repo_dir.join("plugins");
        let mut plugins = Vec::new();
        if plugins_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&plugins_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if !path.is_dir() {
                        continue;
                    }
                    let manifest_path = path.join("plugin.json");
                    if !manifest_path.exists() {
                        continue;
                    }
                    match std::fs::read_to_string(&manifest_path) {
                        Ok(content) => {
                            match serde_json::from_str::<PluginManifest>(&content) {
                                Ok(manifest) => {
                                    let entry = MarketplacePluginEntry {
                                        name: manifest.name.clone(),
                                        version: manifest.version.clone().unwrap_or_default(),
                                        description: manifest.description.clone().unwrap_or_default(),
                                        homepage: manifest.repository.clone(),
                                        source: Some(MarketplacePluginSource {
                                            source: "github".into(),
                                            url: manifest.repository.clone().unwrap_or_default(),
                                            path: None,
                                            r#ref: None,
                                            sha: None,
                                        }),
                                        repository: manifest.repository.clone(),
                                        tags: Vec::new(),
                                        owner: mc.owner.clone(),
                                        repo: mc.repo.clone(),
                                        branch: mc.branch.clone(),
                                        url: None,
                                    };
                                    plugins.push(entry);
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        path = %manifest_path.display(),
                                        error = %e,
                                        "failed to parse plugin manifest — skipping"
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                path = %manifest_path.display(),
                                error = %e,
                                "failed to read plugin manifest — skipping"
                            );
                        }
                    }
                }
            }
        }

        Ok(MarketplaceIndex {
            name: mc.name.clone(),
            version: "1.0".into(),
            description: format!("Plugins from {}", mc.name),
            plugins,
        })
    }

    /// Fetch an HTTP marketplace index with status-code validation.
    async fn refresh_http_marketplace(&self, mc: &MarketplaceConfig) -> Result<MarketplaceIndex, String> {
        info!("Refreshing marketplace '{}' from {}", mc.name, mc.url);

        let mut builder = reqwest::Client::builder();
        if let Some(ref proxy_cfg) = self.proxy {
            builder = builder.proxy(proxy_cfg.to_reqwest_proxy()?);
        }
        let client = builder
            .build()
            .map_err(|e| format!("failed to build HTTP client: {e}"))?;

        let response = client.get(&mc.url)
            .send()
            .await
            .map_err(|e| format!("failed to fetch marketplace '{}': {e}", mc.name))?;

        let status = response.status();
        if !status.is_success() {
            return Err(format!(
                "marketplace '{}' returned HTTP {}",
                mc.name,
                status.as_u16()
            ));
        }

        let body = response
            .text()
            .await
            .map_err(|e| format!("failed to read marketplace response: {e}"))?;

        serde_json::from_str(&body)
            .map_err(|e| format!("invalid marketplace index from '{}': {e}", mc.name))
    }

    /// Scan a local directory for plugins (same structure as git marketplace).
    fn refresh_local_marketplace(&self, mc: &MarketplaceConfig) -> Result<MarketplaceIndex, String> {
        let path = mc.path.as_deref()
            .ok_or_else(|| "Local source requires 'path'".to_string())?;

        if !path.exists() || !path.is_dir() {
            return Err(format!(
                "local marketplace path does not exist: {}",
                path.display()
            ));
        }

        let plugins_dir = path.join("plugins");
        let mut plugins = Vec::new();
        if plugins_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&plugins_dir) {
                for entry in entries.flatten() {
                    let entry_path = entry.path();
                    if !entry_path.is_dir() {
                        continue;
                    }
                    let manifest_path = entry_path.join("plugin.json");
                    if !manifest_path.exists() {
                        continue;
                    }
                    if let Ok(content) = std::fs::read_to_string(&manifest_path) {
                        if let Ok(manifest) = serde_json::from_str::<PluginManifest>(&content) {
                            plugins.push(MarketplacePluginEntry {
                                name: manifest.name.clone(),
                                version: manifest.version.clone().unwrap_or_default(),
                                description: manifest.description.clone().unwrap_or_default(),
                                homepage: manifest.repository.clone(),
                                source: Some(MarketplacePluginSource {
                                    source: "local".into(),
                                    url: manifest.repository.clone().unwrap_or_default(),
                                    path: None,
                                    r#ref: None,
                                    sha: None,
                                }),
                                repository: manifest.repository.clone(),
                                tags: Vec::new(),
                                owner: None,
                                repo: None,
                                branch: None,
                                url: None,
                            });
                        }
                    }
                }
            }
        }

        Ok(MarketplaceIndex {
            name: mc.name.clone(),
            version: "1.0".into(),
            description: format!("Local plugins from {}", path.display()),
            plugins,
        })
    }

    // ── Convenience: install by name ──

    /// Install a plugin by name from configured marketplaces.
    ///
    /// Strategy:
    /// 1. Search cached indexes (fast path, no network)
    /// 2. If not found, refresh all marketplaces one-by-one and search each
    /// 3. Install the first match found
    ///
    /// `marketplace_filter` — if `Some("name")`, only search that marketplace.
    pub async fn install_plugin(
        &mut self,
        name: &str,
        marketplace_filter: Option<&str>,
    ) -> Result<InstalledPlugin, String> {
        // Already installed?
        if self.is_installed(name) {
            return Err(format!("plugin '{name}' is already installed"));
        }

        let mp_names: Vec<String> = if let Some(mp) = marketplace_filter {
            if !self.marketplaces.iter().any(|m| m.name == mp) {
                return Err(format!("marketplace '{mp}' not found in configuration"));
            }
            vec![mp.to_string()]
        } else {
            self.marketplaces.iter().map(|m| m.name.clone()).collect()
        };

        if mp_names.is_empty() {
            return Err(format!(
                "no marketplaces configured. Add [[plugins.marketplaces]] to agent.toml."
            ));
        }

        // ── Phase 1: search cached indexes (fast) ──
        let mut cached_indexes: Vec<MarketplaceIndex> = Vec::new();
        for mp_name in &mp_names {
            if let Ok(Some(index)) = self.load_cached_index(mp_name) {
                cached_indexes.push(index);
            }
        }
        if !cached_indexes.is_empty() {
            let results = self.search(&cached_indexes, name);
            if let Some(entry) = results.into_iter().next() {
                info!(cache_hit = true, plugin = %entry.name, "Plugin found in cached index");
                return self.install_from_marketplace(&entry).await;
            }
        }

        // ── Phase 2: refresh and search live ──
        for mp_name in &mp_names {
            match self.refresh_marketplace(mp_name).await {
                Ok(fresh_index) => {
                    let results = self.search(&[fresh_index], name);
                    if let Some(entry) = results.into_iter().next() {
                        info!(plugin = %entry.name, marketplace = %mp_name, "Plugin found after refresh");
                        return self.install_from_marketplace(&entry).await;
                    }
                }
                Err(e) => {
                    tracing::warn!(marketplace = %mp_name, error = %e, "Failed to refresh marketplace");
                }
            }
        }

        // ── Not found ──
        Err(format!(
            "plugin '{name}' not found in any configured marketplace: {}. \
             Use a different name or add more marketplaces to agent.toml.",
            mp_names.join(", ")
        ))
    }

    /// Search for plugins by name or tag across all marketplaces.
    ///
    /// Results are sorted by relevance:
    /// 1. Exact name match
    /// 2. Name starts with query
    /// 3. Name contains query
    /// 4. Description or tags contain query
    pub fn search(&self, cached_indexes: &[MarketplaceIndex], query: &str) -> Vec<MarketplacePluginEntry> {
        let query_lower = query.to_lowercase();
        let mut results: Vec<(u8, MarketplacePluginEntry)> = Vec::new();

        for index in cached_indexes {
            for plugin in &index.plugins {
                let name_lower = plugin.name.to_lowercase();
                let desc_lower = plugin.description.to_lowercase();
                let tag_match = plugin.tags.iter().any(|t| t.to_lowercase().contains(&query_lower));

                let priority = if name_lower == query_lower {
                    0 // exact match
                } else if name_lower.starts_with(&query_lower) {
                    1 // prefix match
                } else if name_lower.contains(&query_lower) {
                    2 // substring match
                } else if desc_lower.contains(&query_lower) || tag_match {
                    3 // description or tag match
                } else {
                    continue;
                };

                results.push((priority, plugin.clone()));
            }
        }

        // Sort by priority, then by name length (shorter = closer match)
        results.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.1.name.len().cmp(&b.1.name.len()))
        });

        results.into_iter().map(|(_, entry)| entry).collect()
    }

    /// Load a cached marketplace index from disk.
    pub fn load_cached_index(&self, name: &str) -> Result<Option<MarketplaceIndex>, String> {
        let cache_path = self.plugin_dir.join("marketplaces").join(format!("{name}.json"));
        if !cache_path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&cache_path)
            .map_err(|e| format!("failed to read cached index '{}': {e}", cache_path.display()))?;
        let index: MarketplaceIndex = serde_json::from_str(&content)
            .map_err(|e| format!("invalid cached index '{}': {e}", cache_path.display()))?;
        Ok(Some(index))
    }

    /// Install a plugin from a marketplace entry.
    ///
    /// Handles three source types:
    /// 1. External git clone (`source.url` / `repository` / `owner`+`repo`)
    /// 2. Bundled in marketplace repo (`source` is a relative path like `./plugins/frontend-design`)
    /// 3. git-subdir (sparse checkout from a larger repo)
    pub async fn install_from_marketplace(
        &mut self,
        entry: &MarketplacePluginEntry,
    ) -> Result<InstalledPlugin, String> {
        let dest = self.plugin_dir.join(&entry.name);
        if dest.exists() {
            return Err(format!(
                "plugin '{}' is already installed at `{}`",
                entry.name,
                dest.display()
            ));
        }

        // ── Case 1: Bundled plugin (relative path in marketplace repo) ──
        if is_bundled_source(entry) {
            return self.install_bundled_plugin(entry, &dest).await;
        }

        // ── Case 2: External clone ──
        let (git_url, branch) = resolve_plugin_source(entry)?;

        info!(%git_url, branch, name = %entry.name, "Installing plugin from marketplace");
        clone_git_repo(&git_url, &branch, &dest, self.proxy.as_ref())?;

        self.finalize_install(&dest, entry).await
    }

    /// Finalize installation: read manifest, register plugin.
    async fn finalize_install(
        &mut self,
        dest: &Path,
        entry: &MarketplacePluginEntry,
    ) -> Result<InstalledPlugin, String> {
        let manifest_path = find_plugin_manifest(dest)
            .ok_or_else(|| {
                let _ = std::fs::remove_dir_all(dest);
                format!("installed plugin '{}' has no plugin.json or .claude-plugin/plugin.json", entry.name)
            })?;
        let content = std::fs::read_to_string(&manifest_path)
            .map_err(|e| format!("failed to read plugin manifest: {e}"))?;
        let manifest: PluginManifest = serde_json::from_str(&content)
            .map_err(|e| format!("invalid plugin.json: {e}"))?;

        let installed = InstalledPlugin {
            path: dest.to_path_buf(),
            manifest: manifest.clone(),
            source: PluginSource::Local { path: dest.to_path_buf() },
        };

        self.installed
            .insert(manifest.name.clone(), installed.clone());
        Ok(installed)
    }

    /// Install a plugin that is bundled inside the marketplace repo.
    ///
    /// These entries have `"source": "./plugins/some-name"` — the plugin
    /// already lives inside the cloned marketplace repo at
    /// `{plugin_dir}/marketplaces/{marketplace}/plugins/{name}/`.
    async fn install_bundled_plugin(
        &mut self,
        entry: &MarketplacePluginEntry,
        dest: &Path,
    ) -> Result<InstalledPlugin, String> {
        let rel_path = entry.source.as_ref()
            .map(|s| s.url.as_str())
            .filter(|u| u.starts_with("./") || u.starts_with("../"))
            .ok_or_else(|| format!(
                "plugin '{}': bundled source path not found in entry",
                entry.name
            ))?;

        let mp_name = self.resolve_marketplace_for_entry(entry)?;
        let source_path = self.plugin_dir
            .join("marketplaces")
            .join(&mp_name)
            .join(rel_path.trim_start_matches("./"));


        if !source_path.exists() {
            return Err(format!(
                "plugin '{}': bundled source path does not exist: {}",
                entry.name,
                source_path.display()
            ));
        }

        info!(
            from = %source_path.display(),
            to = %dest.display(),
            "Installing bundled plugin from marketplace repo"
        );
        copy_dir_all(&source_path, dest)
            .map_err(|e| format!("failed to copy bundled plugin '{}': {e}", entry.name))?;

        self.finalize_install(dest, entry).await
    }

    /// Find which marketplace contains this entry (by searching cached indexes).
    fn resolve_marketplace_for_entry(&self, entry: &MarketplacePluginEntry) -> Result<String, String> {
        // Try cached indexes first
        for mc in &self.marketplaces {
            if let Ok(Some(index)) = self.load_cached_index(&mc.name) {
                if index.plugins.iter().any(|p| p.name == entry.name) {
                    return Ok(mc.name.clone());
                }
            }
        }
        Err(format!(
            "plugin '{}': cannot determine which marketplace it belongs to",
            entry.name
        ))
    }

    /// Install a plugin from a local filesystem path.
    pub async fn install_from_path(&mut self, path: &Path) -> Result<InstalledPlugin, String> {
        let manifest_path = find_plugin_manifest(path)
            .ok_or_else(|| format!("no plugin.json or .claude-plugin/plugin.json found at `{}`", path.display()))?;
        let content = std::fs::read_to_string(&manifest_path)
            .map_err(|e| format!("failed to read `{}`: {e}", manifest_path.display()))?;
        let manifest: PluginManifest = serde_json::from_str(&content)
            .map_err(|e| format!("invalid plugin.json at `{}`: {e}", manifest_path.display()))?;

        // Copy to plugin_dir
        let dest = self.plugin_dir.join(&manifest.name);
        if dest.exists() {
            return Err(format!(
                "plugin '{}' is already installed at `{}`",
                manifest.name,
                dest.display()
            ));
        }

        copy_dir_all(path, &dest)
            .map_err(|e| format!("failed to install plugin '{}': {e}", manifest.name))?;

        let installed = InstalledPlugin {
            path: dest,
            manifest: manifest.clone(),
            source: PluginSource::Local {
                path: path.to_path_buf(),
            },
        };

        self.installed
            .insert(manifest.name.clone(), installed.clone());
        Ok(installed)
    }

    /// Get all installed plugins.
    pub fn installed_plugins(&self) -> Vec<&InstalledPlugin> {
        self.installed.values().collect()
    }

    /// Check if a plugin is installed by name.
    pub fn is_installed(&self, name: &str) -> bool {
        self.installed.contains_key(name)
    }

    /// Install a plugin by name from configured marketplaces.
    /// This is the primary API for `fox-code plugin install <name>`.
    ///
    /// `marketplace_filter` — if `Some("name")`, only search that marketplace.
    /// If `None`, search all configured marketplaces.
    ///
    /// Strategy:
    /// 1. Search cached indexes (fast path, no network)
    /// 2. If not found, refresh uncached marketplaces and search live
    /// 3. Install the first exact name match found
    pub async fn install_plugin_by_name(
        &mut self,
        name: &str,
        marketplace_filter: Option<&str>,
    ) -> Result<InstalledPlugin, String> {
        if self.marketplaces.is_empty() {
            return Err("no marketplaces configured — add a `[[plugins.marketplaces]]` section to agent.toml".into());
        }

        let candidates: Vec<&MarketplaceConfig> = if let Some(filter) = marketplace_filter {
            let found = self.marketplaces.iter().find(|m| m.name == filter)
                .ok_or_else(|| format!("marketplace '{filter}' not found in configuration"))?;
            vec![found]
        } else {
            self.marketplaces.iter().collect()
        };

        let marketplace_names: Vec<String> = candidates.iter().map(|m| m.name.clone()).collect();

        // ── Refresh marketplaces that are missing cache ──
        let mut indexes = Vec::new();
        for mc in &candidates {
            match self.load_cached_index(&mc.name) {
                Ok(Some(index)) => {
                    indexes.push(index);
                }
                Ok(None) | Err(_) => {
                    // Cache miss or corrupted — refresh from upstream
                    info!(marketplace = %mc.name, "Cache miss, refreshing marketplace");
                    match self.refresh_marketplace(&mc.name).await {
                        Ok(index) => indexes.push(index),
                        Err(e) => {
                            tracing::warn!(
                                marketplace = %mc.name,
                                error = %e,
                                "failed to refresh marketplace — skipping"
                            );
                        }
                    }
                }
            }
        }

        if indexes.is_empty() {
            return Err(format!(
                "failed to load any marketplace index ({} configured: {})",
                candidates.len(),
                marketplace_names.join(", ")
            ));
        }

        // ── Search for exact match ──
        let results = self.search(&indexes, name);
        let entry = results.into_iter().find(|e| e.name.to_lowercase() == name.to_lowercase())
            .ok_or_else(|| {
                format!(
                    "plugin '{}' not found in any configured marketplace ({}). \
                     Use --marketplace <name> to specify, or provide a local path.",
                    name,
                    marketplace_names.join(", ")
                )
            })?;

        info!(name = %entry.name, "Found plugin in marketplace — installing");
        self.install_from_marketplace(&entry).await
    }

    /// Uninstall a plugin by name. Returns the removed plugin if it existed.
    pub fn uninstall(&mut self, name: &str) -> Option<InstalledPlugin> {
        let plugin = self.installed.remove(name)?;
        let _ = std::fs::remove_dir_all(&plugin.path);
        Some(plugin)
    }

    /// Names of all configured marketplaces.
    pub fn marketplace_names(&self) -> Vec<&str> {
        self.marketplaces.iter().map(|m| m.name.as_str()).collect()
    }
}

// ── Helpers ──

fn collect_skills_from_dir(dir: &Path, plugin_name: &str, out: &mut Vec<Skill>) {
    if !dir.exists() {
        return;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_skills_from_dir(&path, plugin_name, out);
            } else if path.extension().map(|e| e == "md").unwrap_or(false) {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    match Skill::from_file(stem, &path) {
                        Ok(mut skill) => {
                            skill.source = SkillSource::Plugin(plugin_name.to_string());
                            out.push(skill);
                        }
                        Err(e) => {
                            // Many .md files in plugin repos are regular docs,
                            // not skills (README, AGENTS, CLAUDE, CODE_OF_CONDUCT,
                            // design specs, etc.). Silently skip them to avoid
                            // log-spam — use RUST_LOG=trace if you need to see them.
                            tracing::trace!(
                                path = %path.display(),
                                error = %e,
                                "skipped non-skill .md file in plugin"
                            );
                        }
                    }
                }
            }
        }
    }
}

/// Recursively copy a directory.
fn copy_dir_all(src: &Path, dst: &Path) -> Result<(), std::io::Error> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let dest_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_all(&entry.path(), &dest_path)?;
        } else {
            std::fs::copy(entry.path(), &dest_path)?;
        }
    }
    Ok(())
}

/// Resolve the git clone URL and branch from a marketplace entry.
///
/// Order of precedence:
/// 1. `source.url` (Claude Code nested `{source, url, sha}` descriptor)
/// 2. `repository` (flat field, custom marketplaces)
/// 3. `owner`/`repo` (legacy flat fields → `https://github.com/{owner}/{repo}.git`)
fn resolve_plugin_source(entry: &MarketplacePluginEntry) -> Result<(String, String), String> {
    // ── Claude Code nested source ──
    if let Some(ref src) = entry.source {
        if !src.url.is_empty() {
            let git_url = if src.url.starts_with("https://") || src.url.starts_with("git@") {
                src.url.clone()
            } else if src.url.contains('/') {
                // Shorthand like "owner/repo" → expand to full GitHub URL
                format!("https://github.com/{}.git", src.url)
            } else {
                return Err(format!(
                    "plugin '{}': unrecognized source URL '{}'",
                    entry.name, src.url
                ));
            };
            let branch = src.r#ref.as_deref().unwrap_or("main").to_string();
            return Ok((git_url, branch));
        }
    }

    // ── Flat repository field ──
    if let Some(ref repo_url) = entry.repository {
        let branch = entry.branch.as_deref().unwrap_or("main").to_string();
        return Ok((repo_url.clone(), branch));
    }

    // ── Legacy owner/repo fields ──
    if let (Some(owner), Some(repo)) = (&entry.owner, &entry.repo) {
        let branch = entry.branch.as_deref().unwrap_or("main").to_string();
        return Ok((format!("https://github.com/{owner}/{repo}.git"), branch));
    }

    Err(format!(
        "plugin '{}': no installable source found (neither source.url, repository, nor owner/repo)",
        entry.name
    ))
}

/// Clone a Git repository to a destination directory.
///
/// When `proxy` is set, passes `-c http.proxy=<url>` and `-c https.proxy=<url>`
/// so that `git clone` respects the configured proxy.
fn clone_git_repo(url: &str, branch: &str, dest: &Path, proxy: Option<&ProxyConfig>) -> Result<(), String> {
    use std::process::Command;
    let mut cmd = Command::new("git");
    cmd.arg("clone")
        .arg("--depth").arg("1")
        .arg("--branch").arg(branch)
        .arg("--single-branch");

    // Inject proxy via git -c flags (works without modifying global .gitconfig)
    if let Some(p) = proxy {
        cmd.arg("-c").arg(format!("http.proxy={}", p.url));
        cmd.arg("-c").arg(format!("https.proxy={}", p.url));
    }

    cmd.arg(url).arg(dest);

    let output = cmd.output()
        .map_err(|e| format!("failed to spawn git: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git clone failed: {stderr}"));
    }
    Ok(())
}

/// Fast-update an existing shallow clone via `git fetch --depth 1` + `git reset`.
fn git_fetch_update(repo_dir: &Path, proxy: Option<&ProxyConfig>) -> Result<(), String> {
    use std::process::Command;

    let git_with_proxy = |cmd: &mut Command| {
        if let Some(p) = proxy {
            cmd.arg("-c").arg(format!("http.proxy={}", p.url));
            cmd.arg("-c").arg(format!("https.proxy={}", p.url));
        }
    };

    // Fetch latest from origin
    let mut fetch = Command::new("git");
    fetch.arg("-C").arg(repo_dir);
    git_with_proxy(&mut fetch);
    fetch.args(["fetch", "--depth", "1", "origin"]);
    let fetch_out = fetch.output()
        .map_err(|e| format!("git fetch failed: {e}"))?;
    if !fetch_out.status.success() {
        let stderr = String::from_utf8_lossy(&fetch_out.stderr);
        return Err(format!("git fetch failed: {stderr}"));
    }

    // Reset working tree to origin/HEAD
    let mut reset = Command::new("git");
    reset.arg("-C").arg(repo_dir);
    git_with_proxy(&mut reset);
    reset.args(["reset", "--hard", "origin/HEAD"]);
    let reset_out = reset.output()
        .map_err(|e| format!("git reset failed: {e}"))?;
    if !reset_out.status.success() {
        let stderr = String::from_utf8_lossy(&reset_out.stderr);
        return Err(format!("git reset failed: {stderr}"));
    }
    Ok(())
}

/// Download and extract a plugin from an HTTP URL.
#[expect(dead_code)]
async fn download_and_extract(_url: &str, _dest: &Path) -> Result<(), String> {
    // For now, downloads simple archives (tar.gz / zip).
    // Full implementation would use reqwest + flate2/tar crates.
    Err("HTTP plugin download not yet implemented (use git or local instead)".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_plugin_manifest_parse() {
        let json = r#"{
            "name": "code-review",
            "version": "1.0.0",
            "description": "Auto code review",
            "entry": {
                "skills": ["skills/"]
            }
        }"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert_eq!(manifest.name, "code-review");
        assert_eq!(manifest.version.as_deref(), Some("1.0.0"));
        assert!(manifest.entry.is_some());
    }

    #[test]
    fn test_collect_skills_from_dir() {
        let dir = std::env::temp_dir().join(format!("plugin-skills-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("review.md"),
            "---\nname: review\ndescription: Review code\n---\n\nDo a review.",
        )
        .unwrap();

        let mut skills = Vec::new();
        collect_skills_from_dir(&dir, "test-plugin", &mut skills);
        assert_eq!(skills.len(), 1);
        let skill = &skills[0];
        assert_eq!(skill.name, "review");
        assert_eq!(skill.prompt, "Do a review.");

        match &skill.source {
            SkillSource::Plugin(name) => assert_eq!(name, "test-plugin"),
            other => panic!("expected Plugin source, got {other:?}"),
        }

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_plugin_manager_discover_empty() {
        let dir = std::env::temp_dir().join(format!("plugin-empty-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();

        let mut manager = PluginManager::new(dir.clone(), Vec::new());
        let count = manager.discover_installed().unwrap();
        assert_eq!(count, 0);

        fs::remove_dir_all(&dir).ok();
    }
}
