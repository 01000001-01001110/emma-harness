//! `TaskGet` — one task, with the prose a human wrote under it.
//!
//! **An id that is not in the file is `BadArguments`.** The call named
//! something that is not there, which is a fact about the call, not about the
//! world: it is the difference between "there are no tasks" — a perfectly good
//! `Ok` from `TaskList` — and "you asked for #a3f1 and there is no #a3f1".
//! Returning an empty result for a bad id would teach the model that the task
//! it just created has vanished, and its next move would be to create it again.

use emma_tool_api::{Tool, ToolCtx, ToolError, ToolMeta, ToolOutcome};
use serde_json::{json, Value};

use crate::args;
use crate::doc::RELATIVE_PATH;
use crate::render;
use crate::store;

const NAME: &str = "TaskGet";
const KEYS: &[&str] = &["id"];

#[derive(Default)]
pub struct TaskGet;

impl TaskGet {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl Tool for TaskGet {
    fn name(&self) -> &'static str {
        NAME
    }

    fn description(&self) -> &str {
        include_str!("descriptions/task_get.md")
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "The task's handle, as shown by TaskList. The leading # is optional."
                }
            },
            "required": ["id"],
            "additionalProperties": false
        })
    }

    fn meta(&self) -> ToolMeta {
        ToolMeta {
            read_only: true,
            reaches_network: false,
            idempotent: true,
        }
    }

    fn validate_args(&self, args_v: &Value) -> Result<(), ToolError> {
        args::deny_unknown(args_v, NAME, KEYS)?;
        let id = args::req_str(args_v, NAME, "id")?;
        if id.trim().trim_start_matches('#').is_empty() {
            return Err(ToolError::BadArguments(format!("{NAME}.id is empty")));
        }
        Ok(())
    }

    async fn invoke(
        &self,
        ctx: &ToolCtx,
        args_v: Value,
    ) -> anyhow::Result<Result<ToolOutcome, ToolError>> {
        Ok(self.run(ctx, args_v))
    }
}

impl TaskGet {
    fn run(&self, ctx: &ToolCtx, args_v: Value) -> Result<ToolOutcome, ToolError> {
        self.validate_args(&args_v)?;
        let id = args::req_str(&args_v, NAME, "id")?;
        let file = store::tasks_path(ctx)?;
        let (doc, _) = store::load(&file)?;

        let Some(task) = doc.get(id) else {
            let total = doc.tasks().len();
            return Err(ToolError::BadArguments(format!(
                "no task with id {} in {RELATIVE_PATH} ({total} tasks there); \
                 TaskList shows the ids that exist",
                id.trim().trim_start_matches('#')
            )));
        };

        let mut out = render::line(&task);
        for note in &task.notes {
            out.push_str("\n    ");
            out.push_str(note);
        }
        Ok(ToolOutcome::new(out).with_display(render::line(&task)))
    }
}
