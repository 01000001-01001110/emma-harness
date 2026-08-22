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
//!
//! # Two ways to say where, and why it is one tool
//!
//! `Edit` takes either `old_string` — quote the text, it must be unique — or
//! `lines`, an address of the form `12#a3f9`: the line number, and the four
//! characters `Read` printed beside it, which are a hash of what that line said.
//!
//! **Why not a second tool.** A `EditLines` alongside `Edit` would cost its
//! whole description in every request, on every turn, forever, and would hand
//! the model a choice it has no good basis for making — the two are not for
//! different jobs, they are two spellings of the same job. It would also break
//! the one compatibility promise this crate makes: the six tool names are
//! Claude Code's exactly, so a hook matcher or an allow-list written for one
//! works for the other, and a seventh name is outside that set. **Why not
//! replace `old_string`.** Every model that has ever been trained on a
//! Claude-Code-shaped tool surface already knows how to drive `old_string`, and
//! the addressing scheme here can only be driven by a model that has seen a
//! `Read` from *this* harness in *this* conversation. Removing the familiar form
//! would break the first edit of every session and every edit made from context
//! the model was handed rather than read. So: one tool, two forms, mutually
//! exclusive, and a call that supplies both or neither is refused rather than
//! resolved by precedence — precedence would mean silently ignoring one of two
//! things the model asked for.
//!
//! # What the hash actually buys, given the tracker already exists
//!
//! It would be easy to call this a second copy of the read-before-write rule.
//! It is not, and the difference is the whole point.
//!
//! The tracker's stamp answers *did the file change*. Before this, that answer
//! was fatal: `Stale` meant refuse, and the model's only recovery was to `Read`
//! the whole file again and re-derive its edit — the expensive path, paid in
//! full even when the drift was a hundred lines away from the change. With a
//! per-line record, a stale file is no longer automatically a refusal. The
//! addressed lines are checked one by one against what the harness recorded when
//! it showed them; if the drift missed them, the edit goes ahead, and if it hit
//! them the refusal names *which line* and *what it says now*.
//!
//! That is why `lines` mode accepts `Stale` where `old_string` mode still
//! refuses it. It is not a weaker rule — it is a sharper one. `old_string` on a
//! stale file could match text that moved somewhere it does not belong; an
//! address is checked at a position.
//!
//! The model's own hash is a third, separate check, and it catches a failure
//! neither of the other two can see: the model aiming at a line number it worked
//! out several turns ago, from a `Read` that has since been superseded by
//! another. The tracker knows what the world last looked like. It has no idea
//! what the model thinks it looks like. Only the hash the model quotes says
//! that, which is exactly why the model has to quote it rather than the harness
//! looking it up.
//!
//! # Where this design departs from the one it came from
//!
//! `oh-my-pi` (MIT, no rider — checked, because a sibling project in that space
//! carries one that voids all rights for Anthropic) is where the idea comes
//! from, and this is a reimplementation from the idea rather than a port; the
//! two schemes are not the same shape. It tags each *file* with one 4-hex hash
//! and addresses lines by bare number, and it normalises trailing whitespace
//! away before hashing — which means a formatter stripping trailing spaces is
//! invisible to its staleness check, and is an open complaint against it. Emma
//! hashes each line including its trailing whitespace, so that change is caught.
//! The cost of per-line hashes is real and is paid in `Read`; see the note there.
//!
//! The known failure of per-line hashing is the mirror image: hash the endpoints
//! of a range and the lines *between* them can drift unnoticed. That is closed
//! here by the tracker rather than by more hashes in the arguments — the model
//! sends two, and the whole run is matched against the harness's own record. A
//! range costs the same two anchors whether it spans two lines or two hundred.
//!
//! That run is matched *wherever it appears* in the record rather than at the
//! same line numbers, and the distinction is not a detail. It is the difference
//! between asking "did this text change" and asking "did this text move", and
//! only the first is this check's business — moving is what the model's address
//! already handles. Comparing in place looks equivalent right up until something
//! inserts a line above, at which point every number below shifts and a range
//! nobody touched is refused. Worse, it contradicted the relocation refusal
//! below, which tells the model "your line is now line 7, retry there": the
//! retry hit the in-place check and was refused for following the advice. That
//! was found by watching a real model do exactly that, not by review.
//!
//! One more thing carried over deliberately: a successful addressed edit reports
//! the lines it wrote, **with their new numbers and new hashes**. Every edit
//! shifts the numbering of everything below it, and a model working from the
//! numbers it read before the edit will aim its next one wrong — a cascade
//! reported against `oh-my-pi` and closed there as not planned. Handing back the
//! new labels costs a few lines of output and removes the reason to re-read.

