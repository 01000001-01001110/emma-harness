//! `Glob` — matching nothing is an answer.

use emma_tool_api::{Tool, ToolCtx, ToolError, ToolMeta, ToolOutcome};
use globset::{Glob as GlobPattern, GlobMatcher};
use serde_json::{json, Value};

use crate::args;
use crate::path;
use crate::walk;

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
