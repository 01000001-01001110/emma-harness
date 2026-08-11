//! Reading a `.claude/` directory as an Emma harness.
//!
//! Emma runs on the Anthropic API; it does not shell out to the `claude` binary
//! and does not use the Agent SDK (`notes/claude-code-compatibility.md`). What
//! remains is one cheap, useful thing: **recognise `.claude/` configuration when
//! we find it**, so a skill or command already written for Claude Code can be
//! used without being rewritten.
//!
//! `skills/<name>/SKILL.md` and `commands/<name>.md` are the same shape and are
//! read by the same code as Emma's own — see `load_skills` and `load_commands`
//! in `lib.rs`, neither of which appears here. This file exists for the two
//! things that are not the same shape: where the standing instructions live, and
//! the spelling of the hooks block. It also carries `agents/`, which is the
//! nearest thing Claude Code has to a persona.
//!
//! Skill frontmatter found under `.claude/` is read permissively — unknown keys
//! are ignored and a file that cannot be parsed is skipped with a warning rather
//! than taking the boot down. It was strict, and real skills carrying
//! `model-role`, `version` or `allowed-tools` meant Emma would not start at all;
//! `ClaudeFront` in `lib.rs` argues the ruling. `.emma/`'s own skills stay
//! strict, because there an unknown key is a typo in Emma's format.
//!
//! **Two rules from the note are firm and are enforced here and in `discover`:**
//! `.emma/` wins outright when both exist — never merged, because merging is how
//! the answer to "where did this instruction come from" stops being a file — and
//! a config naming a hook event Emma does not implement is a loud startup error,
//! never a silent skip.
//!
//! **Where this file deliberately relaxes `deny_unknown_fields`, and why.**
//! Emma's own `config.json` denies unknown fields everywhere: it is Emma's
//! format, so a key Emma does not know is a typo. `settings.json` is *Claude
//! Code's* format and legitimately carries `permissions`, `model`, `env`,
//! `statusLine` and more that Emma has no opinion about. Denying those would
//! mean Emma refuses to start in essentially every real Claude Code repository —
//! a rule that fires on correct configuration is not a safety property, it is an
//! outage. So the outer object is permissive and the **hooks block is strict**,
//! which is where a silently-ignored key would cost something.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::hooks::{HookDef, HookEvent};

// region: settings.json, and translating a hook command
// ---------------------------------------------------------------------------
// settings.json, and translating a hook command
//
// Claude Code's spelling of the hooks block, flattened into Emma's. This is the
// half of the file where the two systems disagree rather than merely differ:
// permissive at the outer level because the format is not Emma's, strict inside
// the hooks block because that is Emma's security surface.
// ---------------------------------------------------------------------------

/// The subset of `settings.json` Emma reads. Unknown keys are ignored on
/// purpose — see the module docs.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct Settings {
    #[serde(default)]
    hooks: BTreeMap<String, Vec<Group>>,
    /// The bottom row of the screen, when the user named a program to draw it.
    /// Read rather than ignored as of `statusline.rs`, which carries the whole
    /// argument — including why this one key is honoured while the rest of the
    /// file still is not.
    #[serde(default, rename = "statusLine")]
    pub(crate) status_line: Option<crate::statusline::StatusLineBlock>,
}

/// One matcher and the commands attached to it. Strict, because this is the
/// security-relevant part: a misspelled `mathcer` here would produce a hook that
/// guards every tool instead of one, and it would look like it was working.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Group {
    #[serde(default)]
    matcher: Option<String>,
    #[serde(default)]
    hooks: Vec<Entry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    #[serde(rename = "type")]
    kind: String,
    command: String,
    /// Claude Code counts this in **seconds**. Emma's spine counts milliseconds.
    #[serde(default)]
    timeout: Option<u64>,
}

/// Anything in a command string that means a shell would have to interpret it.
/// Emma execs an argv, so a string needing a shell cannot be honoured — see
/// `translate_command`.
///
/// Whitespace is handled separately and as a class rather than listed here. It
/// used to be one explicit `' '` check beside this set, which left the tab out:
/// `hooks/guard.sh\t--strict` walked past the refusal and failed further down as
/// a missing file, so the operator got the right verdict with the wrong sentence
/// pointing at the wrong problem. Every character that separates arguments has
/// to be caught by the same rule, or the next one to be forgotten is the next
/// one added.
///
/// The rest are the shell's own operators: redirection and pipes, command and
/// variable substitution, quoting, globbing and brace/tilde expansion, history
/// and comments, and `%` for Windows' own expansion. A filename containing one
/// of these is refused; renaming a hook script is cheaper than deciding at
/// startup which of them a shell would have acted on.
const SHELL_METACHARACTERS: &[char] = &[
    '|', '&', ';', '<', '>', '(', ')', '$', '`', '"', '\'', '*', '?', '[', ']', '{', '}', '~', '#',
    '!', '%', '=',
];

