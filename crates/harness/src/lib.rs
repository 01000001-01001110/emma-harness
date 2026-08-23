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
//! `.claude/settings.json` and at `.claude/` skill frontmatter (`claude.rs`,
//! `ClaudeFront`). The through-line is that the earlier
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
//! "Absent" is decided in `discover_in`, and a directory existing is not enough
//! to make it present: in the home directory a `.emma/` holding only credentials
//! is walked past rather than adopted, because `emma api` creates it and nobody
//! chose it. There it is the absent case, not the empty one.
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
//! `notes/design/claude-code-compatibility.md`.

mod claude;
pub mod hash;
mod hooks;
mod statusline;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub use crate::hooks::{
    HookCall, HookEvent, HookOutcome, HookResult, HookRun, HookVerdict, PromptCall, PromptVerdict,
};
use crate::hooks::{HookDef, ResolvedHook};
pub use crate::statusline::{
    StatusContext, StatusCost, StatusLine, StatusModel, StatusPayload, StatusWorkspace,
};

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
/// The harness directory this one shadowed, if it shadowed one.
///
/// **Precedence is not the defect; the silence was.** `discover_from`'s doc
/// states the rule outright — within one directory `.emma/` beats `.claude/`
/// outright, and the loser is ignored entirely rather than merged — with the
/// argument that merging is how a configuration system becomes impossible to
/// reason about. That reasoning stands and this does not touch it.
///
/// What it adds is a sentence. Certified against the real binary: a `.emma/`
/// holding nothing but junk is chosen over a sibling `.claude/` containing a
/// real skill, and the result is `skills (none)` with nothing said. Every other
/// loser in this codebase is announced — an unevaluable permission rule, a
/// skipped skill, a dropped hook, a duplicate name — and a whole ignored
/// configuration directory is the largest silent skip in the system, taking
/// skills, agents, commands, hooks and permissions with it at once.
pub fn shadowed_by(root: &Path) -> Option<PathBuf> {
    // Only the `.emma/`-over-`.claude/` case exists: those are the two names,
    // and the other order cannot happen because `.emma/` is tried first.
    if root.file_name()?.to_str()? != ROOT_DIR_NAME {
        return None;
    }
    let sibling = root.parent()?.join(CLAUDE_DIR_NAME);
    sibling.is_dir().then_some(sibling)
}

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
    discover_in(start, overridden, home_dir())
}

