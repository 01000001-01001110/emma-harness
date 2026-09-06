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

    // ----------------------------------------------------------------------
    // The blocks below arrived with the port of the macOS fork (2026-09-06).
    // Each is additive and skipped when empty, the `tools` rule: a settings
    // file must not grow a `"voice": {}` block because this build knows the
    // word. Where a block's reader has not landed yet, the field doc says
    // which package brings it, so a key that is stored and not yet honoured
    // is never mistaken for one that is.
    // ----------------------------------------------------------------------
    /// Whether session transcripts are kept in the shape the training
    /// exporter reads. **Absent means on**, per [`TRAINING_CAPTURE_DEFAULT`]:
    /// a settings file written before this build knew the word reads as
    /// capturing. Nothing leaves the machine either way: the export is a local
    /// file built from a local transcript. Read by the `export-training`
    /// command once it lands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub training_capture: Option<bool>,

    /// Whether answers are read aloud, and with which voice. See
    /// [`VoiceSettings`] and `crate::speech`.
    #[serde(default, skip_serializing_if = "VoiceSettings::is_empty")]
    pub voice: VoiceSettings,

    /// provider -> the sampling knobs set by hand for it. The `models` map's
    /// shape, for its reason: these are per-provider answers and one global
    /// value would be wrong for whichever provider it was not set on. See
    /// [`SamplingSettings`]; the resolver arrives with the temperature port.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sampling: BTreeMap<String, SamplingSettings>,

    /// Which language servers Emma may start. See [`LspSettings`]; read once
    /// the multi-language `tools/lsp` port lands.
    #[serde(default, skip_serializing_if = "LspSettings::is_empty")]
    pub lsp: LspSettings,

    /// What the user has said about the memory wiki beyond the `memory`
    /// toggle above. See [`MemoryPolicy`].
    ///
    /// **Named `memory_policy` and not `memory`, and the name is a
    /// compatibility decision.** The fork wrote this block under `"memory"`;
    /// mainline has read `"memory"` as a boolean since the wiki was ported,
    /// and neither shape parses as the other. A file written by either build
    /// keeps working under both keys.
    #[serde(default, skip_serializing_if = "MemoryPolicy::is_empty")]
    pub memory_policy: MemoryPolicy,

    /// The look of the frame beyond the theme name. See
    /// [`AppearanceSettings`]. `theme` stays a top-level field above.
    #[serde(default, skip_serializing_if = "AppearanceSettings::is_empty")]
    pub appearance: AppearanceSettings,

    /// What the interface says without being asked. See [`UiSettings`].
    #[serde(default, skip_serializing_if = "UiSettings::is_empty")]
    pub ui: UiSettings,

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

/// The training-capture default when the key is absent. A named constant
/// rather than a literal so the resolver and the pin test cannot drift.
pub const TRAINING_CAPTURE_DEFAULT: bool = true;

/// The hints default when the key is absent.
pub const HINTS_DEFAULT: bool = true;

impl Settings {
    /// Whether transcripts are kept in the exporter's shape.
    pub fn capture_training(&self) -> bool {
        self.training_capture.unwrap_or(TRAINING_CAPTURE_DEFAULT)
    }

    /// Whether the informational one-liners are printed.
    pub fn hints(&self) -> bool {
        self.ui.hints.unwrap_or(HINTS_DEFAULT)
    }
}

/// Which language servers Emma may start. An absent list means the default
/// set; a name this build does not know is kept and reported, never an error.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<Vec<String>>,
}

impl LspSettings {
    pub fn is_empty(&self) -> bool {
        self.enabled.is_none()
    }
}

/// What the user has said about spoken output. See `crate::speech`.
///
/// `name: None` is not "no voice": it is the platform default. The
/// distinction matters enough that the absent case is documented rather than
/// inferred.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<bool>,
    /// The voice by the name the platform's speech engine lists, exactly. A
    /// name rather than an identifier, for the reason `theme` is a name: a
    /// machine that lacks this voice falls back to its default, which is the
    /// right failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// How many bytes of an answer are read aloud before the rest is left on
    /// screen. Absent means the built-in default. Not clamped: a person who
    /// sets it to their whole screen has said what they want.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spoken_limit: Option<usize>,
}

