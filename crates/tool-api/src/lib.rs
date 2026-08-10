//! The boundary between the loop and everything it can do.
//!
//! The shape here was arrived at by measurement rather than design: five of
//! eight failure classes were ending turns silently, the user saw "Something
//! went wrong on my side", and the model never learned anything had failed.
//! Emma's tool surface is larger and fails more often — a file is missing, a
//! patch does not apply, a directory is not writable, a command cannot be
//! spawned — so the lesson applies harder here.
//!
//! **The one rule that governs this file.** A `ToolError` is a fact about the
//! *call*, never about what the world contains. A read of an empty file
//! succeeds and returns nothing. A glob matching zero paths succeeds and
//! returns an empty list. Emptiness is a result; only the machinery failing is
//! an error. Blurring that turns "I could not look" and "I looked and there
//! was nothing" into the same message, and the model cannot route around a
//! failure it has been told is an answer.
//!
//! That rule was written about the filesystem and did not survive first
//! contact with `Bash`. A non-zero exit had been documented as `Failed`, which
//! makes `grep -q` answering "no" a tool failure — and inside the loop, which
//! refuses to repeat a call that already failed with nothing changed since,
//! one legitimate miss would have blocked that command for the turn. The line
//! is now **whether the command ran**: could-not-spawn, timed-out and killed
//! are `Failed`; anything that started and finished is `Ok`, carrying
//! `exit status <n>` as the first line of its content. Deliberately not
//! resolved as "Bash is the documented exception", because "except X" is how a
//! rule starts becoming folklore.
//!
//! The rest of the file is the shape that rule implies: the error taxonomy,
//! the success type, the facts the runtime needs *before* it runs anything
//! ([`ToolMeta`]), what a tool is told about the call ([`ToolCtx`]), the
//! [`Tool`] trait itself, and a [`Registry`] that renders the whole surface
//! for the wire and hashes it.

use std::sync::Arc;

// region: The failure channel
// ---------------------------------------------------------------------------
// The failure channel
//
// The taxonomy the governing rule is about. Three variants, a stable `kind`
// string the model routes on, and prose kept separate from it.
// ---------------------------------------------------------------------------

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
    /// The tool ran and did not succeed: the file could not be written, the
    /// patch did not apply, the child process could not be spawned or was
    /// killed.
    ///
    /// Note what is *not* here. A command that started and exited non-zero is
    /// `Ok` — see the ruling in the module doc — because an exit status is the
    /// world answering, not the machinery breaking.
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

// endregion: The failure channel

// region: What success looks like
// ---------------------------------------------------------------------------
// What success looks like
//
// One type, two audiences: `content` is for the model, `display` is for the
// human watching the terminal, and `truncated` is the admission that owes the
// model a warning.
// ---------------------------------------------------------------------------

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

// endregion: What success looks like

// region: What the runtime knows before it runs anything
// ---------------------------------------------------------------------------
// What the runtime knows before it runs anything
//
// The approval gate reads `ToolMeta` without invoking the tool, and `ToolCtx`
// is everything the tool is told once it has been allowed to run. Both are
// short; both have a load-bearing absence documented on them.
// ---------------------------------------------------------------------------

