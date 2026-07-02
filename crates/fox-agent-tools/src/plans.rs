use async_trait::async_trait;
use fox_agent_core::{Tool, ToolContext, ToolError, ToolOutput};
pub use fox_agent_core::{
    PlanItem, PlanStatus, PlanPriority, PlanningStore, VersionedPlan,
    load_plan, load_plan_with_store, save_plan, save_plan_with_store,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

/// Like `PlanItem` but with optional fields so that `merge=true` updates
/// can omit unchanged fields and only supply `id` + `status`.
#[derive(Debug, Deserialize)]
pub(crate) struct PlanPatchItem {
    pub id: String,
    #[serde(default)]
    pub content: Option<String>,
    pub status: PlanStatus,
    #[serde(default)]
    pub priority: Option<PlanPriority>,
    #[serde(default)]
    pub assigned_to: Option<String>,
    #[serde(default)]
    pub blocked_by: Vec<String>,
}

impl PlanPatchItem {
    fn apply(self, existing: &mut PlanItem) {
        existing.status = self.status;
        if let Some(content) = self.content {
            existing.content = content;
        }
        if let Some(priority) = self.priority {
            existing.priority = priority;
        }
        if self.assigned_to.is_some() {
            existing.assigned_to = self.assigned_to;
        }
        if !self.blocked_by.is_empty() {
            existing.blocked_by = self.blocked_by;
        }
    }

    fn into_plan_item(self) -> PlanItem {
        PlanItem {
            id: self.id,
            content: self.content.unwrap_or_default(),
            status: self.status,
            priority: self.priority.unwrap_or(PlanPriority::Medium),
            assigned_to: self.assigned_to,
            blocked_by: self.blocked_by,
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct PlanToolInput {
    #[serde(default)] pub items: Option<Vec<PlanPatchItem>>,
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
                        "required": ["id", "status"],
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
            Some(patches) => {
                if params.merge {
                    plan_merge(self.store.as_ref(), &ctx.session_id, patches)
                } else {
                    save_plan_with_store(
                        self.store.as_ref(),
                        &ctx.session_id,
                        patches.into_iter().map(|p| p.into_plan_item()).collect(),
                        false,
                    )
                }
            }
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

/// Merge patch items into existing plan — only overwrite provided fields.
fn plan_merge(store: &dyn PlanningStore, session_id: &str, patches: Vec<PlanPatchItem>) -> VersionedPlan {
    use fox_agent_core::update_session_snapshot;
    update_session_snapshot(store, session_id, Some("plan"), |snapshot| {
        snapshot.plan.version += 1;
        for patch in patches {
            if let Some(existing) = snapshot.plan.items.iter_mut().find(|i| i.id == patch.id) {
                patch.apply(existing);
            } else {
                snapshot.plan.items.push(patch.into_plan_item());
            }
        }
    })
    .map(|s| s.plan)
    .unwrap_or_default()
}
