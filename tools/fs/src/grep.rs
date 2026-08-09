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
//! A count, a file list, or matching lines are three different questions, and
//! forcing every one of them through "all matching lines" wastes context on the
//! two occasions in three where the model wanted to know *where* to look next
//! rather than what the lines said. `head_limit` bounds the rest, and hitting
//! it is reported rather than silently applied — the same rule as `Read`, for
//! the same reason.
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
                "head_limit": { "type": "integer", "minimum": 1, "description": format!("Cap on returned lines. Capped at {MAX_OUTPUT_LINES}.") }
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
        let limit = args::opt_u64(&args_v, NAME, "head_limit")?
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

        let single_file = base_meta.map(|m| m.is_file()).unwrap_or(false);
        let (candidates, walk_truncated) = if single_file {
            (vec![base.clone()], false)
        } else {
            let walked = walk::files(&base);
            (walked.files, walked.truncated)
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
        candidates.sort();

        let mut lines: Vec<String> = Vec::new();
        let mut files_with_hits = 0usize;
        let mut total_hits = 0usize;
        let mut capped = false;

        for file in &candidates {
            let Some(text) = readable_text(file) else {
                continue;
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
                        lines.push(format!("{shown}:{}:{}", idx + 1, clip(line)));
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

        if lines.is_empty() {
            // The search ran and the pattern was absent. Saying so as an error
            // would tell the model it could not look, and it would waste a turn
            // looking again another way.
            let mut outcome = ToolOutcome::new(String::new()).with_display(format!(
                "{pattern}: no matches in {} files",
                candidates.len()
            ));
            if walk_truncated {
                outcome = outcome.truncated();
            }
            return Ok(outcome);
        }

        let mut content = lines.join("\n");
        let truncated = capped || walk_truncated;
        if truncated {
            content.push_str(&format!(
                "\n[truncated: {} of {total_hits} matches across {files_with_hits} files{}]",
                lines.len(),
                if walk_truncated {
                    format!("; the {}-entry walk ceiling was reached", walk::MAX_VISITED)
                } else {
                    String::new()
                }
            ));
        }

        let outcome = ToolOutcome::new(content).with_display(format!(
            "{pattern}: {total_hits} matches in {files_with_hits} files"
        ));
        Ok(if truncated {
            outcome.truncated()
        } else {
            outcome
        })
    }
}

/// `None` for anything that is not searchable text. Skipping is right: a binary
/// file is not a failed search, it is a file with no lines, and one JPEG in a
/// tree must not fail the whole call.
fn readable_text(file: &Path) -> Option<String> {
    let meta = std::fs::metadata(file).ok()?;
    if meta.len() > MAX_FILE_BYTES {
        return None;
    }
    std::fs::read(file)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
}

fn clip(line: &str) -> String {
    const MAX: usize = 400;
    if line.chars().count() <= MAX {
        return line.to_string();
    }
    let head: String = line.chars().take(MAX).collect();
    format!("{head} … [line clipped]")
}