/// Facts the runtime needs *before* deciding whether to run a tool.
///
/// In tustle-agent the equivalent struct had a `needs_approval` field that
/// nothing ever read — a declaration pretending to be a mechanism. It was
/// deliberately not ported. Here `read_only` and `reaches_network` are both
/// load-bearing: they are the two questions `emma::approval` asks, separately,
/// before deciding whether to interrupt a human. Each tool crate carries a test
/// that runs its `read_only` tools against a populated sandbox and asserts the
/// tree is unchanged afterwards (`tools/fs/tests/read_only.rs`,
/// `tools/tasks/tests/tools.rs`) — plus a pin on *which* tools claim it, so a
/// new claim is noticed rather than merely unexercised. If that stops being
/// true, the gate has quietly stopped protecting anything.
///
/// **There is deliberately no `Default`.** Adding a field here breaks every
/// construction site in the workspace, and that is the feature: a tool must
/// *state* each of these rather than inherit it. The predecessor added a
/// defaulted field and thereby granted a safety property to every tool written
/// afterwards, chosen by nobody, noticed by no review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolMeta {
    /// Cannot change local state: no file written, no process spawned.
    ///
    /// **One bit doing one job, and the network is not it.** This field has
    /// previously been documented as "cannot reach the network" as well, and
    /// that reading was never true of the tools as they are: `WebFetch` and
    /// `WebSearch` declare `read_only: true` and both talk to the outside — one
    /// of them by driving a browser. The declaration is honest about what this
    /// bit asks, which is "can this damage this machine", and neither can.
    /// Egress is asked by [`ToolMeta::reaches_network`] instead, because it is
    /// a different risk with a different answer: flipping the web tools to
    /// `read_only: false` was considered and refused, since a gate that fires
    /// on every page read trains the operator to click through it and costs the
    /// gate on `Write` too.
    pub read_only: bool,
    /// Sends bytes off this machine, by construction.
    ///
    /// The second axis, and the reason it exists: **writing is a risk the model
    /// takes deliberately; egress is how a prompt-injected page turns a read
    /// tool into an exfiltration channel.** A model that reads an attacker's
    /// page and then "searches" for the contents of a `.env` has written
    /// nothing and destroyed nothing, and passes every local-damage check on
    /// the way out. `emma::approval` gates this per host, granted once per
    /// session by a human — not per call, which would be the same
    /// click-through trainer by another route.
    ///
    /// **What it does not cover, and none of these are oversights.**
    ///
    /// - *What comes back.* An approved host is a destination the user chose,
    ///   not a source they trust. The page is still attacker-controlled text
    ///   arriving in the model's context, and nothing here reads it.
    /// - *A tool that reaches the network incidentally.* `Bash` can `curl`, and
    ///   declares `false`. It is not exempt: `read_only: false` already means
    ///   every `Bash` call is shown to a human as the command itself, so its
    ///   egress is approved at the same moment and with more information than a
    ///   host name. Declaring `true` would demand a destination it cannot name
    ///   without parsing shell, which is an arms race, not a gate.
    /// - *Enforcement.* Like `read_only`, this is a declaration. Nothing stops
    ///   a lying tool from opening a socket; what stops it is that the tool
    ///   crates own tests asserting their declarations hold.
    pub reaches_network: bool,
    /// Running it twice with the same arguments has the same effect as once.
    ///
    /// Declared by every tool and, as of today, **read by nothing** — Emma has
    /// no crash-recovery fold that replays a journalled call. That makes it the
    /// same shape as the `needs_approval` field this struct's doc rejects
    /// above, and it is recorded here rather than in a plan so nobody reads it
    /// as a mechanism that exists. It is kept because the answer is a genuine
    /// per-tool fact worth writing down at the point the tool is defined
    /// (`Edit` is deliberately not idempotent, and says so), not because
    /// something consults it.
    pub idempotent: bool,
}

/// Where one call is about to send bytes, and what errand it is running.
///
/// Produced by the tool, never derived by the gate. The alternative — the
/// approval loop reaching into `args["url"]` and parsing it — has been tried
/// twice in this project's history in other forms (a hard-coded `query`
/// argument check, and inferring retrieval from an empty `chunks` array) and
/// both times it made "adding a tool is a crate plus one registry line" false.
/// The gate asks; the tool answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkTarget {
    /// The host bytes leave for, lowercased and without a trailing dot.
    ///
    /// A host and never a URL, because this is the unit a session grant is
    /// keyed on: granting per URL is a prompt per page, which is the
    /// click-through trainer the whole design is arranged to avoid.
    pub host: String,
    /// One line naming the errand, for the human reading the prompt — the URL,
    /// the query. A prompt that names a host and not what is being sent there
    /// is a prompt nobody can evaluate, and an unevaluatable prompt
    /// manufactures consent.
    pub detail: String,
}

impl NetworkTarget {
    /// Normalises the host so the gate's `HashSet` and the tool agree on what
    /// "the same host" means. `Docs.RS.` and `docs.rs` are one grant; `docs.rs`
    /// and `www.docs.rs` are deliberately two, because deciding they are the
    /// same is a policy nobody asked for.
    pub fn new(host: impl AsRef<str>, detail: impl Into<String>) -> Self {
        Self {
            host: host
                .as_ref()
                .trim()
                .trim_end_matches('.')
                .to_ascii_lowercase(),
            detail: detail.into(),
        }
    }
}

