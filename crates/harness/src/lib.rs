//! The harness: a `.emma/` directory on disk, resolved once at startup into a
//! value the turn loop consumes.
//!
//! Most of the reasoning below was argued out and paid for in an earlier design
//! and carried over intact. Four things were deliberately not carried over,
//! because they were wrong for Emma rather than merely differently named, and
//! each is documented at the code that changed: discovery no longer walks into
//! `~/.claude/` (`discover_from`); the "persona files nothing selects → refuse"
//! rule applies to `.emma/` only (`select_persona`); a persona's `tools` list is
//! a real filter rather than an assertion that removes nothing
//! (`select_tools`); and `deny_unknown_fields` stops at the outer level of
//! `.claude/settings.json` (`claude.rs`). The through-line is that the earlier
//! tool surface was read-only by construction while Emma's writes files and runs
//! commands, so several rules that were merely tidy there are load-bearing here
//! — and one that was safe there would be an outage here.
//!
//! **The discipline this crate exists to enforce.** The loop gets a struct. It
//! never reads a file, never learns that a persona was selected, never learns
//! which layer a sentence came from, and never branches on configuration. Every
//! public item is shaped so the wrong thing is hard to write: the instructions
//! are a `String` and not a loader; the hook payload builder takes a `HookCall`
//! and **not** a `&ToolOutcome`, so plumbing an internal field through to an
//! external process needs a signature change a reviewer will see. This is the
//! only reason a configuration system can grow without the loop growing.
//!
//! Running a hook lives in `hooks.rs` and is re-exported here, so the loop still
//! sees one surface. That file is a subprocess supervisor and fails for its own
//! reasons; this one fails when configuration starts sprouting behaviour.
//!
//! **The boot states, and they are not open here:** absent → refuse to start,
//! naming every path searched; present but empty → boot and answer nothing;
//! malformed → refuse to start, naming the file and the problem; and, in
//! `.emma/` only, persona files that nothing selects → refuse, because an empty
//! harness is a statement and an unselected one is an accident. That last rule
//! deliberately does not extend to `.claude/agents/`; `select_persona` says why.
//!
//! The reasoning, because the edges follow from it: an agent booted with the
//! wrong prompt does not crash. It acts fluently and confidently, attributed to
//! an `instructions_hash` nobody reviewed — and in Emma it acts by writing files
//! and running commands. So any ambiguity about *what the model was told*
//! resolves to not starting. The empty case is the one that is not ambiguous:
//! someone made the directory and put nothing in it, so Emma boots with no
//! standing instructions and needs no special case anywhere in the loop. That
//! the empty case needs no code is the proof the harness is separable from the
//! engine.
//!
//! **Hashing.** No prompt layer is trimmed, normalised or re-wrapped, so with
//! one file present the assembled prompt is that file's bytes. That property is
//! what lets a prompt's hash be compared across a refactor and mean something.
//! The rule is about the prompt specifically, and two things outside it do get
//! adjusted: a skill body is taken from after the frontmatter with leading
//! whitespace stripped (`split_skill`), and a command body is trimmed at both
//! ends (`load_commands`). Neither is always-on prompt text — a skill body
//! arrives only when the model loads it, and a command body is text a person
//! typed a `/name` to summon.
//!
//! **Two directory names.** Emma also recognises `.claude/`, so skills and
//! commands already written for Claude Code work unchanged. `.emma/` wins
//! outright where both exist — never merged. See `claude.rs` and
//! `notes/claude-code-compatibility.md`.

pub mod hash;
mod claude;
mod hooks;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::hooks::{HookDef, ResolvedHook};
pub use crate::hooks::{HookCall, HookEvent, HookOutcome, HookResult, HookRun, HookVerdict};

use emma_tool_api::Registry;

// region: The names, and the two flavours
// ---------------------------------------------------------------------------
// The names, and the two flavours
//
// The four strings the outside world uses to reach the harness, and the enum
// that records which of the two directory layouts was found. Everything below
// branches on `Flavor`, so it is declared before anything that reads it.
// ---------------------------------------------------------------------------

