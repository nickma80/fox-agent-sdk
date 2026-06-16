use crate::todos::{todo_status_label, load_todos};
use crate::plans::{load_plan};
use crate::goals::{GoalScope, Goal, goal_status_label, load_goals};

pub fn render_planning_context(session_id: &str) -> String {
    let todos = load_todos(session_id);
    let plan = load_plan(session_id);
    let session_goals = load_goals(session_id, GoalScope::Session);
    let global_goals = load_goals(session_id, GoalScope::Global);
    let mut sections = Vec::new();

    if !todos.is_empty() {
        let todo_lines = todos.iter().map(|todo| {
            format!("- [{}|{:?}] {} ({})", todo_status_label(&todo.status), todo.priority, todo.content, todo.id)
        }).collect::<Vec<_>>().join("\n");
        sections.push(format!("Session todos:\n{todo_lines}"));
    }

    if !plan.items.is_empty() {
        let plan_lines = plan.items.iter().map(|item| {
            let assigned = item.assigned_to.clone().unwrap_or_else(|| "unassigned".to_string());
            let blocked = if item.blocked_by.is_empty() { "none".to_string() } else { item.blocked_by.join(", ") };
            format!("- [{}|{}] {} ({}) assigned_to={} blocked_by={}", item.status, item.priority, item.content, item.id, assigned, blocked)
        }).collect::<Vec<_>>().join("\n");
        sections.push(format!("Shared plan v{}:\n{plan_lines}", plan.version));
    }

    if !session_goals.is_empty() {
        let goal_lines = render_goal_lines(&session_goals);
        sections.push(format!("Session goals:\n{goal_lines}"));
    }

    if !global_goals.is_empty() {
        let goal_lines = render_goal_lines(&global_goals);
        sections.push(format!("Global goals:\n{goal_lines}"));
    }

    sections.join("\n\n")
}

fn render_goal_lines(goals: &[Goal]) -> String {
    goals.iter().map(|goal| {
        let focused = if goal.focused { "focused" } else { "unfocused" };
        format!("- [{}|{}%|{}] {} ({})", goal_status_label(&goal.status), goal.progress, focused, goal.title, goal.id)
    }).collect::<Vec<_>>().join("\n")
}