/// What a tool is told about the call it is serving.
///
/// Note what is missing: there is nowhere here for **session state**. The
/// read-tracking that makes `Write`'s refusal-to-clobber meaningful therefore
/// lives in the tool structs instead, which means "these tools share one
/// tracker" is a wiring convention rather than something the type system holds
/// — a `Write` built with the wrong tracker refuses nothing, and it compiles.
/// Known, and cheaper to fix at six tools than at sixteen.
pub struct ToolCtx {
    /// Every relative path resolves against this, and nothing may escape it.
    pub cwd: std::path::PathBuf,
    pub session_id: String,
    pub turn_id: String,
}

// endregion: What the runtime knows before it runs anything

// region: The tool trait
// ---------------------------------------------------------------------------
// The tool trait
//
// Everything a tool must answer, and the two-layer result that keeps "the
// model should see this" separate from "the turn cannot continue".
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    /// Matches Claude Code's names — `Read`, `Write`, `Edit`, `Glob`, `Grep`,
    /// `Bash` — so a hook matcher or allow-list written for one works for the
    /// other. Better names would cost compatibility and buy nothing.
    fn name(&self) -> &'static str;

    /// Shown to the model verbatim. The built-in tools hold theirs in a
    /// markdown file next to the source and `include_str!` it, so a prose
    /// change is reviewable as a diff and lands in [`Registry::schema_hash`].
    /// Returning `&str` rather than `&'static str` is what lets `Skill` be a
    /// tool too — its description is composed at load time from the skill file
    /// on disk.
    fn description(&self) -> &str;

    fn input_schema(&self) -> serde_json::Value;

    fn meta(&self) -> ToolMeta;

    /// Where this particular call would send bytes, if anywhere.
    ///
    /// Answered by the tool because the approval gate must not learn what any
    /// tool's arguments look like — see [`NetworkTarget`] for the two times
    /// that went wrong here. `None` is the default and is right for every tool
    /// that declares `reaches_network: false`.
    ///
    /// **`None` from a tool that declares `reaches_network: true` is a
    /// refusal, not a pass.** The gate cannot grant a destination nobody
    /// named, so it denies the call and says so. Fail-closed, because the
    /// alternative is that a tool which forgets to answer gets silent egress.
    fn network_target(&self, _args: &serde_json::Value) -> Option<NetworkTarget> {
        None
    }

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

// endregion: The tool trait

// region: The registry, and the surface it renders
// ---------------------------------------------------------------------------
// The registry, and the surface it renders
//
// Holds the tools, looks them up by the name the model used, and renders the
// whole surface twice: once for the wire and once as a digest, so a change to
// a description is attributable rather than invisible.
// ---------------------------------------------------------------------------

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

    /// A digest of every byte of the tool surface the model is shown — names,
    /// descriptions and schemas — so a change to a description is attributable
    /// rather than invisible. The loop records it on the `goal` event at the
    /// start of a run, and `emma config check` prints it.
    ///
    /// Truncated to 16 hex characters: this is a change detector for a human
    /// reading a log, not a collision-resistant identifier.
    pub fn schema_hash(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        for def in self.wire_definitions() {
            hasher.update(def.to_string().as_bytes());
        }
        format!("{:x}", hasher.finalize())[..16].to_string()
    }
}

// endregion: The registry, and the surface it renders

// region: Tests
// ---------------------------------------------------------------------------
// Tests
//
// Two guard the taxonomy rather than the prose: `kind` is what the model
// routes on, and every variant must keep carrying its detail. The third guards
// the one piece of normalisation in this file, because the approval gate keys a
// session grant on the string it produces.
// ---------------------------------------------------------------------------

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
    fn a_host_is_normalised_so_one_grant_covers_one_host() {
        // The gate keys a session grant on this string. If two spellings of one
        // host produce two keys, the second fetch to a host the user already
        // approved prompts again — and a prompt that fires when it should not
        // is how the operator learns to answer without reading.
        assert_eq!(NetworkTarget::new("Docs.RS.", "x").host, "docs.rs");
        assert_eq!(NetworkTarget::new("  docs.rs ", "x").host, "docs.rs");
        // …and the mirror image, which is the part worth pinning: a subdomain
        // is a different host and a different grant. Collapsing them would be
        // a policy, and nobody asked for it.
        assert_ne!(
            NetworkTarget::new("www.docs.rs", "x").host,
            NetworkTarget::new("docs.rs", "x").host
        );
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

// endregion: Tests