pub const ROOT_DIR_NAME: &str = ".emma";
/// Also discovered, and read as a harness. Claude Code's directory.
pub const CLAUDE_DIR_NAME: &str = ".claude";
/// Overrides discovery entirely. Named for the root, not a file: it points at
/// the directory the whole harness is read from.
pub const ROOT_ENV: &str = "EMMA_ROOT";
/// Per-session persona override. Selection is a runtime decision, never the
/// model's: a model choosing its own persona is a self-modifying prompt.
pub const PERSONA_ENV: &str = "EMMA_PERSONA";

/// Which directory was found, and therefore how it is read.
///
/// Carried on the resolved value and reported in `snapshot` so the choice is
/// never a mystery — the note requires it to be logged at startup, and a value
/// nobody can read cannot be logged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Flavor {
    /// `.emma/`: `config.json`, `personas/`, `skills/`, `commands/`, `hooks/`.
    Emma,
    /// `.claude/`: `settings.json`, `CLAUDE.md`, `agents/`, and the same
    /// `skills/` and `commands/` layout.
    Claude,
}

impl Flavor {
    /// Inferred from the directory's own name, so `Harness::load(path)` does the
    /// right thing without the caller having to remember which it handed over.
    pub fn of(root: &Path) -> Self {
        match root.file_name().and_then(|s| s.to_str()) {
            Some(CLAUDE_DIR_NAME) => Self::Claude,
            _ => Self::Emma,
        }
    }

    fn spine_file(self) -> &'static str {
        match self {
            Self::Emma => "config.json",
            Self::Claude => "settings.json",
        }
    }
}

// endregion: The names, and the two flavours

// region: Discovery
// ---------------------------------------------------------------------------
// Discovery
//
// Answering one question — which directory is the harness — before anything is
// read from it. The security of the whole crate starts here, because a wrong
// answer boots Emma on a prompt nobody chose.
// ---------------------------------------------------------------------------

/// Find the harness the way git finds `.git`: walk up from the working
/// directory. `EMMA_ROOT` overrides it outright.
pub fn discover() -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("reading the working directory")?;
    discover_from(&cwd, std::env::var_os(ROOT_ENV).map(PathBuf::from))
}

/// The testable half: `overridden` is threaded rather than read, so a test does
/// not have to mutate process environment.
///
/// **Precedence, decided before it is discovered.** Nearest ancestor wins, and
/// within a single directory `.emma/` beats `.claude/` outright — the loser is
/// ignored entirely, never merged. Merging is convenient and is exactly how a
/// configuration system becomes impossible to reason about, because the answer
/// to "where did this instruction come from" stops being a file.
pub fn discover_from(start: &Path, overridden: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = overridden {
        if p.is_dir() {
            return Ok(p);
        }
        // An override that fell back to a search would boot the agent on
        // configuration nobody asked for.
        bail!("{ROOT_ENV}=`{}` is not a directory", p.display());
    }
    let home = home_dir();
    let mut searched = Vec::new();
    for dir in start.ancestors() {
        // `~/.claude/` is Claude Code's **user-scope** configuration — global
        // permissions, global agents, global skills, for a different program.
        // Adopting it as this project's harness would hand Emma standing
        // instructions from a directory the user never associated with this
        // project, which is the "booted on the wrong prompt" failure arriving
        // through the front door. It is skipped.
        //
        // `~/.emma/` is not skipped: that one is Emma's, and a user who creates
        // it has chosen a default harness for Emma deliberately.
        //
        // The skip also keeps `~/.claude/` out of `searched`, so the refusal
        // below never names a directory Emma would not have used — an error
        // listing a path it declined to consider reads as a bug in the search.
        //
        // The comparison is plain path equality, so it is exact: a `$HOME` that
        // does not match the ancestor byte for byte — a different spelling of
        // the same directory on Windows, say — leaves the skip inert and the
        // walk adopts whatever it finds.
        let names: &[&str] = if home.as_deref() == Some(dir) {
            &[ROOT_DIR_NAME]
        } else {
            &[ROOT_DIR_NAME, CLAUDE_DIR_NAME]
        };
        for name in names {
            let candidate = dir.join(name);
            if candidate.is_dir() {
                return Ok(candidate);
            }
            searched.push(format!("  {}", candidate.display()));
        }
    }
    // No compiled-in fallback prompt, on purpose: an agent that boots without
    // its configuration acts confidently from whatever it did have.
    bail!(
        "no `{ROOT_DIR_NAME}/` or `{CLAUDE_DIR_NAME}/` found. Searched, nearest first:\n{}",
        searched.join("\n")
    )
}