impl VoiceSettings {
    pub fn is_empty(&self) -> bool {
        self.on.is_none() && self.name.is_none() && self.spoken_limit.is_none()
    }
}

/// Per-provider sampling overrides, keyed by provider name in
/// [`Settings::sampling`]. Every field is optional and an absent field means
/// the provider's own default, which for `temperature` means nothing is sent.
///
/// ```json
/// "sampling": {
///   "anthropic": { "temperature": 0.2, "max_output_tokens": 16000, "stream": false }
/// }
/// ```
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct SamplingSettings {
    /// Not clamped here: the legal range differs per host, and an
    /// out-of-range value is the host's 400 to explain, which names the range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// The ceiling on one turn's output, thinking included where thinking is
    /// billed as output. Providers already clamp to the model's own maximum,
    /// so a value above it is lowered rather than rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// Whether an interactive turn streams. Absent means on. `-p` is always
    /// batch whatever this says: there is no terminal to stream into.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
}

impl SamplingSettings {
    pub fn is_empty(&self) -> bool {
        self.temperature.is_none() && self.max_output_tokens.is_none() && self.stream.is_none()
    }
}

/// Retention meaning "keep forever", the absent value's reading.
pub const RETENTION_KEEP_FOREVER: u64 = 0;

/// What the user has said about the memory wiki beyond the capture toggle.
///
/// Three `Option`s rather than three values: "never said" and "said no" are
/// different answers. Nothing prunes yet and recall does not consult this
/// yet; the keys are stored so a choice survives the session that made it.
/// Deleting somebody's notes is not a side effect a settings screen acquires
/// quietly, so the pruner is its own change with its own tests.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryPolicy {
    /// How many days a captured page is kept. Absent, or
    /// [`RETENTION_KEEP_FOREVER`], means nothing is ever pruned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_days: Option<u64>,
    /// Whether recall consults the wiki without being asked. Absent means on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_recall: Option<bool>,
    /// `project` or `global`. Absent means `project`, which is what every
    /// write does today.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

impl MemoryPolicy {
    pub fn is_empty(&self) -> bool {
        self.retention_days.is_none() && self.auto_recall.is_none() && self.scope.is_none()
    }
}

/// What the interface says of its own accord.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiSettings {
    /// Whether the informational one-liners are printed. Absent means on, per
    /// [`HINTS_DEFAULT`]. A hint is informational, repeats on a path a person
    /// takes many times, and can be dropped without losing a fact about what
    /// happened; refusals, receipts and warnings are never gated by this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hints: Option<bool>,
}

impl UiSettings {
    pub fn is_empty(&self) -> bool {
        self.hints.is_none()
    }
}

/// The look of the frame beyond the theme name.
///
/// Names, not values, wherever a table exists to index into: a name this build
/// does not know resolves to the default, which is the right failure. The
/// font fields are the exception and have to be, because a font family is the
/// terminal's vocabulary rather than Emma's.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppearanceSettings {
    /// Which role's swatch the accent borrows. Read by the palette once the
    /// accent port lands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent: Option<String>,
    /// `unicode`, `ascii`, or absent for auto-detection. Read once, at
    /// startup, where the skin is built. `unicode` is a request and not a
    /// guarantee: a console that cannot do UTF-8 still gets ASCII.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glyphs: Option<String>,
    /// `full` or `compact`. Read by the status bar once its density port
    /// lands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_bar: Option<String>,
    /// The font families the Font Family row cycles. Absent means the seed
    /// list in `term::termfont`; the row's Add path writes a longer one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub font_families: Vec<String>,
    /// The family last asked for, re-applied at startup. Absent means Emma
    /// has never touched the terminal's font, which is not the same as the
    /// terminal having no font: nothing is asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_family: Option<String>,
    /// The size last asked for, in points. Absent means the same as an absent
    /// family.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_size: Option<u32>,
}

