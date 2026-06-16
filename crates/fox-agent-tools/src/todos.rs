use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::RwLock as StdRwLock;

/// Status of a single todo item.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    /// Not yet started
    Pending,
    /// Currently being worked on
    InProgress,
    /// Completed
    Completed,
}

/// Priority level for a todo item.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TodoPriority {
    High,
    Medium,
    Low,
}

/// A single item on the session-local todo list.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TodoItem {
    /// Unique item identifier
    pub id: String,
    /// Task description
    pub content: String,
    /// Current status
    pub status: TodoStatus,
    /// Priority level
    pub priority: TodoPriority,
}

fn todo_store() -> &'static StdRwLock<HashMap<String, Vec<TodoItem>>> {
    static STORE: OnceLock<StdRwLock<HashMap<String, Vec<TodoItem>>>> = OnceLock::new();
    STORE.get_or_init(|| StdRwLock::new(HashMap::new()))
}

/// Load the todo list for a session.
pub fn load_todos(session_id: &str) -> Vec<TodoItem> {
    todo_store().read().ok().and_then(|store| store.get(session_id).cloned()).unwrap_or_default()
}

/// Save (and optionally merge) the todo list for a session.
pub fn save_todos(session_id: &str, todos: Vec<TodoItem>, merge: bool) -> Vec<TodoItem> {
    let Ok(mut store) = todo_store().write() else { return todos };
    let entry = store.entry(session_id.to_string()).or_default();
    if !merge { *entry = todos; return entry.clone(); }
    for incoming in todos {
        if let Some(existing) = entry.iter_mut().find(|item| item.id == incoming.id) {
            *existing = incoming;
        } else { entry.push(incoming); }
    }
    entry.clone()
}

/// Human-readable label for a todo status.
pub fn todo_status_label(status: &TodoStatus) -> &'static str {
    match status {
        TodoStatus::Pending => "pending",
        TodoStatus::InProgress => "in_progress",
        TodoStatus::Completed => "completed",
    }
}

use async_trait::async_trait;
use fox_agent_core::{Tool, ToolContext, ToolError, ToolOutput};
use serde_json::{Value, json};

#[derive(Debug, Deserialize)]
pub(crate) struct TodoToolInput {
    #[serde(default)] pub todos: Option<Vec<TodoItem>>,
    #[serde(default)] pub merge: bool,
}

/// Tool that reads or updates the session-local todo list.
pub struct TodoTool;

#[async_trait]
impl Tool for TodoTool {
    fn name(&self) -> &str { "todo" }
    fn description(&self) -> &str { "Read or update the session-local todo list" }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["id", "content", "status", "priority"],
                        "properties": {
                            "id": { "type": "string" },
                            "content": { "type": "string" },
                            "status": { "type": "string", "enum": ["pending", "in_progress", "completed"] },
                            "priority": { "type": "string", "enum": ["high", "medium", "low"] }
                        }
                    }
                },
                "merge": { "type": "boolean" }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let params: TodoToolInput = serde_json::from_value(input).map_err(|err| ToolError::Message {
            message: format!("invalid todo input: {err}"),
        })?;
        let todos = match params.todos {
            Some(todos) => save_todos(&ctx.session_id, todos, params.merge),
            None => load_todos(&ctx.session_id),
        };
        let remaining = todos.iter().filter(|item| item.status != TodoStatus::Completed).count();
        Ok(ToolOutput {
            text: serde_json::to_string_pretty(&todos).map_err(|err| ToolError::Message {
                message: format!("failed to serialize todos: {err}"),
            })?,
            is_error: false,
            json: Some(json!({ "todos": todos, "remaining": remaining })),
        })
    }
}
