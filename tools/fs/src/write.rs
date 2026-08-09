//! `Write` — refuses to clobber what it has not seen.

use std::sync::Arc;

use emma_tool_api::{Tool, ToolCtx, ToolError, ToolMeta, ToolOutcome};
use serde_json::{json, Value};

use crate::args;
use crate::path;
use crate::session::{ReadState, ReadTracker};

const NAME: &str = "Write";
const KEYS: &[&str] = &["file_path", "content"];

pub struct Write {
    tracker: Arc<ReadTracker>,
}

impl Write {
    pub fn new(tracker: Arc<ReadTracker>) -> Self {
        Self { tracker }
    }
}

#[async_trait::async_trait]
impl Tool for Write {
    fn name(&self) -> &'static str {
        NAME
    }

    fn description(&self) -> &str {
        include_str!("descriptions/write.md")
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Path to write, relative to the working directory or absolute inside it. Parent directories are created."
                },
                "content": {
                    "type": "string",
                    "description": "The complete new contents of the file."
                }
            },
            "required": ["file_path", "content"],
            "additionalProperties": false
        })
    }

    fn meta(&self) -> ToolMeta {
        ToolMeta {
            read_only: false,
            // Writing the same bytes twice leaves the same file. The mtime
            // moves, which nothing here consults.
            idempotent: true,
        }
    }

    fn validate_args(&self, args_v: &Value) -> Result<(), ToolError> {
        args::deny_unknown(args_v, NAME, KEYS)?;
        args::req_str(args_v, NAME, "file_path")?;
        args::req_str(args_v, NAME, "content")?;
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

impl Write {
    fn run(&self, ctx: &ToolCtx, args_v: Value) -> Result<ToolOutcome, ToolError> {
        self.validate_args(&args_v)?;
        let root = path::root(ctx)?;
        let raw = args::req_str(&args_v, NAME, "file_path")?;
        let content = args::req_str(&args_v, NAME, "content")?;

        let target = path::resolve(&root, raw)?;

        let existing = std::fs::symlink_metadata(&target);
        let existed = match &existing {
            Ok(m) if m.is_dir() => {
                return Err(ToolError::BadArguments(format!("{raw} is a directory")))
            }
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => {
                return Err(ToolError::Failed(format!("{raw} cannot be stat'd: {e}")));
            }
        };

        // The whole point of the tool. A refusal here is a `BadArguments`
        // because it is entirely actionable: read the file, then write it.
        if existed {
            match self.tracker.state(&ctx.session_id, &target) {
                ReadState::Fresh => {}
                ReadState::Never => {
                    return Err(ToolError::BadArguments(format!(
                        "{raw} exists and has not been read in this session; \
                         Read it first so the overwrite is not blind"
                    )))
                }
                ReadState::Partial => {
                    return Err(ToolError::BadArguments(format!(
                        "{raw} was only read in part; Read it in full before \
                         replacing it, or use Edit to change the part you saw"
                    )))
                }
                ReadState::Stale => {
                    return Err(ToolError::BadArguments(format!(
                        "{raw} changed on disk after you read it; Read it again \
                         before overwriting so the change is not discarded"
                    )))
                }
            }
        }

        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                ToolError::Failed(format!(
                    "the parent directory of {raw} could not be created: {e}"
                ))
            })?;
        }

        std::fs::write(&target, content)
            .map_err(|e| ToolError::Failed(format!("{raw} could not be written: {e}")))?;

        // The agent authored these bytes, so it has seen them: a follow-up
        // Write is not blind and should not be refused.
        self.tracker.record(&ctx.session_id, &target, true);

        let shown = path::display(&root, &target);
        let lines = content.lines().count();
        let verb = if existed { "replaced" } else { "created" };
        Ok(ToolOutcome::new(format!(
            "{verb} {shown} ({lines} lines, {} bytes)",
            content.len()
        ))
        .with_display(format!("{verb} {shown}")))
    }
}