/// The same walk with the home directory threaded in as well, so the result does
/// not depend on the environment of the process that called it.
///
/// It exists because `discover_from` did not thread it and every test of the
/// home-scope rules below therefore either mutated `HOME` — shared state in a
/// binary cargo runs threaded — or walked out of its scratch directory into the
/// developer's real home and answered differently on different machines. Both of
/// those happened, and the second one is how the `~/.emma/` defect below was
/// found. `discover_from` is kept and delegates here; it is the signature the
/// binary calls.
pub fn discover_in(
    start: &Path,
    overridden: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Result<PathBuf> {
    if let Some(p) = overridden {
        if p.is_dir() {
            return Ok(p);
        }
        // An override that fell back to a search would boot the agent on
        // configuration nobody asked for.
        bail!("{ROOT_ENV}=`{}` is not a directory", p.display());
    }
    let home = home.map(|h| real(&h));
    let mut searched = Vec::new();
    for dir in start.ancestors() {
        // The home directory gets different rules, and this is the comparison
        // that decides whether they apply. It canonicalises both sides rather
        // than comparing bytes, because "is this the same directory" is a
        // question for the filesystem and not for a string: case is only one of
        // the ways two spellings of one directory differ, alongside a trailing
        // separator, a `..` in the middle, a Windows 8.3 short name, and a home
        // reached through a symlink. A case-insensitive compare would fix the one
        // that bit us — `C:\Users\Owner` against `C:\Users\owner`, which left the
        // skip below inert and adopted the user's global `.claude/` — and leave
        // the rest. `real` falls back to the path as given when it cannot ask, so
        // a home that does not exist is still compared, just exactly.
        let at_home = home.as_deref() == Some(real(dir).as_path());

        // `~/.claude/` is Claude Code's **user-scope** configuration — global
        // permissions, global agents, global skills, for a different program.
        // Adopting it as this project's harness would hand Emma standing
        // instructions from a directory the user never associated with this
        // project, which is the "booted on the wrong prompt" failure arriving
        // through the front door. It is skipped, and skipped so completely that
        // it never reaches `searched`: an error listing a path Emma declined to
        // consider reads as a bug in the search.
        let names: &[&str] = if at_home {
            &[ROOT_DIR_NAME]
        } else {
            &[ROOT_DIR_NAME, CLAUDE_DIR_NAME]
        };
        for name in names {
            let candidate = dir.join(name);
            // `~/.emma/` is Emma's own, so it is eligible — but only when it
            // holds something a person put there on purpose.
            //
            // It used to be eligible for existing, and `emma api` creates it:
            // storing a credential wrote `~/.emma/credentials.json`, and from
            // that moment every project without a harness of its own adopted the
            // home directory and booted with no instructions, no persona and the
            // full write-capable tool surface. Nobody chose that; it was the side
            // effect of saving a key. **The thing that makes a directory a
            // harness has to be the thing a person put there on purpose** —
            // `config.json` or `personas/`. Credentials and `sessions/` are
            // Emma's bookkeeping and say nothing about how Emma is configured.
            //
            // Note what this is *not*: it is not the empty-harness boot. An
            // `~/.emma/` holding only credentials is not a harness at all, the
            // walk goes past it, and the operator gets the absent-harness
            // refusal rather than a silent boot on nothing.
            let eligible = !at_home || configured_by_hand(&candidate);
            if candidate.is_dir() && eligible {
                return Ok(candidate);
            }
            searched.push(match candidate.is_dir() {
                // Named anyway, and named with the reason. The operator can see
                // the directory; a refusal that omitted it would read as a search
                // that never looked.
                true => format!(
                    "  {} (present, but holds no config.json or personas/, so it is \
                     not a harness)",
                    candidate.display()
                ),
                false => format!("  {}", candidate.display()),
            });
        }
        // The home directory is the top of the search. Above it are `C:\Users`
        // and `/home` — directories that belong to the machine rather than to any
        // project, so a harness adopted from one of them is the same wrong-prompt
        // boot with a longer walk. It also keeps discovery's answer a function of
        // the tree the caller named.
        if at_home {
            break;
        }
    }
    // No compiled-in fallback prompt, on purpose: an agent that boots without
    // its configuration acts confidently from whatever it did have.
    bail!(
        "no `{ROOT_DIR_NAME}/` or `{CLAUDE_DIR_NAME}/` found. Searched, nearest first:\n{}",
        searched.join("\n")
    )
}

/// What makes a directory in the home directory a harness rather than a place
/// Emma keeps its own files. Deliberately narrow: these are the two things that
/// only exist because someone wrote them.
///
/// The three files that turn up in `~/.emma/` and are **not** counted:
/// `credentials.json` and `sessions/`, which Emma writes for itself, and
/// `settings.json`, which is the person's own model preference and whose own
/// module doc says it is deliberately not the harness. A personal default for
/// one field is not a statement about what Emma should be told.
fn configured_by_hand(dir: &Path) -> bool {
    dir.join("config.json").is_file() || dir.join("personas").is_dir()
}

/// The filesystem's own answer for a path, for comparing two of them. Falls back
/// to the path as given when the path does not exist or cannot be read, which
/// makes the comparison exact again rather than wrong.
fn real(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
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
    /// The same block `.claude/settings.json` spells `statusLine`, so Emma's own
    /// format can express it too. Deliberately *not* renamed to a prettier
    /// snake_case spelling: the inner keys are Claude Code's (`type`, `command`,
    /// `padding`) and a block that had to be re-typed to move between the two
    /// directories would defeat the reason the feature was implemented.
    #[serde(default, rename = "statusLine")]
    status_line: Option<crate::statusline::StatusLineBlock>,
    /// The same `permissions` block `.claude/settings.json` carries, so a
    /// project that chose `.emma/` can express standing grants too. Deliberately
    /// not renamed: the rule strings are Claude Code's and so are the three list
    /// names, and a block that had to be re-typed to move between the two
    /// directories would defeat the reason it is read at all.
    #[serde(default)]
    permissions: PermissionBlock,
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

// region: Agent types
// ---------------------------------------------------------------------------
// Agent types
//
// The same `agents/<name>.md` files a persona can be selected from, read for a
// second and independent purpose: as things a *model* may delegate to. The file
// format is Claude Code's and is not Emma's to change — `.emma/agents/` uses
// it too, and `.emma/` still wins outright where both directories exist,
// because `discover` already resolved that and this reads whatever it chose.
//
// The two uses share one format and have opposite selection rules, which is why
// they have separate accessors and no function takes a boolean saying which it
// is: `Harness::persona` is chosen by a human at startup, and an `AgentDef` is
// chosen by the model at call time from a closed list. `PERSONA_ENV`'s comment —
// *a model choosing its own persona is a self-modifying prompt* — is what makes
// that distinction load-bearing rather than tidy.
// ---------------------------------------------------------------------------

/// One delegation target, resolved from `agents/<name>.md`.
///
/// Every field here is consumed by `emma::delegate`, which is the rule this
/// struct is written to: `name` and `description` compose the tool's catalogue
/// and its closed enum, `instructions` is the sub-run's system prompt, `tools`
/// selects its registry, `model` picks its provider, and the two budget fields
/// bound what it may spend. A field nobody reads is a declaration pretending to
/// be a mechanism.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDef {
    /// The file stem. See `claude::agents` for why it beats a `name:` in
    /// frontmatter.
    pub name: String,
    /// What the calling model reads to decide whether to delegate here.
    pub description: String,
    /// The body of the file: the sub-run's standing instructions.
    pub instructions: String,
    /// `None` means "inherit whatever the caller has", which is Claude Code's
    /// meaning for an absent list and — deliberately — for an empty one too.
    /// See `claude::Tools::allowlist`.
    pub tools: Option<Vec<String>>,
    /// A model id, honoured rather than ignored.
    pub model: Option<String>,
    pub max_turns: Option<u32>,
    pub max_tokens: Option<i64>,
}

// endregion: Agent types

// region: Permission rules
// ---------------------------------------------------------------------------
// Permission rules
//
// The harness reads them; it does not understand them. Every string here goes
// out to `emma::permissions` exactly as it was written, tagged with the list it
// was in and the file it came from — because the two sentences an operator needs
// when a rule does not fire are "which file" and "which list", and a merged
// `Vec<String>` can answer neither.
//
// The scopes and the order are stated here because this is the only place that
// knows them. Nearest wins is *not* how these compose: permission lists **merge
// across scopes** rather than override, the way Claude Code documents, and the
// merge is safe precisely because `deny` beats `allow` at match time — so a
// restrictive rule from any file survives a permissive rule in any other.
// ---------------------------------------------------------------------------

/// The `permissions` object as it appears on disk.
///
/// Permissive on purpose: `defaultMode`, `additionalDirectories`,
/// `disableBypassPermissionsMode` and whatever Claude Code adds next are keys
/// Emma has no opinion about, and `deny_unknown_fields` here would refuse to
/// boot in a working repository.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct PermissionBlock {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
    #[serde(default)]
    pub ask: Vec<String>,
}