impl Settings {
    pub(crate) fn read(root: &Path) -> Result<(Self, String)> {
        let path = root.join("settings.json");
        if !path.is_file() {
            return Ok((Self::default(), String::new()));
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        if raw.trim().is_empty() {
            return Ok((Self::default(), raw));
        }
        let parsed = serde_json::from_str(&raw)
            .with_context(|| format!("{} is malformed", path.display()))?;
        Ok((parsed, raw))
    }

    /// Flatten Claude Code's event → groups → commands nesting into Emma's flat
    /// `name → HookDef` map.
    ///
    /// Names are synthesised as `<Event>[g][h]` from the position in the file.
    /// They are stable for a given file and they sort deterministically, which
    /// is all the ordering contract requires; there is nothing in the source
    /// format to name them after.
    pub(crate) fn into_hook_defs(self, root: &Path) -> Result<BTreeMap<String, HookDef>> {
        let mut out = BTreeMap::new();
        for (event, groups) in self.hooks {
            // The loud failure. Emma implements two events; the rest are real
            // Claude Code events that would silently never fire here, and an
            // operator who wrote a `Stop` guard would believe they had one.
            HookEvent::parse(&event).with_context(|| {
                format!("{}: hooks.{event}", root.join("settings.json").display())
            })?;
            for (g, group) in groups.into_iter().enumerate() {
                for (h, entry) in group.hooks.into_iter().enumerate() {
                    let name = format!("{event}[{g}][{h}]");
                    if entry.kind != "command" {
                        bail!(
                            "{}: hook `{name}` has type `{}`; Emma runs command hooks only",
                            root.join("settings.json").display(),
                            entry.kind
                        );
                    }
                    out.insert(
                        name.clone(),
                        HookDef {
                            event: event.clone(),
                            command: translate_command(
                                root,
                                &format!("hook `{name}`"),
                                &entry.command,
                            )?,
                            matcher: group.matcher.clone(),
                            // Seconds there, milliseconds here.
                            timeout_ms: entry.timeout.map(|s| s.saturating_mul(1_000)),
                            text: None,
                        },
                    );
                }
            }
        }
        Ok(out)
    }
}

/// Turn a Claude Code command string into a path Emma can exec, or refuse.
///
/// Used by both things a `settings.json` can point Emma at — a hook and a
/// `statusLine` — because they are the same hazard wearing two names, and
/// `name` is what puts the caller into the sentence.
///
/// **This is the one place the two systems genuinely disagree, and Emma does not
/// blink.** Claude Code's `command` is a shell string: it may pipe, expand
/// variables, or name any executable on the box. Emma canonicalises a path,
/// containment-checks it inside `hooks/`, and execs an argv with a cleared
/// environment. Honouring a shell string would mean dropping every one of those,
/// at the exact point where Emma is deciding whether to let a model run `Bash`.
///
/// So: `$CLAUDE_PROJECT_DIR/.claude/hooks/x.sh` and `.claude/hooks/x.sh` and
/// `hooks/x.sh` all resolve. Anything else is a startup error that says what to
/// do about it — which is the same ruling as the unimplemented event, for the
/// same reason. A guard that cannot be honoured must not be quietly dropped.
pub(crate) fn translate_command(root: &Path, name: &str, raw: &str) -> Result<String> {
    let dir_name = root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".claude".into());
    let mut rest = raw.trim();
    for prefix in ["${CLAUDE_PROJECT_DIR}/", "$CLAUDE_PROJECT_DIR/"] {
        if let Some(stripped) = rest.strip_prefix(prefix) {
            rest = stripped;
            break;
        }
    }
    // `$CLAUDE_PROJECT_DIR` names the directory *containing* `.claude/`, so a
    // path relative to it still carries the directory name.
    let rest = rest
        .strip_prefix(&format!("{dir_name}/"))
        .unwrap_or(rest)
        .trim_start_matches("./");

    if rest.is_empty() || rest.contains(SHELL_METACHARACTERS) || rest.contains(char::is_whitespace)
    {
        bail!(
            "{name}: `{raw}` is a shell command. Emma execs a contained \
             executable with a cleared environment and cannot honour a shell \
             string without dropping that. Move it into {}/hooks/ and reference \
             it by path",
            root.display()
        );
    }
    // `is_absolute()` alone is not enough: on Windows `/usr/bin/true` is a
    // *relative* path, so a unix-authored config would fall through to the
    // canonicalise-and-contain check and be refused for the wrong reason — with
    // an error about a missing file rather than about containment. The rule is
    // the same on both platforms, so the check has to be too.
    let rooted =
        rest.starts_with('/') || rest.starts_with('\\') || rest.as_bytes().get(1) == Some(&b':');
    if rooted || Path::new(rest).is_absolute() {
        bail!(
            "{name}: `{raw}` is an absolute path. A command named by \
             configuration must live inside {}/hooks/ so the config cannot run \
             arbitrary executables",
            root.display()
        );
    }
    Ok(rest.to_string())
}

