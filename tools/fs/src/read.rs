//! `Read` — bounded, and loud about it.
//!
//! A read has to be capped: a model handed a 40 MB log will spend the session's
//! entire context on it and then reason about the wrong end. So the cap is not
//! negotiable. What *is* a design decision is what happens when it bites.
//!
//! **Silent truncation is the failure worth engineering against.** A truncated
//! file and a short file look identical to a model that was not told, and it
//! will go on to state confident things about a function it never saw — the
//! most expensive kind of wrong, because nothing about the answer looks
//! uncertain. So truncation is reported twice: `ToolOutcome::truncated` for the
//! runtime, and a line in the content itself, because the model reads the
//! content and not the struct. Both, deliberately, until the contract makes
//! that structural.
//!
//! **A partial read is tracked as partial.** `offset`/`limit` exist so the
//! model can page through something large, but a file seen in part is not a
//! file seen. `Write` refuses to overwrite on the strength of one — see
//! `session.rs` — while `Edit` accepts it, because an anchored edit only claims
//! to know the text it matched. That asymmetry is the whole reason completeness
//! is recorded rather than just the fact of a read.
//!
//! Reading a file that exists and is empty **succeeds** and returns nothing.
//! Emptiness is a result; only the machinery failing is an error.
//!
//! **Every line is labelled `<number>#<hash>`, and that is a real cost paid on
//! every read to make some edits cheaper.** The trade was measured rather than
//! assumed, and the measurement is closer than the idea's reputation suggests,
//! so it is written down here instead of being waved at.
//!
//! Counted with Anthropic's `count_tokens` endpoint over 2000 lines of this
//! repository's own Rust: the label costs **3.6 tokens a line**, a 23% increase
//! on the rendered read. That is more than it looks like it should be — four hex
//! characters are three or four tokens because they are not a word — and it is
//! irreducible, not an encoding mistake. Base36 at the same width is worse
//! (3.97), dropping the `#` separator saves only 0.5, and narrowing to three hex
//! characters saves 0.7 at the price of four times the collision exposure on the
//! one check nothing else backs up. Hex at four characters is the cheapest thing
//! that is still worth having.
//!
//! On the other side, over fourteen real single-line edits sampled from this
//! repository, the addressed form costs **26% fewer output tokens** than the
//! `old_string` form it replaces — 842 against 1140 — because the anchor stops
//! having to be long enough to be unique. Those are the two honest numbers, and
//! they do not settle it on their own: at list prices a 400-line read costs
//! about 1400 extra input tokens and each edit saves about 21 output tokens, so
//! the direct arithmetic breaks even at roughly fourteen edits per read
//! uncached, or one and a half per read once the read is cached — which it
//! normally is within a turn.
//!
//! **So the token count is close to a wash, and the case does not rest on it.**
//! It rests on the calls that no longer happen: a literal anchor that misses
//! costs a whole second `Read` of the file plus a re-emitted edit, and an edit
//! against a file that drifted used to cost exactly that every single time.
//! That is the expensive path, it is the common one on a machine where somebody
//! has the file open in an editor, and it is the one this removes. Anyone
//! revisiting this should re-measure rather than trust the paragraph above; see
//! `notes/improvements.md`, which records that the 61% figure quoted from
//! elsewhere is a different scheme's author benchmarking their own scheme.
//!
//! The hash covers the line **exactly as this file shows it**, which is the
//! property that makes it safe to quote back — including trailing whitespace,
//! and see `hashline.rs` for why that is deliberate rather than incidental. The
//! one line that cannot honour that promise is a clipped one: the model has seen
//! its front and not its end, so it gets `#----` instead of a hash and `Edit`
//! refuses that marker by name.
//!
//! Three separate things count as truncation and any of them sets the flag:
//! there are lines after the window, the byte cap bit before the line cap did,
//! or some single line was too long to show whole. `render` collects all three
//! reasons and states each one it hit, because "truncated" on its own does not
//! tell the model whether to page forward, narrow the range, or stop expecting
//! the rest of a line.

use std::path::Path;
use std::sync::Arc;

use emma_tool_api::{Tool, ToolCtx, ToolError, ToolMeta, ToolOutcome};
use serde_json::{json, Value};

use crate::args;
use crate::hashline;
use crate::path;
use crate::session::{LineHashes, ReadTracker};

