//! Memory tool for storing and recalling information across sessions.

use async_trait::async_trait;
use fox_agent_core::{
    MemoryCategory, MemoryEntry, MemoryManager, MemoryScope, RecallMode, Tool, ToolContext,
    ToolError, ToolOutput,
};
use serde::Deserialize;
use serde_json::{Value, json};

pub struct MemoryTool {
    manager: MemoryManager,
}

impl MemoryTool {
    pub fn new(config: &fox_agent_core::MemoryConfig) -> Self {
        Self {
            manager: MemoryManager::new(config),
        }
    }

    /// Create in test mode (isolated temp storage).
    pub fn new_test() -> Self {
        Self {
            manager: MemoryManager::new_test(),
        }
    }

    /// Create with a custom manager (for dependency injection).
    pub fn with_manager(manager: MemoryManager) -> Self {
        Self { manager }
    }

    /// Access the underlying manager (for harness integration).
    pub fn manager(&self) -> &MemoryManager {
        &self.manager
    }

    fn parse_scope(s: Option<&str>, default: MemoryScope) -> Result<MemoryScope, ToolError> {
        match s.unwrap_or(match default {
            MemoryScope::Project => "project",
            MemoryScope::Global => "global",
            MemoryScope::All => "all",
        }) {
            "project" => Ok(MemoryScope::Project),
            "global" => Ok(MemoryScope::Global),
            "all" => Ok(MemoryScope::All),
            other => Err(ToolError::Message {
                message: format!("Unknown scope: {other}. Use project, global, or all"),
            }),
        }
    }

    fn parse_category(s: Option<&str>) -> MemoryCategory {
        match s {
            Some(c) => MemoryCategory::from_extracted(c),
            None => MemoryCategory::Fact,
        }
    }
}

#[derive(Debug, Deserialize)]
struct MemoryInput {
    action: String,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    tags: Option<Vec<String>>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    from_id: Option<String>,
    #[serde(default)]
    to_id: Option<String>,
    #[serde(default)]
    weight: Option<f32>,
    #[serde(default)]
    depth: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    file_path: Option<String>,
    #[serde(default)]
    merge: Option<bool>,
    #[serde(default)]
    max_age_hours: Option<u64>,
    #[serde(default)]
    replacement: Option<String>,
}

#[async_trait]
impl Tool for MemoryTool {
    fn name(&self) -> &str {
        "memory"
    }

    fn description(&self) -> &str {
        "Manage cross-session memory. Supports remember, recall, search, list, forget, disable, enable, redact, tag, link, related, stats, reembed, reindex, refresh_clusters, rebuild_ann, export, import, compact."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["remember", "recall", "search", "list", "forget", "disable", "enable", "redact", "tag", "link", "related", "stats", "reembed", "reindex", "refresh_clusters", "rebuild_ann", "export", "import", "compact"],
                    "description": "Action to perform."
                },
                "content": { "type": "string", "description": "Content to remember (for remember action)." },
                "category": { "type": "string", "enum": ["fact", "preference", "entity", "correction"], "description": "Memory category (default: fact)." },
                "query": { "type": "string", "description": "Search/recall query." },
                "id": { "type": "string", "description": "Memory ID." },
                "tags": { "type": "array", "items": { "type": "string" }, "description": "Tags to attach." },
                "scope": { "type": "string", "enum": ["project", "global", "all"], "description": "Memory scope (default: project)." },
                "from_id": { "type": "string", "description": "Source memory ID for link action." },
                "to_id": { "type": "string", "description": "Target memory ID for link action." },
                "weight": { "type": "number", "description": "Link weight 0.0-1.0 (default: 0.5)." },
                "depth": { "type": "integer", "description": "Graph traversal depth (default: 2)." },
                "limit": { "type": "integer", "description": "Max results (default: 10)." },
                "mode": { "type": "string", "enum": ["recent", "keyword", "semantic", "cascade"], "description": "Recall mode (default: keyword)." },
                "file_path": { "type": "string", "description": "Path used by export/import actions." },
                "merge": { "type": "boolean", "description": "Whether import should merge with existing memories (default: true)." },
                "max_age_hours": { "type": "integer", "description": "Max age hours for compact/gc action (default: 720)." },
                "replacement": { "type": "string", "description": "Replacement content for redact action." }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let input: MemoryInput = serde_json::from_value(input).map_err(|e| ToolError::Message {
            message: format!("invalid memory input: {e}"),
        })?;

