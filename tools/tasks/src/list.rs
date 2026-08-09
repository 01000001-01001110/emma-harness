//! `TaskList` — the one that lands in the context window over and over.
//!
//! **The default filter is `open`, not `all`.** Completed tasks are kept in the
//! file forever (see `descriptions/task_update.md` for why deleting them is
//! worse), so by the end of a long run they outnumber the live ones several to
//! one. Reprinting them every turn spends context re-establishing what is
//! already settled, and a list that is mostly noise is a list the model stops
//! reading carefully. The file grows; the context does not. `status: "all"`
//! says otherwise in one word.
//!
//! **No tasks is `Ok`.** An empty list is what the world contains, not a
//! failure of the call. The content still carries a sentence rather than being
//! the empty string, because "" is indistinguishable from a tool that broke.

use emma_tool_api::{Tool, ToolCtx, ToolError, ToolMeta, ToolOutcome};
use serde_json::{json, Value};

use crate::args;
use crate::doc::{Status, TaskView, RELATIVE_PATH};
use crate::render;
use crate::store;

const NAME: &str = "TaskList";
const KEYS: &[&str] = &["status"];
const FILTERS: &[&str] = &["open", "all", "pending", "in_progress", "completed"];

#[derive(Default)]
pub struct TaskList;

impl TaskList {
    pub fn new() -> Self {
        Self
    }
}

fn keep(filter: &str, task: &TaskView) -> bool {
    match filter {
        "all" => true,
        "open" => task.status.is_open(),
        other => Status::from_wire(other) == Some(task.status),
    }
}

#[async_trait::async_trait]
impl Tool for TaskList {
    fn name(&self) -> &'static str {
        NAME
    }

    fn description(&self) -> &str {
        include_str!("descriptions/task_list.md")
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "status": {
                    "type": "string",
                    "enum": FILTERS,
                    "description": "Which tasks to return. Defaults to open, meaning pending and in_progress. Use all to include completed ones."
                }
            },
            "required": [],
            "additionalProperties": false
        })
    }

    fn meta(&self) -> ToolMeta {
        ToolMeta {
            read_only: true,
            idempotent: true,
        }
    }

    fn validate_args(&self, args_v: &Value) -> Result<(), ToolError> {
        args::deny_unknown(args_v, NAME, KEYS)?;
        if let Some(s) = args::opt_str(args_v, NAME, "status")? {
            if !FILTERS.contains(&s) {
                return Err(ToolError::BadArguments(format!(
                    "{NAME}.status is {s}; it must be one of {}",
                    FILTERS.join(", ")
                )));
            }
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

impl TaskList {
    fn run(&self, ctx: &ToolCtx, args_v: Value) -> Result<ToolOutcome, ToolError> {
        self.validate_args(&args_v)?;
        let filter = args::opt_str(&args_v, NAME, "status")?.unwrap_or("open");

        let file = store::tasks_path(ctx)?;
        let (doc, _) = store::load(&file)?;
        let all = doc.tasks();
        let shown: Vec<TaskView> = all.iter().filter(|t| keep(filter, t)).cloned().collect();

        // The counts describe the whole file, not the filtered slice: a model
        // that asked for `pending` still needs to know whether anything is in
        // progress before it concludes the work is stalled.
        let counts = render::counts(&all);
        if shown.is_empty() {
            let what = if all.is_empty() {
                format!("{RELATIVE_PATH} has no tasks")
            } else {
                format!("no {filter} tasks ({counts})")
            };
            return Ok(ToolOutcome::new(what.clone()).with_display(what));
        }

        let content = format!("{counts}\n{}", render::block(&shown));
        Ok(ToolOutcome::new(content).with_display(counts))
    }
}