// region: The tool surface
// ---------------------------------------------------------------------------
// The tool surface
//
// The three caps, the schema, and the tracker this tool shares with `Write` and
// `Edit`. The caps are consts because they are quoted back to the model in two
// other places, and a number written out three times starts disagreeing.
// ---------------------------------------------------------------------------

const NAME: &str = "Read";
const KEYS: &[&str] = &["file_path", "offset", "limit"];

/// Caps, in one place because they are quoted in the description and in the
/// message the model sees when they fire. Three copies of a number is how the
/// message starts lying about the behaviour.
pub const MAX_LINES: usize = 2000;
pub const MAX_LINE_CHARS: usize = 2000;
pub const MAX_BYTES: usize = 256 * 1024;

pub struct Read {
    tracker: Arc<ReadTracker>,
}

impl Read {
    pub fn new(tracker: Arc<ReadTracker>) -> Self {
        Self { tracker }
    }
}

#[async_trait::async_trait]
impl Tool for Read {
    fn name(&self) -> &'static str {
        NAME
    }

    fn description(&self) -> &str {
        include_str!("descriptions/read.md")
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Path to the file, relative to the working directory or absolute inside it."
                },
                "offset": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "1-based line number to start at. Use it to continue after a truncated read."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "description": format!("How many lines to return. Capped at {MAX_LINES}.")
                }
            },
            "required": ["file_path"],
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
        args::req_str(args_v, NAME, "file_path")?;
        for key in ["offset", "limit"] {
            if let Some(0) = args::opt_u64(args_v, NAME, key)? {
                return Err(ToolError::BadArguments(format!(
                    "Read.{key} must be at least 1"
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

// endregion: The tool surface

// region: Reading, and what gets recorded
// ---------------------------------------------------------------------------
// Reading, and what gets recorded
//
// The call itself, and the sighting it leaves behind. The sighting is the part
// with consequences elsewhere: it is what later licenses or refuses a `Write`,
// so whether this read counts as complete is decided here and nowhere else.
// ---------------------------------------------------------------------------

impl Read {
    fn run(&self, ctx: &ToolCtx, args_v: Value) -> Result<ToolOutcome, ToolError> {
        self.validate_args(&args_v)?;
        let root = path::root(ctx)?;
        let raw = args::req_str(&args_v, NAME, "file_path")?;
        let offset = args::opt_u64(&args_v, NAME, "offset")?.unwrap_or(1) as usize;
        let limit = args::opt_u64(&args_v, NAME, "limit")?
            .map(|v| (v as usize).min(MAX_LINES))
            .unwrap_or(MAX_LINES);

        let (file, meta) = path::resolve_existing(&root, raw)?;
        if meta.is_dir() {
            return Err(ToolError::BadArguments(format!(
                "{raw} is a directory; use Glob to list it"
            )));
        }

        let bytes = std::fs::read(&file)
            .map_err(|e| ToolError::Failed(format!("{raw} could not be read: {e}")))?;
        // Not `BadArguments`: the path was a perfectly good path and the caller
        // could not have known. The machinery — a UTF-8 decode — is what failed.
        let text = String::from_utf8(bytes).map_err(|_| {
            ToolError::Failed(format!(
                "{raw} is not valid UTF-8; Read returns text and cannot show it"
            ))
        })?;

        let shown = render(&root, &file, &text, offset, limit);
        // A truncated read is recorded as a *partial* sighting. Treating "I saw
        // the first 2000 lines" as "I have read this file" is precisely how a
        // subsequent `Write` throws away the other ten thousand.
        // `offset > 1` is a deliberate slice and is partial for the same reason.
        //
        // The line hashes recorded alongside it are the window that was
        // rendered and nothing more. A `Read` of lines 1-100 leaves lines
        // 101-onwards with no record, so an addressed `Edit` reaching into them
        // is refused for want of one — the same "you have not seen this" rule as
        // the completeness flag, at line resolution instead of file resolution.
        self.tracker.record(
            &ctx.session_id,
            &file,
            !shown.outcome.truncated && offset == 1,
            shown.lines,
        );

        Ok(shown.outcome)
    }
}

// endregion: Reading, and what gets recorded

// region: Rendering, and saying what was cut
// ---------------------------------------------------------------------------
// Rendering, and saying what was cut
//
// Line numbering, the three independent caps, and the note that admits to each
// one that fired. This half is where silent truncation would live if it were
// going to, so every early exit either returns a complete answer or says it is
// not one.
// ---------------------------------------------------------------------------

/// What one `Read` produced: the answer for the model, and the record for the
/// tracker. They are returned together because they must describe the same set
/// of lines — a record covering lines the render dropped would license an
/// addressed `Edit` against text nobody was shown.
struct Shown {
    outcome: ToolOutcome,
    lines: LineHashes,
}

impl Shown {
    fn nothing(outcome: ToolOutcome) -> Self {
        Self {
            outcome,
            lines: LineHashes::default(),
        }
    }
}

fn render(root: &Path, file: &Path, text: &str, offset: usize, limit: usize) -> Shown {
    let shown = path::display(root, file);
    if text.is_empty() {
        // Content stays genuinely empty rather than gaining a prose stand-in,
        // because anything written here would be indistinguishable from file
        // content the next time the model quotes it back.
        return Shown::nothing(
            ToolOutcome::new(String::new()).with_display(format!("{shown}: empty file")),
        );
    }

    let total = text.lines().count();
    // Reading past the end is not an error either. The model paging through a
    // file will eventually ask for a window that is not there, and the useful
    // answer is the line count, not a refusal.
    if offset > total {
        return Shown::nothing(ToolOutcome::new(String::new()).with_display(format!(
            "{shown}: offset {offset} is past the last line ({total})"
        )));
    }

    let mut out = String::new();
    let mut clipped_line = false;
    let mut byte_capped = false;
    let mut last = offset;
    let mut hashes = Vec::new();

    for (idx, line) in text.lines().enumerate().skip(offset - 1).take(limit) {
        let number = idx + 1;
        let (body, clipped) = clip(line, MAX_LINE_CHARS);
        clipped_line |= clipped;
        // The label describes the *whole* line even when the body shown is a
        // prefix of it — except that a clipped line gets the marker instead of
        // a hash, so there is never a hash in the output standing for text the
        // model was not shown.
        let tag = if clipped {
            hashline::CLIPPED.to_string()
        } else {
            hashline::short(hashline::hash_line(line))
        };
        let rendered = format!("{number:>6}#{tag}\t{body}\n");
        if out.len() + rendered.len() > MAX_BYTES {
            byte_capped = true;
            break;
        }
        // `last` advances only for a line that was actually emitted, which is
        // why the break happens before the push. The continuation offset
        // reported below is `last + 1`, so a line counted here but dropped by
        // the cap would tell the model to resume one line past what it saw.
        out.push_str(&rendered);
        // Recorded even for a clipped line. The record is the harness's own
        // note of what the file said, used to detect drift under a range; it is
        // the *printed* marker, not a gap in the record, that stops the model
        // addressing a line it only half saw.
        hashes.push(hashline::hash_line(line));
        last = number;
    }

    let lines = LineHashes {
        first: offset,
        hashes,
    };

    let more = last < total;
    let truncated = more || clipped_line || byte_capped;

    if truncated {
        // The note goes in `content`, not only in the flag, because the flag is
        // for the runtime and the model reads the content. Both are set: a
        // caller that renders only one of them still tells the truth.
        let mut why = Vec::new();
        if more {
            why.push(format!(
                "showing lines {offset}-{last} of {total}; continue with offset {}",
                last + 1
            ));
        }
        if byte_capped {
            why.push(format!("hit the {MAX_BYTES}-byte cap"));
        }
        if clipped_line {
            why.push(format!(
                "lines longer than {MAX_LINE_CHARS} characters were clipped"
            ));
        }
        out.push_str(&format!("\n[truncated: {}]\n", why.join("; ")));
        return Shown {
            outcome: ToolOutcome::new(out)
                .with_display(format!(
                    "{shown}: lines {offset}-{last} of {total} (truncated)"
                ))
                .truncated(),
            lines,
        };
    }

    Shown {
        outcome: ToolOutcome::new(out).with_display(format!("{shown}: {total} lines")),
        lines,
    }
}

/// Taken in `char`s, so a multi-byte character is never cut in half. The peek
/// at the next char is what distinguishes a line of exactly `max_chars` from
/// one that was actually clipped — taking and comparing lengths would call the
/// first one truncated and set the flag that stops `Write` from working.
fn clip(line: &str, max_chars: usize) -> (String, bool) {
    let mut chars = line.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_none() {
        (head, false)
    } else {
        (format!("{head} … [line clipped]"), true)
    }
}

// endregion: Rendering, and saying what was cut