// endregion: settings.json, and translating a hook command

// region: Agents — Claude Code's nearest thing to a persona
// ---------------------------------------------------------------------------
// Agents — Claude Code's nearest thing to a persona
//
// One file carrying two things Emma wants: a prompt layer and a tool allowlist.
// Permissive frontmatter throughout, because an agent file is written for a
// program with more settings than Emma has.
// ---------------------------------------------------------------------------

/// The frontmatter fields Emma reads from `agents/<name>.md`. Permissive for the
/// same reason `Settings` is: `category`, `version`, `color` and whatever else
/// Claude Code grows are not Emma's business, and refusing to boot over them
/// would make this compatibility feature an obstacle. **It must never gain
/// `deny_unknown_fields`** — measured against a real library of 90 agent files,
/// `category` and `version` appear throughout and every one of them would have
/// been a boot failure.
///
/// Four fields are read and every one of them has a reader:
///
/// - `name` — checked against the file stem, which wins. See [`agents`].
/// - `description` — what the calling model reads to choose an agent, and the
///   only field a delegation target cannot do without. 63 of those 90 files
///   carry it beside a `name`, 23 carry it alone, and 4 carry neither.
/// - `tools` — the allowlist, in either spelling. See [`Tools::allowlist`] for
///   what an *empty* one means, which is the trap in this format.
/// - `model` — a per-agent model override. Honoured, because a file that says
///   `model: claude-sonnet-4-5` and runs on something else is a lie the user
///   cannot see.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct AgentFront {
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) description: Option<String>,
    #[serde(default)]
    pub(crate) tools: Option<Tools>,
    #[serde(default)]
    pub(crate) model: Option<String>,
    /// Model calls this agent gets. Claude Code spells it `maxTurns`; both
    /// spellings are accepted because Emma's own files are snake_case and the
    /// foreign ones are not, and a budget that silently does not apply is worse
    /// than no budget.
    #[serde(default, alias = "maxTurns")]
    pub(crate) max_turns: Option<u32>,
    /// Weighted tokens this agent may spend, out of what remains of the
    /// caller's. See `emma::delegate` for the ceiling it is clamped to.
    #[serde(default, alias = "maxTokens")]
    pub(crate) max_tokens: Option<i64>,
}

/// Claude Code writes `tools` either as a YAML list or as one comma-separated
/// string. Both are common in the wild; accepting one and silently ignoring the
/// other would produce an empty allowlist, which under Emma's rules means "no
/// tools at all" — a spectacular way to fail quietly.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum Tools {
    List(Vec<String>),
    Csv(String),
}

impl Tools {
    pub(crate) fn into_vec(self) -> Vec<String> {
        match self {
            Self::List(v) => v,
            Self::Csv(s) => s
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
        }
    }

    /// The allowlist an agent file declares, where **empty means "inherit"
    /// rather than "nothing"**.
    ///
    /// This is the trap in the format and it is live. `tools:` with no value
    /// parses as YAML null and never reaches here; `tools: []` and `tools: ""`
    /// do, and under Emma's rules an empty allowlist means *no tools at all* —
    /// which is the failure the note on [`Tools`] predicts, arriving through the
    /// value rather than through the spelling. In Claude Code an absent `tools`
    /// means inherit everything, and a present-but-empty one is the same
    /// statement written differently: nobody writes `tools: []` meaning "this
    /// agent may do nothing".
    ///
    /// So all three collapse to `None`, and `None` is the caller's signal to
    /// inherit. An agent that really should have no tools is expressed by not
    /// registering it, not by a list that reads as a typo.
    pub(crate) fn allowlist(front: Option<Self>) -> Option<Vec<String>> {
        let names = front?.into_vec();
        (!names.is_empty()).then_some(names)
    }
}

