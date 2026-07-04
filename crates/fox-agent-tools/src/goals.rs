use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

use async_trait::async_trait;
use fox_agent_core::{PlanningStore, Tool, ToolContext, ToolError, ToolOutput, intent_schema_property};
pub use fox_agent_core::{
    Goal, GoalCheckpoint, GoalMilestone, GoalScope, GoalStatus, MilestoneStatus, goal_status_label,
    load_goals, load_goals_with_store, save_goals, save_goals_with_store,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoalAction { Create, List, Show, Resume, Update, Checkpoint, Focus }

#[derive(Debug, Deserialize)]
pub(crate) struct GoalToolInput {
    pub action: GoalAction,
    #[serde(default)] pub scope: Option<GoalScope>,
    #[serde(default)] pub id: Option<String>,
    #[serde(default)] pub title: Option<String>,
    #[serde(default)] pub description: Option<String>,
    #[serde(default)] pub status: Option<GoalStatus>,
    #[serde(default)] pub progress: Option<u8>,
    #[serde(default)] pub milestones: Option<Vec<GoalMilestone>>,
    #[serde(default)] pub checkpoint_summary: Option<String>,
}

/// Tool that creates, updates, and tracks goals.
pub struct GoalTool {
    store: Arc<dyn PlanningStore>,
}

impl GoalTool {
    pub fn new(store: Arc<dyn PlanningStore>) -> Self {
        Self { store }
    }
}

impl Default for GoalTool {
    fn default() -> Self {
        Self::new(fox_agent_core::default_planning_store())
    }
}

#[async_trait]
impl Tool for GoalTool {
    fn name(&self) -> &str { "goal" }
    fn description(&self) -> &str { "Create, update, and track goals (session or global scope)" }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "intent": intent_schema_property(),
                "action": { "type": "string", "enum": ["create", "list", "show", "resume", "update", "checkpoint", "focus"] },
                "scope": { "type": "string", "enum": ["session", "global"] },
                "id": { "type": "string" },
                "title": { "type": "string" },
                "description": { "type": "string" },
                "status": { "type": "string", "enum": ["active", "paused", "completed", "abandoned"] },
                "progress": { "type": "integer", "minimum": 0, "maximum": 100 },
                "milestones": {
                    "type": "array", "items": {
                        "type": "object", "required": ["id", "content", "status"],
                        "properties": {
                            "id": { "type": "string" },
                            "content": { "type": "string" },
                            "status": { "type": "string", "enum": ["pending", "in_progress", "completed"] }
                        }
                    }
                },
                "checkpoint_summary": { "type": "string" }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let params: GoalToolInput = serde_json::from_value(input).map_err(|err| ToolError::Message {
            message: format!("invalid goal input: {err}"),
        })?;
        let scope = params.scope.clone().unwrap_or(GoalScope::Session);

        let result = match params.action {
            GoalAction::Create => handle_goal_create(self.store.as_ref(), params, scope, &ctx)?,
            GoalAction::List => handle_goal_list(self.store.as_ref(), &ctx, scope),
            GoalAction::Show => handle_goal_show(self.store.as_ref(), params, &ctx, scope)?,
            GoalAction::Resume => handle_goal_resume(self.store.as_ref(), params, &ctx, scope)?,
            GoalAction::Update => handle_goal_update(self.store.as_ref(), params, &ctx, scope)?,
            GoalAction::Checkpoint => handle_goal_checkpoint(self.store.as_ref(), params, &ctx, scope)?,
            GoalAction::Focus => handle_goal_focus(self.store.as_ref(), params, &ctx, scope)?,
        };

        Ok(ToolOutput {
            text: serde_json::to_string_pretty(&result).map_err(|err| ToolError::Message {
                message: format!("failed to serialize goal output: {err}"),
            })?,
            is_error: false,
            json: Some(result),
        })
    }
}

// ── Action handlers ──

fn handle_goal_create(
    store: &dyn PlanningStore,
    params: GoalToolInput,
    scope: GoalScope,
    ctx: &ToolContext,
) -> Result<Value, ToolError> {
    let title = params.title.ok_or_else(|| ToolError::Message {
        message: "missing required field `title` for action=create".to_string(),
    })?;
    let id = params.id.unwrap_or_else(|| Uuid::new_v4().to_string());
    let progress = params.progress.unwrap_or(0).min(100);
    let milestones = params.milestones.unwrap_or_default();
    let mut created = Goal {
        id, title, description: params.description, scope: scope.clone(),
        status: GoalStatus::Active, progress, milestones, checkpoints: Vec::new(), focused: true,
    };
    let mut goals = load_goals_with_store(store, &ctx.session_id, scope.clone());
    for goal in &mut goals {
        goal.focused = false;
    }
    if let Some(existing) = goals.iter_mut().find(|goal| goal.id == created.id) {
        created.checkpoints = existing.checkpoints.clone();
        *existing = created.clone();
    } else {
        goals.push(created.clone());
    }
    let _ = save_goals_with_store(store, &ctx.session_id, scope, goals, false, Some("goal_create"));
    Ok(json!({ "goal": created }))
}

fn handle_goal_list(store: &dyn PlanningStore, ctx: &ToolContext, scope: GoalScope) -> Value {
    let goals = load_goals_with_store(store, &ctx.session_id, scope);
    json!({ "goals": goals })
}