/// Read rather than pulled from a crate: this is used for one comparison and a
/// dependency would be a larger surface than the thing it replaces. `HOME` on
/// unix, `USERPROFILE` on Windows; absent means "no home", which makes the skip
/// above a no-op and is the safe direction — it searches more, never less.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

// endregion: Discovery

// region: The spine — .emma/config.json
// ---------------------------------------------------------------------------
// The spine — `.emma/config.json`
//
// The one file in the harness that is configuration rather than prompt text.
// It is strict everywhere, because it is Emma's own format and a key Emma does
// not recognise in it can only be a typo.
// ---------------------------------------------------------------------------

/// `deny_unknown_fields` throughout: a typo'd key is a load error, not a
/// silently ignored setting. A misspelled `mathcer` that quietly matched every
/// tool is the failure this costs one attribute to prevent.
///
/// (`.claude/settings.json` is deliberately *not* strict at its outer level —
/// it is a foreign format carrying keys Emma has no opinion about. See
/// `claude.rs`.)
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Spine {
    #[serde(default)]
    default_persona: Option<String>,
    #[serde(default)]
    personas: BTreeMap<String, PersonaBlock>,
    #[serde(default)]
    hooks: BTreeMap<String, HookDef>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersonaBlock {
    /// Prose for whoever reads the spine. Nothing branches on it.
    #[serde(default)]
    #[allow(dead_code)]
    description: Option<String>,
    /// A real allowlist. See `Harness::select_tools`.
    #[serde(default)]
    tools: Option<Vec<String>>,
    /// Absent means every skill in the pool; present means exactly these.
    #[serde(default)]
    skills: Option<Vec<String>>,
    #[serde(default)]
    hooks: Option<Vec<String>>,
}

// endregion: The spine — .emma/config.json

// region: Skills
// ---------------------------------------------------------------------------
// Skills
//
// Prompt text the model asks for by name rather than carries all the time. The
// harness resolves the pool; the loading of a body is a tool call, and lives in
// the binary.
// ---------------------------------------------------------------------------

/// One skill, resolved by the harness.
///
/// `hash` is over the body only, so it identifies the exact text a turn was
/// given, independent of how the catalogue described it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDef {
    pub name: String,
    pub description: String,
    pub body: String,
    pub hash: String,
}

impl SkillDef {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        let body = body.into();
        let hash = hash::short(&body);
        Self {
            name: name.into(),
            description: description.into(),
            body,
            hash,
        }
    }
}

// endregion: Skills

// region: The resolved value
// ---------------------------------------------------------------------------
// The resolved value
//
// `Harness` and its whole public surface — everything the turn loop is allowed
// to know about configuration. Nothing here reads a file; by this point the
// directory has already become a value.
// ---------------------------------------------------------------------------

/// What the loop consumes. Everything here is already decided.
#[derive(Debug)]
pub struct Harness {
    pub root: PathBuf,
    pub flavor: Flavor,
    /// `None` is the empty-harness boot, not an error.
    pub persona: Option<String>,
    /// The assembled system prompt, byte-exact. Empty when there is nothing to
    /// assemble.
    pub instructions: String,
    pub config_hash: String,
    skills: Vec<SkillDef>,
    commands: BTreeMap<String, String>,
    hooks: Vec<ResolvedHook>,
    tools: Option<Vec<String>>,
}

/// A command expansion. Both halves are kept so the log can record what the user
/// typed and what the model was actually asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expansion {
    pub command: String,
    pub raw: String,
    pub text: String,
}

impl Harness {
    /// Discover and load. The whole startup path.
    pub fn boot() -> Result<Self> {
        Self::load(discover()?)
    }

    /// Load a directory, reading it as whichever flavour its name says it is.
    pub fn load(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let flavor = Flavor::of(&root);
        Self::load_as(root, flavor)
    }