/// Split an agent file into the frontmatter Emma reads and the body it puts in
/// the prompt.
///
/// The frontmatter is **stripped**, and that is the one place the "nothing is
/// trimmed or re-wrapped" rule is bent. It is bent knowingly: `model:` and
/// `tools:` are configuration, and sending them to the model as standing
/// instructions would be telling it about machinery it cannot use. A file with
/// no frontmatter is all body, which is also the correct answer.
/// **Line endings are tolerated, and that is not tidiness.** This required
/// `---\n` exactly, so every CRLF agent file — which is every one of them on a
/// Windows machine, and 90 out of 90 in the library this was measured against —
/// fell through to "no frontmatter": its `tools` was ignored, its `description`
/// was invisible, and its `name:`, `model:` and `category:` lines were handed to
/// the model as standing instructions. Nothing failed; the file simply did not
/// mean what it said. A byte-exact opener is a parser that works on the machine
/// its author used.
///
/// The third element is the YAML error, when the frontmatter was found and could
/// not be read. It used to be `unwrap_or_default()` and nothing else, which is
/// how the failure above stayed invisible: an unreadable file and a file with no
/// frontmatter produced the same empty value, and neither said so.
pub(crate) fn split_agent(text: &str) -> (AgentFront, &str, Option<String>) {
    // A byte-order mark before the opener is common from Windows editors, and it
    // is the same class of silent miss.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some(rest) = open_frontmatter(text) else {
        return (AgentFront::default(), text, None);
    };
    let Some(end) = rest.find("\n---") else {
        return (AgentFront::default(), text, None);
    };
    let body = rest[end + 4..].trim_start_matches(['\r', '\n']);
    match serde_yaml::from_str(&rest[..end]) {
        Ok(front) => (front, body, None),
        Err(e) => (AgentFront::default(), body, Some(e.to_string())),
    }
}

/// `---` on the first line, whatever the file's line endings are.
fn open_frontmatter(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("---")?;
    let rest = rest.strip_prefix('\r').unwrap_or(rest);
    rest.strip_prefix('\n')
}

/// The agent files available to select **or to delegate to**, by file stem,
/// sorted.
///
/// **The file stem is the name, and a `name:` in frontmatter that disagrees
/// loses.** The stem is what `EMMA_PERSONA` names, what `agent_path` builds,
/// and the only one of the two guaranteed unique — two files may declare the
/// same `name`, and a catalogue keyed on a value that can collide silently
/// drops one of them. The disagreement is worth saying out loud rather than
/// resolving quietly, so `load_agents` notes it.
///
/// Top-level `*.md` only. A real library has subdirectories under `agents/`
/// (23 of them, 205 files, on the machine this was measured against) and
/// walking into them would flatten two namespaces into one where a collision is
/// resolved by directory iteration order. Stated rather than discovered.
pub(crate) fn agents(root: &Path) -> Result<Vec<String>> {
    let dir = root.join("agents");
    let mut out = Vec::new();
    if !dir.is_dir() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) == Some("md") {
            out.push(
                path.file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    out.sort();
    Ok(out)
}

pub(crate) fn agent_path(root: &Path, name: &str) -> PathBuf {
    root.join("agents").join(format!("{name}.md"))
}

/// Every agent file in `<root>/agents/`, read as a delegation target, plus the
/// notes about the ones that were dropped and why.
///
/// **Nothing here fails the boot.** A persona is selected by the operator at
/// startup and a bad one is their mistake; an agent library is a directory
/// somebody accumulated over months, and refusing to start because one file in
/// ninety has no `description` is the outage this crate keeps ruling against.
/// The dropped ones are named instead, and the names are shown by
/// `emma config check` and at startup.
///
/// `description` is the one field a target cannot do without: it is what the
/// calling model reads to choose. A file with a body and no description is a
/// persona, not a delegation target, and it stays available as the former.
pub(crate) fn load_agents(root: &Path) -> Result<(Vec<crate::AgentDef>, Vec<String>)> {
    let mut out = Vec::new();
    let mut notes = Vec::new();
    for name in agents(root)? {
        let path = agent_path(root, &name);
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(e) => {
                notes.push(format!("agent `{name}` was not read: {e}"));
                continue;
            }
        };
        let (front, body, bad_yaml) = split_agent(&raw);
        if let Some(problem) = bad_yaml {
            notes.push(format!(
                "agent `{name}`: its frontmatter could not be read, so nothing in it applied — \n                 not offered for delegation. {problem}"
            ));
            continue;
        }
        if let Some(declared) = front.name.as_deref() {
            if declared != name {
                notes.push(format!(
                    "agent `{name}` declares name `{declared}`; the file name wins"
                ));
            }
        }
        let Some(description) = front.description.filter(|d| !d.trim().is_empty()) else {
            notes.push(format!(
                "agent `{name}` has no description, so nothing could tell a model when to \
                 use it — not offered for delegation"
            ));
            continue;
        };
        out.push(crate::AgentDef {
            name,
            description: description.trim().to_string(),
            instructions: body.to_string(),
            tools: Tools::allowlist(front.tools),
            model: front.model,
            max_turns: front.max_turns,
            max_tokens: front.max_tokens,
        });
    }
    Ok((out, notes))
}

// endregion: Agents — Claude Code's nearest thing to a persona
