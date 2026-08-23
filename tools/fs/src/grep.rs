//! `Grep` — no hits is a successful search.
//!
//! "That symbol does not appear anywhere in this repository" is one of the most
//! useful things a coding agent can learn, and it is the *result* of a search
//! that worked. Reporting it as a failure would tell the model the search
//! machinery was broken and invite it to try again differently, when the honest
//! answer was already in hand. Only the machinery failing — an unreadable
//! directory, a pattern that will not compile — is a `ToolError`.
//!
//! That distinction is why this tool exists alongside `Bash`. `rg pattern .`
//! run through a shell answers "no matches" with **exit code 1**, and the
//! difference between "no matches" and "ripgrep is not installed" is then a
//! matter of parsing stderr. Here the two are structurally different outcomes.
//!
//! **Output modes exist because the useful answer is rarely the whole answer.**
//! A count, a file list, and matching lines are three different questions.
//! "Where is this defined" and "how widely is this used" both want a shape of
//! answer that fits in a line or two; forcing them through "every matching
//! line" spends context on text the model was not asking about. `head_limit`
//! bounds the rest, and hitting it is reported rather than silently applied —
//! the same rule as `Read`, for the same reason.
//!
//! Like `Glob`, the walk does not follow symlinks: an out-of-root link would
//! otherwise turn a search into an exfiltration path without a `..` in sight.

use std::path::Path;

use emma_tool_api::{Tool, ToolCtx, ToolError, ToolMeta, ToolOutcome};
use regex::RegexBuilder;
use serde_json::{json, Value};

use crate::args;
use crate::glob::compile as compile_glob;
use crate::path;
use crate::walk;

// region: The tool surface
// ---------------------------------------------------------------------------
// The tool surface
//
// The caps, the schema, and the checks that need no filesystem — a regex or a
// glob that will not compile, and a mode that is not one of the three.
// ---------------------------------------------------------------------------

const NAME: &str = "Grep";
const KEYS: &[&str] = &[
    "pattern",
    "path",
    "glob",
    "case_insensitive",
    "output_mode",
    "head_limit",
];

pub const MAX_OUTPUT_LINES: usize = 500;
/// Files larger than this are skipped. A 200 MB log is not what "search the
/// project" meant, and reading it costs the whole call's latency.
pub const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// How much of one matching line is shown. Deliberately shorter than `Read`'s
/// 2000: a grep hit is an address, and a minified bundle producing four
/// thousand characters of one line is not what "show me the matches" meant.
pub const MAX_MATCH_CHARS: usize = 400;

#[derive(Default)]
pub struct Grep;

impl Grep {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl Tool for Grep {
    fn name(&self) -> &'static str {
        NAME
    }