/// Which of the three lists a rule was in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionKind {
    Allow,
    Deny,
    Ask,
}

impl PermissionKind {
    /// For a sentence. `"a {} rule"`.
    pub fn word(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
            Self::Ask => "ask",
        }
    }
}

/// One rule, still a string, with everything needed to talk about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionEntry {
    pub rule: String,
    pub kind: PermissionKind,
    /// The file it was written in. Carried per entry rather than per file
    /// because rules merge across scopes, so by the time one misfires there is
    /// no other way back to the document that holds it.
    pub source: PathBuf,
}

impl PermissionBlock {
    fn into_entries(self, source: &Path, entries: &mut Vec<PermissionEntry>) {
        // Deny first, then ask, then allow — the same order they are consulted
        // in. Nothing depends on it; it means a debug print of this vector reads
        // in precedence order, which is one fewer thing to hold in your head.
        for (kind, list) in [
            (PermissionKind::Deny, self.deny),
            (PermissionKind::Ask, self.ask),
            (PermissionKind::Allow, self.allow),
        ] {
            for rule in list {
                entries.push(PermissionEntry {
                    rule,
                    kind,
                    source: source.to_path_buf(),
                });
            }
        }
    }
}

/// The file a `settings.local.json` block is read from and written back to.
///
/// One name in both flavours. See `emma::permissions::file_for` for why a grant
/// lands beside the harness that actually loaded rather than always in
/// `.claude/`.
pub const LOCAL_SETTINGS_FILE: &str = "settings.local.json";

/// The project's rules: the spine file's block, then `settings.local.json`.
///
/// **A malformed `settings.local.json` fails the boot**, unlike a rule Emma
/// cannot evaluate — which is only a note. The two are different mistakes. An
/// unevaluatable rule is a file written correctly for a program with a larger
/// vocabulary; unparseable JSON is a file that says nothing at all, and booting
/// past it would mean running with a `deny` list the operator believes is in
/// force. This is the same ruling the crate makes everywhere else: malformed
/// configuration refuses to start, naming the file and the problem.
fn read_permissions(
    root: &Path,
    spine: PermissionBlock,
    spine_path: &Path,
) -> Result<Vec<PermissionEntry>> {
    let mut entries = Vec::new();
    spine.into_entries(spine_path, &mut entries);
    let local = root.join(LOCAL_SETTINGS_FILE);
    let raw = read_if_present(&local)?;
    if !raw.trim().is_empty() {
        #[derive(Deserialize)]
        struct Local {
            #[serde(default)]
            permissions: PermissionBlock,
        }
        let parsed: Local = serde_json::from_str(&raw)
            .with_context(|| format!("{} is malformed", local.display()))?;
        parsed.permissions.into_entries(&local, &mut entries);
    }
    Ok(entries)
}

