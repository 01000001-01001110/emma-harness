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
use crate::session::{LineHashes, ReadState, ReadTracker};

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

        // **Asked before the write, because the rename destroys the answer.**
        // Afterwards the count is one and the fact that there was ever another
        // name is gone from the filesystem.
        let severed = path::severed_link_note(&target);

        path::write_atomically(&target, content.as_bytes())
            .map_err(|e| ToolError::Failed(format!("{raw} could not be written: {e}")))?;

        // The agent authored these bytes, so it has seen them: a follow-up
        // Write is not blind and should not be refused. The per-line hashes go
        // in for the same reason — a file the agent just wrote is one it can
        // address by line without reading back, and refusing that would make
        // `Write` then `Edit` cost a `Read` in between for no information.
        self.tracker.record(
            &ctx.session_id,
            &target,
            true,
            LineHashes::of_text(content),
            content,
        );

        let shown = path::display(&root, &target);
        let lines = content.lines().count();
        let verb = if existed { "replaced" } else { "created" };
        // **A name Windows cannot easily undo.** Emma's root canonicalises to
        // the `\\?\` verbatim form, which is why `NUL` here is a real file that
        // round-trips rather than a write to the null device — the classic data
        // loss, and Emma does not have it. What it does have is the other side
        // of the same coin: `cmd`, Explorer and most tooling reach files through
        // the non-verbatim API and cannot open, move or delete a name like this
        // at all. Creating one silently leaves somebody a file they cannot get
        // rid of without knowing the trick.
        //
        // Said rather than refused. The write worked, the content is retrievable
        // through Emma, and refusing a name the filesystem accepted would be
        // Emma deciding what a user may call a file.
        let note = awkward_on_windows(&target)
            .map(|why| {
                format!(
                    "\n[note: `{shown}` {why}. Emma can read and overwrite it, but cmd, \
                     Explorer and most tools cannot — deleting it needs a \\\\?\\ path.]"
                )
            })
            .unwrap_or_default();
        // Same rule, second fact: said, not refused. The write is what was
        // asked for and it worked.
        let severed = severed
            .map(|why| {
                format!(
                    "
[note: `{shown}` — {why}.]"
                )
            })
            .unwrap_or_default();
        Ok(ToolOutcome::new(format!(
            "{verb} {shown} ({lines} lines, {} bytes){note}{severed}",
            content.len()
        ))
        .with_display(format!("{verb} {shown}")))
    }
}

/// Why a filename will be awkward for ordinary Windows tooling, if it will be.
///
/// `None` everywhere else, and on Windows for every ordinary name. This does not
/// refuse anything — it exists so a file that is hard to delete does not arrive
/// silently.
fn awkward_on_windows(path: &std::path::Path) -> Option<&'static str> {
    if !cfg!(windows) {
        return None;
    }
    let name = path.file_name()?.to_str()?;
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    const DEVICES: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if DEVICES.contains(&stem.as_str()) {
        return Some("is a reserved device name on Windows");
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Some("ends with a dot or a space, which Windows normally strips");
    }
    None
}

// endregion: The refusal, and the write
