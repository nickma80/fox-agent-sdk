use async_trait::async_trait;
use fox_agent_core::{PlanningStore, Tool, ToolContext, ToolError, ToolOutput};
pub use fox_agent_core::{TodoItem, TodoPriority, TodoStatus, load_todos, load_todos_with_store, save_todos, save_todos_with_store, todo_status_label};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Debug, Deserialize)]
pub(crate) struct TodoToolInput {
    #[serde(default)] pub todos: Option<Vec<TodoItem>>,
    #[serde(default)] pub merge: bool,
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
            Some(todos) => save_todos_with_store(self.store.as_ref(), &ctx.session_id, todos, params.merge),
            None => load_todos_with_store(self.store.as_ref(), &ctx.session_id),
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