    /// For the caller who knows better than the directory name — a copied
    /// `.claude/` renamed, say. Nothing in Emma calls this; it exists so that
    /// `Flavor::of` never becomes a wall.
    pub fn load_as(root: impl AsRef<Path>, flavor: Flavor) -> Result<Self> {
        let selected = std::env::var(PERSONA_ENV)
            .ok()
            .filter(|s| !s.trim().is_empty());
        Self::load_selecting(root, flavor, selected)
    }

    /// The testable half, on the same principle as `discover_from`: the persona
    /// override is threaded rather than read, so a test does not have to mutate
    /// process environment — which in a threaded test binary is a race, and a
    /// race in a test is a green light nobody earned.
    pub fn load_selecting(
        root: impl AsRef<Path>,
        flavor: Flavor,
        selected: Option<String>,
    ) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let spine_path = root.join(flavor.spine_file());

        let (spine, raw, hook_defs) = match flavor {
            Flavor::Emma => {
                let raw = read_if_present(&spine_path)?;
                let spine: Spine = if raw.trim().is_empty() {
                    Spine::default()
                } else {
                    serde_json::from_str(&raw)
                        .with_context(|| format!("{} is malformed", spine_path.display()))?
                };
                let hooks = spine.hooks.clone();
                (spine, raw, hooks)
            }
            Flavor::Claude => {
                let (settings, raw) = claude::Settings::read(&root)?;
                let hooks = settings.into_hook_defs(&root)?;
                (Spine::default(), raw, hooks)
            }
        };

        let persona = select_persona(&root, flavor, &spine, &spine_path, selected)?;
        let (instructions, agent_tools) = assemble(&root, flavor, persona.as_deref())?;

        let block = match flavor {
            Flavor::Emma => persona
                .as_deref()
                .and_then(|p| spine.personas.get(p).cloned())
                .unwrap_or_default(),
            // A Claude agent file declares its tool list in frontmatter and has
            // no equivalent of `skills`/`hooks`, so the rest of the block is
            // default and the allowlist carries across.
            Flavor::Claude => PersonaBlock {
                tools: agent_tools,
                ..Default::default()
            },
        };

