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

use std::path::Path;
use std::sync::Arc;

use emma_tool_api::{Tool, ToolCtx, ToolError, ToolMeta, ToolOutcome};
use serde_json::{json, Value};

use crate::args;
use crate::path;
use crate::session::ReadTracker;

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

        let outcome = render(&root, &file, &text, offset, limit);
        // A truncated read is recorded as a *partial* sighting. Treating "I saw
        // the first 2000 lines" as "I have read this file" is precisely how a
        // subsequent `Write` throws away the other ten thousand.
        // `offset > 1` is a deliberate slice and is partial for the same reason.
        self.tracker
            .record(&ctx.session_id, &file, !outcome.truncated && offset == 1);

        Ok(outcome)
    }
}

fn render(root: &Path, file: &Path, text: &str, offset: usize, limit: usize) -> ToolOutcome {
    let shown = path::display(root, file);
    if text.is_empty() {
        // Content stays genuinely empty rather than gaining a prose stand-in,
        // because anything written here would be indistinguishable from file
        // content the next time the model quotes it back.
        return ToolOutcome::new(String::new()).with_display(format!("{shown}: empty file"));
    }

    let total = text.lines().count();
    if offset > total {
        return ToolOutcome::new(String::new()).with_display(format!(
            "{shown}: offset {offset} is past the last line ({total})"
        ));
    }

    let mut out = String::new();
    let mut clipped_line = false;
    let mut byte_capped = false;
    let mut last = offset;

    for (idx, line) in text.lines().enumerate().skip(offset - 1).take(limit) {
        let number = idx + 1;
        let (body, clipped) = clip(line, MAX_LINE_CHARS);
        clipped_line |= clipped;
        let rendered = format!("{number:>6}\t{body}\n");
        if out.len() + rendered.len() > MAX_BYTES {
            byte_capped = true;
            break;
        }
        out.push_str(&rendered);
        last = number;
    }

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
        return ToolOutcome::new(out)
            .with_display(format!(
                "{shown}: lines {offset}-{last} of {total} (truncated)"
            ))
            .truncated();
    }

    ToolOutcome::new(out).with_display(format!("{shown}: {total} lines"))
}

fn clip(line: &str, max_chars: usize) -> (String, bool) {
    let mut chars = line.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_none() {
        (head, false)
    } else {
        (format!("{head} … [line clipped]"), true)
    }
}
