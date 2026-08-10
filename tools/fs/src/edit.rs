//! `Edit` — an ambiguous anchor is a refusal, never a guess.
//!
//! `old_string` occurs three times and the model meant one of them. Every way
//! of guessing which is wrong in a way that is hard to see afterwards: taking
//! the first edits code the model was not looking at; taking the last is the
//! same bug with different luck; editing all three is a refactor nobody asked
//! for. All three return `Ok`, and the damage surfaces later as a test failure
//! in a file the transcript never mentions.
//!
//! So a match count other than one is `BadArguments` **naming the count**, which
//! is the part that makes it actionable — the model's next move is to extend
//! the anchor with surrounding lines, and it can only choose that if it knows
//! the anchor was ambiguous rather than absent. Zero matches and four matches
//! are different problems and get different messages.
//!
//! **`replace_all` is a separate, explicit flag** rather than a fallback. The
//! difference between "fix this one call site" and "rename every occurrence" is
//! a decision, and it should be one the model states in the call rather than
//! one it stumbles into because the tool was accommodating.
//!
//! **Not idempotent, and that is correct.** Running the same edit twice fails
//! the second time, because after the first the anchor is gone. A tool that
//! reported success for a no-op would be lying about what it did.
//!
//! `idempotent: false` is what the tool declares about that, and it is worth
//! knowing what reads it, because an earlier version of this paragraph claimed
//! crash recovery does. Nothing does: there is no crash-recovery fold in this
//! project, and no replay decision anywhere consults the field. Its one reader
//! is `Registry::register`, which checks it against `read_only` — and `Edit`
//! declaring `read_only: false` is exactly what makes `idempotent: false` a
//! legal thing to say here rather than a contradiction.
//!
//! Unlike `Write`, a partial read *does* license an edit: an anchored change
//! only claims to know the text it matched, which is text the model has seen.
//!
//! The order of checks in `run` is itself a decision: the read-state refusal
//! comes before the file is opened, so a model editing a file it has never
//! looked at is told exactly that rather than being told its anchor was not
//! found. Two different problems, two different next moves.

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
// The schema, the honest `idempotent: false`, and the two refusals decidable
// from the arguments alone — an empty anchor and an anchor identical to its
// replacement. Both are caught here so neither needs a file to exist.
// ---------------------------------------------------------------------------

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
            reaches_network: false,
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

// endregion: The tool surface

// region: The anchored edit
// ---------------------------------------------------------------------------
// The anchored edit
//
// Read state first, then the file, then the match count — three refusals in the
// order that gives the model the most useful of them. Only after all three does
// anything get written, and the sighting recorded afterwards carries the
// completeness of the read that licensed the edit rather than upgrading it.
// ---------------------------------------------------------------------------

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

        // `str::matches` counts non-overlapping occurrences left to right, which
        // is the same walk `replacen`/`replace` below will make — so the count
        // reported in the error is the count that would have been changed, not
        // an estimate of it.
        let hits = before.matches(old).count();
        match (hits, replace_all) {
            // Zero is refused whatever `replace_all` says: "change all of them"
            // is not satisfied by changing none, and silently succeeding here
            // would let the model believe an edit landed that never did.
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

// endregion: The anchored edit
