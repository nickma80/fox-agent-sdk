use fox_agent_core::{MarketplaceConfig, SkillSource, Skill};
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplacePluginEntry {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default)]
    pub source: Option<String>,       // "github", "git", "http", "local"
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub owner: Option<String>,        // GitHub owner
    #[serde(default)]
    pub repo: Option<String>,         // GitHub repo name
    #[serde(default)]
    pub branch: Option<String>,       // Git branch
    #[serde(default)]
    pub url: Option<String>,          // HTTP URL or Git URL
}

/// Marketplace index format (downloaded from marketplace server).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplaceIndex {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub plugins: Vec<MarketplacePluginEntry>,
}

pub struct PluginManager {
    installed: HashMap<String, InstalledPlugin>,
    marketplaces: Vec<MarketplaceConfig>,
    plugin_dir: PathBuf,
}

impl PluginManager {
    pub fn new(plugin_dir: PathBuf, marketplaces: Vec<MarketplaceConfig>) -> Self {
        Self {
            installed: HashMap::new(),
            marketplaces,
            plugin_dir,
        }
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
                let manifest_path = path.join("plugin.json");
                if !manifest_path.exists() {
                    continue;
                }
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

    /// Refresh marketplace index from the configured URL.
    ///
    /// Downloads the `index.json` and caches it to
    /// `{plugin_dir}/marketplaces/{name}.json`.
    pub async fn refresh_marketplace(&self, name: &str) -> Result<MarketplaceIndex, String> {
        let mc = self
            .marketplaces
            .iter()
            .find(|m| m.name == name)
            .ok_or_else(|| format!("marketplace '{name}' not found"))?;

        let url = build_marketplace_url(mc)?;
        info!("Refreshing marketplace '{name}' from {url}");

        let response = reqwest::get(&url)
            .await
            .map_err(|e| format!("failed to fetch marketplace '{name}': {e}"))?;

        let body = response
            .text()
            .await
            .map_err(|e| format!("failed to read marketplace response: {e}"))?;

        let index: MarketplaceIndex = serde_json::from_str(&body)
            .map_err(|e| format!("invalid marketplace index from '{name}': {e}"))?;

        // Cache the index to disk
        let cache_dir = self.plugin_dir.join("marketplaces");
        std::fs::create_dir_all(&cache_dir).map_err(|e| {
            format!("failed to create marketplace cache dir: {e}")
        })?;
        let cache_path = cache_dir.join(format!("{name}.json"));
        std::fs::write(&cache_path, &body).map_err(|e| {
            format!("failed to cache marketplace index: {e}")
        })?;

        info!(
            name = %index.name,
            plugins = index.plugins.len(),
            "Marketplace refreshed successfully"
        );
        Ok(index)
    }

    /// Search for plugins by name or tag across all marketplaces.
    pub fn search(&self, cached_indexes: &[MarketplaceIndex], query: &str) -> Vec<MarketplacePluginEntry> {
        let query_lower = query.to_lowercase();
        let mut results = Vec::new();
        for index in cached_indexes {
            for plugin in &index.plugins {
                if plugin.name.to_lowercase().contains(&query_lower)
                    || plugin.description.to_lowercase().contains(&query_lower)
                    || plugin.tags.iter().any(|t| t.to_lowercase().contains(&query_lower))
                {
                    results.push(plugin.clone());
                }
            }
        }
        results
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
    /// Supports Git clone, HTTP download (zip), and local copy.
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

        let source = entry.source.as_deref().unwrap_or("github");

        match source {
            "github" => {
                let owner = entry.owner.as_ref()
                    .ok_or_else(|| "GitHub source requires 'owner'".to_string())?;
                let repo = entry.repo.as_ref()
                    .ok_or_else(|| "GitHub source requires 'repo'".to_string())?;
                let branch = entry.branch.as_deref().unwrap_or("main");
                let git_url = format!("https://github.com/{owner}/{repo}.git");
                info!(%git_url, branch, "Cloning plugin from GitHub");
                clone_git_repo(&git_url, branch, &dest)?;
            }
            "git" => {
                let git_url = entry.url.as_ref()
                    .or(entry.repository.as_ref())
                    .ok_or_else(|| "Git source requires 'url' or 'repository'".to_string())?;
                let branch = entry.branch.as_deref().unwrap_or("main");
                info!(%git_url, branch, "Cloning plugin from Git");
                clone_git_repo(git_url, branch, &dest)?;
            }
            "http" => {
                let url = entry.url.as_ref()
                    .or(entry.repository.as_ref())
                    .ok_or_else(|| "HTTP source requires 'url'".to_string())?;
                info!(%url, "Downloading plugin from HTTP");
                download_and_extract(url, &dest).await?;
            }
            "local" => {
                let path = entry.url.as_ref()
                    .map(PathBuf::from)
                    .ok_or_else(|| "Local source requires 'url' (path)".to_string())?;
                info!(path = %path.display(), "Copying plugin from local path");
                copy_dir_all(&path, &dest)
                    .map_err(|e| format!("failed to copy plugin: {e}"))?;
            }
            other => return Err(format!("unsupported plugin source: {other}")),
        }

        // Read the installed plugin's manifest
        let manifest_path = dest.join("plugin.json");
        if !manifest_path.exists() {
            // Clean up on missing manifest
            let _ = std::fs::remove_dir_all(&dest);
            return Err(format!("installed plugin '{}' has no plugin.json", entry.name));
        }
        let content = std::fs::read_to_string(&manifest_path)
            .map_err(|e| format!("failed to read plugin.json: {e}"))?;
        let manifest: PluginManifest = serde_json::from_str(&content)
            .map_err(|e| format!("invalid plugin.json: {e}"))?;

        let installed = InstalledPlugin {
            path: dest.clone(),
            manifest: manifest.clone(),
            source: PluginSource::Local { path: dest },
        };

        self.installed
            .insert(manifest.name.clone(), installed.clone());
        Ok(installed)
    }

    /// Install a plugin from a local filesystem path.
    pub async fn install_from_path(&mut self, path: &Path) -> Result<InstalledPlugin, String> {
        let manifest_path = path.join("plugin.json");
        if !manifest_path.exists() {
            return Err(format!("no plugin.json found at `{}`", path.display()));
        }
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
                            tracing::warn!(
                                path = %path.display(),
                                error = %e,
                                "failed to load plugin skill — skipping"
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

/// Build the index URL for a marketplace config.
fn build_marketplace_url(mc: &MarketplaceConfig) -> Result<String, String> {
    match mc.source.as_str() {
        "GitHub" => {
            let owner = mc.owner.as_deref().unwrap_or("");
            let repo = mc.repo.as_deref().unwrap_or("");
            Ok(format!(
                "https://raw.githubusercontent.com/{owner}/{repo}/main/index.json"
            ))
        }
        "Git" | "Http" => Ok(mc.url.clone()),
        "Local" => {
            let path = mc.path.as_deref().unwrap_or_else(|| std::path::Path::new("."));
            Ok(format!("file://{}", path.display()))
        }
        other => Err(format!("unsupported marketplace source: {other}")),
    }
}

/// Clone a Git repository to a destination directory.
fn clone_git_repo(url: &str, branch: &str, dest: &Path) -> Result<(), String> {
    use std::process::Command;
    let output = Command::new("git")
        .args([
            "clone",
            "--depth", "1",
            "--branch", branch,
            url,
        ])
        .arg(dest)
        .output()
        .map_err(|e| format!("failed to spawn git: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git clone failed: {stderr}"));
    }
    Ok(())
}

/// Download and extract a plugin from an HTTP URL.
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