        // Set project directory from context
        let manager = if let Some(ref wd) = ctx.working_dir {
            self.manager.clone().with_project_dir(wd.clone())
        } else {
            self.manager.clone()
        };

        match input.action.as_str() {
            "remember" => {
                let content = input.content.ok_or_else(|| ToolError::Message {
                    message: "content required for remember action".to_string(),
                })?;
                let category = Self::parse_category(input.category.as_deref());
                let scope_str = input.scope.as_deref().unwrap_or("project");
                let scope = Self::parse_scope(Some(scope_str), MemoryScope::Project)?;

                let entry = MemoryEntry::new(category.clone(), &content)
                    .with_source(&ctx.session_id)
                    .with_tags(input.tags.unwrap_or_default());

                let id = match scope {
                    MemoryScope::Global => manager.remember_global(entry).map_err(|e| ToolError::Message { message: e })?,
                    _ => manager.remember_project(entry).map_err(|e| ToolError::Message { message: e })?,
                };

                Ok(ToolOutput {
                    text: format!("Remembered ({category}): \"{content}\" [id: {id}]"),
                    is_error: false,
                    json: Some(json!({
                        "action": "remember",
                        "id": id,
                        "category": category.to_string(),
                        "scope": scope_str,
                    })),
                })
            }

            "recall" => {
                let limit = input.limit.unwrap_or(10);
                let scope = Self::parse_scope(input.scope.as_deref(), MemoryScope::All)?;
                let query = input.query.as_deref();
                let mode = match input.mode.as_deref().unwrap_or("keyword") {
                    "recent" => RecallMode::Recent,
                    "keyword" => RecallMode::Keyword,
                    "semantic" => RecallMode::Semantic,
                    "cascade" => RecallMode::Cascade,
                    other => return Err(ToolError::Message {
                        message: format!("Unknown mode: {other}. Use recent, keyword, semantic, or cascade"),
                    }),
                };

                let results = manager.recall_detailed(query, limit, mode, scope).map_err(|e| ToolError::Message { message: e })?;

                if results.is_empty() {
                    return Ok(ToolOutput {
                        text: match query {
                            Some(q) => format!("No memories found matching '{q}'."),
                            None => "No memories stored yet.".to_string(),
                        },
                        is_error: false,
                        json: Some(json!({ "action": "recall", "count": 0 })),
                    });
                }

                let mut out = format!("Found {} memories:\n\n", results.len());
                for hit in &results {
                    let entry = &hit.entry;
                    let tags = if entry.tags.is_empty() {
                        String::new()
                    } else {
                        format!(" [{}]", entry.tags.join(", "))
                    };
                    out.push_str(&format!(
                        "- [{}] {}{}\n  id: {} (score: {:.0}%, source: {:?})\n",
                        entry.category, entry.content, tags, entry.id, hit.score * 100.0, hit.retrieval_source
                    ));
                    out.push_str(&format!(
                        "  breakdown: semantic={:?}, keyword={:?}, graph={:?}, recency={:.2}, trust={:.2}, final={:.2}\n\n",
                        hit.score_breakdown.semantic_score,
                        hit.score_breakdown.keyword_score,
                        hit.score_breakdown.graph_score,
                        hit.score_breakdown.recency_score,
                        hit.score_breakdown.trust_score,
                        hit.score_breakdown.final_score
                    ));
                }

                let ids: Vec<String> = results.iter().map(|hit| hit.entry.id.clone()).collect();
                Ok(ToolOutput {
                    text: out,
                    is_error: false,
                    json: Some(json!({
                        "action": "recall",
                        "count": results.len(),
                        "memory_ids": ids,
                        "results": results.iter().map(|hit| json!({
                            "id": hit.entry.id,
                            "score": hit.score,
                            "source": format!("{:?}", hit.retrieval_source),
                            "score_breakdown": {
                                "semantic": hit.score_breakdown.semantic_score,
                                "keyword": hit.score_breakdown.keyword_score,
                                "graph": hit.score_breakdown.graph_score,
                                "recency": hit.score_breakdown.recency_score,
                                "trust": hit.score_breakdown.trust_score,
                                "final": hit.score_breakdown.final_score
                            }
                        })).collect::<Vec<_>>(),
                    })),
                })
            }

            "search" => {
                let query = input.query.as_deref().unwrap_or("");
                let scope = Self::parse_scope(input.scope.as_deref(), MemoryScope::All)?;
                let results = manager.search(query, scope).map_err(|e| ToolError::Message { message: e })?;

                if results.is_empty() {
                    return Ok(ToolOutput {
                        text: format!("No memories matching '{query}'"),
                        is_error: false,
                        json: Some(json!({ "action": "search", "count": 0 })),
                    });
                }

                let mut out = format!("Found {} memories:\n\n", results.len());
                for e in &results {
                    out.push_str(&format!("- [{}] {}\n  id: {}\n\n", e.category, e.content, e.id));
                }
                Ok(ToolOutput {
                    text: out,
                    is_error: false,
                    json: Some(json!({
                        "action": "search",
                        "count": results.len(),
                    })),
                })
            }

            "list" => {
                let scope = Self::parse_scope(input.scope.as_deref(), MemoryScope::All)?;
                let all = manager.list(scope).map_err(|e| ToolError::Message { message: e })?;
                if all.is_empty() {
                    return Ok(ToolOutput {
                        text: "No memories stored.".to_string(),
                        is_error: false,
                        json: Some(json!({ "action": "list", "count": 0 })),
                    });
                }
                let mut out = format!("All memories ({}):\n\n", all.len());
                for e in &all {
                    out.push_str(&format!("- [{}] {}\n  id: {}\n\n", e.category, e.content, e.id));
                }
                Ok(ToolOutput {
                    text: out,
                    is_error: false,
                    json: Some(json!({ "action": "list", "count": all.len() })),
                })
            }

            "forget" => {
                let id = input.id.ok_or_else(|| ToolError::Message {
                    message: "id required for forget action".to_string(),
                })?;
                let found = manager.forget(&id).map_err(|e| ToolError::Message { message: e })?;
                Ok(ToolOutput {
                    text: if found { format!("Forgot: {id}") } else { format!("Not found: {id}") },
                    is_error: false,
                    json: Some(json!({ "action": "forget", "id": id, "found": found })),
                })
            }

            "disable" => {
                let id = input.id.ok_or_else(|| ToolError::Message {
                    message: "id required for disable action".to_string(),
                })?;
                let found = manager.disable_memory(&id).map_err(|e| ToolError::Message { message: e })?;
                Ok(ToolOutput {
                    text: if found { format!("Disabled memory: {id}") } else { format!("Not found: {id}") },
                    is_error: false,
                    json: Some(json!({ "action": "disable", "id": id, "found": found })),
                })
            }

            "enable" => {
                let id = input.id.ok_or_else(|| ToolError::Message {
                    message: "id required for enable action".to_string(),
                })?;
                let found = manager.enable_memory(&id).map_err(|e| ToolError::Message { message: e })?;
                Ok(ToolOutput {
                    text: if found { format!("Enabled memory: {id}") } else { format!("Not found: {id}") },
                    is_error: false,
                    json: Some(json!({ "action": "enable", "id": id, "found": found })),
                })
            }

            "redact" => {
                let id = input.id.ok_or_else(|| ToolError::Message {
                    message: "id required for redact action".to_string(),
                })?;
                let replacement = input.replacement.unwrap_or_else(|| "[redacted]".to_string());
                let found = manager
                    .redact_memory(&id, &replacement)
                    .map_err(|e| ToolError::Message { message: e })?;
                Ok(ToolOutput {
                    text: if found {
                        format!("Redacted memory {id}.")
                    } else {
                        format!("Not found: {id}")
                    },
                    is_error: false,
                    json: Some(json!({
                        "action": "redact",
                        "id": id,
                        "found": found,
                        "replacement": replacement,
                    })),
                })
            }

            "tag" => {
                let id = input.id.ok_or_else(|| ToolError::Message {
                    message: "id required for tag action".to_string(),
                })?;
                let tags = input.tags.ok_or_else(|| ToolError::Message {
                    message: "tags required for tag action".to_string(),
                })?;
                for tag in &tags {
                    manager.tag_memory(&id, tag).map_err(|e| ToolError::Message { message: e })?;
                }
                Ok(ToolOutput {
                    text: format!("Tagged memory {id} with: {}", tags.join(", ")),
                    is_error: false,
                    json: Some(json!({ "action": "tag", "id": id, "tags": tags })),
                })
            }

            "link" => {
                let from_id = input.from_id.ok_or_else(|| ToolError::Message {
                    message: "from_id required for link action".to_string(),
                })?;
                let to_id = input.to_id.ok_or_else(|| ToolError::Message {
                    message: "to_id required for link action".to_string(),
                })?;
                let weight = input.weight.unwrap_or(0.5);
                manager.link_memories(&from_id, &to_id, weight).map_err(|e| ToolError::Message { message: e })?;
                Ok(ToolOutput {
                    text: format!("Linked memories {from_id} -> {to_id} (weight {weight:.2})"),
                    is_error: false,
                    json: Some(json!({ "action": "link", "from_id": from_id, "to_id": to_id, "weight": weight })),
                })
            }

            "related" => {
                let id = input.id.ok_or_else(|| ToolError::Message {
                    message: "id required for related action".to_string(),
                })?;
                let depth = input.depth.unwrap_or(2);
                let related = manager.get_related(&id, depth).map_err(|e| ToolError::Message { message: e })?;
                if related.is_empty() {
                    return Ok(ToolOutput {
                        text: format!("No related memories found for {id}"),
                        is_error: false,
                        json: Some(json!({ "action": "related", "count": 0 })),
                    });
                }
                let mut out = format!("Found {} memories related to {id} (depth {depth}):\n\n", related.len());
                for e in &related {
                    out.push_str(&format!("- [{}] {}\n  id: {}\n\n", e.category, e.content, e.id));
                }
                Ok(ToolOutput {
                    text: out,
                    is_error: false,
                    json: Some(json!({ "action": "related", "count": related.len() })),
                })
            }

            "stats" => {
                let stats = manager.graph_stats().map_err(|e| ToolError::Message { message: e })?;
                Ok(ToolOutput {
                    text: format!(
                        "Memory Graph Statistics:\n  Memories: {}\n  Tags: {}\n  Edges: {}\n  Clusters: {}",
                        stats.0, stats.1, stats.2, stats.3
                    ),
                    is_error: false,
                    json: Some(json!({
                        "action": "stats",
                        "memories": stats.0,
                        "tags": stats.1,
                        "edges": stats.2,
                        "clusters": stats.3,
                    })),
                })
            }

            "reembed" => {
                let scope = Self::parse_scope(input.scope.as_deref(), MemoryScope::All)?;
                let count = manager.reembed(scope).map_err(|e| ToolError::Message { message: e })?;
                Ok(ToolOutput {
                    text: format!("Re-embedded {count} memories."),
                    is_error: false,
                    json: Some(json!({
                        "action": "reembed",
                        "count": count,
                    })),
                })
            }

            "reindex" => {
                let scope = Self::parse_scope(input.scope.as_deref(), MemoryScope::All)?;
                let count = manager.reindex(scope).map_err(|e| ToolError::Message { message: e })?;
                Ok(ToolOutput {
                    text: format!("Reindexed {count} memories."),
                    is_error: false,
                    json: Some(json!({
                        "action": "reindex",
                        "count": count,
                    })),
                })
            }

            "refresh_clusters" => {
                let scope = Self::parse_scope(input.scope.as_deref(), MemoryScope::All)?;
                let stats = manager.refresh_clusters(scope).map_err(|e| ToolError::Message { message: e })?;
                Ok(ToolOutput {
                    text: format!(
                        "Refreshed clusters. project_clusters={}, global_clusters={}",
                        stats.project_clusters, stats.global_clusters
                    ),
                    is_error: false,
                    json: Some(json!({
                        "action": "refresh_clusters",
                        "project_clusters": stats.project_clusters,
                        "global_clusters": stats.global_clusters,
                    })),
                })
            }

            "rebuild_ann" => {
                let scope = Self::parse_scope(input.scope.as_deref(), MemoryScope::All)?;
                let stats = manager.rebuild_ann(scope).map_err(|e| ToolError::Message { message: e })?;
                Ok(ToolOutput {
                    text: format!(
                        "Rebuilt ANN indexes. project_vectors={}, global_vectors={}",
                        stats.project_vectors, stats.global_vectors
                    ),
                    is_error: false,
                    json: Some(json!({
                        "action": "rebuild_ann",
                        "project_vectors": stats.project_vectors,
                        "global_vectors": stats.global_vectors,
                    })),
                })
            }

            "export" => {
                let scope = Self::parse_scope(input.scope.as_deref(), MemoryScope::All)?;
                let file_path = input.file_path.ok_or_else(|| ToolError::Message {
                    message: "file_path required for export action".to_string(),
                })?;
                let stats = manager
                    .export_to_path(scope, std::path::Path::new(&file_path))
                    .map_err(|e| ToolError::Message { message: e })?;
                Ok(ToolOutput {
                    text: format!(
                        "Exported memories to {} (project={}, global={}).",
                        file_path, stats.project_memories, stats.global_memories
                    ),
                    is_error: false,
                    json: Some(json!({
                        "action": "export",
                        "file_path": file_path,
                        "project_memories": stats.project_memories,
                        "global_memories": stats.global_memories,
                    })),
                })
            }

            "import" => {
                let file_path = input.file_path.ok_or_else(|| ToolError::Message {
                    message: "file_path required for import action".to_string(),
                })?;
                let merge = input.merge.unwrap_or(true);
                let stats = manager
                    .import_from_path(std::path::Path::new(&file_path), merge)
                    .map_err(|e| ToolError::Message { message: e })?;
                Ok(ToolOutput {
                    text: format!(
                        "Imported memories from {} (merge={}) -> project={}, global={}.",
                        file_path, merge, stats.project_memories, stats.global_memories
                    ),
                    is_error: false,
                    json: Some(json!({
                        "action": "import",
                        "file_path": file_path,
                        "merge": merge,
                        "project_memories": stats.project_memories,
                        "global_memories": stats.global_memories,
                    })),
                })
            }

            "compact" => {
                let max_age_hours = input.max_age_hours.unwrap_or(24 * 30);
                let scope = Self::parse_scope(input.scope.as_deref(), MemoryScope::All)?;
                let compact = manager.compact(scope, max_age_hours).map_err(|e| ToolError::Message { message: e })?;
                Ok(ToolOutput {
                    text: format!(
                        "Compacted memories. project_removed={}, global_removed={}, removed_files={}, scanned={}.",
                        compact.project_removed, compact.global_removed, compact.removed_files, compact.total_scanned
                    ),
                    is_error: false,
                    json: Some(json!({
                        "action": "compact",
                        "project_removed": compact.project_removed,
                        "global_removed": compact.global_removed,
                        "removed_files": compact.removed_files,
                        "total_scanned": compact.total_scanned,
                    })),
                })
            }

            other => Err(ToolError::Message {
                message: format!("Unknown action: {other}"),
            }),
        }
    }
}