use std::path::Path;
use std::sync::Arc;

use emma_tool_api::{Tool, ToolCtx, ToolError, ToolMeta, ToolOutcome};
use serde_json::{json, Value};

use crate::args;
use crate::hashline::{self, Address};
use crate::path;
use crate::read::MAX_LINE_CHARS;
use crate::session::{LineHashes, ReadState, ReadTracker};

// region: The tool surface
// ---------------------------------------------------------------------------
// The tool surface
//
// The schema, the honest `idempotent: false`, and the refusals decidable from
// the arguments alone — an empty anchor, an anchor identical to its
// replacement, and the mode confusions. All are caught here so none needs a
// file to exist.
// ---------------------------------------------------------------------------

const NAME: &str = "Edit";
const KEYS: &[&str] = &[
    "file_path",
    "old_string",
    "new_string",
    "replace_all",
    "lines",
];

/// How many replacement lines a successful addressed edit will label back to
/// the model before it summarises instead.
///
/// The labels exist so the next edit does not need a re-read, and past a couple
/// of dozen lines that stops being true anyway — a model that just rewrote
/// forty lines is going to want to look at them in context. Echoing an entire
/// large replacement back would also double the cost of the very call this
/// scheme exists to make cheaper.
const ECHO_LIMIT: usize = 20;

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
                    "description": "Exact text to replace, including indentation. Must occur exactly once unless replace_all is set. Give this or lines, not both."
                },
                "lines": {
                    "type": "string",
                    "description": "The lines to replace, addressed as printed by Read: \"12#a3f9\" for one line, or \"12#a3f9-15#b7c1\" for an inclusive range. Give this or old_string, not both."
                },
                "new_string": {
                    "type": "string",
                    "description": "Text to put in its place. With lines, this is the replacement lines; empty deletes them."
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Replace every occurrence instead of requiring exactly one. Default false. Only valid with old_string."
                }
            },
            "required": ["file_path", "new_string"],
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
        let new = args::req_str(args_v, NAME, "new_string")?;
        let old = args::opt_str(args_v, NAME, "old_string")?;
        let lines = args::opt_str(args_v, NAME, "lines")?;
        let replace_all = args::opt_bool(args_v, NAME, "replace_all")?;

        // Cheap and synchronous, so all of it belongs here rather than in
        // `invoke`. Nothing below needs the file to exist, and a test proves it
        // by pointing these at a path that does not.
        match (old, lines) {
            // Refused rather than resolved by precedence. Picking one silently
            // means ignoring something the model asked for, and the model would
            // see a successful edit in a place it did not expect.
            (Some(_), Some(_)) => Err(ToolError::BadArguments(
                "Edit takes old_string or lines, not both; they are two ways of \
                 saying where, and supplying both does not say where twice"
                    .into(),
            )),
            (None, None) => Err(ToolError::BadArguments(
                "Edit needs somewhere to change: old_string with the exact text \
                 to replace, or lines with an address from Read such as 12#a3f9"
                    .into(),
            )),
            (Some(old), None) => {
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
            (None, Some(lines)) => {
                // `replace_all` has no meaning against an address — an address
                // is already exactly one place. Accepting and ignoring it would
                // teach the model the flag does nothing, which is the lesson it
                // must never learn about the `old_string` form.
                if replace_all.is_some() {
                    return Err(ToolError::BadArguments(
                        "Edit.replace_all applies to old_string only; a lines address \
                         already names exactly one place"
                            .into(),
                    ));
                }
                hashline::parse(lines)
                    .map(|_| ())
                    .map_err(|why| ToolError::BadArguments(format!("Edit.lines {why}")))
            }
        }
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

// region: Deciding where, then writing
// ---------------------------------------------------------------------------
// Deciding where, then writing
//
// `run` is a two-line dispatch and the two forms below share nothing but their
// ending. That is deliberate: the checks each form needs are genuinely
// different, and a merged function would be a chain of `if addressed` that made
// both harder to read than either.
//
// What they do share is the order of refusals — read state, then the file, then
// where — and the rule that nothing touches the disk until every refusal has
// had its chance.
// ---------------------------------------------------------------------------

impl Edit {
    fn run(&self, ctx: &ToolCtx, args_v: Value) -> Result<ToolOutcome, ToolError> {
        self.validate_args(&args_v)?;
        let root = path::root(ctx)?;
        let raw = args::req_str(&args_v, NAME, "file_path")?;
        let new = args::req_str(&args_v, NAME, "new_string")?;

        let (file, meta) = path::resolve_existing(&root, raw)?;
        if meta.is_dir() {
            return Err(ToolError::BadArguments(format!("{raw} is a directory")));
        }

        match args::opt_str(&args_v, NAME, "lines")? {
            Some(address) => {
                // Parsed twice — once in `validate_args` so a syntax error
                // needs no file to exist, once here. The alternative is
                // threading the parsed value out of a validator whose signature
                // the `Tool` trait fixes, and parsing a fifteen-byte string
                // twice is cheaper than that contortion.
                let address = hashline::parse(address)
                    .map_err(|why| ToolError::BadArguments(format!("Edit.lines {why}")))?;
                self.by_address(ctx, &root, &file, raw, address, new)
            }
            None => {
                let old = args::req_str(&args_v, NAME, "old_string")?;
                let replace_all = args::opt_bool(&args_v, NAME, "replace_all")?.unwrap_or(false);
                self.by_anchor(ctx, &root, &file, raw, old, new, replace_all)
            }
        }
    }
}

// endregion: Deciding where, then writing

// region: The literal anchor
// ---------------------------------------------------------------------------
// The literal anchor
//
// Unchanged behaviour, moved into its own function. Every refusal here was
// learned from a way of editing the wrong thing, and none of them is loosened
// by the addressed form existing alongside it. Two of the messages now name
// that form as an alternative, which is the only difference.
// ---------------------------------------------------------------------------

impl Edit {
    #[allow(clippy::too_many_arguments)]
    fn by_anchor(
        &self,
        ctx: &ToolCtx,
        root: &Path,
        file: &Path,
        raw: &str,
        old: &str,
        new: &str,
        replace_all: bool,
    ) -> Result<ToolOutcome, ToolError> {
        // Same rule as Write, for the same reason: an anchor that happened to
        // match inside a file nobody looked at is a coincidence, not a
        // location. Edit is safer than Write — it is anchored — so a *partial*
        // read is accepted here, since the anchor came from what was shown.
        let prior = self.tracker.state(&ctx.session_id, file);
        match prior {
            ReadState::Fresh | ReadState::Partial => {}
            ReadState::Never => {
                return Err(ToolError::BadArguments(format!(
                    "{raw} has not been read in this session; Read it first"
                )))
            }
            // Still fatal for this form, and the message now names the cheaper
            // way out. A literal anchor on a drifted file can match text that
            // moved somewhere it does not belong, and nothing about the match
            // would reveal it; an address is checked at a position, so it can
            // survive drift that happened elsewhere in the file.
            ReadState::Stale => {
                return Err(ToolError::BadArguments(format!(
                    "{raw} changed on disk after you read it; Read it again before \
                     editing, or address the lines you want with Edit.lines, which \
                     checks each one against what it said when you read it"
                )))
            }
        }

        let before = std::fs::read_to_string(file)
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
                     and Read's line labels are not part of the file"
                )))
            }
            // The named edge. Reporting the count is what makes the failure
            // actionable: the model knows whether to extend the anchor or to
            // say it meant all of them.
            (n, false) if n > 1 => {
                return Err(ToolError::BadArguments(format!(
                    "old_string occurs {n} times in {raw}; Edit will not choose \
                     between them. Extend old_string with surrounding lines until \
                     it is unique, pass replace_all: true to change all {n}, or \
                     address the one you mean with Edit.lines"
                )))
            }
            _ => {}
        }

        let after = if replace_all {
            before.replace(old, new)
        } else {
            before.replacen(old, new, 1)
        };

        path::write_atomically(file, after.as_bytes())
            .map_err(|e| ToolError::Failed(format!("{raw} could not be written: {e}")))?;
        // Re-stamped so the edit does not read as an outside change, but the
        // completeness carries over: editing one anchor inside a file seen only
        // in part does not mean the rest has now been seen. The line hashes are
        // for the file as it is *now*, because the numbering below the edit has
        // just moved and the old record describes a file that no longer exists.
        self.tracker.record(
            &ctx.session_id,
            file,
            prior == ReadState::Fresh,
            LineHashes::of_text(&after),
        );

        let shown = path::display(root, file);
        let what = if hits == 1 {
            "1 replacement".to_string()
        } else {
            format!("{hits} replacements")
        };
        Ok(ToolOutcome::new(format!("{shown}: {what}")).with_display(format!("{shown}: {what}")))
    }
}

