use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::RwLock as StdRwLock;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

// ── Types ──

/// Scope of a goal (session-local or global).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GoalScope { Session, Global }

/// Lifecycle status of a goal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus { Active, Paused, Completed, Abandoned }

/// Status of a single milestone within a goal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MilestoneStatus { Pending, InProgress, Completed }

/// A milestone within a goal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GoalMilestone {
    pub id: String,
    pub content: String,
    pub status: MilestoneStatus,
}

/// A checkpoint recorded against a goal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GoalCheckpoint {
    pub at_secs: u64,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<u8>,
}

/// A tracked goal with milestones, checkpoints, and progress.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Goal {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub scope: GoalScope,
    pub status: GoalStatus,
    pub progress: u8,
    #[serde(default)]
    pub milestones: Vec<GoalMilestone>,
    #[serde(default)]
    pub checkpoints: Vec<GoalCheckpoint>,
    #[serde(default)]
    pub focused: bool,
}

pub fn goal_status_label(status: &GoalStatus) -> &'static str {
    match status {
        GoalStatus::Active => "active",
        GoalStatus::Paused => "paused",
        GoalStatus::Completed => "completed",
        GoalStatus::Abandoned => "abandoned",
    }
}

// ── Global in-memory store ──

fn goal_store_session() -> &'static StdRwLock<HashMap<String, Vec<Goal>>> {
    static STORE: OnceLock<StdRwLock<HashMap<String, Vec<Goal>>>> = OnceLock::new();
    STORE.get_or_init(|| StdRwLock::new(HashMap::new()))
}

fn goal_store_global() -> &'static StdRwLock<Vec<Goal>> {
    static STORE: OnceLock<StdRwLock<Vec<Goal>>> = OnceLock::new();
    STORE.get_or_init(|| StdRwLock::new(Vec::new()))
}

pub fn load_goals(session_id: &str, scope: GoalScope) -> Vec<Goal> {
    match scope {
        GoalScope::Session => goal_store_session().read().ok()
            .and_then(|store| store.get(session_id).cloned()).unwrap_or_default(),
        GoalScope::Global => goal_store_global().read().ok()
            .map(|goals| goals.clone()).unwrap_or_default(),
    }
}

pub fn with_goals_mut<F, R>(session_id: &str, scope: GoalScope, f: F) -> R
where F: FnOnce(&mut Vec<Goal>) -> R {
    match scope {
        GoalScope::Session => {
            let Ok(mut store) = goal_store_session().write() else { return f(&mut Vec::new()) };
            let entry = store.entry(session_id.to_string()).or_default();
            f(entry)
        }
        GoalScope::Global => {
            let Ok(mut goals) = goal_store_global().write() else { return f(&mut Vec::new()) };
            f(&mut goals)
        }
    }
}

// ── Tool implementation ──

use async_trait::async_trait;
use fox_agent_core::{Tool, ToolContext, ToolError, ToolOutput};
use serde_json::{Value, json};

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
pub struct GoalTool;

