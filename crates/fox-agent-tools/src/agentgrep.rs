mod args;
mod render;

use args::{
    build_find_args, build_grep_args, build_outline_args, build_smart_args_and_query,
    resolve_search_root,
};
use fox_agent_core::{Tool, ToolContext, ToolError, ToolOutput, intent_schema_property};
use render::{
    render_find_output, render_grep_output, render_outline_output, render_smart_output,
};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Deserialize)]
struct AgentGrepInput {
    #[serde(default = "default_mode")]
    mode: String,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    terms: Option<Vec<String>>,
    #[serde(default)]
    regex: Option<bool>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    glob: Option<String>,
    #[serde(rename = "type", default)]
    file_type: Option<String>,
    #[serde(default)]
    hidden: Option<bool>,
    #[serde(default)]
    no_ignore: Option<bool>,
    #[serde(default)]
    max_files: Option<usize>,
    #[serde(default)]
    max_regions: Option<usize>,
    #[serde(default)]
    full_region: Option<String>,
    #[serde(default)]
    debug_plan: Option<bool>,
    #[serde(default)]
    debug_score: Option<bool>,
    #[serde(default)]
    #[expect(dead_code)]
    intent: Option<String>,
    #[serde(default)]
    paths_only: Option<bool>,
}

fn default_mode() -> String {
    "grep".to_string()
}

pub struct AgentGrepTool;

impl AgentGrepTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl Tool for AgentGrepTool {
    fn name(&self) -> &str {
        "agentgrep"
    }

    fn description(&self) -> &str {
        "Search code and file names. Defaults to grep mode when mode is omitted. Supports grep (text search), find (file search), outline (file summarization), and trace (DSL-based relationship search). If a search returns 0 results twice in a row, switch to a different tool like `grep` or `ls` instead of retrying with similar queries."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "intent": intent_schema_property(),
                "mode": {
                    "type": "string",
                    "enum": ["grep", "find", "outline", "trace"],
                    "description": "Optional search mode. Defaults to grep. Use grep for normal code/text search, find for file-name/path search, outline to summarize one file, and trace for DSL-based relationship search."
                },
                "query": {
                    "type": "string",
                    "description": "Search query. Required for grep. For find, provide query terms to rank matching file paths. Grep treats query as literal text unless regex=true."
                },
                "file": {
                    "type": "string",
                    "description": "Single file to inspect. Required for outline."
                },
                "terms": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Trace DSL terms, for example [\"subject:auth_status\", \"relation:rendered\", \"support:ui\"]."
                },
                "regex": {
                    "type": "boolean",
                    "description": "When true in grep mode, interpret query as a regular expression. Defaults to false."
                },
                "path": {
                    "type": "string",
                    "description": "Directory or file to search, relative to the working directory. Omit to search the workspace."
                },
                "glob": {
                    "type": "string",
                    "description": "Optional file glob filter such as **/*.rs."
                },
                "type": {
                    "type": "string",
                    "description": "Optional file type filter, such as rs, py, js, ts, or md."
                },
                "max_files": {
                    "type": "integer",
                    "description": "Maximum number of files to return for find/trace modes."
                },
                "max_regions": {
                    "type": "integer",
                    "description": "Maximum number of matching regions to return."
                },
                "paths_only": {
                    "type": "boolean",
                    "description": "Return only matching paths instead of match excerpts where supported."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let params: AgentGrepInput = serde_json::from_value(input).map_err(|e| ToolError::Message {
            message: format!("invalid agentgrep input: {e}"),
        })?;

        let outcome = execute_agentgrep(&params, &ctx);
        match outcome {
            Ok(output) => Ok(output),
            Err(err) => Err(ToolError::Message {
                message: format!("agentgrep {} failed: {}", params.mode, err),
            }),
        }
    }
}

fn execute_agentgrep(
    params: &AgentGrepInput,
    ctx: &ToolContext,
) -> Result<ToolOutput, ToolError> {
    match params.mode.as_str() {
        "grep" => {
            let args = build_grep_args(params, ctx)?;
            let root = resolve_search_root(ctx, args.path.as_deref());
            let result = agentgrep::search::run_grep(&root, &args)
                .map_err(|e| ToolError::Message { message: format!("grep failed: {e}") })?;
            Ok(ToolOutput {
                text: render_grep_output(&result, &args, params.max_regions),
                is_error: false,
                json: Some(json!({
                    "mode": "grep",
                    "query": &args.query,
                    "total_matches": result.total_matches,
                    "total_files": result.total_files,
                })),
            })
        }
        "find" => {
            let args = build_find_args(params, ctx)?;
            let root = resolve_search_root(ctx, args.path.as_deref());
            let result = agentgrep::find::run_find(&root, &args);
            Ok(ToolOutput {
                text: render_find_output(&result, &args),
                is_error: false,
                json: Some(json!({
                    "mode": "find",
                    "files": result.files.len(),
                })),
            })
        }
        "outline" => {
            let args = build_outline_args(params, ctx)?;
            let root = resolve_search_root(ctx, args.path.as_deref());
            let result = agentgrep::outline::run_outline(&root, &args)
                .map_err(|e| ToolError::Message { message: format!("outline failed: {e}") })?;
            Ok(ToolOutput {
                text: render_outline_output(&result),
                is_error: false,
                json: Some(json!({
                    "mode": "outline",
                    "file": &result.path,
                    "language": &result.language,
                    "total_lines": result.total_lines,
                })),
            })
        }
        "trace" | "smart" => {
            let (args, query) = build_smart_args_and_query(params, ctx)?;
            let root = resolve_search_root(ctx, args.path.as_deref());
            let result = agentgrep::smart_engine::run_smart(&root, &query, &args)
                .map_err(|e| ToolError::Message { message: format!("trace failed: {e}") })?;
            Ok(ToolOutput {
                text: render_smart_output(&result, &args),
                is_error: false,
                json: Some(json!({
                    "mode": params.mode,
                    "subject": &result.query.subject,
                    "relation": result.query.relation.as_str(),
                    "total_files": result.summary.total_files,
                    "total_regions": result.summary.total_regions,
                })),
            })
        }
        _ => Err(ToolError::Message {
            message: format!(
                "Unsupported agentgrep mode: {}. Use grep, find, outline, or trace.",
                params.mode
            ),
        }),
    }
}
