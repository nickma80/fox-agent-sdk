use async_trait::async_trait;
use fox_agent_core::{PlanningStore, Tool, ToolContext, ToolError, ToolOutput};
pub use fox_agent_core::{
    TodoItem, TodoPriority, TodoStatus, load_todos, load_todos_with_store, save_todos,
    save_todos_with_store, todo_status_label,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

/// Like `TodoItem` but with optional `content` and `priority` so that
/// `merge=true` updates can omit unchanged fields.
#[derive(Debug, Deserialize)]
pub(crate) struct TodoPatchItem {
    pub id: String,
    #[serde(default)]
    pub content: Option<String>,
    pub status: TodoStatus,
    #[serde(default)]
    pub priority: Option<TodoPriority>,
}

impl TodoPatchItem {
    fn apply(self, existing: &mut TodoItem) {
        existing.status = self.status;
        if let Some(content) = self.content {
            existing.content = content;
        }
        if let Some(priority) = self.priority {
            existing.priority = priority;
        }
    }

    fn into_todo_item(self) -> TodoItem {
        TodoItem {
            id: self.id,
            content: self.content.unwrap_or_default(),
            status: self.status,
            priority: self.priority.unwrap_or(TodoPriority::Medium),
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct TodoToolInput {
    #[serde(default)]
    pub todos: Option<Vec<TodoPatchItem>>,
    #[serde(default)]
    pub merge: bool,
}

/// Tool that reads or updates the session-local todo list.
pub struct TodoTool {
    store: Arc<dyn PlanningStore>,
}

impl TodoTool {
    pub fn new(store: Arc<dyn PlanningStore>) -> Self {
        Self { store }
    }
}

impl Default for TodoTool {
    fn default() -> Self {
        Self::new(fox_agent_core::default_planning_store())
    }
}

#[async_trait]
impl Tool for TodoTool {
    fn name(&self) -> &str {
        "todo"
    }
    fn description(&self) -> &str {
        "Read or update the session-local todo list"
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["id", "status"],
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
        let params: TodoToolInput =
            serde_json::from_value(input).map_err(|err| ToolError::Message {
                message: format!("invalid todo input: {err}"),
            })?;
        let todos = match params.todos {
            Some(patches) => {
                if params.merge {
                    todo_merge(self.store.as_ref(), &ctx.session_id, patches)
                } else {
                    save_todos_with_store(
                        self.store.as_ref(),
                        &ctx.session_id,
                        patches.into_iter().map(|p| p.into_todo_item()).collect(),
                        false,
                    )
                }
            }
            None => load_todos_with_store(self.store.as_ref(), &ctx.session_id),
        };
        let remaining = todos
            .iter()
            .filter(|item| item.status != TodoStatus::Completed)
            .count();
        Ok(ToolOutput {
            text: serde_json::to_string_pretty(&todos).map_err(|err| ToolError::Message {
                message: format!("failed to serialize todos: {err}"),
            })?,
            is_error: false,
            json: Some(json!({ "todos": todos, "remaining": remaining })),
        })
    }
}

/// Merge patch items into existing todos — only overwrite provided fields.
fn todo_merge(
    store: &dyn PlanningStore,
    session_id: &str,
    patches: Vec<TodoPatchItem>,
) -> Vec<TodoItem> {
    use fox_agent_core::update_session_snapshot;
    update_session_snapshot(store, session_id, Some("todo"), |snapshot| {
        for patch in patches {
            if let Some(existing) = snapshot.todos.iter_mut().find(|t| t.id == patch.id) {
                patch.apply(existing);
            } else {
                snapshot.todos.push(patch.into_todo_item());
            }
        }
    })
    .map(|s| s.todos)
    .unwrap_or_default()
}
