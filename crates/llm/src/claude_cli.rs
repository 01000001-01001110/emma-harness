//! The `claude` CLI as a provider *name*, and deliberately not as a provider.
//!
//! Selecting `claude` does not put a client behind Emma's loop. It replaces the
//! loop: `main.rs` hands the whole goal to `claude -p` and streams its events
//! back. A `Provider` that drove the CLI turn by turn was the obvious
//! alternative and is not what shipped; `crates/emma/src/engine/claude.rs` is
//! the argument.
//!
//! So why is there a [`ProviderKind`] here at all? Because provider selection is
//! one mechanism and must stay one. `--provider claude`, `emma set-provider
//! claude`, the Settings screen's cycler, the Keys Stored row and the per
//! provider model entry all read the registry, and a second selection surface
//! beside it would be two places a person could set this and two places a bug
//! could hide.
//!
//! [`Refuses`] is the price of that. `ProviderKind::build` has to return
//! something, and the honest something is an object that cannot pretend: every
//! call fails, loudly, naming the branch in `main.rs` that should have caught it
//! before anything got here. It is an assertion wearing a trait, not a
//! half-working client. A `send` that answered prose would be far worse: it
//! would make a mis-wired dispatch look like a working one, which is the exact
//! failure `kind()` refuses an unknown name to avoid.

use std::sync::Arc;

use crate::kind::ProviderKind;
use crate::{ApiKey, Provider, Request};
use crate::{AssistantTurn, LlmError, Mode};

/// The registry name. Referenced by the dispatch branch rather than typed
/// twice, so the string that selects the engine and the string that routes to
/// it cannot drift apart.
pub const NAME: &str = "claude";

/// What `claude --model` is given when nothing else names one.
///
/// An alias rather than a dated id: the CLI resolves `sonnet` to whatever the
/// latest Sonnet is, and pinning a dated id here would silently stop tracking
/// the model the user's own `claude` sessions run on.
pub const DEFAULT_MODEL: &str = "sonnet";

pub struct ClaudeCli;

impl ProviderKind for ClaudeCli {
    fn name(&self) -> &'static str {
        NAME
    }

    /// Not a key, and nothing here reads it. The CLI carries its own
    /// authentication (OAuth, keychain, or `ANTHROPIC_API_KEY` read by the child
    /// process itself), so Emma has no credential to store and does not want
    /// one: a second copy of a key in `~/.emma` would be a second thing to
    /// rotate and a second thing to leak. The variable is named for the row that
    /// has to print something, and it is the one the child would honour.
    fn env_var(&self) -> &'static str {
        "ANTHROPIC_API_KEY"
    }

    fn default_model(&self) -> &'static str {
        DEFAULT_MODEL
    }

    /// Keyless, exactly like Ollama and for a related reason: demanding a
    /// credential Emma never sends would force a fictitious one into
    /// `~/.emma/credentials.json`, where a later reader could not tell it from a
    /// real one. This is what makes the Keys Stored row read `n/a`.
    fn requires_key(&self) -> bool {
        false
    }

    fn build(&self, _key: ApiKey, model: Option<String>) -> Arc<dyn Provider> {
        Arc::new(Refuses {
            model: model.unwrap_or_else(|| DEFAULT_MODEL.to_string()),
        })
    }
}

/// A `Provider` that exists to be unreachable.
///
/// `model_id` answers honestly, because it is asked: the status line, the
/// settings screen and the `engine` session record all want to know which model
/// the next `claude -p` will be given, and that is this string. Everything else
/// refuses.
pub struct Refuses {
    model: String,
}

#[async_trait::async_trait]
impl Provider for Refuses {
    fn model_id(&self) -> &str {
        &self.model
    }

    async fn send(
        &self,
        _request: Request,
        _mode: Mode,
        _events: Option<tokio::sync::mpsc::Sender<crate::Event>>,
    ) -> Result<AssistantTurn, LlmError> {
        Err(LlmError::Protocol(format!(
            "the `{NAME}` provider runs the claude CLI for a whole goal and has no request \
             endpoint. Reaching this means a caller ran Emma's agent loop on it instead of the \
             goal handoff in main.rs. Nothing was sent and nothing was billed."
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The refusal is the whole point, so it is the thing that gets a test: a
    /// `send` that ever succeeded would mean Emma's loop had quietly become the
    /// thing driving this engine.
    #[tokio::test]
    async fn every_request_is_refused_and_says_where_the_dispatch_should_have_gone() {
        let p = ClaudeCli.build(ApiKey::new(""), None);
        assert_eq!(p.model_id(), DEFAULT_MODEL);
        // Matched rather than unwrapped: `AssistantTurn` has no `Debug`, so
        // `unwrap_err` will not compile here.
        let err = match p
            .send(Request::new("", Vec::new()), Mode::Batch, None)
            .await
        {
            Ok(_) => panic!("a request to the claude CLI kind must never succeed"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("main.rs"), "{err}");
        assert!(err.contains("nothing was billed"), "{err}");
    }

    /// The two rows the Settings screen reads off the kind without building
    /// anything. `requires_key` false is what prints `n/a`.
    #[test]
    fn the_kind_is_keyless_and_named_claude() {
        assert_eq!(ClaudeCli.name(), "claude");
        assert!(!ClaudeCli.requires_key());
        assert!(crate::kind::known().contains(&"claude"));
        assert_eq!(crate::kind::kind("Claude").unwrap().name(), "claude");
    }
}
