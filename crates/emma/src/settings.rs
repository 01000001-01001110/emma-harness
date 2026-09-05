//! `~/.emma/settings.json` — the small amount of state that belongs to the
//! person rather than to the project.
//!
//! It lives beside `credentials.json` under the home directory and **never** in
//! the working tree, for the same reason the key does: Emma runs inside
//! repositories, and anything it writes next to the code is something the next
//! `git add .` publishes. A model id is not a secret, but a file Emma creates
//! in someone's repository without being asked is a surprise either way.
//!
//! Deliberately *not* the harness. `.emma/config.json` is the project's
//! configuration, read by `emma-harness`, and putting a personal default in it
//! would mean two files answering "what is the model" with no rule for
//! disagreeing.
//!
//! **A model is not a model without a provider.** `claude-opus-5` means nothing
//! to anyone but Anthropic, so the stored unit is a provider plus a model *per
//! provider*: switching away and back must not cost you the model you had
//! chosen, because re-typing it is the entire annoyance of switching. The file
//! written before providers existed — a bare `{"model": …}` — is read as
//! Anthropic's, silently, because the only possible answer to a prompt asking
//! about it is yes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use emma_llm::{ProviderKind, UnknownProvider, DEFAULT_PROVIDER};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,

    /// provider -> the model last chosen for it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub models: BTreeMap<String, String>,

    /// provider -> what is known about the model stored for it.
    ///
    /// Nothing writes this yet: checking a model id means asking the provider
    /// for its list, which is the next stage and the first thing here that
    /// needs a network. It is read and written back so that a file written by a
    /// build which *does* validate is not stripped by one that does not — and
    /// so that "never validated" and "set with --force" can read differently
    /// from each other when there is finally a difference.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub validated: BTreeMap<String, Validation>,

    /// The sidebar's user tools — which shell, which editor, which directory
    /// the data explorer opens. Read and acted on by `crate::usertools`; an
    /// absent field means "probe the machine", which is why the whole block is
    /// skipped when empty rather than written as four nulls a user would have
    /// to wonder about. Additive to this struct on purpose: the provider
    /// fields above belong to another workstream and this one only appends.
    #[serde(default, skip_serializing_if = "ToolSettings::is_empty")]
    pub tools: ToolSettings,

    /// Which theme is in force, by the name of the file that holds it — see
    /// [`crate::term::theme`].
    ///
    /// **Here and not in `.emma/config.json`**, and that is the module doc's
    /// ruling applied a second time rather than a new one: the project's
    /// configuration is read by `emma-harness` and shared by everyone who
    /// clones the repository, so a theme named there would be two files
    /// answering "what colour is this" with no rule for disagreeing — and the
    /// answer that won would be the repository's, on somebody else's screen.
    /// Theme *files* may live in either place; the selection is a personal
    /// preference and only ever lives here.
    ///
    /// Absent means the built-in. A name that resolves to nothing is a notice
    /// and never a startup failure, because a theme is decoration and refusing
    /// to start over it would be the outage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,

    /// Whether Emma keeps durable knowledge in the project's memory wiki.
    ///
    /// **Absent means on**, and `Option<bool>` is what keeps "never said" and
    /// "said no" different answers — a settings file written by a build that
    /// did not know the word must not read as a refusal. Written only when
    /// set, like `theme`.
    ///
    /// Owner ruling, 2026-08-27, and it decides what this key governs: the
    /// wiki keeps **knowledge distilled from** what Emma read, never the thing
    /// it read. The fork this was ported from also wrote the verbatim body of
    /// every successful `WebFetch` under `.emma/memory/raw/web/`; that is not
    /// here, and `crate::memory`'s module doc carries the argument for why its
    /// absence is a security fix rather than a missing feature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<bool>,

    /// Whether superseded tool results are dropped from the history sent back
    /// to the model. See [`crate::prune`].
    ///
    /// **Absent means off**, which is the opposite default to `memory` above
    /// and deliberately so. This one changes what the model is shown, so a
    /// build that starts doing it silently changes answers; the saving it
    /// claims was measured somewhere else and has not been reproduced here.
    /// Off until it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prune_history: Option<bool>,

    /// Whether the provider may search the web for the model, on its own side
    /// of the wire and on the same key. See `emma_llm::Request::web_search`.
    ///
    /// **Absent means off**, which is the opposite reading to `memory` and the
    /// same one as `prune_history`, for the same reason: this changes what the
    /// model can do and what the user is billed for. The model already has a
    /// search by default, Emma's own `WebSearch` tool over the local Chrome,
    /// which costs nothing per call and goes through the egress gate. The
    /// provider's server-side search is a second path for whoever wants it:
    /// `true` turns it on, and every run then says so at startup, because each
    /// search is a separate charge and the query leaves with the conversation.
    /// On a provider with no search of its own a `true` is read and does
    /// nothing, and the startup line says so rather than letting the setting
    /// look honoured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_search: Option<bool>,

    /// The pre-provider spelling. Deserialized and never written back, so it
    /// survives being read and disappears on the first save. Private because
    /// nothing outside this module has any business setting it: it is an input
    /// to [`load`]'s migration and nothing else.
    #[serde(default, rename = "model", skip_serializing)]
    legacy_model: Option<String>,
}

