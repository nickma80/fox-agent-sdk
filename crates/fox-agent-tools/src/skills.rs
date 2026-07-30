use fox_agent_core::{
    Skill, SkillRegistry, Tool, ToolContext, ToolError, ToolOutput, intent_schema_property,
};
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Tool that lets the Agent manage skills on-demand.
///
/// Uses the same mechanism as Claude Code:
/// - `list` — show available skills
/// - `activate` — load a skill's prompt into context
/// - `deactivate` — unload the current skill
///
/// Activation state is stored in a shared `active` handle
/// so the Agent's prompt builder can inject the active skill's prompt.
pub struct SkillTool {
    registry: Arc<RwLock<SkillRegistry>>,
    active: Arc<RwLock<Option<Skill>>>,
}

impl SkillTool {
    pub fn new(registry: Arc<RwLock<SkillRegistry>>, active: Arc<RwLock<Option<Skill>>>) -> Self {
        Self { registry, active }
    }
}

#[async_trait::async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        "skill"
    }

    fn description(&self) -> &str {
        "Manage skills. Use action=\"list\" to see available skills. \
         Use action=\"activate\" with name to load a skill. \
         Use action=\"deactivate\" to unload the current skill."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "intent": intent_schema_property(),
                "action": {
                    "type": "string",
                    "enum": ["list", "activate", "deactivate"],
                    "description": "The action to perform"
                },
                "name": {
                    "type": "string",
                    "description": "The skill name (required for activate)"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let params: SkillToolInput =
            serde_json::from_value(input).map_err(|e| ToolError::Message {
                message: format!("invalid input: {e}"),
            })?;

        match params.action.as_str() {
            "list" => {
                let reg = self.registry.read().await;
                let skills = reg.list();
                if skills.is_empty() {
                    return Ok(ToolOutput {
                        text: "No skills available. Place `.md` skill files in `.claude/skills/`."
                            .into(),
                        is_error: false,
                        json: None,
                    });
                }
                let active_name = self.active.read().await.as_ref().map(|s| s.name.clone());
                let mut lines = vec!["Available skills:".to_string()];
                for s in &skills {
                    let mark = if active_name.as_deref() == Some(&s.name) {
                        "★"
                    } else {
                        " "
                    };
                    lines.push(format!("  {mark} /{:<20} — {}", s.name, s.description));
                }
                lines.push("\nUse action=\"activate\" with name to load a skill.".to_string());
                Ok(ToolOutput {
                    text: lines.join("\n"),
                    is_error: false,
                    json: None,
                })
            }

            "activate" => {
                let name = params.name.as_deref().unwrap_or("");
                if name.is_empty() {
                    return Err(ToolError::Message {
                        message: "`name` is required for action=activate".into(),
                    });
                }
                let reg = self.registry.read().await;
                let skill = reg.get(name).cloned().ok_or_else(|| ToolError::Message {
                    message: format!(
                        "skill `{name}` not found. Use action=\"list\" to see available skills."
                    ),
                })?;
                let prompt_len = skill.prompt.len();
                *self.active.write().await = Some(skill.clone());
                Ok(ToolOutput {
                    text: format!(
                        "Skill `/{}` activated ({} chars of expertise loaded).",
                        skill.name, prompt_len
                    ),
                    is_error: false,
                    json: None,
                })
            }

            "deactivate" => {
                let prev = self.active.write().await.take();
                match prev {
                    Some(s) => Ok(ToolOutput {
                        text: format!("Skill `/{}` deactivated.", s.name),
                        is_error: false,
                        json: None,
                    }),
                    None => Ok(ToolOutput {
                        text: "No skill is currently active.".into(),
                        is_error: false,
                        json: None,
                    }),
                }
            }

            other => Err(ToolError::Message {
                message: format!(
                    "unknown action: {other}. Use `list`, `activate`, or `deactivate`."
                ),
            }),
        }
    }
}

#[derive(Debug, Deserialize)]
struct SkillToolInput {
    action: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    #[expect(dead_code)]
    intent: Option<String>,
}