        Ok(Self {
            instructions,
            persona,
            config_hash: hash::short(&raw),
            skills: load_skills(&root, &spine_path, block.skills.as_deref())?,
            commands: load_commands(&root)?,
            hooks: hooks::resolve(&root, &spine_path, &hook_defs, block.hooks.as_deref())?,
            tools: block.tools,
            flavor,
            root,
        })
    }

    /// The digest the loop logs with every model call, so a prompt change is
    /// visible and any behaviour delta is attributable to it.
    pub fn instructions_hash(&self) -> String {
        hash::short(&self.instructions)
    }

    /// The empty-harness boot: Emma starts with no standing instructions.
    pub fn is_empty(&self) -> bool {
        self.instructions.is_empty()
    }

    /// Sorted by name, always.
    ///
    /// The catalogue rides in the prompt prefix, which the provider caches as an
    /// exact match, so directory iteration order — or the order a persona
    /// happened to list them in — must not decide the prompt bytes. A persona's
    /// `skills` array is therefore a **set**, not an ordering.
    pub fn skills(&self) -> &[SkillDef] {
        &self.skills
    }

    pub fn skill(&self, name: &str) -> Option<&SkillDef> {
        self.skills.iter().find(|s| s.name == name)
    }

    /// The names, in catalogue order — the closed enum a `load_skill` schema
    /// puts in its `name` argument. A tool that reads a model-supplied path is an
    /// arbitrary-file-read tool with a friendly description; a closed enum is
    /// what stops that.
    pub fn skill_names(&self) -> Vec<&str> {
        self.skills.iter().map(|s| s.name.as_str()).collect()
    }

    /// The catalogue lines a `load_skill` description is composed from, or
    /// `None` when there are no skills.
    ///
    /// `None` is the signal not to register the tool at all. Offering a
    /// capability that cannot work is a trap with a description attached, and
    /// returning an empty string instead would make that mistake invisible at
    /// the call site. The preamble is the tool's own prose and stays in the
    /// tool's crate; only the part that varies with the loaded harness is here.
    pub fn skill_catalog(&self) -> Option<String> {
        if self.skills.is_empty() {
            return None;
        }
        let mut out = String::new();
        for s in &self.skills {
            out.push_str(&format!("\n- `{}` — {}", s.name, s.description.trim()));
        }
        out.push('\n');
        Some(out)
    }

    /// The persona's tool allowlist, if it declared one. `None` means every
    /// registered tool.
    pub fn tools(&self) -> Option<&[String]> {
        self.tools.as_deref()
    }

    /// Apply the allowlist to a registry, consuming it.
    ///
    /// **This is a real filter, and in the predecessor it was not.** There the
    /// same field validated that each named tool existed and then removed nothing —
    /// four sentences of documentation, three of them warning the reader it was
    /// not access control. That was survivable because every tool there was a
    /// read-only search. Emma runs `Bash` and `Write`. An operator who writes
    /// `"tools": ["Read", "Grep"]` and gets `Bash` anyway has been handed a
    /// permission boundary that is a comment, at the one place in the system
    /// where the failure costs them their working tree.
    ///
    /// The startup assertion is kept as well: a name the binary does not
    /// register is a load error here, not a silent narrowing of the allowlist to
    /// a typo. Both halves matter — filtering without asserting turns `"Bahs"`
    /// into "no shell for you" with no explanation.
    ///
    /// It takes the registry by value so the caller cannot keep the unfiltered
    /// one around by accident. Getting that wrong is the whole failure this
    /// method exists to prevent.
    pub fn select_tools(&self, registry: Registry) -> Result<Registry> {
        let Some(allowed) = self.tools.as_deref() else {
            return Ok(registry);
        };
        let registered = registry.names();
        for want in allowed {
            if !registered.contains(&want.as_str()) {
                bail!(
                    "{}: persona names tool `{want}`, which this binary does not \
                     register (has: {})",
                    self.root.join(self.flavor.spine_file()).display(),
                    registered.join(", ")
                );
            }
        }
        let mut out = Registry::new();
        // Registry order, not allowlist order: the tool schema rides in the
        // cached prompt prefix, so the bytes must not depend on how the operator
        // happened to type the list.
        for tool in registry.iter() {
            if allowed.iter().any(|a| a == tool.name()) {
                out.register(tool.clone());
            }
        }
        Ok(out)
    }

    /// Server-side expansion at intake. `None` for anything that is not a known
    /// command, including an unknown `/word`, which passes through as ordinary
    /// text. The model never learns commands exist.
    pub fn expand_command(&self, raw: &str) -> Option<Expansion> {
        let rest = raw.strip_prefix('/')?;
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let (name, tail) = rest.split_at(end);
        let body = self.commands.get(name)?;
        let tail = tail.trim();
        Some(Expansion {
            command: name.to_string(),
            raw: raw.to_string(),
            text: if tail.is_empty() {
                body.clone()
            } else {
                format!("{body}\n\n{tail}")
            },
        })
    }

    pub fn command_names(&self) -> Vec<&str> {
        self.commands.keys().map(String::as_str).collect()
    }

    /// What startup logs and a `config check` prints: identity only. A snapshot
    /// carrying prompt text would be a second copy to drift.
    pub fn snapshot(&self) -> serde_json::Value {
        let hooks: Vec<_> = self.hooks.iter().map(ResolvedHook::identity).collect();
        let skills: Vec<_> = (self.skills.iter())
            .map(|s| serde_json::json!({ "name": s.name, "hash": s.hash }))
            .collect();
        serde_json::json!({
            "root": self.root.display().to_string(),
            "flavor": self.flavor,
            "persona": self.persona,
            "instructions_hash": self.instructions_hash(),
            "config_hash": self.config_hash,
            "empty": self.is_empty(),
            "tools": self.tools,
            "skills": skills,
            "commands": self.commands.keys().collect::<Vec<_>>(),
            "hooks": hooks,
        })
    }

    /// Run every hook attached to `event` that matches this call. The dispatch
    /// site the loop calls; the supervision, the fail-closed/fail-open asymmetry
    /// and every pre-flight check live in `hooks.rs`.
    pub async fn run_hooks(&self, event: HookEvent, call: &HookCall<'_>) -> HookVerdict {
        hooks::run(&self.hooks, event, call).await
    }
}

// endregion: The resolved value