#[async_trait]
impl Tool for GoalTool {
    fn name(&self) -> &str { "goal" }
    fn description(&self) -> &str { "Create, update, and track goals (session or global scope)" }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
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
            GoalAction::Create => handle_goal_create(params, scope, &ctx)?,
            GoalAction::List => handle_goal_list(&ctx, scope),
            GoalAction::Show => handle_goal_show(params, &ctx, scope)?,
            GoalAction::Resume => handle_goal_resume(params, &ctx, scope)?,
            GoalAction::Update => handle_goal_update(params, &ctx, scope)?,
            GoalAction::Checkpoint => handle_goal_checkpoint(params, &ctx, scope)?,
            GoalAction::Focus => handle_goal_focus(params, &ctx, scope)?,
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

fn handle_goal_create(params: GoalToolInput, scope: GoalScope, ctx: &ToolContext) -> Result<Value, ToolError> {
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
    with_goals_mut(&ctx.session_id, scope, |goals| {
        for g in goals.iter_mut() { g.focused = false; }
        if let Some(existing) = goals.iter_mut().find(|g| g.id == created.id) {
            created.checkpoints = existing.checkpoints.clone();
            *existing = created.clone();
        } else { goals.push(created.clone()); }
    });
    Ok(json!({ "goal": created }))
}

fn handle_goal_list(ctx: &ToolContext, scope: GoalScope) -> Value {
    let goals = load_goals(&ctx.session_id, scope);
    json!({ "goals": goals })
}

fn handle_goal_show(params: GoalToolInput, ctx: &ToolContext, scope: GoalScope) -> Result<Value, ToolError> {
    let id = params.id.ok_or_else(|| ToolError::Message {
        message: "missing required field `id` for action=show".to_string(),
    })?;
    let goals = load_goals(&ctx.session_id, scope);
    let goal = goals.into_iter().find(|g| g.id == id).ok_or_else(|| ToolError::Message {
        message: "goal not found".to_string(),
    })?;
    Ok(json!({ "goal": goal }))
}

fn handle_goal_resume(params: GoalToolInput, ctx: &ToolContext, scope: GoalScope) -> Result<Value, ToolError> {
    let id = params.id.ok_or_else(|| ToolError::Message {
        message: "missing required field `id` for action=resume".to_string(),
    })?;
    let mut updated: Option<Goal> = None;
    with_goals_mut(&ctx.session_id, scope, |goals| {
        let idx = goals.iter().position(|g| g.id == id)?;
        for (i, goal) in goals.iter_mut().enumerate() {
            goal.focused = i == idx;
            if i == idx { goal.status = GoalStatus::Active; updated = Some(goal.clone()); }
        }
        Some(())
    });
    updated.ok_or_else(|| ToolError::Message {
        message: "goal not found".to_string(),
    }).map(|goal| json!({ "goal": goal }))
}

fn handle_goal_update(params: GoalToolInput, ctx: &ToolContext, scope: GoalScope) -> Result<Value, ToolError> {
    let id = params.id.ok_or_else(|| ToolError::Message {
        message: "missing required field `id` for action=update".to_string(),
    })?;
    let mut updated: Option<Goal> = None;
    with_goals_mut(&ctx.session_id, scope, |goals| {
        if let Some(goal) = goals.iter_mut().find(|g| g.id == id) {
            if let Some(title) = params.title { goal.title = title; }
            if params.description.is_some() { goal.description = params.description; }
            if let Some(status) = params.status { goal.status = status; }
            if let Some(progress) = params.progress { goal.progress = progress.min(100); }
            if let Some(milestones) = params.milestones { goal.milestones = milestones; }
            updated = Some(goal.clone());
        }
    });
    updated.ok_or_else(|| ToolError::Message {
        message: "goal not found".to_string(),
    }).map(|goal| json!({ "goal": goal }))
}

fn handle_goal_checkpoint(params: GoalToolInput, ctx: &ToolContext, scope: GoalScope) -> Result<Value, ToolError> {
    let id = params.id.ok_or_else(|| ToolError::Message {
        message: "missing required field `id` for action=checkpoint".to_string(),
    })?;
    let summary = params.checkpoint_summary.ok_or_else(|| ToolError::Message {
        message: "missing required field `checkpoint_summary` for action=checkpoint".to_string(),
    })?;
    let now_secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let mut updated: Option<Goal> = None;
    with_goals_mut(&ctx.session_id, scope, |goals| {
        if let Some(goal) = goals.iter_mut().find(|g| g.id == id) {
            let checkpoint = GoalCheckpoint {
                at_secs: now_secs, summary,
                progress: params.progress.map(|p| p.min(100)),
            };
            if let Some(progress) = checkpoint.progress { goal.progress = progress; }
            goal.checkpoints.push(checkpoint);
            updated = Some(goal.clone());
        }
    });
    updated.ok_or_else(|| ToolError::Message {
        message: "goal not found".to_string(),
    }).map(|goal| json!({ "goal": goal }))
}

fn handle_goal_focus(params: GoalToolInput, ctx: &ToolContext, scope: GoalScope) -> Result<Value, ToolError> {
    let id = params.id.ok_or_else(|| ToolError::Message {
        message: "missing required field `id` for action=focus".to_string(),
    })?;
    let mut updated: Option<Goal> = None;
    with_goals_mut(&ctx.session_id, scope, |goals| {
        for goal in goals.iter_mut() {
            goal.focused = goal.id == id;
            if goal.focused { updated = Some(goal.clone()); }
        }
    });
    updated.ok_or_else(|| ToolError::Message {
        message: "goal not found".to_string(),
    }).map(|goal| json!({ "goal": goal }))
}
