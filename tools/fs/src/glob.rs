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

/// How many paths come back at most.
///
/// **Deliberately not reachable from an argument, and that is the decision
/// rather than an omission.** `WebFetch` grew a `max_links` because its cap bit
/// on a real page where the links *were* the answer and nothing could raise it;
/// the shape here is different. The call that surfaced this returned 1000 of
/// 64,097 paths in this repository, and 63,097 of those are build output under
/// `target/`. A `max_results` argument would have turned a useless answer into a
/// sixty-times larger useless answer, spent the session's context on it, and
/// still not told the model the thing it actually needed to know — which is
/// that the pattern was too broad.
///
/// So the fix for this cap is the sentence it prints, not a knob: it names the
/// number, says which 1000 these are, and points at `pattern` and `path`, which
/// are remedies that work. A remedy that cannot work is the failure this whole
/// mechanism exists to prevent.
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
            reaches_network: false,
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

/// One sentence per cut, in the order a reader needs them.
///
/// Separate sentences and never a merged summary — the same reasoning as the
/// web digest's `cuts`. A result whose *result cap* bit and one whose *walk*
/// stopped early are different facts with different remedies, and one line
/// covering both tells the model to narrow a pattern when the pattern was fine.
///
/// A free function so the four combinations can be read in a test without
/// building a two-hundred-thousand-entry tree; `run` has one caller of it and
/// no second opinion about when a result is whole.
fn cuts(shown: usize, total: usize, capped: bool, walk_truncated: bool) -> Vec<String> {
    let mut out = Vec::new();
    if capped {
        // Three things this has to say and one it must not. It names the cap,
        // says *which* paths these are — newest first is not a detail here,
        // it is the difference between a sample and an arbitrary slice — and
        // gives a remedy that works. What it must not do is imply a knob: see
        // `MAX_RESULTS` for why raising this cap would make the answer worse
        // rather than better.
        out.push(format!(
            "{shown} of {total} matching paths shown — the {shown} most recently modified — and \
             {} dropped by a fixed {MAX_RESULTS}-result cap no argument raises. A pattern \
             matching {total} paths is usually reaching into build output; narrow `pattern` or \
             set `path` to a subdirectory, which is the only thing that returns the rest",
            total - shown
        ));
    }
    if walk_truncated {
        out.push(walk::ceiling_notice());
    }
    out
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
        // one makes the answer incomplete, and each is stated three times over:
        // in the content, because the model reads the content; in the flag, for
        // the runtime; and in `truncation`, which is the one the terminal and
        // the model's result note both quote verbatim.
        let total = hits.len();
        let capped = total > MAX_RESULTS;
        hits.truncate(MAX_RESULTS);

        // Assembled before the empty check, not after, so there is exactly one
        // place that decides whether this answer is whole. The empty branch used
        // to be that second place, and it returned "no matches" unflagged even
        // when the walk had stopped early — an unqualified emptiness is the most
        // confidently misread answer a search can give, because it reads as a
        // fact about the tree when it is a fact about the part that was reached.
        let reason = match cuts(hits.len(), total, capped, walked.truncated) {
            cuts if cuts.is_empty() => None,
            // Joined with the same connective the web tools use, so a reader who
            // has seen one multi-cut notice recognises the second.
            cuts => Some(cuts.join("; also ")),
        };

        if hits.is_empty() {
            // Not an error. The tree was searched and held nothing matching,
            // which is a result the model can act on.
            return Ok(match reason {
                None => {
                    ToolOutcome::new(String::new()).with_display(format!("{pattern}: no matches"))
                }
                Some(reason) => ToolOutcome::new(format!("[truncated: {reason}]"))
                    .with_display(format!("{pattern}: no matches (search incomplete)"))
                    .truncated_because(reason),
            });
        }

        let listing: Vec<String> = hits.iter().map(|p| path::display(&root, p)).collect();
        let mut content = listing.join("\n");
        if let Some(reason) = &reason {
            content.push_str(&format!("\n[truncated: {reason}]"));
        }

        let outcome = ToolOutcome::new(content)
            .with_display(format!("{pattern}: {} of {total} paths", hits.len()));
        Ok(match reason {
            Some(reason) => outcome.truncated_because(reason),
            None => outcome,
        })
    }
}

// endregion: Matching, and the walk

// region: Tests
// ---------------------------------------------------------------------------
// Tests
//
// The four combinations of the two cuts, read directly. `tests/truncation.rs`
// drives the result cap through the real tool against a real tree; this covers
// the walk ceiling, which the integration test cannot reach without two hundred
// thousand files.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The sentence the reported session should have printed. It has to carry
    /// three numbers and a remedy — how many are here, how many matched, how
    /// many were dropped — because "1000 of 64097" reached the terminal and
    /// none of it reached the model.
    #[test]
    fn the_result_cap_names_itself_the_loss_and_a_remedy_that_works() {
        let out = cuts(1000, 64_097, true, false);
        assert_eq!(out.len(), 1, "one cut fired, one sentence: {out:?}");
        let s = &out[0];
        assert!(s.contains("1000 of 64097"), "{s}");
        assert!(s.contains("63097 dropped"), "the loss is unstated: {s}");
        assert!(s.contains("most recently modified"), "which 1000? {s}");
        assert!(s.contains("no argument raises"), "{s}");
        assert!(s.contains("`pattern`") && s.contains("`path`"), "{s}");
        // The remedy must be one that exists. `WebFetch` once advised narrowing
        // a request when what was dropped was an inventory, and this is the
        // mirror image: advertising a size knob here would send the model to
        // ask for sixty times more build output.
        assert!(!s.contains("max_results"), "invented a knob: {s}");
    }

    /// A walk that stopped early is a different fact with a different remedy,
    /// so it is a different sentence — and when both fire, both are said.
    /// Merging them is how a reader concludes the pattern was too broad when
    /// the traversal was what gave up.
    #[test]
    fn the_two_cuts_stay_two_sentences() {
        assert!(cuts(3, 3, false, false).is_empty());

        let walk_only = cuts(3, 3, false, true);
        assert_eq!(walk_only.len(), 1);
        assert!(walk_only[0].contains("never looked at"), "{walk_only:?}");
        assert!(
            !walk_only[0].contains("dropped by a fixed"),
            "a walk ceiling is not a result cap: {walk_only:?}"
        );

        let both = cuts(1000, 5000, true, true);
        assert_eq!(both.len(), 2, "one of the two cuts went unsaid: {both:?}");
    }
}

// endregion: Tests