/// The **user scope**: `~/.claude/settings.json`, and `deny` rules only.
///
/// **This is a deliberate divergence from Claude Code, and it is the one
/// judgement call in the feature.** There, user settings carry `allow`, `deny`
/// and `ask`, and all three apply in every project. Here only `deny` crosses,
/// for the reason `discover_in` already refuses to adopt `~/.claude/` as a
/// harness: it is *another program's* global configuration, and a grant made
/// there was made for that program, in a session Emma was not part of, possibly
/// years ago.
///
/// This is not hypothetical. The owner's own `~/.claude/settings.json`, at the
/// time this was written, carries `Bash(rm:*)`, `Bash(bash:*)` and
/// `Bash(powershell.exe:*)` in its `allow` list, plus `"defaultMode":
/// "dontAsk"`. Honouring user-scope `allow` would have handed Emma an
/// unprompted shell in every repository on the machine, granted by nobody, on
/// the first run after this feature shipped.
///
/// `deny` is safe in the other direction and is imported for exactly that
/// reason: it only ever removes a capability, it cannot be the mechanism of a
/// surprise, and a host somebody blocked globally is a host they meant. `ask`
/// is left out with `allow` rather than with `deny` — it is not restrictive
/// enough to be worth importing and not permissive enough to be dangerous, and
/// a rule that only prompts more is not worth a second scope to explain.
///
/// `home` is threaded rather than read from the environment, for the reason
/// `discover_in` threads it: a test that reads the real `HOME` either mutates
/// shared state in a threaded test binary or answers differently on different
/// machines, and both have happened here.
/// Returns the entries and any notes the caller must show. The notes vector
/// follows `load_agents`: a configuration problem that is not fatal still has to
/// reach a human, and the caller owns where it is printed.
pub fn user_permissions(home: Option<&Path>) -> Result<(Vec<PermissionEntry>, Vec<String>)> {
    let Some(home) = home else {
        return Ok((Vec::new(), Vec::new()));
    };
    let file = home.join(CLAUDE_DIR_NAME).join("settings.json");
    // Not `?`, for the same reason the parse below is not: a file this project
    // does not own must not decide whether Emma starts. An unreadable one — bad
    // ACL, a locked file, an I/O fault — is exactly as silent as a malformed one
    // was, and reaches the operator the same way.
    let raw = match read_if_present(&file) {
        Ok(raw) => raw,
        Err(e) => {
            return Ok((
                Vec::new(),
                vec![format!(
                    "{} could not be read, so none of its `deny` rules are in force: {e:#}. \
                     Emma started anyway, but nothing in that file is protecting you.",
                    file.display()
                )],
            ));
        }
    };
    if raw.trim().is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    #[derive(Deserialize)]
    struct User {
        #[serde(default)]
        permissions: PermissionBlock,
    }
    // Never `?`. A personal settings file belonging to another program is not
    // this project's to refuse to start over — and the only thing taken from it
    // is restrictions, so failing to read it can only leave Emma asking more
    // often.
    //
    // **But it must never be quiet about it.** That argument covers refusing to
    // *boot*; it does not license silence. The operator wrote deny rules and
    // believes they apply, and until this note existed the only report of their
    // absence was `config check` printing "(none)" — which reads as "you have no
    // rules", not as "your rules could not be read". One trailing comma was
    // enough. The sibling case is already loud: `read_permissions` fails the boot
    // on a malformed *project* settings file, so the quiet path here was
    // inconsistent with the function above it rather than a considered exception.
    let parsed = match serde_json::from_str::<User>(&raw) {
        Ok(parsed) => parsed,
        Err(e) => {
            return Ok((
                Vec::new(),
                vec![format!(
                    "{} could not be parsed, so none of its `deny` rules are in force: {e}. \
                     Emma started anyway — a file belonging to another program is not Emma's to \
                     refuse to start over — but nothing in it is protecting you. Fix the file, \
                     or delete it if it is no longer wanted.",
                    file.display()
                )],
            ));
        }
    };
    let mut entries = Vec::new();
    PermissionBlock {
        deny: parsed.permissions.deny,
        ..Default::default()
    }
    .into_entries(&file, &mut entries);
    Ok((entries, Vec::new()))
}

