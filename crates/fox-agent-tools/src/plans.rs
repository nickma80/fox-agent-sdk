use async_trait::async_trait;
use fox_agent_core::{Tool, ToolContext, ToolError, ToolOutput};
pub use fox_agent_core::{
    PlanItem, PlanStatus, PlanPriority, PlanningStore, VersionedPlan,
    load_plan, load_plan_with_store, save_plan, save_plan_with_store,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Debug, Deserialize)]
pub(crate) struct PlanToolInput {
    #[serde(default)] pub items: Option<Vec<PlanItem>>,
    #[serde(default)] pub merge: bool,
}

/// Tool that reads or updates the session-local shared plan.
pub struct PlanTool {
    store: Arc<dyn PlanningStore>,
}

impl PlanTool {
    pub fn new(store: Arc<dyn PlanningStore>) -> Self {
        Self { store }
    }
}

impl Default for PlanTool {
    fn default() -> Self {
        Self::new(fox_agent_core::default_planning_store())
    }
}

#[async_trait]
impl Tool for PlanTool {
    fn name(&self) -> &str { "plan" }
    fn description(&self) -> &str { "Read or update the session-local shared plan" }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "items": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["id", "content", "status", "priority"],
                        "properties": {
                            "id": { "type": "string" },
                            "content": { "type": "string" },
                            "status": { "type": "string", "enum": ["pending", "in_progress", "completed"] },
                            "priority": { "type": "string", "enum": ["high", "medium", "low"] },
                            "assigned_to": { "type": "string" },
                            "blocked_by": { "type": "array", "items": { "type": "string" } }
                        }
                    }
                },
                "merge": { "type": "boolean" }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let params: PlanToolInput = serde_json::from_value(input).map_err(|err| ToolError::Message {
            message: format!("invalid plan input: {err}"),
        })?;
        let plan = match params.items {
            Some(items) => save_plan_with_store(self.store.as_ref(), &ctx.session_id, items, params.merge),
            None => load_plan_with_store(self.store.as_ref(), &ctx.session_id),
        };
        Ok(ToolOutput {
            text: serde_json::to_string_pretty(&plan).map_err(|err| ToolError::Message {
                message: format!("failed to serialize plan: {err}"),
            })?,
            is_error: false,
            json: Some(json!({ "version": plan.version, "items": plan.items })),
        })
    }
}