impl AppearanceSettings {
    pub fn is_empty(&self) -> bool {
        self.accent.is_none()
            && self.glyphs.is_none()
            && self.status_bar.is_none()
            && self.font_families.is_empty()
            && self.font_family.is_none()
            && self.font_size.is_none()
    }
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
    // Temp file beside the target, renamed over it: the shape
    // `permissions::remember` and `platform` already use, and this was the
    // one writer left doing a direct write. A crash between the truncate and
    // the last byte of a direct write leaves a settings.json that no longer
    // parses, and the next boot reads a file with nothing in it where the
    // provider, model and theme were. The rename is atomic within a
    // directory, so a reader sees the old file or the new one, never half.
    let temp = path.with_extension("json.emma-tmp");
    std::fs::write(&temp, body).with_context(|| format!("writing {}", temp.display()))?;
    std::fs::rename(&temp, &path).with_context(|| format!("writing {}", path.display()))?;
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

#[cfg(test)]
mod port_blocks_tests {
    use super::*;

    /// A file written by the fork, with `"memory"` as a block, and one written
    /// by mainline, with `"memory"` as a bool, both parse; and the fork's
    /// block lands under `memory_policy` when spelled that way.
    #[test]
    fn both_spellings_of_memory_parse() {
        let mainline: Settings = serde_json::from_str(r#"{"memory": true}"#).unwrap();
        assert_eq!(mainline.memory, Some(true));
        let policy: Settings =
            serde_json::from_str(r#"{"memory_policy": {"retention_days": 30}}"#).unwrap();
        assert_eq!(policy.memory_policy.retention_days, Some(30));
        assert!(policy.memory.is_none());
    }

    /// An empty block is not written back: a file must not grow keys because
    /// this build knows the words. Removing any `skip_serializing_if` fails it.
    #[test]
    fn empty_blocks_are_not_written() {
        let raw = serde_json::to_string(&Settings::default()).unwrap();
        for key in [
            "voice",
            "sampling",
            "lsp",
            "memory_policy",
            "appearance",
            "ui",
            "training_capture",
        ] {
            assert!(!raw.contains(key), "{key} in {raw}");
        }
    }

    /// Absent means on for capture and hints, and the constants are the ones
    /// the resolvers read.
    #[test]
    fn capture_and_hints_default_on() {
        let s = Settings::default();
        assert_eq!(s.capture_training(), TRAINING_CAPTURE_DEFAULT);
        assert_eq!(s.hints(), HINTS_DEFAULT);
        let off: Settings =
            serde_json::from_str(r#"{"training_capture": false, "ui": {"hints": false}}"#).unwrap();
        assert!(!off.capture_training());
        assert!(!off.hints());
    }
}

#[cfg(test)]
mod atomic_save_tests {
    use super::*;

    /// A save goes through a temp file and a rename, and leaves neither the
    /// temp file nor a stale target behind. Writing the target directly
    /// instead (the old code) leaves this green; dropping the rename turns it
    /// red on both counts, which is the half of atomicity a test can see
    /// without a crash injected into the filesystem.
    #[test]
    fn a_save_renames_its_temp_file_over_the_target_and_keeps_nothing_else() {
        let home = tempfile::tempdir().unwrap();
        let mut s = Settings::default();
        s.theme = Some("first".into());
        let path = save(home.path(), &s).unwrap();
        s.theme = Some("second".into());
        save(home.path(), &s).unwrap();
        let back = load(home.path());
        assert_eq!(back.theme.as_deref(), Some("second"));
        let temp = path.with_extension("json.emma-tmp");
        assert!(
            !temp.exists(),
            "the temp file must be renamed away, not left beside the target"
        );
        // A stale temp file from an interrupted earlier save is overwritten,
        // never read.
        std::fs::write(&temp, "{ not json").unwrap();
        s.theme = Some("third".into());
        save(home.path(), &s).unwrap();
        assert_eq!(load(home.path()).theme.as_deref(), Some("third"));
        assert!(!temp.exists());
    }
}