// endregion: Permission rules

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
    agents: Vec<AgentDef>,
    agent_notes: Vec<String>,
    /// What the skill loader had to say: duplicate names, and how many files
    /// were skipped. Returned rather than only printed, so a test can see them.
    skill_notes: Vec<String>,
    command_notes: Vec<String>,
    /// The configured status-line program, when one resolved.
    status_line: Option<StatusLine>,
    /// Why there is not one, when configuration asked for something Emma could
    /// not honour. A sentence rather than a boot failure — see `statusline.rs`.
    status_line_note: Option<String>,
    /// The harness directory this one shadowed, named so the operator is told
    /// that a whole configuration was passed over rather than merged.
    shadowed_note: Option<String>,
    /// Project-scope permission rules, still as strings. See the region above
    /// for why they are not parsed here.
    permissions: Vec<PermissionEntry>,
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

        let (spine, raw, hook_defs, status_block, permission_block) = match flavor {
            Flavor::Emma => {
                let raw = read_if_present(&spine_path)?;
                let spine: Spine = if raw.trim().is_empty() {
                    Spine::default()
                } else {
                    serde_json::from_str(&raw)
                        .with_context(|| format!("{} is malformed", spine_path.display()))?
                };
                let hooks = spine.hooks.clone();
                let status = spine.status_line.clone();
                let permissions = spine.permissions.clone();
                (spine, raw, hooks, status, permissions)
            }
            Flavor::Claude => {
                let (mut settings, raw) = claude::Settings::read(&root)?;
                // Taken before the hooks block is consumed, and separately from
                // it: a status line that cannot be resolved must not be able to
                // stop a hook from resolving, or the other way round. The
                // permissions block is taken on the same rule and for the same
                // reason — one unreadable key must not cost another.
                let status = settings.status_line.take();
                let permissions = std::mem::take(&mut settings.permissions);
                let hooks = settings.into_hook_defs(&root)?;
                (Spine::default(), raw, hooks, status, permissions)
            }
        };

        // Never `?`. A hook that will not resolve stops the boot because a
        // policy the operator believes they have is worse than none; the bottom
        // row of the screen is decoration, and refusing to start over it would
        // be an outage. The sentence is kept and shown instead.
        let (status_line, status_line_note) = match StatusLine::resolve(&root, status_block) {
            Ok(found) => (found, None),
            Err(why) => (
                None,
                Some(format!(
                    "{}: {why}. The built-in status line is being drawn instead.",
                    spine_path.display()
                )),
            ),
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

        // Sorted by name, for the same reason `skills()` is: the catalogue rides
        // in the cached prompt prefix and directory iteration order must not
        // decide the prompt bytes. `claude::agents` sorts, and this preserves it.
        let (agents, agent_notes) = claude::load_agents(&root)?;
        let (commands, command_notes) = load_commands(&root)?;
        let (skills, skill_notes) =
            load_skills(&root, &spine_path, flavor, block.skills.as_deref())?;

        Ok(Self {
            instructions,
            persona,
            config_hash: hash::short(&raw),
            skills,
            skill_notes,
            commands,
            command_notes,
            hooks: hooks::resolve(&root, &spine_path, &hook_defs, block.hooks.as_deref())?,
            tools: block.tools,
            agents,
            agent_notes,
            status_line,
            status_line_note,
            shadowed_note: shadowed_by(&root).map(|other| {
                format!(
                    "{} is being used, so {} is ignored entirely — its skills, agents, commands, \
                     hooks and permissions are all passed over rather than merged",
                    root.display(),
                    other.display()
                )
            }),
            permissions: read_permissions(&root, permission_block, &spine_path)?,
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

    /// Every delegation target this harness resolved, sorted by name.
    ///
    /// Deliberately a different accessor from [`Harness::persona`] even though
    /// both come from `agents/`: one file format, two selection rules, and the
    /// cheap guard against the rule from one leaking into the other is that no
    /// function serves both. See the region comment on [`AgentDef`].
    pub fn agent_types(&self) -> &[AgentDef] {
        &self.agents
    }

    /// Agent files that were found and not offered, and why — one sentence each.
    ///
    /// Read by `main` at startup and by `emma config check`. A catalogue quietly
    /// shorter than the directory is the gap nobody notices until the model
    /// cannot find an agent that is plainly there.
    /// What `load_commands` passed over. Beside [`Harness::skill_notes`],
    /// because they are the same claim about a different directory.
    pub fn command_notes(&self) -> &[String] {
        &self.command_notes
    }

    pub fn skill_notes(&self) -> &[String] {
        &self.skill_notes
    }

    pub fn agent_notes(&self) -> &[String] {
        &self.agent_notes
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
        // `$ARGUMENTS` where the author put it, appended only when they did not
        // ask. 37 of the 116 real commands on this machine use the placeholder,
        // and every one of them was getting its arguments pasted at the end
        // instead — so a command reading "review the file $ARGUMENTS and report"
        // sent the model that sentence with the placeholder still in it, and the
        // filename tacked on two lines below.
        const PLACEHOLDER: &str = "$ARGUMENTS";
        let text = if body.contains(PLACEHOLDER) {
            body.replace(PLACEHOLDER, tail)
        } else if tail.is_empty() {
            body.clone()
        } else {
            format!("{body}\n\n{tail}")
        };
        Some(Expansion {
            command: name.to_string(),
            raw: raw.to_string(),
            text,
        })
    }

    pub fn command_names(&self) -> Vec<&str> {
        self.commands.keys().map(String::as_str).collect()
    }

    /// The configured status-line program, or `None` for the built-in status.
    /// See `statusline.rs` for what it is allowed to be and why.
    pub fn status_line(&self) -> Option<&StatusLine> {
        self.status_line.as_ref()
    }

    /// Why configuration asked for a status line and did not get one. `None`
    /// when nothing was asked for, or when what was asked for resolved.
    pub fn shadowed_note(&self) -> Option<&str> {
        self.shadowed_note.as_deref()
    }

    pub fn status_line_note(&self) -> Option<&str> {
        self.status_line_note.as_deref()
    }

    /// The project's permission rules, unparsed. See the region above.
    pub fn permissions(&self) -> &[PermissionEntry] {
        &self.permissions
    }

    /// Where a rule the user asks to be remembered is written.
    ///
    /// Beside whichever directory this harness actually loaded, so answering a
    /// prompt in a `.emma/` project cannot conjure a `.claude/` directory for a
    /// program that is not running.
    pub fn permissions_file(&self) -> PathBuf {
        self.root.join(LOCAL_SETTINGS_FILE)
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
            "agents": self.agents.iter().map(|a| &a.name).collect::<Vec<_>>(),
            "agent_notes": self.agent_notes,
            "commands": self.commands.keys().collect::<Vec<_>>(),
            "hooks": hooks,
            // The rule text and the list it is in, never a resolved verdict: the
            // question a reader has is "what is written down", and a snapshot
            // that showed conclusions would be a second copy of the matcher to
            // drift away from the first.
            "permissions": self.permissions.iter().map(|p| {
                serde_json::json!({ "rule": p.rule, "kind": p.kind.word(),
                                    "source": p.source.display().to_string() })
            }).collect::<Vec<_>>(),
            // Identity of the program, never its text — the same rule a hook
            // follows, and for the same reason: a log that records a name
            // cannot answer "was this the program that ran".
            "status_line": self.status_line.as_ref().map(StatusLine::identity),
        })
    }

    /// Run every hook attached to `event` that matches this call. The dispatch
    /// site the loop calls; the supervision, the fail-closed/fail-open asymmetry
    /// and every pre-flight check live in `hooks.rs`.
    pub async fn run_hooks(&self, event: HookEvent, call: &HookCall<'_>) -> HookVerdict {
        hooks::run(&self.hooks, event, call).await
    }

    /// Run every `UserPromptSubmit` hook over the words a person just typed.
    ///
    /// **Deliberately not reachable from the turn loop.** The loop calls the
    /// model many times per goal and a delegated run calls it many times more;
    /// this fires once, at the place a human's text becomes a goal. Keeping the
    /// call site out there rather than in `run_goal` is what makes "once per
    /// user prompt, and never for a subagent's brief" a property of the call
    /// graph instead of a flag someone has to remember to set — a delegation
    /// constructs its `Goal` directly and there is no path from it to here.
    ///
    /// Returns cheaply when nothing is configured, so the caller needs no
    /// `if` around it: no hooks means no runs, no context and no block.
    pub async fn on_user_prompt(
        &self,
        prompt: &str,
        session_id: &str,
        transcript_path: &str,
    ) -> PromptVerdict {
        // The working directory is read here rather than passed, because it is
        // the process's and the caller has no better answer than
        // `current_dir()` — a parameter would only be a chance to send a
        // different one. A directory that cannot be read is sent as empty
        // rather than guessed at.
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        hooks::run_prompt(
            &self.hooks,
            &PromptCall {
                prompt,
                session_id,
                transcript_path,
                cwd: &cwd,
            },
        )
        .await
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
                let (front, body, _) = claude::split_agent(&raw);
                // An empty list means "inherit", not "nothing" — see
                // `claude::Tools::allowlist`. It matters here as well as for
                // delegation: a selected persona whose `tools: []` was read as an
                // empty allowlist would boot with no tools at all and look like a
                // model that refuses to work.
                tools = claude::Tools::allowlist(front.tools);
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
/// **Strict for `.emma/`.** Both fields are required and `deny_unknown_fields`
/// is on, so a `SKILL.md` missing either — or carrying any third key — fails the
/// load and takes the boot with it. In Emma's own format an unknown key can only
/// be the user's typo, and this is the attribute that catches it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Front {
    name: String,
    description: String,
}

/// The same two fields, read from a `.claude/skills/` file, where the rest of the
/// frontmatter is somebody else's business.
///
/// **The ruling, and it is the `settings.json` argument one level down.** Real
/// skills on a working machine carry `model-role`, `version` and
/// `allowed-tools`; under the strict struct above, every one of them took the
/// whole boot down. A rule that fires on correct configuration is an outage, not
/// a safety property — and unlike `.emma/`, an unrecognised key here is not a
/// typo in Emma's format, it is a key Emma has no opinion about in someone
/// else's. So unknown keys are ignored, and a file that cannot be read at all is
/// skipped with a warning rather than taken as a reason not to start.
///
/// `name` and `description` stay required, because they are not decoration: they
/// are the catalogue line the model chooses the skill from, and a skill with no
/// name cannot be asked for.
#[derive(Deserialize)]
struct ClaudeFront {
    name: String,
    description: String,
}

impl From<ClaudeFront> for Front {
    fn from(f: ClaudeFront) -> Self {
        Self {
            name: f.name,
            description: f.description,
        }
    }
}

/// Load the skills, and hand back what was said about them.
///
/// **The notes are returned, not merely printed, and that is the whole of this
/// change.** `HARD-001` exists because skipped skills were named on stderr and
/// never counted; the fix added a count — on stderr. So the guarantee stayed
/// exactly as observable as it had been, which is to say not at all, and a
/// mutation deleting the entire summary left the suite green.
///
/// The shape to copy was one file away, as usual: `claude::load_agents` already
/// returns `(agents, notes)` and `Harness::agent_notes` already exposes them.
/// Skills never got it.
fn load_skills(
    root: &Path,
    spine_path: &Path,
    flavor: Flavor,
    elected: Option<&[String]>,
) -> Result<(Vec<SkillDef>, Vec<String>)> {
    let dir = root.join("skills");
    let mut notes: Vec<String> = Vec::new();
    // A `BTreeMap` keyed on the front-matter name gives the sorted catalogue the
    // prompt prefix needs, whatever order the directory iterated in.
    let mut found: BTreeMap<String, SkillDef> = BTreeMap::new();
    // Counted, not merely named. Each skip already printed a line, and on a real
    // corpus that is 116 lines nobody adds up — the one genuine YAML error hides
    // among 101 line-ending complaints, and a catalogue a third short looks
    // exactly like a catalogue that is complete. The total is the number that
    // tells an operator something is wrong.
    let mut skipped = 0usize;
    if dir.is_dir() {
        for entry in
            std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))?
        {
            // A subdirectory without a `SKILL.md` is skipped without complaint,
            // and a loose file in `skills/` is ignored.
            let path = entry?.path().join("SKILL.md");
            if !path.is_file() {
                continue;
            }
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let (front, body) = match (split_skill(&text, &path, flavor), flavor) {
                (Ok(v), _) => v,
                // Emma's own format: a file this loader cannot read is a mistake
                // in a file the operator wrote for Emma, and it stops the boot
                // like every other one.
                (Err(e), Flavor::Emma) => return Err(e),
                // A foreign format. The skill is unusable either way; the choice
                // is only whether it costs the operator this skill or every
                // skill. It is named on stderr rather than dropped, because a
                // catalogue quietly one shorter than the directory is the kind of
                // gap nobody notices until the model cannot find a skill that is
                // plainly there.
                (Err(e), Flavor::Claude) => {
                    eprintln!("emma: skipping skill {}: {e:#}", path.display());
                    skipped += 1;
                    continue;
                }
            };
            // Two skills may declare the same `name:` from different
            // directories, and the catalogue is keyed on the name — so one of
            // them silently replaced the other, with the winner decided by
            // whatever order the filesystem handed the directory back. On the
            // owner's own machine two directories both declare `gstack`.
            //
            // Reported for the same reason the parse skip above is: a catalogue
            // quietly one shorter than the directory is the gap nobody notices
            // until the model cannot find a skill that is plainly there. The
            // sibling agent loader already does this with a boot note
            // (`claude.rs`, `load_agents`); skills never got one.
            if let Some(previous) = found.get(&front.name) {
                // **Counted, not only named.** A collision drops a skill from
                // the catalogue exactly as a parse failure does, and this arm
                // did not touch `skipped` — so the total below under-reported
                // the real shortfall, and an operator reconciling "I have 12
                // skills and Emma lists 10" was given a number that did not
                // account for both of them.
                skipped += 1;
                notes.push(format!(
                    "two skills both declare the name `{}` — {} is in the catalogue and {} \
                     replaces it; which one wins depends on the order the filesystem returned \
                     the directory in, so it may differ between runs",
                    front.name,
                    previous.name,
                    path.display()
                ));
                eprintln!(
                    "emma: two skills both declare the name `{}` — {} is in the catalogue and \
                     {} replaces it. Only one can be loaded under one name, and which one wins \
                     depends on the order the filesystem returned the directory in, so it may \
                     differ between runs. Rename one of them.",
                    front.name,
                    previous.name,
                    path.display()
                );
            }
            found.insert(
                front.name.clone(),
                SkillDef::new(front.name, front.description, body),
            );
        }
    }
    if skipped > 0 {
        notes.push(format!(
            "{skipped} skill(s) in {} were skipped and are not in the catalogue",
            dir.display()
        ));
        eprintln!(
            "emma: {skipped} skill(s) in {} were skipped and are not in the catalogue; \
             each is named above with its reason.",
            dir.display()
        );
    }
    let Some(elected) = elected else {
        return Ok((found.into_values().collect(), notes));
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
    Ok((out.into_values().collect(), notes))
}