fn handle_goal_show(
    store: &dyn PlanningStore,
    params: GoalToolInput,
    ctx: &ToolContext,
    scope: GoalScope,
) -> Result<Value, ToolError> {
    let id = params.id.ok_or_else(|| ToolError::Message {
        message: "missing required field `id` for action=show".to_string(),
    })?;
    let goals = load_goals_with_store(store, &ctx.session_id, scope);
    let goal = goals.into_iter().find(|g| g.id == id).ok_or_else(|| ToolError::Message {
        message: "goal not found".to_string(),
    })?;
    Ok(json!({ "goal": goal }))
}

fn handle_goal_resume(
    store: &dyn PlanningStore,
    params: GoalToolInput,
    ctx: &ToolContext,
    scope: GoalScope,
) -> Result<Value, ToolError> {
    let id = params.id.ok_or_else(|| ToolError::Message {
        message: "missing required field `id` for action=resume".to_string(),
    })?;
    let mut goals = load_goals_with_store(store, &ctx.session_id, scope.clone());
    let idx = goals.iter().position(|goal| goal.id == id).ok_or_else(|| ToolError::Message {
        message: "goal not found".to_string(),
    })?;
    let mut updated: Option<Goal> = None;
    for (i, goal) in goals.iter_mut().enumerate() {
        goal.focused = i == idx;
        if i == idx {
            goal.status = GoalStatus::Active;
            updated = Some(goal.clone());
        }
    }
    let _ = save_goals_with_store(store, &ctx.session_id, scope, goals, false, Some("goal_resume"));
    updated.ok_or_else(|| ToolError::Message {
        message: "goal not found".to_string(),
    }).map(|goal| json!({ "goal": goal }))
}

fn handle_goal_update(
    store: &dyn PlanningStore,
    params: GoalToolInput,
    ctx: &ToolContext,
    scope: GoalScope,
) -> Result<Value, ToolError> {
    let id = params.id.ok_or_else(|| ToolError::Message {
        message: "missing required field `id` for action=update".to_string(),
    })?;
    let mut goals = load_goals_with_store(store, &ctx.session_id, scope.clone());
    let mut updated: Option<Goal> = None;
    if let Some(goal) = goals.iter_mut().find(|goal| goal.id == id) {
        if let Some(title) = params.title { goal.title = title; }
        if params.description.is_some() { goal.description = params.description; }
        if let Some(status) = params.status { goal.status = status; }
        if let Some(progress) = params.progress { goal.progress = progress.min(100); }
        if let Some(milestones) = params.milestones { goal.milestones = milestones; }
        updated = Some(goal.clone());
    }
    let _ = save_goals_with_store(store, &ctx.session_id, scope, goals, false, Some("goal_update"));
    updated.ok_or_else(|| ToolError::Message {
        message: "goal not found".to_string(),
    }).map(|goal| json!({ "goal": goal }))
}

fn handle_goal_checkpoint(
    store: &dyn PlanningStore,
    params: GoalToolInput,
    ctx: &ToolContext,
    scope: GoalScope,
) -> Result<Value, ToolError> {
    // If no explicit `id`, fall back to the first focused active goal.
    let goals = load_goals_with_store(store, &ctx.session_id, scope.clone());
    let id = params.id.or_else(|| {
        goals.iter()
            .find(|g| g.focused && matches!(g.status, GoalStatus::Active))
            .map(|g| g.id.clone())
    }).ok_or_else(|| ToolError::Message {
        message: "missing required field `id` for action=checkpoint (and no focused active goal to default to)".to_string(),
    })?;
    let summary = params.checkpoint_summary.ok_or_else(|| ToolError::Message {
        message: "missing required field `checkpoint_summary` for action=checkpoint".to_string(),
    })?;
    let now_secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let mut goals = goals;
    let mut updated: Option<Goal> = None;
    if let Some(goal) = goals.iter_mut().find(|goal| goal.id == id) {
        let checkpoint = GoalCheckpoint {
            at_secs: now_secs, summary,
            progress: params.progress.map(|p| p.min(100)),
        };
        if let Some(progress) = checkpoint.progress { goal.progress = progress; }
        goal.checkpoints.push(checkpoint);
        updated = Some(goal.clone());
    }
    let _ = save_goals_with_store(store, &ctx.session_id, scope, goals, false, Some("goal_checkpoint"));
    updated.ok_or_else(|| ToolError::Message {
        message: "goal not found".to_string(),
    }).map(|goal| json!({ "goal": goal }))
}

fn handle_goal_focus(
    store: &dyn PlanningStore,
    params: GoalToolInput,
    ctx: &ToolContext,
    scope: GoalScope,
) -> Result<Value, ToolError> {
    let id = params.id.ok_or_else(|| ToolError::Message {
        message: "missing required field `id` for action=focus".to_string(),
    })?;
    let mut goals = load_goals_with_store(store, &ctx.session_id, scope.clone());
    let mut updated: Option<Goal> = None;
    for goal in goals.iter_mut() {
        goal.focused = goal.id == id;
        if goal.focused {
            updated = Some(goal.clone());
        }
    }
    let _ = save_goals_with_store(store, &ctx.session_id, scope, goals, false, Some("goal_focus"));
    updated.ok_or_else(|| ToolError::Message {
        message: "goal not found".to_string(),
    }).map(|goal| json!({ "goal": goal }))
}