// region: Loading
// ---------------------------------------------------------------------------
// Loading
//
// The private half: the filesystem reads and the refusals. Every function here
// either produces a value the section above can hold or fails the boot, and the
// choice between those two is the design decision in each one.
// ---------------------------------------------------------------------------

fn read_if_present(path: &Path) -> Result<String> {
    if !path.is_file() {
        return Ok(String::new());
    }
    std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
}

fn select_persona(
    root: &Path,
    flavor: Flavor,
    spine: &Spine,
    spine_path: &Path,
    selected: Option<String>,
) -> Result<Option<String>> {
    // The override wins over the spine's default. Selection is a deployment
    // decision either way; the model is never given a way to make it.
    let chosen = selected.or_else(|| spine.default_persona.clone());

    match flavor {
        Flavor::Emma => {
            let dir = root.join("personas");
            let mut available: Vec<String> = Vec::new();
            if dir.is_dir() {
                for entry in
                    std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))?
                {
                    let entry = entry?;
                    let name = entry.file_name().to_string_lossy().into_owned();
                    // `_shared` is not a persona and can never be selected.
                    if name != "_shared" && entry.path().is_dir() {
                        available.push(name);
                    }
                }
            }
            available.sort();
            match chosen {
                Some(name) if dir.join(&name).is_dir() => Ok(Some(name)),
                Some(name) => bail!(
                    "{}: persona `{name}` is selected but {} does not exist (have: {})",
                    spine_path.display(),
                    dir.join(&name).display(),
                    available.join(", ")
                ),
                // Persona content that nothing selects is the silent-drop case:
                // the operator wrote a prompt and Emma would have started
                // without it. An empty harness is deliberate; an unselected one
                // is an accident.
                None if !available.is_empty() => bail!(
                    "{}: no `default_persona`, but {} contains {}",
                    spine_path.display(),
                    dir.display(),
                    available.join(", ")
                ),
                None => Ok(None),
            }
        }
        // **A deliberate divergence, and the one place the ported rule was wrong
        // for Emma rather than merely different.** In `.emma/` a persona
        // directory nobody selected is an accident, because personas *are* the
        // prompt. In `.claude/` the prompt is `CLAUDE.md`, and `agents/` holds
        // optional sub-agent definitions — a normal repository has a dozen and
        // selects none. Applying the refusal here would mean Emma declines to
        // start in almost every Claude Code checkout, which is not a safety
        // property, it is an outage. Selecting an agent that does not exist is
        // still a refusal: that one really is a mistake.
        Flavor::Claude => {
            let available = claude::agents(root)?;
            match chosen {
                Some(name) if claude::agent_path(root, &name).is_file() => Ok(Some(name)),
                Some(name) => bail!(
                    "{PERSONA_ENV}: agent `{name}` is selected but {} does not exist (have: {})",
                    claude::agent_path(root, &name).display(),
                    available.join(", ")
                ),
                None => Ok(None),
            }
        }
    }
}

/// Fixed order, shared first, absent files skipped, and **nothing trimmed or
/// re-wrapped** — so with one file present the assembled text is that file's
/// bytes. That is what makes a prompt hash comparable across a refactor.
///
/// `.emma/`: `_shared/rules.md`, `_shared/business.md`, `<persona>/rules.md`,
/// `<persona>/soul.md`. A persona file cannot remove a shared rule, because
/// assembly is plain concatenation — the shared text is still there for a
/// reviewer to see. Overriding `_shared` is a review on `_shared`. It is a
/// review discipline, not an enforcement mechanism.
///
/// `.claude/`: the project's `CLAUDE.md` (beside the directory, where Claude
/// Code puts it), then `.claude/CLAUDE.md`, then the selected agent's body.
/// Same rule, same order-is-fixed property.
///
/// Returns the tool allowlist alongside, because in the Claude flavour it is
/// carried in the same file as the prompt layer and reading that file twice
/// would be two chances to disagree.
fn assemble(
    root: &Path,
    flavor: Flavor,
    persona: Option<&str>,
) -> Result<(String, Option<Vec<String>>)> {
    let mut parts: Vec<String> = Vec::new();
    let mut tools = None;

    match flavor {
        Flavor::Emma => {
            let Some(persona) = persona else {
                return Ok((String::new(), None));
            };
            let personas = root.join("personas");
            let (shared, own) = (personas.join("_shared"), personas.join(persona));
            for path in [
                shared.join("rules.md"),
                shared.join("business.md"),
                own.join("rules.md"),
                own.join("soul.md"),
            ] {
                if let Some(text) = read_layer(&path)? {
                    parts.push(text);
                }
            }
        }
        Flavor::Claude => {
            let project = root.parent().unwrap_or(root);
            for path in [project.join("CLAUDE.md"), root.join("CLAUDE.md")] {
                if let Some(text) = read_layer(&path)? {
                    parts.push(text);
                }
            }
            if let Some(name) = persona {
                let path = claude::agent_path(root, name);
                let raw = std::fs::read_to_string(&path)
                    .with_context(|| format!("reading {}", path.display()))?;
                let (front, body) = claude::split_agent(&raw);
                tools = front.tools.map(claude::Tools::into_vec);
                if !body.is_empty() {
                    parts.push(body.to_string());
                }
            }
        }
    }
    Ok((parts.join("\n\n"), tools))
}