fn split_skill(text: &str, path: &Path, flavor: Flavor) -> Result<(Front, String)> {
    let named = |what: &str| format!("{}: {what}", path.display());
    let text = match flavor {
        Flavor::Emma => text,
        Flavor::Claude => after_licence_header(text),
    };
    // `claude::open_frontmatter`, not a local byte-exact opener. The exact
    // form dropped 101 of 323 real skills on this machine — every one written
    // by an editor that ends lines with CRLF — while the agent parser one file
    // away had already been fixed for precisely that. See its doc.
    let rest = crate::claude::open_frontmatter(text)
        .with_context(|| named("expected YAML frontmatter"))?;
    // `close_frontmatter`, not a local `find("\n---")`. The local form required a
    // newline before the closer, so an EMPTY block -- `---` immediately followed
    // by `---` -- was never closed, and this call refused the file with
    // "frontmatter is not closed": loud, and the wrong diagnosis. The two
    // sibling callers got two DIFFERENT wrong answers from the same missing
    // case. Same argument as the opener above: share the function.
    let (yaml, body) = crate::claude::close_frontmatter(rest)
        .with_context(|| named("frontmatter is not closed"))?;
    let front: Front = match flavor {
        Flavor::Emma => serde_yaml::from_str(yaml),
        Flavor::Claude => serde_yaml::from_str::<ClaudeFront>(yaml).map(Front::from),
    }
    .with_context(|| named("bad frontmatter"))?;
    Ok((front, body.trim_start().to_string()))
}

