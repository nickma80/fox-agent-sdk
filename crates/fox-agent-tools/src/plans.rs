use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::RwLock as StdRwLock;

/// A single item in a swarm plan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanItem {
    /// Unique item identifier
    pub id: String,
    /// Task description
    pub content: String,
    /// Execution status (pending, in_progress, completed)
    pub status: String,
    /// Priority level
    pub priority: String,
    /// Worker assigned to this item
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assigned_to: Option<String>,
    /// Ids of items that must complete before this item can run
    #[serde(default)]
    pub blocked_by: Vec<String>,
}

/// A versioned shared plan used by swarm coordinator.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct VersionedPlan {
    /// Monotonically increasing plan version
    pub version: u64,
    /// Plan items in execution order
    pub items: Vec<PlanItem>,
}

fn plan_store() -> &'static StdRwLock<HashMap<String, VersionedPlan>> {
    static STORE: OnceLock<StdRwLock<HashMap<String, VersionedPlan>>> = OnceLock::new();
    STORE.get_or_init(|| StdRwLock::new(HashMap::new()))
}

/// Load the shared plan for a session.
pub fn load_plan(session_id: &str) -> VersionedPlan {
    plan_store().read().ok().and_then(|store| store.get(session_id).cloned()).unwrap_or_default()
}

/// Save (and optionally merge) the shared plan for a session.
pub fn save_plan(session_id: &str, items: Vec<PlanItem>, merge: bool) -> VersionedPlan {
    let Ok(mut store) = plan_store().write() else { return VersionedPlan { version: 0, items } };
    let entry = store.entry(session_id.to_string()).or_default();
    entry.version += 1;
    if !merge { entry.items = items; return entry.clone(); }
    for incoming in items {
        if let Some(existing) = entry.items.iter_mut().find(|item| item.id == incoming.id) {
            *existing = incoming;
        } else { entry.items.push(incoming); }
    }
    entry.clone()
}

use async_trait::async_trait;
use fox_agent_core::{Tool, ToolContext, ToolError, ToolOutput};
use serde_json::{Value, json};

#[derive(Debug, Deserialize)]
pub(crate) struct PlanToolInput {
    #[serde(default)] pub items: Option<Vec<PlanItem>>,
    #[serde(default)] pub merge: bool,
}

/// Tool that reads or updates the session-local shared plan.
pub struct PlanTool;

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
                            "status": { "type": "string" },
                            "priority": { "type": "string" },
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
            Some(items) => save_plan(&ctx.session_id, items, params.merge),
            None => load_plan(&ctx.session_id),
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