/// A file that exists but is empty is skipped, so it cannot contribute a stray
/// blank-line separator to the assembled bytes.
fn read_layer(path: &Path) -> Result<Option<String>> {
    if !path.is_file() {
        return Ok(None);
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Ok((!text.is_empty()).then_some(text))
}

/// Only `name` and `description` are read. A skill is markdown; anything the
/// runtime must branch on belongs in the spine.
///
/// Both are required and `deny_unknown_fields` is on, so this is strict in both
/// directions: a `SKILL.md` missing either, or carrying any third key, fails the
/// load and takes the whole boot with it. That is the right answer for a skill
/// written for Emma, where a stray key is a typo. It is a sharper edge than it
/// looks for a `.claude/skills/` directory, where files in the wild routinely
/// carry `allowed-tools`, `version`, `model-role` and a licence header above the
/// frontmatter — none of which Emma reads, all of which stop it starting.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Front {
    name: String,
    description: String,
}

fn load_skills(root: &Path, spine_path: &Path, elected: Option<&[String]>) -> Result<Vec<SkillDef>> {
    let dir = root.join("skills");
    // A `BTreeMap` keyed on the front-matter name gives the sorted catalogue the
    // prompt prefix needs, whatever order the directory iterated in.
    let mut found: BTreeMap<String, SkillDef> = BTreeMap::new();
    if dir.is_dir() {
        for entry in std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))?
        {
            // A subdirectory without a `SKILL.md` is skipped without complaint,
            // and a loose file in `skills/` is ignored.
            let path = entry?.path().join("SKILL.md");
            if !path.is_file() {
                continue;
            }
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let (front, body) = split_skill(&text, &path)?;
            found.insert(
                front.name.clone(),
                SkillDef::new(front.name, front.description, body),
            );
        }
    }
    let Some(elected) = elected else {
        return Ok(found.into_values().collect());
    };
    let mut out = BTreeMap::new();
    for want in elected {
        let skill = found.remove(want).with_context(|| {
            format!(
                "{}: persona elects skill `{want}`, which {} does not contain",
                spine_path.display(),
                dir.display()
            )
        })?;
        out.insert(skill.name.clone(), skill);
    }
    Ok(out.into_values().collect())
}

fn split_skill(text: &str, path: &Path) -> Result<(Front, String)> {
    let named = |what: &str| format!("{}: {what}", path.display());
    let rest = text
        .strip_prefix("---\n")
        .with_context(|| named("expected YAML frontmatter"))?;
    let end = rest
        .find("\n---")
        .with_context(|| named("frontmatter is not closed"))?;
    let front: Front =
        serde_yaml::from_str(&rest[..end]).with_context(|| named("bad frontmatter"))?;
    Ok((front, rest[end + 4..].trim_start().to_string()))
}

fn load_commands(root: &Path) -> Result<BTreeMap<String, String>> {
    let dir = root.join("commands");
    let mut out = BTreeMap::new();
    if !dir.is_dir() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let name = path.file_stem().unwrap_or_default().to_string_lossy();
        let body = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        out.insert(name.into_owned(), body.trim().to_string());
    }
    Ok(out)
}

// endregion: Loading
