//! `TaskCreate` — one call, one plan.
//!
//! It takes a list rather than a single task because a plan arrives all at
//! once. Making the model spend four tool calls to write down four steps costs
//! four round trips and gives it three chances to change its mind halfway
//! through, leaving a half-written list on disk.
//!
//! Both `["do the thing"]` and `[{"text": "do the thing", "status":
//! "in_progress"}]` are accepted. Two shapes is one branch and one test here,
//! against a required wrapper the model would get wrong occasionally forever.

use emma_tool_api::{Tool, ToolCtx, ToolError, ToolMeta, ToolOutcome};
use serde_json::{json, Value};

use crate::args;
use crate::doc::{Doc, Status, TaskView};
use crate::render;
use crate::store;

const NAME: &str = "TaskCreate";
const KEYS: &[&str] = &["tasks"];

#[derive(Default)]
pub struct TaskCreate;

impl TaskCreate {
    pub fn new() -> Self {
        Self
    }
}

/// One requested task, already validated.
struct Requested {
    text: String,
    status: Status,
}

fn parse_one(value: &Value, at: usize) -> Result<Requested, ToolError> {
    let bad = |m: String| ToolError::BadArguments(m);
    match value {
        Value::String(s) => {
            if s.trim().is_empty() {
                return Err(bad(format!("{NAME}.tasks[{at}] is empty")));
            }
            Ok(Requested {
                text: s.trim().to_string(),
                status: Status::Pending,
            })
        }
        Value::Object(map) => {
            let mut unknown: Vec<&str> = map
                .keys()
                .map(String::as_str)
                .filter(|k| !["text", "status"].contains(k))
                .collect();
            if !unknown.is_empty() {
                unknown.sort_unstable();
                return Err(bad(format!(
                    "{NAME}.tasks[{at}] does not take {}; accepted keys are text, status",
                    unknown.join(", ")
                )));
            }
            let text = match map.get("text") {
                Some(Value::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
                Some(Value::String(_)) | None => {
                    return Err(bad(format!("{NAME}.tasks[{at}] requires a non-empty text")))
                }
                Some(other) => {
                    return Err(bad(format!(
                        "{NAME}.tasks[{at}].text must be a string, got {}",
                        args::type_name(other)
                    )))
                }
            };
            let status = match map.get("status") {
                None | Some(Value::Null) => Status::Pending,
                Some(Value::String(s)) => Status::from_wire(s).ok_or_else(|| {
                    bad(format!(
                        "{NAME}.tasks[{at}].status is {s}; it must be pending, in_progress or completed"
                    ))
                })?,
                Some(other) => {
                    return Err(bad(format!(
                        "{NAME}.tasks[{at}].status must be a string, got {}",
                        args::type_name(other)
                    )))
                }
            };
            Ok(Requested { text, status })
        }
        other => Err(bad(format!(
            "{NAME}.tasks[{at}] must be a string or an object with a text field, got {}",
            args::type_name(other)
        ))),
    }
}

#[async_trait::async_trait]
impl Tool for TaskCreate {
    fn name(&self) -> &'static str {
        NAME
    }

    fn description(&self) -> &str {
        include_str!("descriptions/task_create.md")
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "tasks": {
                    "type": "array",
                    "minItems": 1,
                    "description": "The tasks to add, in the order they should appear. Each is either a plain string, or an object with text and an optional status of pending, in_progress or completed.",
                    "items": {
                        "oneOf": [
                            { "type": "string" },
                            {
                                "type": "object",
                                "properties": {
                                    "text": { "type": "string", "description": "What the task is, in one line." },
                                    "status": {
                                        "type": "string",
                                        "enum": ["pending", "in_progress", "completed"],
                                        "description": "Defaults to pending."
                                    }
                                },
                                "required": ["text"],
                                "additionalProperties": false
                            }
                        ]
                    }
                }
            },
            "required": ["tasks"],
            "additionalProperties": false
        })
    }

    fn meta(&self) -> ToolMeta {
        ToolMeta {
            read_only: false,
            // Called twice with the same list, you get the list twice. Nothing
            // here deduplicates by text, because two genuinely identical steps
            // in a plan are a thing that happens.
            idempotent: false,
        }
    }

    fn validate_args(&self, args_v: &Value) -> Result<(), ToolError> {
        args::deny_unknown(args_v, NAME, KEYS)?;
        let list = args::req_array(args_v, NAME, "tasks")?;
        if list.is_empty() {
            return Err(ToolError::BadArguments(format!(
                "{NAME}.tasks is empty; there is nothing to create"
            )));
        }
        for (at, value) in list.iter().enumerate() {
            parse_one(value, at)?;
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

impl TaskCreate {
    fn run(&self, ctx: &ToolCtx, args_v: Value) -> Result<ToolOutcome, ToolError> {
        self.validate_args(&args_v)?;
        let list = args::req_array(&args_v, NAME, "tasks")?;
        let requested: Vec<Requested> = list
            .iter()
            .enumerate()
            .map(|(at, v)| parse_one(v, at))
            .collect::<Result<_, _>>()?;

        let file = store::tasks_path(ctx)?;
        let created: Vec<TaskView> = store::edit(&file, |doc: &mut Doc| {
            let mut made = Vec::new();
            for req in &requested {
                let id = doc.create(&req.text, req.status);
                made.push(TaskView {
                    id,
                    status: req.status,
                    text: req.text.clone(),
                    notes: Vec::new(),
                });
            }
            Ok(made)
        })?;

        let word = if created.len() == 1 { "task" } else { "tasks" };
        let heading = format!(
            "created {} {word} in {}",
            created.len(),
            crate::doc::RELATIVE_PATH
        );
        Ok(
            ToolOutcome::new(format!("{heading}\n{}", render::block(&created)))
                .with_display(heading),
        )
    }
}
