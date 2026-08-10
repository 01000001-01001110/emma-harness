//! `Write` — refuses to clobber what it has not seen.
//!
//! This is the destructive move that looks like progress. An agent asked to
//! "add a function to utils.rs" can satisfy the request by writing a file
//! containing exactly that function, and the result reads as success — the
//! tool returned `Ok`, the file exists, the function is in it. What is missing
//! is everything that used to be there, and nothing in the transcript says so.
//!
//! **So the rule is a whole-file write requires a whole-file read, in this
//! session.** Three ways that read can fail to license the write, each learned
//! from a distinct way of losing work:
//!
//! - **Never read.** The model is writing from an assumption about what the
//!   file contains. Refused.
//! - **Read, but the file changed since** — length or mtime moved. Something
//!   else edited it: the user in their editor, a formatter, a build step, a
//!   concurrent agent. The model's picture is stale and overwriting silently
//!   discards whatever arrived. Refused, and the error says so, because "read
//!   it again" is a thing the model can actually act on.
//! - **Read only in part** (offset or truncated). Paging through a large file
//!   tells you about a window, not a file. Refused — though an anchored `Edit`
//!   is fine, since it only claims to know the text it matched.
//!
//! Creating a file that does not exist yet needs no prior read: there is
//! nothing to lose, and requiring a read of a missing file would be a rule
//! that only ever produces confusion.
//!
//! The tracking lives in a `ReadTracker` shared with `Read` and `Edit` — see
//! `session.rs` for what that costs and why `fs_tools()` is the only supported
//! way to build these.

use std::sync::Arc;

use emma_tool_api::{Tool, ToolCtx, ToolError, ToolMeta, ToolOutcome};
use serde_json::{json, Value};

use crate::args;
use crate::path;
use crate::session::{ReadState, ReadTracker};

// region: The tool surface
// ---------------------------------------------------------------------------
// The tool surface
//
// Two parameters and no options. There is deliberately no `force` and no
// `create_only`: the refusal below is the tool's reason to exist, and a flag
// that turns it off is a flag a model under pressure will find.
// ---------------------------------------------------------------------------

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
            reaches_network: false,
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

// endregion: The tool surface

// region: The refusal, and the write
// ---------------------------------------------------------------------------
// The refusal, and the write
//
// Order is the whole design of this function: contain the path, decide whether
// anything is at risk, refuse if the read does not license the overwrite, and
// only then create directories and touch the disk. Nothing before the refusal
// changes the filesystem, which is what makes a refusal actually a refusal.
// ---------------------------------------------------------------------------

impl Write {
    fn run(&self, ctx: &ToolCtx, args_v: Value) -> Result<ToolOutcome, ToolError> {
        self.validate_args(&args_v)?;
        let root = path::root(ctx)?;
        let raw = args::req_str(&args_v, NAME, "file_path")?;
        let content = args::req_str(&args_v, NAME, "content")?;

        let target = path::resolve(&root, raw)?;

        // `symlink_metadata`, so a dangling symlink counts as something that
        // exists and the read requirement applies to it. `resolve` has already
        // canonicalised any link whose target is real, so a link pointing out
        // of the root never reaches this line at all — it was refused as a path.
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

        // Creating the parents is safe to do unconditionally because `target`
        // is already contained: every directory made here is inside the root by
        // construction. Doing it after the refusal check and not before means a
        // refused write leaves no directories behind either.
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

// endregion: The refusal, and the write