/// What the user has said about their tools, all optional. Values are a bare
/// program name (looked up on PATH) or an absolute path — never a command
/// *line*: `usertools` spawns argv directly and refuses to word-split a
/// string, because splitting is the first half of running text through a
/// shell and a path with a space in it is not an argument boundary.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolSettings {
    /// The shell the Shell tool opens. Windows honours this directly; see
    /// `usertools` for why mac/linux v1 lets the terminal app pick instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    /// The editor the Code and Settings tools open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editor: Option<String>,
    /// The file manager, when the OS default (`explorer`/`open`/`xdg-open`)
    /// is not the one wanted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_browser: Option<String>,
    /// Where the Data Explorer points. Absolute path; defaults to `~/.emma`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_dir: Option<String>,
}

impl ToolSettings {
    pub fn is_empty(&self) -> bool {
        self.shell.is_none()
            && self.editor.is_none()
            && self.file_browser.is_none()
            && self.data_dir.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Validation {
    pub at: String,
    pub how: How,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum How {
    /// Confirmed against a list the provider returned.
    Checked,
    /// Set past the check, on purpose. A model can exist before it is listed.
    Forced,
}

pub fn path(home: &Path) -> PathBuf {
    home.join(".emma").join("settings.json")
}

/// A missing or unreadable file is an empty `Settings`, not an error: this is a
/// preference store, and refusing to start because a preference could not be
/// read would be the tail wagging the dog.
///
/// Reading is also where the pre-provider file becomes a provider-scoped one.
/// It is done here rather than in a migration step so that every reader gets
/// the same answer whether or not anything has written since — there is no
/// window in which `emma` and `emma config check` disagree about the model.
pub fn load(home: &Path) -> Settings {
    let mut settings: Settings = std::fs::read_to_string(path(home))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    if let Some(model) = settings.legacy_model.take() {
        // Anthropic is safe to assume: it is the only provider that has ever
        // existed here, so a model set before providers were a concept was set
        // for that one.
        settings
            .models
            .entry(DEFAULT_PROVIDER.to_string())
            .or_insert(model);
        settings
            .provider
            .get_or_insert_with(|| DEFAULT_PROVIDER.to_string());
    }
    settings
}

pub fn save(home: &Path, settings: &Settings) -> Result<PathBuf> {
    let path = path(home);
    let dir = path.parent().expect("settings path always has a parent");
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let body = serde_json::to_string_pretty(settings)?;
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// Which provider and which model, and where each answer came from — the second
/// half matters, because "emma is using the wrong model" is otherwise a question
/// with no way to ask it.
#[derive(Debug, PartialEq, Eq)]
pub struct Resolved {
    pub provider: String,
    pub provider_source: &'static str,
    pub model: String,
    pub model_source: &'static str,
}

/// Resolve provider and model, and the kind that will build the client.
///
/// **An unknown provider is an error and never a fallback.** Falling back to
/// Anthropic would make a mis-set provider indistinguishable from a correct
/// one — same output, same bill, no message — and the mistake would only
/// surface as a model id that "does not exist". The error names what this build
/// supports.
///
/// `--model` on its own still means "this model, on the current provider",
/// which is what it means today; a cross-provider override for one run is
/// `--provider X --model Y`.
pub fn resolve_kind(
    flag_provider: Option<&str>,
    flag_model: Option<&str>,
    home: Option<&Path>,
) -> Result<(&'static dyn ProviderKind, Resolved), UnknownProvider> {
    let settings = home.map(load);
    let (provider, provider_source) = match (flag_provider, settings.as_ref()) {
        (Some(name), _) => (name.to_string(), "--provider"),
        (None, Some(s)) if s.provider.is_some() => {
            (s.provider.clone().expect("checked"), "settings.json")
        }
        _ => (DEFAULT_PROVIDER.to_string(), "built-in default"),
    };
    let kind = emma_llm::kind(&provider)?;

    let (model, model_source) = match flag_model {
        Some(model) => (model.to_string(), "--model"),
        // Keyed by the *resolved* provider's canonical name, so a settings file
        // written as `Anthropic` and a flag typed as `anthropic` find the same
        // entry.
        None => match settings.as_ref().and_then(|s| s.models.get(kind.name())) {
            Some(model) => (model.clone(), "settings.json"),
            None => (kind.default_model().to_string(), "built-in default"),
        },
    };
    Ok((
        kind,
        Resolved {
            provider: kind.name().to_string(),
            provider_source,
            model,
            model_source,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **If this breaks:** a settings file written before the key existed
    /// reads as a refusal, or `false` is lost on the way to disk and search
    /// stays on for somebody who turned it off.
    #[test]
    fn web_search_is_absent_until_said_and_false_survives_a_save() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(
            load(home.path()).web_search,
            None,
            "a fresh home has no opinion"
        );

        let mut settings = load(home.path());
        settings.web_search = Some(false);
        save(home.path(), &settings).unwrap();
        assert_eq!(load(home.path()).web_search, Some(false));

        // And a file that never mentions it is not rewritten to mention it.
        let mut settings = load(home.path());
        settings.web_search = None;
        save(home.path(), &settings).unwrap();
        let raw = std::fs::read_to_string(path(home.path())).unwrap();
        assert!(!raw.contains("web_search"), "{raw}");
    }

    #[test]
    fn a_saved_model_round_trips_and_the_flag_still_wins() {
        let home = tempfile::tempdir().unwrap();
        let (_, r) = resolve_kind(None, None, Some(home.path())).unwrap();
        assert_eq!(r.model_source, "built-in default");
        assert_eq!(r.provider, "anthropic");
        assert_eq!(r.provider_source, "built-in default");

        let mut settings = load(home.path());
        settings.provider = Some("anthropic".into());
        settings
            .models
            .insert("anthropic".into(), "claude-something".into());
        save(home.path(), &settings).unwrap();

        let (_, r) = resolve_kind(None, None, Some(home.path())).unwrap();
        assert_eq!(
            (r.model.as_str(), r.model_source),
            ("claude-something", "settings.json")
        );
        assert_eq!(r.provider_source, "settings.json");

        // A one-run override must not be quietly outranked by a preference the
        // user set six weeks ago.
        let (_, r) = resolve_kind(None, Some("other"), Some(home.path())).unwrap();
        assert_eq!((r.model.as_str(), r.model_source), ("other", "--model"));
    }

    #[test]
    fn a_model_set_before_providers_existed_still_runs() {
        // The owner's live file, byte for byte. If this test goes red, an
        // upgrade silently moved somebody off the model they chose.
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".emma")).unwrap();
        std::fs::write(path(home.path()), r#"{"model":"claude-sonnet-5"}"#).unwrap();

        let (kind, r) = resolve_kind(None, None, Some(home.path())).unwrap();
        assert_eq!(kind.name(), "anthropic");
        assert_eq!(r.model, "claude-sonnet-5");
        assert_eq!(r.model_source, "settings.json");

        // Reading migrates; writing is what makes it stick, and the legacy
        // field goes away rather than becoming a second answer.
        let migrated = load(home.path());
        save(home.path(), &migrated).unwrap();
        let raw = std::fs::read_to_string(path(home.path())).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(doc.get("model").is_none(), "{raw}");
        assert_eq!(doc["models"]["anthropic"], "claude-sonnet-5", "{raw}");
        assert_eq!(doc["provider"], "anthropic", "{raw}");
        assert_eq!(
            resolve_kind(None, None, Some(home.path())).unwrap().1.model,
            "claude-sonnet-5"
        );
    }

    #[test]
    fn each_provider_keeps_its_own_model_across_a_switch() {
        // Why the map exists: switching provider and back is a round trip that
        // must cost nothing, because re-typing the model is the whole annoyance
        // of switching.
        let home = tempfile::tempdir().unwrap();
        let mut settings = Settings {
            provider: Some("anthropic".into()),
            ..Default::default()
        };
        settings
            .models
            .insert("anthropic".into(), "claude-x".into());
        settings.models.insert("elsewhere".into(), "model-y".into());
        save(home.path(), &settings).unwrap();

        let back = load(home.path());
        assert_eq!(back.models.get("elsewhere").unwrap(), "model-y");
        assert_eq!(
            resolve_kind(None, None, Some(home.path())).unwrap().1.model,
            "claude-x"
        );
    }

    #[test]
    fn a_provider_this_build_cannot_run_is_refused_rather_than_swapped() {
        // The failure this prevents: `provider: "openai"` in settings.json,
        // Anthropic answering anyway, and nothing saying so.
        let home = tempfile::tempdir().unwrap();
        let mut settings = Settings {
            provider: Some("openai".into()),
            ..Default::default()
        };
        settings.models.insert("openai".into(), "gpt-5.5".into());
        save(home.path(), &settings).unwrap();

        let err = resolve_kind(None, None, Some(home.path()))
            .err()
            .expect("an unimplemented provider resolved to something")
            .to_string();
        assert!(err.contains("openai"), "{err}");
        assert!(err.contains("anthropic"), "{err}");
        // …and the same for a flag, which is the other door to the same
        // mistake.
        assert!(resolve_kind(Some("openai"), None, Some(home.path())).is_err());
    }

    #[test]
    fn a_corrupt_settings_file_is_not_a_startup_failure() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".emma")).unwrap();
        std::fs::write(path(home.path()), "{ not json").unwrap();
        let (_, r) = resolve_kind(None, None, Some(home.path())).unwrap();
        assert_eq!(r.model_source, "built-in default");
        assert_eq!(r.provider_source, "built-in default");
    }

    #[test]
    fn tool_settings_round_trip_and_an_empty_block_is_never_written() {
        // Two halves of the same guarantee. A configured tool survives a
        // load/save cycle — otherwise the Settings tool would eat its own
        // configuration. And a file with no tool choices does not grow a
        // `tools` key just because this build knows the word: a settings file
        // that mutates on every save is one nobody can diff.
        let home = tempfile::tempdir().unwrap();
        let mut settings = load(home.path());
        assert!(settings.tools.is_empty());
        save(home.path(), &settings).unwrap();
        let raw = std::fs::read_to_string(path(home.path())).unwrap();
        assert!(!raw.contains("tools"), "{raw}");

        settings.tools.editor = Some("C:\\Program Files\\odd name\\code.exe".into());
        save(home.path(), &settings).unwrap();
        let back = load(home.path());
        assert_eq!(
            back.tools.editor.as_deref(),
            Some("C:\\Program Files\\odd name\\code.exe")
        );
        assert!(back.tools.shell.is_none());
    }

    #[test]
    fn a_theme_choice_round_trips_and_an_unthemed_file_never_grows_the_key() {
        // Same two halves as the tools block, for the same reason: a settings
        // file that mutates on every save is one nobody can diff, and a
        // selection that does not survive a save is a `/theme --save` that
        // silently does nothing.
        let home = tempfile::tempdir().unwrap();
        let mut settings = load(home.path());
        assert!(settings.theme.is_none());
        save(home.path(), &settings).unwrap();
        assert!(!std::fs::read_to_string(path(home.path()))
            .unwrap()
            .contains("theme"));

        settings.theme = Some("oxide".into());
        save(home.path(), &settings).unwrap();
        assert_eq!(load(home.path()).theme.as_deref(), Some("oxide"));
        // …and it is not the project's to set: this key exists in the personal
        // file and the harness config has no counterpart.
        let raw = std::fs::read_to_string(path(home.path())).unwrap();
        assert!(raw.contains("\"theme\": \"oxide\""), "{raw}");
    }

    #[test]
    fn a_validation_written_by_a_later_build_is_not_stripped_by_this_one() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".emma")).unwrap();
        std::fs::write(
            path(home.path()),
            r#"{"provider":"anthropic","models":{"anthropic":"claude-x"},
                "validated":{"anthropic":{"at":"2026-08-11T09:14:22Z","how":"forced"}}}"#,
        )
        .unwrap();
        let settings = load(home.path());
        assert_eq!(settings.validated["anthropic"].how, How::Forced);
        save(home.path(), &settings).unwrap();
        let raw = std::fs::read_to_string(path(home.path())).unwrap();
        assert!(raw.contains("forced"), "{raw}");
    }
}
