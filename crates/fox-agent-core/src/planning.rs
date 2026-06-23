use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanningScope {
    Session,
    Global,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TodoPriority {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TodoItem {
    pub id: String,
    pub content: String,
    pub status: TodoStatus,
    pub priority: TodoPriority,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    Pending,
    InProgress,
    Completed,
}

impl std::fmt::Display for PlanStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlanStatus::Pending => write!(f, "pending"),
            PlanStatus::InProgress => write!(f, "in_progress"),
            PlanStatus::Completed => write!(f, "completed"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanPriority {
    High,
    Medium,
    Low,
}

impl std::fmt::Display for PlanPriority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlanPriority::High => write!(f, "high"),
            PlanPriority::Medium => write!(f, "medium"),
            PlanPriority::Low => write!(f, "low"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanItem {
    pub id: String,
    pub content: String,
    pub status: PlanStatus,
    pub priority: PlanPriority,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assigned_to: Option<String>,
    #[serde(default)]
    pub blocked_by: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct VersionedPlan {
    pub version: u64,
    pub items: Vec<PlanItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GoalScope {
    Session,
    Global,
}

impl From<GoalScope> for PlanningScope {
    fn from(value: GoalScope) -> Self {
        match value {
            GoalScope::Session => PlanningScope::Session,
            GoalScope::Global => PlanningScope::Global,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    Active,
    Paused,
    Completed,
    Abandoned,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MilestoneStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GoalMilestone {
    pub id: String,
    pub content: String,
    pub status: MilestoneStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GoalCheckpoint {
    pub at_secs: u64,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<u8>,
}

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanningStateSnapshot {
    pub session_id: String,
    pub scope: PlanningScope,
    #[serde(default)]
    pub todos: Vec<TodoItem>,
    #[serde(default)]
    pub plan: VersionedPlan,
    #[serde(default)]
    pub goals: Vec<Goal>,
    pub version: u64,
    pub updated_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

impl PlanningStateSnapshot {
    pub fn new(session_id: impl Into<String>, scope: PlanningScope) -> Self {
        Self {
            session_id: session_id.into(),
            scope,
            todos: Vec::new(),
            plan: VersionedPlan::default(),
            goals: Vec::new(),
            version: 0,
            updated_at: now_secs(),
            source: None,
        }
    }
}

pub trait PlanningStore: Send + Sync {
    fn load_snapshot(
        &self,
        session_id: &str,
        scope: PlanningScope,
    ) -> Result<PlanningStateSnapshot, String>;
    fn save_snapshot(&self, snapshot: &PlanningStateSnapshot) -> Result<(), String>;
    fn delete_snapshot(&self, session_id: &str, scope: PlanningScope) -> Result<(), String>;
    fn list_session_ids(&self) -> Result<Vec<String>, String>;
}

#[derive(Default)]
pub struct InMemoryPlanningStore {
    session_snapshots: RwLock<HashMap<String, PlanningStateSnapshot>>,
    global_snapshot: RwLock<Option<PlanningStateSnapshot>>,
}

impl PlanningStore for InMemoryPlanningStore {
    fn load_snapshot(
        &self,
        session_id: &str,
        scope: PlanningScope,
    ) -> Result<PlanningStateSnapshot, String> {
        match scope {
            PlanningScope::Session => Ok(self
                .session_snapshots
                .read()
                .map_err(|_| "planning session store lock poisoned".to_string())?
                .get(session_id)
                .cloned()
                .unwrap_or_else(|| PlanningStateSnapshot::new(session_id, scope))),
            PlanningScope::Global => Ok(self
                .global_snapshot
                .read()
                .map_err(|_| "planning global store lock poisoned".to_string())?
                .clone()
                .unwrap_or_else(|| PlanningStateSnapshot::new("global", scope))),
        }
    }

    fn save_snapshot(&self, snapshot: &PlanningStateSnapshot) -> Result<(), String> {
        match snapshot.scope {
            PlanningScope::Session => {
                self.session_snapshots
                    .write()
                    .map_err(|_| "planning session store lock poisoned".to_string())?
                    .insert(snapshot.session_id.clone(), snapshot.clone());
            }
            PlanningScope::Global => {
                *self
                    .global_snapshot
                    .write()
                    .map_err(|_| "planning global store lock poisoned".to_string())? =
                    Some(snapshot.clone());
            }
        }
        Ok(())
    }

    fn delete_snapshot(&self, session_id: &str, scope: PlanningScope) -> Result<(), String> {
        match scope {
            PlanningScope::Session => {
                self.session_snapshots
                    .write()
                    .map_err(|_| "planning session store lock poisoned".to_string())?
                    .remove(session_id);
            }
            PlanningScope::Global => {
                *self
                    .global_snapshot
                    .write()
                    .map_err(|_| "planning global store lock poisoned".to_string())? = None;
            }
        }
        Ok(())
    }

    fn list_session_ids(&self) -> Result<Vec<String>, String> {
        let mut ids: Vec<String> = self
            .session_snapshots
            .read()
            .map_err(|_| "planning session store lock poisoned".to_string())?
            .keys()
            .cloned()
            .collect();
        ids.sort();
        Ok(ids)
    }
}

pub struct FilePlanningStore {
    root_dir: PathBuf,
}

impl FilePlanningStore {
    pub fn new(root_dir: impl Into<PathBuf>) -> Self {
        Self {
            root_dir: root_dir.into(),
        }
    }

    fn session_dir(&self) -> PathBuf {
        self.root_dir.join("sessions")
    }

    fn snapshot_path(&self, session_id: &str, scope: PlanningScope) -> PathBuf {
        match scope {
            PlanningScope::Session => self.session_dir().join(format!("{session_id}.json")),
            PlanningScope::Global => self.root_dir.join("global.json"),
        }
    }
}

impl PlanningStore for FilePlanningStore {
    fn load_snapshot(
        &self,
        session_id: &str,
        scope: PlanningScope,
    ) -> Result<PlanningStateSnapshot, String> {
        let path = self.snapshot_path(session_id, scope);
        if !path.exists() {
            return Ok(match scope {
                PlanningScope::Session => PlanningStateSnapshot::new(session_id, scope),
                PlanningScope::Global => PlanningStateSnapshot::new("global", scope),
            });
        }
        let contents = fs::read_to_string(&path)
            .map_err(|e| format!("failed to read planning snapshot {}: {e}", path.display()))?;
        serde_json::from_str(&contents)
            .map_err(|e| format!("failed to parse planning snapshot {}: {e}", path.display()))
    }

    fn save_snapshot(&self, snapshot: &PlanningStateSnapshot) -> Result<(), String> {
        let path = self.snapshot_path(&snapshot.session_id, snapshot.scope);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create planning dir {}: {e}", parent.display()))?;
        }
        let payload = serde_json::to_string_pretty(snapshot)
            .map_err(|e| format!("failed to serialize planning snapshot: {e}"))?;
        fs::write(&path, payload)
            .map_err(|e| format!("failed to write planning snapshot {}: {e}", path.display()))
    }

    fn delete_snapshot(&self, session_id: &str, scope: PlanningScope) -> Result<(), String> {
        let path = self.snapshot_path(session_id, scope);
        if path.exists() {
            fs::remove_file(&path)
                .map_err(|e| format!("failed to delete planning snapshot {}: {e}", path.display()))?;
        }
        Ok(())
    }

    fn list_session_ids(&self) -> Result<Vec<String>, String> {
        let dir = self.session_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut ids = Vec::new();
        for entry in fs::read_dir(&dir)
            .map_err(|e| format!("failed to read planning session dir {}: {e}", dir.display()))?
        {
            let entry = entry.map_err(|e| format!("failed to inspect planning dir entry: {e}"))?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
                ids.push(stem.to_string());
            }
        }
        ids.sort();
        Ok(ids)
    }
}

fn planning_store_cell() -> &'static RwLock<Arc<dyn PlanningStore>> {
    static STORE: OnceLock<RwLock<Arc<dyn PlanningStore>>> = OnceLock::new();
    STORE.get_or_init(|| RwLock::new(Arc::new(InMemoryPlanningStore::default())))
}

pub fn default_planning_store() -> Arc<dyn PlanningStore> {
    planning_store_cell()
        .read()
        .ok()
        .map(|guard| Arc::clone(&*guard))
        .unwrap_or_else(|| Arc::new(InMemoryPlanningStore::default()))
}

pub fn set_default_planning_store(store: Arc<dyn PlanningStore>) {
    if let Ok(mut guard) = planning_store_cell().write() {
        *guard = store;
    }
}

pub fn load_todos(session_id: &str) -> Vec<TodoItem> {
    load_todos_with_store(default_planning_store().as_ref(), session_id)
}

pub fn load_todos_with_store(store: &dyn PlanningStore, session_id: &str) -> Vec<TodoItem> {
    store
        .load_snapshot(session_id, PlanningScope::Session)
        .map(|snapshot| snapshot.todos)
        .unwrap_or_default()
}

pub fn save_todos(session_id: &str, todos: Vec<TodoItem>, merge: bool) -> Vec<TodoItem> {
    let store = default_planning_store();
    save_todos_with_store(store.as_ref(), session_id, todos, merge)
}

pub fn save_todos_with_store(
    store: &dyn PlanningStore,
    session_id: &str,
    todos: Vec<TodoItem>,
    merge: bool,
) -> Vec<TodoItem> {
    update_session_snapshot(store, session_id, Some("todo"), |snapshot| {
        if merge {
            for incoming in todos {
                if let Some(existing) = snapshot.todos.iter_mut().find(|item| item.id == incoming.id) {
                    *existing = incoming;
                } else {
                    snapshot.todos.push(incoming);
                }
            }
        } else {
            snapshot.todos = todos;
        }
    })
    .map(|snapshot| snapshot.todos)
    .unwrap_or_default()
}

pub fn load_plan(session_id: &str) -> VersionedPlan {
    let store = default_planning_store();
    load_plan_with_store(store.as_ref(), session_id)
}

pub fn load_plan_with_store(store: &dyn PlanningStore, session_id: &str) -> VersionedPlan {
    store
        .load_snapshot(session_id, PlanningScope::Session)
        .map(|snapshot| snapshot.plan)
        .unwrap_or_default()
}

pub fn save_plan(session_id: &str, items: Vec<PlanItem>, merge: bool) -> VersionedPlan {
    let store = default_planning_store();
    save_plan_with_store(store.as_ref(), session_id, items, merge)
}

pub fn save_plan_with_store(
    store: &dyn PlanningStore,
    session_id: &str,
    items: Vec<PlanItem>,
    merge: bool,
) -> VersionedPlan {
    update_session_snapshot(store, session_id, Some("plan"), move |snapshot| {
        snapshot.plan.version += 1;
        if merge {
            for incoming in items {
                if let Some(existing) = snapshot.plan.items.iter_mut().find(|item| item.id == incoming.id) {
                    *existing = incoming;
                } else {
                    snapshot.plan.items.push(incoming);
                }
            }
        } else {
            snapshot.plan.items = items;
        }
    })
    .map(|snapshot| snapshot.plan)
    .unwrap_or_default()
}

pub fn load_goals(session_id: &str, scope: GoalScope) -> Vec<Goal> {
    let store = default_planning_store();
    load_goals_with_store(store.as_ref(), session_id, scope)
}

pub fn load_goals_with_store(
    store: &dyn PlanningStore,
    session_id: &str,
    scope: GoalScope,
) -> Vec<Goal> {
    let planning_scope: PlanningScope = scope.clone().into();
    store
        .load_snapshot(snapshot_session_id(session_id, scope), planning_scope)
        .map(|snapshot| snapshot.goals)
        .unwrap_or_default()
}

pub fn save_goals(
    session_id: &str,
    scope: GoalScope,
    goals: Vec<Goal>,
    source: Option<&str>,
) -> Vec<Goal> {
    let store = default_planning_store();
    save_goals_with_store(store.as_ref(), session_id, scope, goals, source)
}

pub fn save_goals_with_store(
    store: &dyn PlanningStore,
    session_id: &str,
    scope: GoalScope,
    goals: Vec<Goal>,
    source: Option<&str>,
) -> Vec<Goal> {
    update_snapshot(store, snapshot_session_id(session_id, scope.clone()), scope.into(), source, |snapshot| {
        snapshot.goals = goals;
    })
    .map(|snapshot| snapshot.goals)
    .unwrap_or_default()
}

pub fn todo_status_label(status: &TodoStatus) -> &'static str {
    match status {
        TodoStatus::Pending => "pending",
        TodoStatus::InProgress => "in_progress",
        TodoStatus::Completed => "completed",
    }
}

pub fn goal_status_label(status: &GoalStatus) -> &'static str {
    match status {
        GoalStatus::Active => "active",
        GoalStatus::Paused => "paused",
        GoalStatus::Completed => "completed",
        GoalStatus::Abandoned => "abandoned",
    }
}

pub fn render_planning_context(session_id: &str) -> String {
    let store = default_planning_store();
    render_planning_context_with_store(store.as_ref(), session_id)
}

pub fn render_planning_context_with_store(store: &dyn PlanningStore, session_id: &str) -> String {
    let todos = load_todos_with_store(store, session_id);
    let plan = load_plan_with_store(store, session_id);
    let session_goals = load_goals_with_store(store, session_id, GoalScope::Session);
    let global_goals = load_goals_with_store(store, session_id, GoalScope::Global);
    let mut sections = Vec::new();

    if !todos.is_empty() {
        let todo_lines = todos
            .iter()
            .map(|todo| {
                format!(
                    "- [{}|{:?}] {} ({})",
                    todo_status_label(&todo.status),
                    todo.priority,
                    todo.content,
                    todo.id
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        sections.push(format!("Session todos:\n{todo_lines}"));
    }

    if !plan.items.is_empty() {
        let plan_lines = plan
            .items
            .iter()
            .map(|item| {
                let assigned = item
                    .assigned_to
                    .clone()
                    .unwrap_or_else(|| "unassigned".to_string());
                let blocked = if item.blocked_by.is_empty() {
                    "none".to_string()
                } else {
                    item.blocked_by.join(", ")
                };
                format!(
                    "- [{}|{}] {} ({}) assigned_to={} blocked_by={}",
                    item.status, item.priority, item.content, item.id, assigned, blocked
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        sections.push(format!("Shared plan v{}:\n{plan_lines}", plan.version));
    }

    if !session_goals.is_empty() {
        sections.push(format!("Session goals:\n{}", render_goal_lines(&session_goals)));
    }
    if !global_goals.is_empty() {
        sections.push(format!("Global goals:\n{}", render_goal_lines(&global_goals)));
    }

    sections.join("\n\n")
}

fn render_goal_lines(goals: &[Goal]) -> String {
    goals
        .iter()
        .map(|goal| {
            let focused = if goal.focused { "focused" } else { "unfocused" };
            format!(
                "- [{}|{}%|{}] {} ({})",
                goal_status_label(&goal.status),
                goal.progress,
                focused,
                goal.title,
                goal.id
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn update_session_snapshot<F>(
    store: &dyn PlanningStore,
    session_id: &str,
    source: Option<&str>,
    update: F,
) -> Result<PlanningStateSnapshot, String>
where
    F: FnOnce(&mut PlanningStateSnapshot),
{
    update_snapshot(store, session_id, PlanningScope::Session, source, update)
}

fn update_snapshot<F>(
    store: &dyn PlanningStore,
    session_id: &str,
    scope: PlanningScope,
    source: Option<&str>,
    update: F,
) -> Result<PlanningStateSnapshot, String>
where
    F: FnOnce(&mut PlanningStateSnapshot),
{
    let mut snapshot = store.load_snapshot(session_id, scope)?;
    update(&mut snapshot);
    snapshot.version += 1;
    snapshot.updated_at = now_secs();
    snapshot.source = source.map(|value| value.to_string());
    store.save_snapshot(&snapshot)?;
    Ok(snapshot)
}

fn snapshot_session_id<'a>(session_id: &'a str, scope: GoalScope) -> &'a str {
    match scope {
        GoalScope::Session => session_id,
        GoalScope::Global => "global",
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[allow(dead_code)]
fn _ensure_path_is_absolute(path: &Path) -> bool {
    path.is_absolute()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_planning_store_roundtrip_persists_session_snapshot() {
        let root = std::env::temp_dir().join(format!("fox-planning-store-{}", uuid::Uuid::new_v4()));
        let store = FilePlanningStore::new(root.clone());
        let snapshot = PlanningStateSnapshot {
            session_id: "s1".to_string(),
            scope: PlanningScope::Session,
            todos: vec![TodoItem {
                id: "t1".to_string(),
                content: "write tests".to_string(),
                status: TodoStatus::InProgress,
                priority: TodoPriority::High,
            }],
            plan: VersionedPlan::default(),
            goals: Vec::new(),
            version: 1,
            updated_at: now_secs(),
            source: Some("test".to_string()),
        };
        store.save_snapshot(&snapshot).unwrap();

        let loaded = store.load_snapshot("s1", PlanningScope::Session).unwrap();
        assert_eq!(loaded.todos.len(), 1);
        assert_eq!(loaded.todos[0].content, "write tests");

        let _ = std::fs::remove_dir_all(root);
    }
}