/// Step over an HTML comment before the frontmatter, `.claude/` only.
///
/// A licence header above the `---` is common enough to be worth handling rather
/// than skipping: the file is well-formed, it simply does not open with its own
/// first line, and refusing it would drop a skill for a reason the operator can
/// do nothing about. It steps over comments and blank lines and nothing else — a
/// file with real prose above its frontmatter is not a skill with a header, it is
/// a file whose frontmatter is somewhere in the middle, and guessing at that is
/// how a parser starts reading text nobody meant as configuration.
///
/// A `---\r\n` opener and a byte-order mark **are** covered, by
/// `claude::open_frontmatter`, which every caller now reaches. This comment used
/// to say they were not, and it was written when that was true: the byte-exact
/// `---\n` opener silently dropped 101 of 323 real skill files on a machine whose
/// editor writes CRLF.
fn after_licence_header(text: &str) -> &str {
    let mut rest = text.trim_start();
    while let Some(body) = rest.strip_prefix("<!--") {
        let Some(end) = body.find("-->") else {
            return rest;
        };
        rest = body[end + 3..].trim_start();
    }
    rest
}

/// A command file's body, with any frontmatter block removed.
///
/// Tolerant in the same direction as everything else that reads a foreign
/// format: a file with no frontmatter, or with an opener that never closes, is
/// returned whole rather than refused. A command is prose a person summons, and
/// losing it over a malformed `---` would be a worse outcome than showing a
/// stray line.
fn strip_frontmatter(text: &str) -> &str {
    let Some(rest) = crate::claude::open_frontmatter(text) else {
        return text;
    };
    match crate::claude::close_frontmatter(rest) {
        Some((_, body)) => body,
        None => text,
    }
}

