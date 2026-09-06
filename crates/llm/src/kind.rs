//! Which provider, as a value rather than as a construction site.
//!
//! `AnthropicProvider::new` used to be called by name in `main.rs`, twice, and
//! that is fine while there is one provider and a disaster on the day there are
//! two: the name of the provider would live in the binary's wiring, where a
//! setting cannot reach it. So the identity — its name, the environment
//! variable that overrides its stored key, its default model, and how to build
//! one — becomes a trait object looked up from a string.
//!
//! Separate from [`Provider`] on purpose. A `Provider` is configured: it has a
//! key and a model. Everything here is answerable *before* either exists, which
//! is what `emma set-provider` needs — it has a name and nothing else.
//!
//! **An unknown name is an error, never a fallback.** A mis-set provider that
//! quietly ran on Anthropic would look exactly like a correctly-set one until
//! the bill or the output said otherwise, and there is no way to notice in
//! between. [`kind`] returns `Err` and the message names what is supported.

use std::sync::Arc;

use crate::{AnthropicProvider, ApiKey, Provider};

/// The provider assumed when nothing has ever been set. Anthropic because it is
/// the only one that has ever existed here — the same assumption the settings
/// migration makes, written once.
pub const DEFAULT_PROVIDER: &str = "anthropic";

/// Provider identity and capability, separable from a configured instance.
pub trait ProviderKind: Send + Sync {
    fn name(&self) -> &'static str;

    /// The environment variable that outranks the stored key for this provider.
    /// One name per provider, because `ANTHROPIC_API_KEY` cannot mean two
    /// things.
    fn env_var(&self) -> &'static str;

    fn default_model(&self) -> &'static str;

    /// Whether a key must be resolved before [`ProviderKind::build`] is called.
    ///
    /// **Defaulted true, because every provider that reaches a network over the
    /// public internet needs one and forgetting to say so should fail closed.**
    /// A local provider overrides it. Without this the caller resolves a key
    /// unconditionally, which is why `ollama.rs` could be written, tested and
    /// exported while remaining unreachable: registering it made Emma refuse to
    /// start with `no API key for ollama`.
    ///
    /// When this is false the key handed to `build` is a placeholder and must
    /// not be sent anywhere.
    fn requires_key(&self) -> bool {
        true
    }

    /// Whether this provider can search the web for the model on its own side
    /// of the wire, given [`Request::web_search`](crate::Request::web_search).
    ///
    /// Defaulted false: a provider that has not said it can search cannot be
    /// asked to, and the loop uses this to tell the user at startup whether
    /// the setting they hold means anything on the provider they chose. A
    /// setting that silently does nothing is the failure this exists to
    /// prevent.
    fn web_search(&self) -> bool {
        false
    }

    fn build(&self, key: ApiKey, model: Option<String>) -> Arc<dyn Provider>;
}

pub struct Anthropic;

impl ProviderKind for Anthropic {
    fn name(&self) -> &'static str {
        DEFAULT_PROVIDER
    }

    fn env_var(&self) -> &'static str {
        crate::auth::ENV_VAR
    }

    fn default_model(&self) -> &'static str {
        crate::DEFAULT_MODEL
    }

    /// The Messages API sells `web_search` as a server tool on the same key.
    /// See `anthropic::web_search_tool` for the wire shape.
    fn web_search(&self) -> bool {
        true
    }

    fn build(&self, key: ApiKey, model: Option<String>) -> Arc<dyn Provider> {
        Arc::new(AnthropicProvider::new(key, model))
    }
}

/// Every provider this build can actually run. One entry, and the list is the
/// point: it is what an unknown name is measured against and what the error
/// message quotes, so a second provider becomes reachable by appending to it.
static KINDS: &[&'static dyn ProviderKind] = &[
    &Anthropic,
    &crate::ollama::Ollama,
    &crate::openai_compat::OPENROUTER_KIND,
    &crate::openai_compat::OPENAI_KIND,
    &crate::claude_cli::ClaudeCli,
];

#[derive(Debug, thiserror::Error)]
#[error("unknown provider `{name}`. This build supports: {}", known().join(", "))]
pub struct UnknownProvider {
    pub name: String,
}

/// Look a provider up by name.
pub fn kind(name: &str) -> Result<&'static dyn ProviderKind, UnknownProvider> {
    KINDS
        .iter()
        .copied()
        // Case-insensitive because the name is typed by a human and `Anthropic`
        // is not a different provider. Nothing else is normalised: a typo is a
        // typo and gets the list.
        .find(|k| k.name().eq_ignore_ascii_case(name.trim()))
        .ok_or_else(|| UnknownProvider {
            name: name.to_string(),
        })
}

pub fn known() -> Vec<&'static str> {
    KINDS.iter().map(|k| k.name()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **If this breaks:** the startup line says search is on for a provider
    /// that has none, or off for the one that sells it, and the setting reads
    /// as honoured when it is not.
    #[test]
    fn only_the_provider_that_sells_search_says_it_can() {
        assert!(kind("anthropic").unwrap().web_search());
        assert!(!kind("ollama").unwrap().web_search());
    }

    #[test]
    fn a_misspelled_provider_is_refused_and_told_what_exists() {
        // The failure this prevents is the silent one: `emma set-provider
        // antropic` running happily on Anthropic, and nothing anywhere saying
        // the name was wrong.
        // `.err()` rather than `unwrap_err`: a `ProviderKind` is a trait object
        // and deliberately has no `Debug`, which `unwrap_err` would require.
        let err = kind("antropic").err().unwrap().to_string();
        assert!(err.contains("antropic"), "{err}");
        assert!(err.contains("anthropic"), "{err}");
        assert!(
            kind("openai").is_ok(),
            "the OpenAI-compatible provider is registered and did not resolve"
        );
        assert!(
            kind("bedrock").is_err(),
            "a provider nobody implemented resolved"
        );
    }

    #[test]
    fn the_known_provider_resolves_however_it_is_capitalised() {
        for spelling in ["anthropic", "Anthropic", " ANTHROPIC "] {
            assert_eq!(kind(spelling).unwrap().name(), "anthropic", "{spelling}");
        }
        assert_eq!(kind("anthropic").unwrap().env_var(), "ANTHROPIC_API_KEY");
        assert_eq!(
            kind("anthropic").unwrap().default_model(),
            crate::DEFAULT_MODEL
        );
    }

    #[test]
    fn a_built_provider_carries_the_model_it_was_given() {
        let k = kind("anthropic").unwrap();
        let p = k.build(ApiKey::new("sk-ant-x"), Some("claude-haiku-4-5".into()));
        assert_eq!(p.model_id(), "claude-haiku-4-5");
        // …and the kind's own default when the caller names nothing, rather
        // than an empty string reaching the wire.
        let p = k.build(ApiKey::new("sk-ant-x"), None);
        assert_eq!(p.model_id(), k.default_model());
    }

    /// **If this breaks:** the startup line tells a claude-engine user the model
    /// cannot look anything up, while the child CLI has `WebSearch` and will use
    /// it. `web_search()` is about *provider-side* search on Emma's key, and for
    /// this kind there is no such thing, which is not the same claim as "no
    /// search". See the claude branch in `main.rs`.
    #[test]
    fn the_claude_kind_sells_no_provider_side_search_and_that_is_not_a_capability_claim() {
        assert!(!kind("claude").unwrap().web_search());
        assert!(!kind("claude").unwrap().requires_key());
    }
}