    fn description(&self) -> &str {
        include_str!("descriptions/grep.md")
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Regular expression, matched per line." },
                "path": { "type": "string", "description": "File or directory to search. Defaults to the working directory." },
                "glob": { "type": "string", "description": "Only search files whose relative path matches this glob." },
                "case_insensitive": { "type": "boolean", "description": "Default false." },
                "output_mode": {
                    "type": "string",
                    "enum": ["content", "files_with_matches", "count"],
                    "description": "Default content."
                },
                // "Capped at 500" read as though 500 were a value to pass, and
                // the schema is where the model learns what an argument can do.
                // The `WebFetch` defect was exactly this: the one knob the model
                // had been told about was the one that would not have helped.
                "head_limit": { "type": "integer", "minimum": 1, "description": format!("Lowers the number of returned lines or paths. The ceiling is {MAX_OUTPUT_LINES} and this cannot raise it.") }
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
        if let Some(g) = args::opt_str(args_v, NAME, "glob")? {
            compile_glob(g)?;
        }
        args::opt_bool(args_v, NAME, "case_insensitive")?;
        if let Some(mode) = args::opt_str(args_v, NAME, "output_mode")? {
            Mode::parse(mode)?;
        }
        if let Some(0) = args::opt_u64(args_v, NAME, "head_limit")? {
            return Err(ToolError::BadArguments(
                "Grep.head_limit must be at least 1".into(),
            ));
        }
        // A pattern that does not compile is a fact about the call, so it is
        // caught here rather than after a walk that was never going to match.
        let insensitive = args::opt_bool(args_v, NAME, "case_insensitive")?.unwrap_or(false);
        build_regex(pattern, insensitive)?;
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

// region: Modes and patterns
// ---------------------------------------------------------------------------
// Modes and patterns
//
// The three questions a search can be asked, parsed from a string rather than
// taken as a boolean pair, so an unrecognised mode is a refusal naming the
// three that exist instead of a silent fallback to the default.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Content,
    Files,
    Count,
}

impl Mode {
    fn parse(s: &str) -> Result<Self, ToolError> {
        match s {
            "content" => Ok(Self::Content),
            "files_with_matches" => Ok(Self::Files),
            "count" => Ok(Self::Count),
            other => Err(ToolError::BadArguments(format!(
                "Grep.output_mode must be content, files_with_matches or count, got {other}"
            ))),
        }
    }
}

fn build_regex(pattern: &str, insensitive: bool) -> Result<regex::Regex, ToolError> {
    RegexBuilder::new(pattern)
        .case_insensitive(insensitive)
        .build()
        .map_err(|e| ToolError::BadArguments(format!("{pattern} is not a valid regex: {e}")))
}

// endregion: Modes and patterns

// region: The search
// ---------------------------------------------------------------------------
// The search
//
// Choose the candidate files, read each one, and count separately what was
// found and what is being shown. The distinction between those two counts is
// what keeps a capped search from reading like a complete one.
// ---------------------------------------------------------------------------

impl Grep {
    fn run(&self, ctx: &ToolCtx, args_v: Value) -> Result<ToolOutcome, ToolError> {
        self.validate_args(&args_v)?;
        let root = path::root(ctx)?;
        let pattern = args::req_str(&args_v, NAME, "pattern")?;
        let insensitive = args::opt_bool(&args_v, NAME, "case_insensitive")?.unwrap_or(false);
        let re = build_regex(pattern, insensitive)?;
        let mode = match args::opt_str(&args_v, NAME, "output_mode")? {
            Some(m) => Mode::parse(m)?,
            None => Mode::Content,
        };
        // Kept as well as `limit`, because the notice below has to say a
        // different thing depending on it: a model that asked for 20 lines and
        // got 20 can raise its own number, and a model that asked for nothing
        // and got 500 has hit a ceiling `head_limit` cannot move.
        let requested_limit = args::opt_u64(&args_v, NAME, "head_limit")?;
        let limit = requested_limit
            .map(|v| (v as usize).min(MAX_OUTPUT_LINES))
            .unwrap_or(MAX_OUTPUT_LINES);
        let filter = match args::opt_str(&args_v, NAME, "glob")? {
            Some(g) => Some(compile_glob(g)?),
            None => None,
        };

        let (base, base_meta) = match args::opt_str(&args_v, NAME, "path")? {
            None => (root.clone(), std::fs::metadata(&root).ok()),
            Some(p) => {
                let (resolved, meta) = path::resolve_existing(&root, p)?;
                (resolved, Some(meta))
            }
        };

        // `path` may name a single file, in which case there is nothing to walk
        // and it is the only candidate. Note the rough edge this leaves: the
        // `glob` filter below matches against the path relative to `base`, and
        // when `base` *is* the file that relative path is empty, so passing
        // `path` and `glob` together for one file matches nothing. Naming one
        // file and then filtering the set of one is a redundant call, which is
        // why it has not bitten, but it is a real asymmetry rather than a rule.
        let single_file = base_meta.map(|m| m.is_file()).unwrap_or(false);
        let (candidates, walk_truncated, walk_unreadable) = if single_file {
            (vec![base.clone()], false, Vec::new())
        } else {
            let walked = walk::files(&base);
            (walked.files, walked.truncated, walked.unreadable)
        };

        let mut candidates: Vec<_> = candidates
            .into_iter()
            .filter(|f| match &filter {
                None => true,
                Some(m) => {
                    let rel = f.strip_prefix(&base).unwrap_or(f);
                    m.is_match(rel.to_string_lossy().replace('\\', "/").as_str())
                }
            })
            .collect();
        // Sorted by path, not by mtime as `Glob` is: a grep result is read as a
        // list of places, and neighbouring files sitting together is worth more
        // than recency. Either way it must be deterministic, for the same
        // cached-prefix reason.
        candidates.sort();

        // `total_hits` and `files_with_hits` count every match in the tree,
        // whatever the mode and whatever `head_limit` allowed through, so the
        // summary can say how much was found rather than how much was shown.
        // `lines` is the shown part; when the two diverge, `capped` is set and
        // the difference is stated. Counting only what was returned is how a
        // capped search comes to look like a complete one.
        let mut lines: Vec<String> = Vec::new();
        let mut files_with_hits = 0usize;
        let mut total_hits = 0usize;
        let mut capped = false;

        // Counted because a file that was never opened cannot be reported as
        // holding no matches. See `readable_text` for why only the size skip is
        // counted and the not-text skip is not.
        let mut skipped_large = 0usize;
        let mut unreadable = 0usize;
        let mut clipped_lines = 0usize;

        for file in &candidates {
            let text = match readable_text(file) {
                Readable::Text(t) => t,
                Readable::TooLarge => {
                    skipped_large += 1;
                    continue;
                }
                Readable::NotText => continue,
                Readable::Unreadable => {
                    unreadable += 1;
                    continue;
                }
            };
            let shown = path::display(&root, file);
            let mut count = 0usize;
            for (idx, line) in text.lines().enumerate() {
                if !re.is_match(line) {
                    continue;
                }
                count += 1;
                total_hits += 1;
                if mode == Mode::Content {
                    if lines.len() < limit {
                        let (body, was_clipped) = clip(line);
                        if was_clipped {
                            clipped_lines += 1;
                        }
                        lines.push(format!("{shown}:{}:{}", idx + 1, body));
                    } else {
                        capped = true;
                    }
                }
            }
            if count == 0 {
                continue;
            }
            files_with_hits += 1;
            match mode {
                Mode::Content => {}
                Mode::Files => {
                    if lines.len() < limit {
                        lines.push(shown);
                    } else {
                        capped = true;
                    }
                }
                Mode::Count => {
                    if lines.len() < limit {
                        lines.push(format!("{shown}:{count}"));
                    } else {
                        capped = true;
                    }
                }
            }
        }

        // Four independent cuts, each its own sentence, joined rather than
        // summarised — see `glob.rs` and the web digest for the argument. The
        // one that matters most here is `capped`, because `head_limit` is a
        // knob the model has and cannot use to get past 500, and a notice that
        // implied otherwise would send it to re-run a search that comes back
        // identical.
        let mut cuts: Vec<String> = Vec::new();
        if capped {
            let ceiling = if requested_limit.is_some() {
                format!(
                    "cut at head_limit={limit}; raising head_limit helps only as far as the fixed \
                     {MAX_OUTPUT_LINES}-line ceiling"
                )
            } else {
                format!(
                    "cut at the fixed {MAX_OUTPUT_LINES}-line ceiling, which `head_limit` can \
                     lower but not raise"
                )
            };
            cuts.push(format!(
                "{} of {total_hits} matches across {files_with_hits} files shown, {ceiling}. \
                 For the rest, narrow `pattern`, `path` or `glob`, or ask \
                 output_mode=count or files_with_matches, which answer how many and where \
                 without spending a line on each match",
                lines.len()
            ));
        }
        if walk_truncated {
            cuts.push(walk::ceiling_notice());
        }
        // A directory the walk could not open is the same claim as an
        // unreadable file, about more files. Both go in `cuts`, which is the
        // list the model is told about.
        if !walk_unreadable.is_empty() {
            cuts.push(walk::unreadable_notice(&walk_unreadable));
        }
        if clipped_lines > 0 {
            cuts.push(format!(
                "{clipped_lines} matching lines longer than {MAX_MATCH_CHARS} characters were \
                 clipped to their first {MAX_MATCH_CHARS}; no argument raises that — Read the \
                 file at the line number shown to see the whole line"
            ));
        }
        if skipped_large > 0 {
            cuts.push(format!(
                "{skipped_large} text files were not opened at all because they exceed the \
                 {MAX_FILE_BYTES}-byte per-file limit, so a match inside them would not appear \
                 here; no argument raises that limit — Read or Bash can reach such a file by name"
            ));
        }
        if unreadable > 0 {
            cuts.push(format!(
                "{unreadable} file(s) could not be opened -- a permission, a lock, or an I/O \
                 fault -- so this search did not look inside them and a match there would not \
                 appear here. This is not the same as finding nothing; Read or Bash can say \
                 which, by name"
            ));
        }
        let truncated = !cuts.is_empty();
        let reason = cuts.join("; also ");

        if lines.is_empty() {
            // The search ran and the pattern was absent. Saying so as an error
            // would tell the model it could not look, and it would waste a turn
            // looking again another way.
            //
            // But "no matches" is the answer a cut damages most. Every sentence
            // above turns it from a fact about the tree into a fact about the
            // part of the tree that was read, so the content carries the reason
            // even here, where there is otherwise nothing to carry it.
            if truncated {
                return Ok(ToolOutcome::new(format!("[truncated: {reason}]"))
                    .with_display(format!(
                        "{pattern}: no matches in {} files (search incomplete)",
                        candidates.len()
                    ))
                    .truncated_because(reason));
            }
            return Ok(ToolOutcome::new(String::new()).with_display(format!(
                "{pattern}: no matches in {} files",
                candidates.len()
            )));
        }

        let mut content = lines.join("\n");
        if truncated {
            content.push_str(&format!("\n[truncated: {reason}]"));
        }

        let outcome = ToolOutcome::new(content).with_display(format!(
            "{pattern}: {total_hits} matches in {files_with_hits} files"
        ));
        Ok(if truncated {
            outcome.truncated_because(reason)
        } else {
            outcome
        })
    }
}

// endregion: The search

// region: What counts as searchable
// ---------------------------------------------------------------------------
// What counts as searchable
//
// Two filters that silently drop content, which is why both are stated here:
// a file too large to be what "search the project" meant, and a file that is
// not text. Neither fails the call.
// ---------------------------------------------------------------------------

/// Why a candidate file did or did not contribute lines.
///
/// The two skips were one `None` until it became clear they are not the same
/// admission. **A file that is not text is a file with no lines** — a JPEG
/// genuinely contains no match, the module doc and `grep.md` both say so, and
/// reporting one would put a truncation warning on every search of a tree that
/// contains an icon. **A large text file is a file nobody looked in**, and it
/// may well be the 20 MB log that holds the answer. Only the second is a gap in
/// the search, so only the second is counted and reported.
enum Readable {
    Text(String),
    TooLarge,
    NotText,
    /// The file could not be opened. **Not the same as `NotText`**, and keeping
    /// them apart is the whole point: `tool-api`'s rule is that "I could not
    /// look" and "I looked and there was nothing" must never render as the same
    /// message, because the model can route around a failure and cannot route
    /// around an answer. Both arms here used to be `.ok()`, so a
    /// permission-denied file silently became "not text" and vanished from the
    /// count -- a file containing the pattern reported as `no matches`.
    Unreadable,
}

fn readable_text(file: &Path) -> Readable {
    let Ok(meta) = std::fs::metadata(file) else {
        return Readable::Unreadable;
    };
    if meta.len() > MAX_FILE_BYTES {
        // Which of the two skips this is cannot be decided on size alone, and
        // the first live run said so loudly: a repo-wide `Grep` here reported
        // **499 files not opened**, every one of them an `.rlib` or a `.pdb`
        // under `target/`. That notice was true and worthless — it made an
        // ordinary search look badly incomplete, which is how a warning stops
        // being read, and the whole point of naming a cut is that naming it
        // means something.
        //
        // So the size cap asks the same question the small-file path asks, on a
        // prefix: is this text? A 20 MB log still says it was skipped, because
        // it might hold the answer. A 30 MB object file says nothing, because
        // it holds no lines either way.
        return if looks_like_text(file) {
            Readable::TooLarge
        } else {
            Readable::NotText
        };
    }
    // Split deliberately. A read that fails is a failure to look; bytes that are
    // not UTF-8 are a real answer about the file.
    match std::fs::read(file) {
        Err(_) => Readable::Unreadable,
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => Readable::Text(text),
            Err(_) => Readable::NotText,
        },
    }
}

