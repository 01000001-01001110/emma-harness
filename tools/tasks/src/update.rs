//! `TaskUpdate` — change a status or the wording, by handle.
//!
//! **Completing a task ticks its box where it sits.** It is not deleted and it
//! is not moved to a `## Done` section. Both alternatives were considered and
//! both lose something a person put there:
//!
//! - *Deleting* throws away the note they wrote underneath, and it is the one
//!   irreversible operation in this crate. The argument for it — a file that
//!   only grows becomes noise — is real, but the noise that matters is in the
//!   model's context, and `TaskList` already defaults to open tasks only. The
//!   file grows; what the loop reads does not.
//! - *Moving to a section* relocates their note and reshuffles an order they
//!   chose, every single time a task completes. It also makes the file's
//!   structure load-bearing, so a person who reorganises the headings changes
//!   the data without meaning to.
//!
//! A ticked box in place is a complete record of what was done, in the order it
//! was done, which is what somebody scrolling the file afterwards actually
//! wants. If it gets long, deleting the done lines is two seconds of a human's
//! time, and this crate copes with that on its next read — which is tested.
//!
//! **An unknown id is `BadArguments`, and so is an update that changes
//! nothing.** Neither status nor text is a call that would return `Ok` having
//! done nothing at all, and a tool that reports success for a no-op is lying
//! about what it did.

use emma_tool_api::{Tool, ToolCtx, ToolError, ToolMeta, ToolOutcome};
use serde_json::{json, Value};

use crate::args;
use crate::doc::{Doc, Status, RELATIVE_PATH};
use crate::render;
use crate::store;

const NAME: &str = "TaskUpdate";
const KEYS: &[&str] = &["id", "status", "text"];
const STATUSES: &[&str] = &["pending", "in_progress", "completed"];

#[derive(Default)]
pub struct TaskUpdate;

impl TaskUpdate {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl Tool for TaskUpdate {
    fn name(&self) -> &'static str {
        NAME
    }

    fn description(&self) -> &str {
        include_str!("descriptions/task_update.md")
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "The task's handle, as shown by TaskList. The leading # is optional."
                },
                "status": {
                    "type": "string",
                    "enum": STATUSES,
                    "description": "The new status. Leave it out to change only the text."
                },
                "text": {
                    "type": "string",
                    "description": "New wording for the task. Leave it out to change only the status."
                }
            },
            "required": ["id"],
            "additionalProperties": false
        })
    }

    fn meta(&self) -> ToolMeta {
        ToolMeta {
            read_only: false,
            // Setting a status to what it already is leaves the same file.
            // Unlike Edit there is no anchor to consume, so a replay is safe.
            idempotent: true,
        }
    }

    fn validate_args(&self, args_v: &Value) -> Result<(), ToolError> {
        args::deny_unknown(args_v, NAME, KEYS)?;
        let id = args::req_str(args_v, NAME, "id")?;
        if id.trim().trim_start_matches('#').is_empty() {
            return Err(ToolError::BadArguments(format!("{NAME}.id is empty")));
        }
        let status = args::opt_str(args_v, NAME, "status")?;
        let text = args::opt_str(args_v, NAME, "text")?;
        if let Some(s) = status {
            if Status::from_wire(s).is_none() {
                return Err(ToolError::BadArguments(format!(
                    "{NAME}.status is {s}; it must be one of {}",
                    STATUSES.join(", ")
                )));
            }
        }
        if let Some(t) = text {
            if t.trim().is_empty() {
                return Err(ToolError::BadArguments(format!(
                    "{NAME}.text is empty; to drop a task, delete its line by hand \
                     or mark it completed"
                )));
            }
        }
        if status.is_none() && text.is_none() {
            return Err(ToolError::BadArguments(format!(
                "{NAME} needs status, text, or both; as written it would change nothing"
            )));
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

impl TaskUpdate {
    fn run(&self, ctx: &ToolCtx, args_v: Value) -> Result<ToolOutcome, ToolError> {
        self.validate_args(&args_v)?;
        let id = args::req_str(&args_v, NAME, "id")?;
        let status = args::opt_str(&args_v, NAME, "status")?.and_then(Status::from_wire);
        let text = args::opt_str(&args_v, NAME, "text")?;

        let file = store::tasks_path(ctx)?;
        let bare = id.trim().trim_start_matches('#').to_string();

        let after = store::edit(&file, |doc: &mut Doc| {
            if !doc.update(&bare, status, text) {
                let total = doc.tasks().len();
                return Err(ToolError::BadArguments(format!(
                    "no task with id {bare} in {RELATIVE_PATH} ({total} tasks there); \
                     TaskList shows the ids that exist"
                )));
            }
            // Read back from the document rather than from the arguments: the
            // line as it now stands is the fact, and reporting what was asked
            // for would hide any way the two could differ.
            Ok(doc.get(&bare).expect("just updated"))
        })?;

        // The terminal gets the status in words; the model gets the line as it
        // now reads, which is the same shape TaskList shows it.
        let watching = format!("#{} is now {}", after.id, after.status.wire());
        Ok(ToolOutcome::new(render::line(&after)).with_display(watching))
    }
}
