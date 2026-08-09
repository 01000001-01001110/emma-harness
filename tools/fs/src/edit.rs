//! `Edit` — an ambiguous anchor is a refusal, never a guess.

use std::sync::Arc;

use emma_tool_api::{Tool, ToolCtx, ToolError, ToolMeta, ToolOutcome};
use serde_json::{json, Value};

use crate::args;
use crate::path;
use crate::session::{ReadState, ReadTracker};

const NAME: &str = "Edit";
const KEYS: &[&str] = &["file_path", "old_string", "new_string", "replace_all"];

pub struct Edit {
    tracker: Arc<ReadTracker>,
}

impl Edit {
    pub fn new(tracker: Arc<ReadTracker>) -> Self {
        Self { tracker }
    }
}

#[async_trait::async_trait]
impl Tool for Edit {
    fn name(&self) -> &'static str {
        NAME
    }

    fn description(&self) -> &str {
        include_str!("descriptions/edit.md")
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Path to the file, relative to the working directory or absolute inside it."
                },
                "old_string": {
                    "type": "string",
                    "description": "Exact text to replace, including indentation. Must occur exactly once unless replace_all is set."
                },
                "new_string": {
                    "type": "string",
                    "description": "Text to put in its place."
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Replace every occurrence instead of requiring exactly one. Default false."
                }
            },
            "required": ["file_path", "old_string", "new_string"],
            "additionalProperties": false
        })
    }

    fn meta(&self) -> ToolMeta {
        ToolMeta {
            read_only: false,
            // Deliberately false. Running the same edit twice fails the second
            // time, because the anchor is gone — which is the correct
            // behaviour and precisely why it is not idempotent.
            idempotent: false,
        }
    }

    fn validate_args(&self, args_v: &Value) -> Result<(), ToolError> {
        args::deny_unknown(args_v, NAME, KEYS)?;
        args::req_str(args_v, NAME, "file_path")?;
        let old = args::req_str(args_v, NAME, "old_string")?;
        let new = args::req_str(args_v, NAME, "new_string")?;
        args::opt_bool(args_v, NAME, "replace_all")?;

        // Cheap and synchronous, so it belongs here rather than in `invoke`.
        if old.is_empty() {
            return Err(ToolError::BadArguments(
                "Edit.old_string is empty; use Write to create a file".into(),
            ));
        }
        if old == new {
            return Err(ToolError::BadArguments(
                "Edit.old_string and Edit.new_string are identical; the edit would do nothing"
                    .into(),
            ));
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

impl Edit {
    fn run(&self, ctx: &ToolCtx, args_v: Value) -> Result<ToolOutcome, ToolError> {
        self.validate_args(&args_v)?;
        let root = path::root(ctx)?;
        let raw = args::req_str(&args_v, NAME, "file_path")?;
        let old = args::req_str(&args_v, NAME, "old_string")?;
        let new = args::req_str(&args_v, NAME, "new_string")?;
        let replace_all = args::opt_bool(&args_v, NAME, "replace_all")?.unwrap_or(false);

        let (file, meta) = path::resolve_existing(&root, raw)?;
        if meta.is_dir() {
            return Err(ToolError::BadArguments(format!("{raw} is a directory")));
        }

        // Same rule as Write, for the same reason: an anchor that happened to
        // match inside a file nobody looked at is a coincidence, not a
        // location. Edit is safer than Write — it is anchored — so a *partial*
        // read is accepted here, since the anchor came from what was shown.
        let prior = self.tracker.state(&ctx.session_id, &file);
        match prior {
            ReadState::Fresh | ReadState::Partial => {}
            ReadState::Never => {
                return Err(ToolError::BadArguments(format!(
                    "{raw} has not been read in this session; Read it first"
                )))
            }
            ReadState::Stale => {
                return Err(ToolError::BadArguments(format!(
                    "{raw} changed on disk after you read it; Read it again before editing"
                )))
            }
        }

        let before = std::fs::read_to_string(&file)
            .map_err(|e| ToolError::Failed(format!("{raw} could not be read for editing: {e}")))?;

        let hits = before.matches(old).count();
        match (hits, replace_all) {
            (0, _) => {
                return Err(ToolError::BadArguments(format!(
                    "old_string was not found in {raw}; it must match byte for byte, \
                     and Read's line numbers are not part of the file"
                )))
            }
            // The named edge. Reporting the count is what makes the failure
            // actionable: the model knows whether to extend the anchor or to
            // say it meant all of them.
            (n, false) if n > 1 => {
                return Err(ToolError::BadArguments(format!(
                    "old_string occurs {n} times in {raw}; Edit will not choose \
                     between them. Extend old_string with surrounding lines until \
                     it is unique, or pass replace_all: true to change all {n}"
                )))
            }
            _ => {}
        }

        let after = if replace_all {
            before.replace(old, new)
        } else {
            before.replacen(old, new, 1)
        };

        std::fs::write(&file, &after)
            .map_err(|e| ToolError::Failed(format!("{raw} could not be written: {e}")))?;
        // Re-stamped so the edit does not read as an outside change, but the
        // completeness carries over: editing one anchor inside a file seen only
        // in part does not mean the rest has now been seen.
        self.tracker
            .record(&ctx.session_id, &file, prior == ReadState::Fresh);

        let shown = path::display(&root, &file);
        let what = if hits == 1 {
            "1 replacement".to_string()
        } else {
            format!("{hits} replacements")
        };
        Ok(ToolOutcome::new(format!("{shown}: {what}")).with_display(format!("{shown}: {what}")))
    }
}