/// Load `commands/*.md`, and **return** what was passed over.
///
/// **The count used to reach an `eprintln!` and nothing else**, which is
/// `HARD-001`'s reopened defect verbatim: a number nothing can read is a
/// comment. That row's own text says the shape to copy was one file away —
/// `load_skills` already returned its notes and `Harness` already exposed
/// `skill_notes`. This is that shape, applied to the sibling that was left out.
///
/// A reviewer proved the gap the direct way: the only test asserts
/// `command_names() == ["top"]`, which stays true whether the nested files are
/// counted, reported, or ignored entirely.
fn load_commands(root: &Path) -> Result<(BTreeMap<String, String>, Vec<String>)> {
    let dir = root.join("commands");
    let mut out = BTreeMap::new();
    let mut notes = Vec::new();
    if !dir.is_dir() {
        return Ok((out, notes));
    }
    // Top level only, by design: a command is summoned as `/name`, and a name
    // taken from a nested path is either ambiguous or ugly. But a subdirectory
    // full of real commands contributing nothing, with no report, is the same
    // quiet gap as a skipped skill — 14 such files exist on the owner's machine.
    let mut nested = 0usize;
    for entry in std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            nested += std::fs::read_dir(&path)
                .map(|it| {
                    it.flatten()
                        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("md"))
                        .count()
                })
                .unwrap_or(0);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let name = path.file_stem().unwrap_or_default().to_string_lossy();
        let body = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        // Frontmatter is metadata about the command, not part of it. 59 of the
        // 116 real commands on the owner's machine carry a `---` block, and all
        // of it used to be pasted into the model's context as if the operator
        // had typed `description: ...` at the prompt — including keys Emma does
        // not act on, which then read as instructions.
        //
        // Third caller of `open_frontmatter`, and the point of it being shared:
        // a command written on Windows has the same CRLF opener a skill does.
        out.insert(
            name.into_owned(),
            strip_frontmatter(&body).trim().to_string(),
        );
    }
    if nested > 0 {
        notes.push(format!(
            "{nested} command file(s) below {} are in subdirectories and were not loaded — \
             only `commands/*.md` at the top level becomes a `/name`. Move them up if they \
             are wanted.",
            dir.display()
        ));
    }
    Ok((out, notes))
}

// endregion: Loading