// endregion: The literal anchor

// region: The addressed edit
// ---------------------------------------------------------------------------
// The addressed edit
//
// Three checks stand between the address and the disk, and they are three
// because they catch three different lies:
//
//   1. the endpoints the model named still say what the model says they say —
//      catches a model aiming with a line number it worked out turns ago;
//   2. every line in the range still says what the harness recorded when it
//      showed it — catches the file drifting under a range whose endpoints
//      happen to be untouched;
//   3. no line in the range is longer than `Read` will show — catches
//      replacing a line whose end nobody has seen.
//
// Each one refuses with the line number, what was expected, and what is there
// instead, because a refusal the model cannot act on just becomes a re-read,
// and avoiding the re-read is the entire reason this form exists.
// ---------------------------------------------------------------------------

impl Edit {
    fn by_address(
        &self,
        ctx: &ToolCtx,
        root: &Path,
        file: &Path,
        raw: &str,
        address: Address,
        new: &str,
    ) -> Result<ToolOutcome, ToolError> {
        let prior = self.tracker.state(&ctx.session_id, file);
        if prior == ReadState::Never {
            return Err(ToolError::BadArguments(format!(
                "{raw} has not been read in this session; Read it first — the \
                 line#hash labels in its output are what Edit.lines addresses"
            )));
        }
        // `Stale` is deliberately *not* refused here. See the module doc: the
        // per-line checks below are a sharper form of the same question, and
        // turning a whole-file re-read into a per-line verification is the
        // point of the scheme. `Partial` is fine for a related reason — a line
        // outside the window that was read has no recorded hash at all, so
        // check 2 refuses it without needing a file-level rule.

        let before = std::fs::read_to_string(file)
            .map_err(|e| ToolError::Failed(format!("{raw} could not be read for editing: {e}")))?;
        // `split_inclusive` keeps each line's own terminator attached, which is
        // what lets an edit preserve CRLF, preserve a missing newline at end of
        // file, and rebuild everything it did not touch byte for byte.
        // Splitting on '\n' and re-joining would quietly convert a CRLF file to
        // LF on every edit — a whole-file rewrite dressed up as a one-line
        // change, and one that would make the diff view useless.
        let raw_lines: Vec<&str> = before.split_inclusive('\n').collect();
        let total = raw_lines.len();

        let (first, last) = (address.first(), address.last());
        if last > total {
            return Err(ToolError::BadArguments(format!(
                "Edit.lines addresses line {last} but {raw} has {total} lines; \
                 it is shorter than it was when you read it — Read it again"
            )));
        }

        // Check 1: the endpoints say what the model says they say.
        for anchor in address.ends() {
            let content = body(raw_lines[anchor.line - 1]);
            if !anchor.matches(content) {
                return Err(ToolError::BadArguments(moved(
                    raw, anchor, content, &raw_lines,
                )));
            }
        }

        // Check 2: the whole run — endpoints and everything between them — is
        // text this harness has actually shown. The model sent two hashes; this
        // is the other ninety-eight, and it is what closes the hole that a range
        // can drift in the middle while both its ends look untouched.
        //
        // Matched as a *run appearing anywhere* in the record rather than
        // position by position. See `ReadTracker::contains_run` for why: a line
        // inserted above shifts every number below it, and an in-place
        // comparison would refuse a range nobody had touched — including, in
        // particular, the corrected retry that the relocation refusal above
        // tells the model to make.
        let run: Vec<u64> = (first..=last)
            .map(|n| hashline::hash_line(body(raw_lines[n - 1])))
            .collect();
        if !self.tracker.contains_run(&ctx.session_id, file, &run) {
            // Two different problems land in this arm and they need different
            // messages, so the record is consulted a second time to tell them
            // apart: a *gap* means the line was never shown and the model
            // should read it, while a mismatch with no gap means it was shown
            // and has changed since. Only reached on the failure path, so the
            // second lookup costs nothing that matters.
            let recorded = self.tracker.line_hashes(&ctx.session_id, file, first, last);
            let unseen = recorded
                .iter()
                .position(Option::is_none)
                .map(|offset| first + offset);
            return Err(match unseen {
                Some(number) => ToolError::BadArguments(format!(
                    "Edit.lines covers line {number} of {raw}, which is outside the part \
                     of the file you have read; Read lines {first}-{last} before \
                     replacing them"
                )),
                None => {
                    let changed = recorded
                        .iter()
                        .enumerate()
                        .find(|(offset, expected)| **expected != Some(run[*offset]))
                        .map(|(offset, _)| first + offset)
                        .unwrap_or(first);
                    let content = body(raw_lines[changed - 1]);
                    ToolError::BadArguments(format!(
                        "line {changed} of {raw} changed on disk after you read it, and \
                         it is inside the range {first}-{last} you asked to replace. It \
                         now reads {content:?}. Read lines {first}-{last} again before \
                         replacing them"
                    ))
                }
            });
        }

        // Check 3: nothing in the range was too long to have been shown whole.
        // Measured against the file rather than remembered from the read,
        // because it is a property of the line and not of the sighting — and a
        // check that cannot go stale is one fewer thing to keep in step.
        for number in first..=last {
            let content = body(raw_lines[number - 1]);
            if content.chars().count() > MAX_LINE_CHARS {
                return Err(ToolError::BadArguments(format!(
                    "line {number} of {raw} is longer than the {MAX_LINE_CHARS} \
                     characters Read shows, so you have not seen the end of it; \
                     replacing it would discard text you never read. Use old_string \
                     to change the part you did see"
                )));
            }
        }

        // The terminator to give any replacement line: whatever the last line
        // being replaced used, so a CRLF file stays CRLF and a file with no
        // newline at the end keeps not having one.
        let ending = terminator(raw_lines[last - 1]);
        let replacement: Vec<String> = if new.is_empty() {
            // An empty replacement deletes the lines rather than leaving a
            // blank one behind. Both readings are defensible; this one is
            // chosen because "replace these lines with nothing" almost always
            // means the lines should go, and because the result says "deleted"
            // in so many words, so a model that meant the other thing finds out
            // immediately rather than from a diff three turns later.
            Vec::new()
        } else {
            new.split('\n')
                .map(|line| line.trim_end_matches('\r').to_string())
                .collect()
        };

        let mut after = String::with_capacity(before.len() + new.len());
        for line in &raw_lines[..first - 1] {
            after.push_str(line);
        }
        for line in &replacement {
            after.push_str(line);
            after.push_str(ending);
        }
        for line in &raw_lines[last..] {
            after.push_str(line);
        }

        if after == before {
            // Not a silent success. A no-op means the model believes it changed
            // something it did not, and everything it does next is built on
            // that belief. `old_string` mode refuses the same thing in
            // `validate_args`; here it can only be known after the file is
            // read, which is why the check sits down here instead.
            return Err(ToolError::BadArguments(format!(
                "the replacement for lines {first}-{last} of {raw} is byte for byte \
                 what is already there; nothing would change"
            )));
        }

        path::write_atomically(file, after.as_bytes())
            .map_err(|e| ToolError::Failed(format!("{raw} could not be written: {e}")))?;

        // Completeness carries over exactly as it does for an anchored edit,
        // with one consequence worth naming: an edit that went through against
        // a `Stale` file does not restore a full sighting, because the drift it
        // stepped around is still there and still unseen. Only a file that was
        // `Fresh` stays `Fresh`, so a later whole-file `Write` is still refused.
        self.tracker.record(
            &ctx.session_id,
            file,
            prior == ReadState::Fresh,
            LineHashes::of_text(&after),
        );

        Ok(report(
            &path::display(root, file),
            first,
            last,
            &replacement,
            ending,
        ))
    }
}

