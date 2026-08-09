//! The boundary between the loop and everything it can do.
//!
//! Ported from tustle-agent, where the shape was arrived at by measurement
//! rather than design: five of eight failure classes were ending turns
//! silently, the user saw "Something went wrong on my side", and the model
//! never learned anything had failed. Emma's tool surface is larger and fails
//! more often — a file is missing, a patch does not apply, a command exits
//! non-zero, a directory is not writable — so the lesson applies harder here.
//!
//! **The one rule that governs this file.** A `ToolError` is a fact about the
//! *call*, never about what the world contains. A read of an empty file
//! succeeds and returns nothing. A glob matching zero paths succeeds and
//! returns an empty list. Emptiness is a result; only the machinery failing is
//! an error. Blurring that turns "I could not look" and "I looked and there
//! was nothing" into the same message, and the model cannot route around a
//! failure it has been told is an answer.

use std::sync::Arc;

/// Failures the **model** should see and route around.
///
/// `#[non_exhaustive]` because a new variant must not silently fall into an
/// existing arm at a call site that was written before it existed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ToolError {
    /// The machinery is absent or unconfigured — no key, no binary, no
    /// permission. Phrased so it reads "I cannot do this" and never "this
    /// cannot be done".
    Unavailable(String),
    /// The tool ran and did not succeed: the command exited non-zero, the file
    /// could not be written, the patch did not apply.
    Failed(String),
    /// The arguments were wrong in a way only discoverable at run time.
    ///
    /// Constructible from `invoke` on purpose: `validate_args` runs first and
    /// cannot stat a path, cannot know whether a string occurs exactly once in
    /// a file, and cannot tell whether a directory is writable. That class of
    /// error is only visible once the tool is running.
    BadArguments(String),
}

impl ToolError {
    /// The stable string the model is shown. Kept separate from `Display` so
    /// changing prose never changes the taxonomy the model routes on.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Unavailable(_) => "tool_unavailable",
            Self::Failed(_) => "tool_failed",
            Self::BadArguments(_) => "bad_arguments",
        }
    }

    pub fn detail(&self) -> &str {
        match self {
            Self::Unavailable(s) | Self::Failed(s) | Self::BadArguments(s) => s,
        }
    }
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind(), self.detail())
    }
}

impl std::error::Error for ToolError {}

/// What a successful call produced.
///
/// `content` is what the model sees. `display` is what a human watching the
/// terminal sees, when a full dump would be noise — the diff rather than the
/// whole file, the first lines of output rather than ten thousand. When it is
/// `None` the terminal shows nothing beyond the fact that the tool ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutcome {
    pub content: String,
    pub display: Option<String>,
    /// Set when output was cut to fit a cap. The model must be told, because
    /// silent truncation is indistinguishable from a short answer and it will
    /// reason confidently about the part it never saw.
    pub truncated: bool,
}

impl ToolOutcome {
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            display: None,
            truncated: false,
        }
    }

    pub fn with_display(mut self, display: impl Into<String>) -> Self {
        self.display = Some(display.into());
        self
    }

    pub fn truncated(mut self) -> Self {
        self.truncated = true;
        self
    }
}

/// Facts the runtime needs *before* deciding whether to run a tool.
///
/// In tustle-agent the equivalent struct had a `needs_approval` field that
/// nothing ever read — a declaration pretending to be a mechanism. Here
/// `read_only` is load-bearing: it is what the approval gate consults, and a
/// test asserts that every tool declaring `read_only` genuinely cannot write.
/// If that stops being true, the gate has quietly stopped protecting anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolMeta {
    /// Cannot modify the filesystem, spawn a process, or reach the network.
    pub read_only: bool,
    /// Running it twice with the same arguments has the same effect as once.
    /// Consulted on crash recovery, never for approval.
    pub idempotent: bool,
}

/// What a tool is told about the call it is serving.
pub struct ToolCtx {
    /// Every relative path resolves against this, and nothing may escape it.
    pub cwd: std::path::PathBuf,
    pub session_id: String,
    pub turn_id: String,
}

#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    /// Matches Claude Code's names — `Read`, `Write`, `Edit`, `Glob`, `Grep`,
    /// `Bash` — so a hook matcher or allow-list written for one works for the
    /// other. Better names would cost compatibility and buy nothing.
    fn name(&self) -> &'static str;

    /// Shown to the model verbatim. Held in a file rather than a string
    /// literal so a prose change is reviewable as a diff and hashable as
    /// config.
    fn description(&self) -> &str;

    fn input_schema(&self) -> serde_json::Value;

    fn meta(&self) -> ToolMeta;

    /// Cheap, synchronous, no I/O. Anything requiring the filesystem belongs
    /// in `invoke` and comes back as `BadArguments`.
    fn validate_args(&self, _args: &serde_json::Value) -> Result<(), ToolError> {
        Ok(())
    }

    /// The outer `Result` is for faults that should end the turn. The inner is
    /// for failures the model should see and route around. Almost everything
    /// is the inner one — a tool that ends the turn is claiming the session
    /// cannot continue, which is rare and should feel rare to write.
    async fn invoke(
        &self,
        ctx: &ToolCtx,
        args: serde_json::Value,
    ) -> anyhow::Result<Result<ToolOutcome, ToolError>>;
}

#[derive(Default)]
pub struct Registry {
    tools: Vec<Arc<dyn Tool>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.push(tool);
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.iter().find(|t| t.name() == name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn Tool>> {
        self.tools.iter()
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.tools.iter().map(|t| t.name()).collect()
    }

    /// The wire form sent to the provider.
    pub fn wire_definitions(&self) -> Vec<serde_json::Value> {
        self.tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name(),
                    "description": t.description(),
                    "input_schema": t.input_schema(),
                })
            })
            .collect()
    }

    /// A digest of every byte of the tool surface the model is shown, logged
    /// with each model call so a change to a description is attributable
    /// rather than invisible.
    pub fn schema_hash(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        for def in self.wire_definitions() {
            hasher.update(def.to_string().as_bytes());
        }
        format!("{:x}", hasher.finalize())[..16].to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kind_is_stable_prose_is_not() {
        // The model routes on `kind`. If a future edit makes these strings
        // depend on the message, every prompt that reasons about failure
        // classes silently changes meaning.
        assert_eq!(ToolError::Failed("anything".into()).kind(), "tool_failed");
        assert_eq!(ToolError::Failed(String::new()).kind(), "tool_failed");
    }

    #[test]
    fn every_variant_carries_its_detail() {
        for e in [
            ToolError::Unavailable("u".into()),
            ToolError::Failed("f".into()),
            ToolError::BadArguments("b".into()),
        ] {
            assert!(!e.detail().is_empty(), "{e:?} lost its detail");
        }
    }
}
