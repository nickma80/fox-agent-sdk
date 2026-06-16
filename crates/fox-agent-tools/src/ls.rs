use async_trait::async_trait;
use fox_agent_core::{Tool, ToolContext, ToolError, ToolOutput};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;

const MAX_ENTRIES: usize = 100;
const DEFAULT_IGNORE: &[&str] = &[
    "node_modules",
    "__pycache__",
    ".git",
    "dist",
    "build",
    "target",
    ".next",
    ".nuxt",
    "venv",
    ".venv",
    "coverage",
    ".cache",
];

fn resolve_path(working_dir: Option<&Path>, path: &Path) -> std::path::PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else if let Some(base) = working_dir {
        base.join(path)
    } else {
        path.to_path_buf()
    }
}

pub struct LsTool;

impl LsTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct LsInput {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    ignore: Option<Vec<String>>,
}

struct DirEntry {
    name: String,
    is_dir: bool,
    depth: usize,
}

#[async_trait]
impl Tool for LsTool {
    fn name(&self) -> &str {
        "ls"
    }

    fn description(&self) -> &str {
        "List directory contents with recursive tree view."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory path. Defaults to current directory."
                },
                "ignore": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Additional ignore patterns."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let params: LsInput = serde_json::from_value(input).map_err(|e| ToolError::Message {
            message: format!("invalid ls input: {e}"),
        })?;

        let base_path = params.path.clone().unwrap_or_else(|| ".".to_string());
        let base = resolve_path(ctx.working_dir.as_deref(), Path::new(&base_path));
        let ignore_extra = params.ignore.clone();

        if !base.exists() {
            return Err(ToolError::Message {
                message: format!("Directory not found: {}", base_path),
            });
        }

        if !base.is_dir() {
            return Err(ToolError::Message {
                message: format!("Not a directory: {}", base_path),
            });
        }

        let entries: Vec<DirEntry> = tokio::task::spawn_blocking(move || {
            let mut ignore_patterns: Vec<String> =
                DEFAULT_IGNORE.iter().map(|s| s.to_string()).collect();
            if let Some(extra) = ignore_extra {
                ignore_patterns.extend(extra);
            }

            let mut entries: Vec<DirEntry> = Vec::new();
            collect_entries(&base, 0, &ignore_patterns, &mut entries, MAX_ENTRIES)?;
            Ok::<_, String>(entries)
        })
        .await
        .map_err(|e| ToolError::Message {
            message: format!("ls task failed: {e}"),
        })?
        .map_err(|e| ToolError::Message {
            message: e,
        })?;

        let truncated = entries.len() >= MAX_ENTRIES;

        let mut output = String::new();
        output.push_str(&format!("{}/\n", base_path));

        for entry in &entries {
            let indent = "  ".repeat(entry.depth);
            let suffix = if entry.is_dir { "/" } else { "" };
            output.push_str(&format!("{}{}{}\n", indent, entry.name, suffix));
        }

        if truncated {
            output.push_str(&format!("\n... truncated at {} entries", MAX_ENTRIES));
        }

        let file_count = entries.iter().filter(|e| !e.is_dir).count();
        let dir_count = entries.iter().filter(|e| e.is_dir).count();
        output.push_str(&format!("\n{} files, {} directories", file_count, dir_count));

        Ok(ToolOutput {
            text: output,
            is_error: false,
            json: Some(json!({
                "path": base_path,
                "file_count": file_count,
                "dir_count": dir_count,
                "truncated": truncated,
            })),
        })
    }
}

fn collect_entries(
    dir: &Path,
    depth: usize,
    ignore: &[String],
    entries: &mut Vec<DirEntry>,
    max: usize,
) -> Result<(), String> {
    if entries.len() >= max {
        return Ok(());
    }

    let mut items: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("failed to read dir `{}`: {e}", dir.display()))?
        .filter_map(|e| e.ok())
        .collect();

    // Cache file_type from DirEntry
    let mut typed_items: Vec<(std::fs::DirEntry, bool)> = items
        .drain(..)
        .map(|e| {
            let is_dir = e.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
            (e, is_dir)
        })
        .collect();

    typed_items.sort_by(|(a, a_dir), (b, b_dir)| match (*a_dir, *b_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.file_name().cmp(&b.file_name()),
    });

    for (item, is_dir) in typed_items {
        if entries.len() >= max {
            break;
        }

        let name = item.file_name().to_string_lossy().to_string();

        if ignore.iter().any(|p| {
            glob::Pattern::new(p)
                .map(|pat| pat.matches(&name))
                .unwrap_or(false)
                || name == *p
        }) {
            continue;
        }

        if name.starts_with('.') && name != "." && name != ".." {
            continue;
        }

        entries.push(DirEntry {
            name: name.clone(),
            is_dir,
            depth: depth + 1,
        });

        if is_dir && depth < 5 {
            let path = item.path();
            collect_entries(&path, depth + 1, ignore, entries, max)?;
        }
    }

    Ok(())
}
