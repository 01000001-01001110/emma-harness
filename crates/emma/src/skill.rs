//! `Skill` — load one of the harness's skills into the conversation.
//!
//! It lives here rather than in `tools/fs` because it is the one tool whose
//! content comes from the harness, and `tools/fs` does not depend on the
//! harness and should not start.
//!
//! **The closed enum is the security property.** The schema's `name` is an enum
//! of exactly the skills the loaded harness resolved. A tool that takes a path
//! and reads it is an arbitrary-file-read tool with a friendly description; the
//! model never gets to name a file here, only to pick from a list the operator
//! wrote.
//!
//! Registered only when `Harness::skill_catalog()` is `Some`. Offering a
//! capability that cannot work is a trap with a description attached.

use std::sync::Arc;

use emma_harness::Harness;
use emma_tool_api::{Tool, ToolCtx, ToolError, ToolMeta, ToolOutcome};
use serde_json::{json, Value};

const NAME: &str = "Skill";

pub struct Skill {
    harness: Arc<Harness>,
    description: String,
}

impl Skill {
    /// `None` when the harness resolved no skills.
    pub fn new(harness: Arc<Harness>) -> Option<Self> {
        let catalog = harness.skill_catalog()?;
        Some(Self {
            description: format!(
                "Load a skill: a set of instructions for one kind of work, written by the \
                 person who configured this agent. Load one before starting work it covers, \
                 and follow it. Available:\n{catalog}"
            ),
            harness,
        })
    }
}

#[async_trait::async_trait]
impl Tool for Skill {
    fn name(&self) -> &'static str {
        NAME
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "enum": self.harness.skill_names(),
                    "description": "Which skill to load."
                }
            },
            "required": ["name"],
            "additionalProperties": false
        })
    }

    fn meta(&self) -> ToolMeta {
        ToolMeta {
            read_only: true,
            idempotent: true,
        }
    }

    fn validate_args(&self, args: &Value) -> Result<(), ToolError> {
        match args.get("name").and_then(Value::as_str) {
            Some(_) => Ok(()),
            None => Err(ToolError::BadArguments(
                "Skill.name is required and must be a string".into(),
            )),
        }
    }

    async fn invoke(
        &self,
        _ctx: &ToolCtx,
        args: Value,
    ) -> anyhow::Result<Result<ToolOutcome, ToolError>> {
        let name = args.get("name").and_then(Value::as_str).unwrap_or_default();
        Ok(match self.harness.skill(name) {
            Some(skill) => Ok(ToolOutcome::new(skill.body.clone())
                .with_display(format!("loaded skill `{}` ({})", skill.name, skill.hash))),
            // The enum is enforced by us as well as declared to the model: a
            // provider that ignores the enum, or a harness reloaded underneath
            // us, must not turn into a lookup that reads something else.
            None => Err(ToolError::BadArguments(format!(
                "no skill named `{name}`. Available: {}",
                self.harness.skill_names().join(", ")
            ))),
        })
    }
}