/// A line's content, without whatever terminated it. `\r\n` and `\n` both go,
/// so the text hashed here is the same text `Read` rendered — `str::lines`
/// strips the `\r` too, and a hash computed over a different string than the
/// one the model was shown is a hash that refuses correct edits.
fn body(raw: &str) -> &str {
    raw.trim_end_matches('\n').trim_end_matches('\r')
}

/// Whatever terminated a line; `""` for the last line of a file that ends
/// without a newline.
fn terminator(raw: &str) -> &str {
    if raw.ends_with("\r\n") {
        "\r\n"
    } else if raw.ends_with('\n') {
        "\n"
    } else {
        ""
    }
}

/// The refusal for check 1, which is the one this whole scheme exists to
/// produce, so it is the one worth spending output on.
///
/// It names the line, repeats the address the model gave, quotes what is
/// actually there now with its real hash — and then does the search the model
/// would otherwise pay a whole `Read` for: if exactly one other line in the file
/// still hashes to what the model was aiming at, that is almost certainly where
/// the line went, and saying so turns a re-read into a corrected retry.
///
/// **It reports; it does not relocate.** Applying the edit at the line the
/// search found would be the "plausible and wrong" move this crate refuses
/// everywhere else: a line that moved may have moved because the code around it
/// changed, and whether the edit still belongs there is the model's call, not a
/// hash comparison's. Sixteen bits is ample evidence for a hint and nowhere near
/// enough to write a file on.
///
/// **The wording is load-bearing and was rewritten after watching a real model
/// read the first draft.** That draft opened with "Edit.lines addressed 5#73b2,
/// but line 5 now reads …" and closed with "…or Read the file again if that is
/// not it". Sonnet 4.5 replied *"I see — I made an error. Let me re-read the
/// file"* and spent the whole `Read` the hint existed to save. Two separate
/// faults, both in the prose rather than the logic:
///
/// - **It did not say the file changed.** Stating only that line 5 says
///   something else reads as *you counted wrong*, and the fix for counting wrong
///   is to look again. Naming the cause first — the file moved under you —
///   makes the corrected address believable.
/// - **It offered the re-read as a coequal option.** A model choosing between
///   "retry" and "read again" under uncertainty will read again, every time.
///   The re-read is still mentioned, because sometimes it is genuinely needed,
///   but as the narrower case and not as an alternative of equal standing.
///
/// The reword worked as far as it can: the model stopped calling it its own
/// error and started acting on the corrected address. **What it did not do is
/// stop the re-read.** In both observed runs Sonnet 4.5 read the file again
/// anyway before retrying — understanding the cause, and choosing to look. That
/// is a reasonable thing for a model to do and it is not something a tool
/// message can be tuned into preventing, so the claim here is deliberately the
/// smaller one: the refusal is *actionable*, the corrected address it offers is
/// one this tool accepts (see `a_line_that_only_moved_can_be_edited_at_its_new_number`),
/// and whether a given model takes the shortcut is the model's business. Nobody
/// should quote a token saving from this paragraph.
fn moved(raw: &str, anchor: &hashline::Anchor, content: &str, lines: &[&str]) -> String {
    let elsewhere: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(idx, line)| idx + 1 != anchor.line && anchor.matches(body(line)))
        .map(|(idx, _)| idx + 1)
        .collect();

    let line = anchor.line;
    let now = hashline::short(hashline::hash_line(content));
    // The cause, first and in plain words. You did not miscount; the file
    // changed. Everything after this sentence is only useful if the model
    // believes that one.
    let head = format!(
        "{raw} changed on disk after you read it, so its line numbers have moved. \
         You addressed {anchor}, but line {line} now holds {content:?} ({line}#{now})"
    );
    match elsewhere.as_slice() {
        [] => format!(
            "{head}, and no line in the file matches {anchor} any more, so that text is \
             genuinely gone rather than moved. Read {raw} again to see what replaced it"
        ),
        // The one case with a cheap fix, so it is stated as an instruction and
        // not as one of two options. See this function's doc for what happened
        // when it was phrased as a choice.
        [only] => format!(
            "{head}. The line you meant is intact and is now line {only}: retry with \
             lines \"{only}#{}\". You only need to Read {raw} again if you also need \
             to see what else changed around it",
            anchor.hash
        ),
        many => format!(
            "{head}, and {} lines now match #{} ({}), so which one you meant is not \
             decidable here. Read {raw} again",
            many.len(),
            anchor.hash,
            many.iter()
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// What a successful addressed edit says.
///
/// The labels on the replacement lines are the part that earns its keep: after
/// this edit every line number below `first` has moved, and a model that makes
/// its next edit from the numbers in its earlier `Read` will aim it at the wrong
/// place. Handing back the new numbers *and* the new hashes means the next edit
/// needs no `Read` at all — the same saving the scheme makes on the first edit,
/// extended to the second. The shift is stated in words as well, because it is
/// what the model needs for the lines it is *not* being handed.
fn report(
    shown: &str,
    first: usize,
    last: usize,
    replacement: &[String],
    ending: &str,
) -> ToolOutcome {
    let span = if first == last {
        format!("line {first}")
    } else {
        format!("lines {first}-{last}")
    };

    if replacement.is_empty() {
        let gone = last - first + 1;
        let summary = format!("{shown}: deleted {span}");
        return ToolOutcome::new(format!(
            "{summary} ({gone} line{}); every line after moved up by {gone}",
            if gone == 1 { "" } else { "s" }
        ))
        .with_display(summary);
    }

    let shift = replacement.len() as isize - (last - first + 1) as isize;
    let summary = format!(
        "{shown}: replaced {span} with {} line{}",
        replacement.len(),
        if replacement.len() == 1 { "" } else { "s" }
    );

    let mut content = summary.clone();
    if replacement.len() <= ECHO_LIMIT {
        content.push_str(", now:\n");
        for (offset, line) in replacement.iter().enumerate() {
            let number = first + offset;
            let tag = hashline::short(hashline::hash_line(line));
            content.push_str(&format!("{number:>6}#{tag}\t{line}\n"));
        }
    } else {
        content.push_str(&format!(
            "; more than {ECHO_LIMIT} lines, so Read {first}-{} for their labels\n",
            first + replacement.len() - 1
        ));
    }
    match shift {
        0 => content.push_str("line numbers below are unchanged"),
        n if n > 0 => content.push_str(&format!("every line after moved down by {n}")),
        n => content.push_str(&format!("every line after moved up by {}", -n)),
    }
    // Trailing off the end of the file without a newline is a real hazard: a
    // model that assumes one is there will build its next edit wrong, and
    // nothing else in the output would reveal it.
    if ending.is_empty() {
        content.push_str("; the file still has no newline at the end");
    }

    ToolOutcome::new(content).with_display(summary)
}

// endregion: The addressed edit
