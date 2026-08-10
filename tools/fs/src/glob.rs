//! `Glob` — matching nothing is an answer.
//!
//! A pattern that matches no files **succeeds** with an empty list. This is the
//! contract's governing rule at its most literal: "there are no `.proto` files
//! here" is a fact about the repository, and it is often exactly the fact the
//! model was checking for. Returning an error would tell it the search could
//! not be performed, which is a different thing entirely, and it would route
//! around a problem that does not exist.
//!
//! **The walk does not follow symlinks.** Not for cycle safety — `walkdir`
//! handles loops — but for containment. A directory symlink pointing outside
//! the root would let a glob enumerate, and then a read reach, anything on the
//! machine, without a single `..` appearing in the pattern. The containment
//! check in `path.rs` guards the arguments; refusing to follow links is what
//! guards the traversal. Both are needed, and the tests exercise the escape
//! through a link rather than only the escape through a path.
//!
//! Results come back newest first — modification time descending, path as the
//! tiebreak — so the most recently touched work is what the model reads before
//! it runs out of attention, and so two calls over an unchanged tree return
//! identical bytes. The determinism matters more than it looks: the output
//! rides in the conversation, the conversation rides in the cached prefix, and
//! a set that reordered between calls would move prefix bytes for no reason.
//! The tiebreak is what supplies it — mtime alone leaves files written in the
//! same instant free to swap places.

use emma_tool_api::{Tool, ToolCtx, ToolError, ToolMeta, ToolOutcome};
use globset::{Glob as GlobPattern, GlobMatcher};
use serde_json::{json, Value};

use crate::args;
use crate::path;
use crate::walk;

// region: The tool surface
// ---------------------------------------------------------------------------
// The tool surface
//
// The result cap, the schema, and the pattern compiled once in `validate_args`
// so a malformed glob is refused before any tree is walked.
// ---------------------------------------------------------------------------

const NAME: &str = "Glob";
const KEYS: &[&str] = &["pattern", "path"];
pub const MAX_RESULTS: usize = 1000;

#[derive(Default)]
pub struct Glob;

impl Glob {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl Tool for Glob {
    fn name(&self) -> &'static str {
        NAME
    }

    fn description(&self) -> &str {
        include_str!("descriptions/glob.md")
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob matched against paths relative to the search root, e.g. **/*.rs"
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search. Defaults to the working directory."
                }
            },
            "required": ["pattern"],
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
        let pattern = args::req_str(args_v, NAME, "pattern")?;
        args::opt_str(args_v, NAME, "path")?;
        // A malformed pattern is a fact about the call and needs no filesystem
        // to detect, so it is caught before anything is walked.
        compile(pattern)?;
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

// region: Matching, and the walk
// ---------------------------------------------------------------------------
// Matching, and the walk
//
// One compiler shared with `Grep`, which also takes a glob, so both tools mean
// the same thing by a pattern and report the same words when one is malformed.
// Then the search itself: walk, match on the relative path, order, cap.
// ---------------------------------------------------------------------------

/// `pub(crate)` because `Grep`'s `glob` parameter compiles through here too.
/// One implementation is the point — two would eventually disagree about what
/// `**` means, and the model would have to know which tool it was talking to.
pub(crate) fn compile(pattern: &str) -> Result<GlobMatcher, ToolError> {
    GlobPattern::new(pattern)
        .map(|g| g.compile_matcher())
        .map_err(|e| ToolError::BadArguments(format!("{pattern} is not a valid glob: {e}")))
}

impl Glob {
    fn run(&self, ctx: &ToolCtx, args_v: Value) -> Result<ToolOutcome, ToolError> {
        self.validate_args(&args_v)?;
        let root = path::root(ctx)?;
        let pattern = args::req_str(&args_v, NAME, "pattern")?;
        let matcher = compile(pattern)?;

        let base = match args::opt_str(&args_v, NAME, "path")? {
            None => root.clone(),
            Some(p) => {
                let (resolved, meta) = path::resolve_existing(&root, p)?;
                if !meta.is_dir() {
                    return Err(ToolError::BadArguments(format!("{p} is not a directory")));
                }
                resolved
            }
        };

        let walked = walk::files(&base);
        let mut hits: Vec<_> = walked
            .files
            .into_iter()
            .filter(|f| {
                let rel = f.strip_prefix(&base).unwrap_or(f);
                // Matched on the forward-slash spelling so one pattern works on
                // both platforms; a model writing `src/**/*.rs` should not have
                // to know which machine it is on.
                matcher.is_match(rel.to_string_lossy().replace('\\', "/").as_str())
            })
            .collect();
        walk::sort_newest_first(&mut hits);

        // Two independent truncations, and the message below has to distinguish
        // them. `capped` means more matched than are being returned; the walk's
        // own `truncated` means the traversal stopped early, so `total` is how
        // many matched *before* the ceiling rather than how many exist. Either
        // one makes the answer incomplete, and both are stated in the content
        // as well as in the flag, because the model reads the content.
        let total = hits.len();
        let capped = total > MAX_RESULTS;
        hits.truncate(MAX_RESULTS);

        if hits.is_empty() {
            // Not an error. The tree was searched and held nothing matching,
            // which is a result the model can act on.
            return Ok(
                ToolOutcome::new(String::new()).with_display(format!("{pattern}: no matches"))
            );
        }

        let listing: Vec<String> = hits.iter().map(|p| path::display(&root, p)).collect();
        let mut content = listing.join("\n");
        let truncated = capped || walked.truncated;
        if truncated {
            content.push_str(&format!(
                "\n[truncated: showing {} of {}{}]",
                hits.len(),
                total,
                if walked.truncated {
                    format!(
                        " matched before the {}-entry walk ceiling",
                        walk::MAX_VISITED
                    )
                } else {
                    String::new()
                }
            ));
        }

        let outcome = ToolOutcome::new(content)
            .with_display(format!("{pattern}: {} of {total} paths", hits.len()));
        Ok(if truncated {
            outcome.truncated()
        } else {
            outcome
        })
    }
}

// endregion: Matching, and the walk