/// Text or not, decided from the front of the file.
///
/// Only reached for files too large to read whole, so it must not read them
/// whole — that is the cost the cap exists to avoid. A prefix is enough for the
/// question being asked, which is not "is every byte valid UTF-8" but "could a
/// line in here have matched": an object file fails in its first hundred bytes
/// and a log does not.
///
/// The final character of the prefix is almost certainly cut in half, so an
/// incomplete sequence at the very end is accepted rather than counted against
/// the file — otherwise one multi-byte character straddling the boundary would
/// reclassify a whole log as binary.
fn looks_like_text(file: &Path) -> bool {
    use std::io::Read;

    const PREFIX: usize = 8 * 1024;
    let Ok(mut handle) = std::fs::File::open(file) else {
        return false;
    };
    let mut buf = vec![0u8; PREFIX];
    let Ok(read) = handle.read(&mut buf) else {
        return false;
    };
    buf.truncate(read);
    // A NUL byte is the one cheap tell that is not a UTF-8 question: it is
    // valid UTF-8 and appears in essentially no text file, which is why `grep`
    // itself has used it as the binary test for decades.
    if buf.contains(&0) {
        return false;
    }
    match std::str::from_utf8(&buf) {
        Ok(_) => true,
        Err(e) => e.error_len().is_none(),
    }
}

/// Counted and taken in `char`s rather than bytes, so a multi-byte character
/// cannot be cut in half and produce output that is not valid UTF-8.
///
/// Returns whether it cut, because the marker in the line is for a human
/// skimming and the count it feeds is for the model: a line ending in
/// `[line clipped]` used to be the only trace that a match had been shown in
/// part, and nothing set the flag that says the result is incomplete.
fn clip(line: &str) -> (String, bool) {
    if line.chars().count() <= MAX_MATCH_CHARS {
        return (line.to_string(), false);
    }
    let head: String = line.chars().take(MAX_MATCH_CHARS).collect();
    (format!("{head} … [line clipped]"), true)
}

// endregion: What counts as searchable
